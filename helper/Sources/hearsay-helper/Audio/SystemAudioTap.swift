import AVFoundation
import CoreAudio
import Darwin

/// Real-time IOProc worker. Runs on the Core Audio HAL thread, so it does the minimum possible: copy
/// the tap's raw mono float samples straight out of the `AudioBufferList` into a lock-free-throughput
/// ``SPSCFloatRing``. No allocation, no `AVAudioConverter`, no amplitude scan, no lock held across a
/// copy loop — all of that moves to the drain worker. Holds only the ring (thread-safe), so it never
/// touches the tap's mutable build state.
private final class TapIOEngine: @unchecked Sendable {
    private let ring: SPSCFloatRing

    init(ring: SPSCFloatRing) {
        self.ring = ring
    }

    func process(_ inData: UnsafePointer<AudioBufferList>) {
        let ab = inData.pointee.mBuffers  // mono tap -> a single buffer
        guard let mData = ab.mData, ab.mDataByteSize > 0 else { return }
        let count = Int(ab.mDataByteSize) / MemoryLayout<Float>.stride
        ring.write(mData.assumingMemoryBound(to: Float.self), count: count)
    }
}

/// System-audio capture ("Them") via a Core Audio process tap.
///
/// Builds a **global-except-self**, mono mixdown tap (dodges the Teams per-process-silent bug and
/// covers browser meeting apps), wraps it in a private aggregate device, and drains its IOProc.
///
/// Real-time hygiene: the HAL IOProc only memcpys raw samples into an ``SPSCFloatRing``; a dedicated
/// worker thread drains that ring, resamples to 16 kHz off the RT thread, and writes into the shared
/// output ``RingBuffer`` the uplink reads. A watchdog listens for the default-output-device and
/// nominal-sample-rate changes that strand a tap on zero buffers and rebuilds both tap and aggregate;
/// it also rebuilds, with backoff, on either of the two ways a tap dies, emitting `tap_health` on each
/// state transition (edge-triggered, so no per-tick event spam):
///
/// - *stuck*: the IOProc stops firing, so no samples reach the ring at all (audio-flow cadence).
/// - *stranded*: the IOProc keeps firing at full cadence but delivers only exact zeros, so cadence
///   looks perfectly healthy while no system audio is captured. Amplitude alone cannot tell this from
///   legitimately quiet system audio — ``outputDeviceIsRunningSomewhere()`` is what disambiguates it,
///   by asking whether anything is playing at all. A needless rebuild during real silence costs a
///   sub-second gap of nothing; a missed one costs the rest of the meeting.
final class SystemAudioTap: AudioSource, @unchecked Sendable {
    private let ring: RingBuffer  // output ring (16 kHz), read by the uplink
    private let onHealth: (_ state: String, _ action: String?) -> Void
    private let log: (String) -> Void
    private let clock = MonotonicClock()

    private let lock = NSLock()
    private var running = false
    private var tapID = AudioObjectID(kAudioObjectUnknown)
    private var aggregateID = AudioObjectID(kAudioObjectUnknown)
    private var procID: AudioDeviceIOProcID?
    private var resampler: Resampler?  // used only by the drain worker
    private var spscRing: SPSCFloatRing?  // raw device-rate handoff, IOProc -> worker

    // Drain worker: resamples off the RT thread. Torn down (flag + join) before the SPSC ring is
    // released, and never touches `lock`, so teardown can hold `lock` while joining it.
    private var workerThread: Thread?
    private var workerStop: StopFlag?
    private var workerDone: DispatchSemaphore?

    // Watchdog.
    private let flow: FlowMonitor
    private let silence = SilenceMonitor()
    private let watchdogQueue = DispatchQueue(label: "hearsay.tap.watchdog")
    private var watchedDevice = AudioObjectID(kAudioObjectUnknown)
    private var defaultDeviceListener: AudioObjectPropertyListenerBlock?
    private var sampleRateListener: AudioObjectPropertyListenerBlock?
    private var listeningDevices = false
    private var watchdogTimer: DispatchSourceTimer?
    private var broken = false  // edge-trigger state for `tap_health`
    private var graphStartNs: UInt64 = 0  // when the current graph started (grace window baseline)
    private var rebuildBackoffNs: UInt64 = 0  // grows per failed rebuild, reset on recovery
    private var nextRebuildAtNs: UInt64 = 0
    // Only exact zeros count as silence: any real playback, however quiet, carries a non-zero sample,
    // so the stranded axis trips on a dead tap rather than on a quiet one. Held well above the stuck
    // threshold because a live meeting can legitimately go a while without far-side audio.
    private let liveness = TapLivenessPolicy(
        stuckThresholdNs: 5_000_000_000, strandedThresholdNs: 60_000_000_000)
    private let maxBackoffNs: UInt64 = 30_000_000_000
    private let workerTick = 0.01  // 10 ms drain period

    init(
        ring: RingBuffer,
        onHealth: @escaping (_ state: String, _ action: String?) -> Void,
        log: @escaping (String) -> Void
    ) {
        self.ring = ring
        self.onHealth = onHealth
        self.log = log
        self.flow = FlowMonitor(nowNs: clock.nowNs())
    }

    func start() throws {
        lock.lock()
        defer { lock.unlock() }
        guard !running else { return }
        try buildGraphLocked()
        running = true
        broken = false
        silence.reset()
        rebuildBackoffNs = 0
        nextRebuildAtNs = 0
        installDeviceListenersLocked()
        installWatchdogTimerLocked()
    }

    func stop() {
        lock.lock()
        defer { lock.unlock() }
        guard running else { return }
        running = false
        removeWatchdogTimerLocked()
        removeDeviceListenersLocked()
        teardownGraphLocked()
    }

    // MARK: - Build / teardown (under `lock`)

    private func buildGraphLocked() throws {
        let exclude: [AudioObjectID] = selfAudioProcessObject().map { [$0] } ?? []
        let desc = CATapDescription(monoGlobalTapButExcludeProcesses: exclude)
        desc.uuid = UUID()
        desc.isPrivate = true
        desc.muteBehavior = .unmuted  // keep audio audible to the user while tapping
        let tapUID = desc.uuid.uuidString

        var newTap = AudioObjectID(kAudioObjectUnknown)
        let tapStatus = AudioHardwareCreateProcessTap(desc, &newTap)
        guard tapStatus == noErr, newTap != AudioObjectID(kAudioObjectUnknown) else {
            throw CaptureError.createTap(tapStatus)
        }
        tapID = newTap

        guard var asbd = tapStreamFormat(tapID),
            let inFormat = AVAudioFormat(streamDescription: &asbd)
        else {
            teardownGraphLocked()
            throw CaptureError.tapFormat
        }
        // The RT IOProc reads the buffer as flat mono float32; require exactly that.
        guard inFormat.commonFormat == .pcmFormatFloat32, inFormat.channelCount == 1 else {
            teardownGraphLocked()
            throw CaptureError.unsupportedTapFormat(
                "\(inFormat.channelCount)ch \(inFormat.commonFormat.rawValue)")
        }
        guard let rs = Resampler(from: inFormat) else {
            teardownGraphLocked()
            throw CaptureError.resamplerInit
        }
        resampler = rs
        // ~2 s of device-rate slack between the RT IOProc and the drain worker.
        let raw = SPSCFloatRing(capacity: max(16_000, Int(inFormat.sampleRate * 2)))
        spscRing = raw

        let aggUID = "com.hearsay.aggregate.\(tapUID)"
        let dict: [String: Any] = [
            kAudioAggregateDeviceUIDKey: aggUID,
            kAudioAggregateDeviceNameKey: "Hearsay Aggregate",
            kAudioAggregateDeviceIsPrivateKey: true,
            kAudioAggregateDeviceTapAutoStartKey: true,
            kAudioAggregateDeviceTapListKey: [
                [kAudioSubTapUIDKey: tapUID, kAudioSubTapDriftCompensationKey: true]
            ],
        ]
        var newAgg = AudioObjectID(kAudioObjectUnknown)
        let aggStatus = AudioHardwareCreateAggregateDevice(dict as CFDictionary, &newAgg)
        guard aggStatus == noErr, newAgg != AudioObjectID(kAudioObjectUnknown) else {
            teardownGraphLocked()
            throw CaptureError.createAggregate(aggStatus)
        }
        aggregateID = newAgg

        let engine = TapIOEngine(ring: raw)
        var newProc: AudioDeviceIOProcID?
        let procStatus = AudioDeviceCreateIOProcIDWithBlock(&newProc, aggregateID, nil) {
            _, inData, _, _, _ in
            engine.process(inData)
        }
        guard procStatus == noErr, let proc = newProc else {
            teardownGraphLocked()
            throw CaptureError.createIOProc(procStatus)
        }
        procID = proc

        let startStatus = AudioDeviceStart(aggregateID, proc)
        guard startStatus == noErr else {
            teardownGraphLocked()
            throw CaptureError.startDevice(startStatus)
        }
        // Stamp the graph start so the watchdog gives this fresh graph a full grace window before it
        // can be judged stuck, and only reports `recovered` once real audio actually flows on it.
        graphStartNs = clock.nowNs()
        startWorkerLocked(raw: raw, resampler: rs)
    }

    private func teardownGraphLocked() {
        stopWorkerLocked()  // join the worker before releasing the ring it drains
        if aggregateID != AudioObjectID(kAudioObjectUnknown) {
            if let proc = procID {
                AudioDeviceStop(aggregateID, proc)
                AudioDeviceDestroyIOProcID(aggregateID, proc)
            }
            AudioHardwareDestroyAggregateDevice(aggregateID)
            aggregateID = AudioObjectID(kAudioObjectUnknown)
        }
        procID = nil
        if tapID != AudioObjectID(kAudioObjectUnknown) {
            AudioHardwareDestroyProcessTap(tapID)
            tapID = AudioObjectID(kAudioObjectUnknown)
        }
        spscRing = nil
        resampler = nil
    }

    /// Start the drain worker. It captures everything it needs at creation and never touches `lock`,
    /// so `teardownGraphLocked` can hold `lock` while joining it. Exits when its `StopFlag` is set.
    private func startWorkerLocked(raw: SPSCFloatRing, resampler: Resampler) {
        let stop = StopFlag()
        let done = DispatchSemaphore(value: 0)
        workerStop = stop
        workerDone = done
        let output = ring
        let flow = self.flow
        let silence = self.silence
        let clock = self.clock
        let tick = workerTick
        let logDrops = log
        let thread = Thread {
            var scratch = [Float](repeating: 0, count: 4096)
            var lastDropped: UInt64 = 0
            while !stop.isSet {
                var flowed = false
                var nonZero = false
                while true {
                    let n = scratch.withUnsafeMutableBufferPointer {
                        raw.read(into: $0.baseAddress!, max: $0.count)
                    }
                    if n == 0 { break }
                    flowed = true  // the tap produced audio; flow is about tap liveness, not ASR output
                    if !nonZero {
                        for i in 0..<n where scratch[i] != 0 {
                            nonZero = true
                            break
                        }
                    }
                    let out = scratch.withUnsafeBufferPointer {
                        resampler.resample($0.baseAddress!, count: n)
                    }
                    if !out.isEmpty { output.write(out) }
                }
                // Both liveness axes, stamped only when the tap actually delivered: `flow` is cadence
                // (did anything arrive), `silence` is amplitude (was any of it non-zero).
                if flowed {
                    let nowNs = clock.nowNs()
                    flow.noteFlow(nowNs: nowNs)
                    silence.record(nonZero: nonZero, nowNs: nowNs)
                }
                let dropped = raw.droppedSamples
                if dropped > lastDropped {
                    logDrops("tap resample ring overrun: dropped \(dropped - lastDropped) samples")
                    lastDropped = dropped
                }
                Thread.sleep(forTimeInterval: tick)
            }
            done.signal()
        }
        thread.name = "hearsay-tap-resample"
        workerThread = thread
        thread.start()
    }

    private func stopWorkerLocked() {
        workerStop?.set()
        workerDone?.wait()  // bounded: the worker exits within one tick + drain
        workerStop = nil
        workerDone = nil
        workerThread = nil
    }

    /// Rebuild both tap and aggregate after a device change or a stuck tap. Emits no health event
    /// itself — the watchdog owns `tap_health` via flow detection, so a seamless rebuild is silent and
    /// a failed one leaves flow stale (the watchdog keeps retrying with backoff).
    private func rebuild() {
        lock.lock()
        defer { lock.unlock() }
        guard running else { return }
        log("tap: rebuilding capture graph")
        removeDeviceListenersLocked()
        teardownGraphLocked()
        do {
            try buildGraphLocked()
            installDeviceListenersLocked()
        } catch {
            log("tap rebuild failed: \(error)")
        }
    }

    // MARK: - Watchdog (under `lock`)

    private func installWatchdogTimerLocked() {
        guard watchdogTimer == nil else { return }
        let timer = DispatchSource.makeTimerSource(queue: watchdogQueue)
        timer.schedule(deadline: .now() + 1.0, repeating: 1.0)
        timer.setEventHandler { [weak self] in self?.watchdogTick() }
        timer.resume()
        watchdogTimer = timer
    }

    private func removeWatchdogTimerLocked() {
        watchdogTimer?.cancel()
        watchdogTimer = nil
    }

    private func installDeviceListenersLocked() {
        guard !listeningDevices else { return }
        var devAddr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDefaultOutputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        let devBlock: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
            self?.rebuild()
        }
        AudioObjectAddPropertyListenerBlock(
            AudioObjectID(kAudioObjectSystemObject), &devAddr, watchdogQueue, devBlock)
        defaultDeviceListener = devBlock

        watchedDevice = defaultOutputDevice() ?? AudioObjectID(kAudioObjectUnknown)
        if watchedDevice != AudioObjectID(kAudioObjectUnknown) {
            var srAddr = AudioObjectPropertyAddress(
                mSelector: kAudioDevicePropertyNominalSampleRate,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain)
            let srBlock: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
                self?.rebuild()
            }
            AudioObjectAddPropertyListenerBlock(watchedDevice, &srAddr, watchdogQueue, srBlock)
            sampleRateListener = srBlock
        }
        listeningDevices = true
    }

    private func removeDeviceListenersLocked() {
        guard listeningDevices else { return }
        if let devBlock = defaultDeviceListener {
            var devAddr = AudioObjectPropertyAddress(
                mSelector: kAudioHardwarePropertyDefaultOutputDevice,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain)
            AudioObjectRemovePropertyListenerBlock(
                AudioObjectID(kAudioObjectSystemObject), &devAddr, watchdogQueue, devBlock)
            defaultDeviceListener = nil
        }
        if let srBlock = sampleRateListener, watchedDevice != AudioObjectID(kAudioObjectUnknown) {
            var srAddr = AudioObjectPropertyAddress(
                mSelector: kAudioDevicePropertyNominalSampleRate,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain)
            AudioObjectRemovePropertyListenerBlock(watchedDevice, &srAddr, watchdogQueue, srBlock)
            sampleRateListener = nil
        }
        watchedDevice = AudioObjectID(kAudioObjectUnknown)
        listeningDevices = false
    }

    /// Once-per-second liveness check. Decides under `lock`, then emits / rebuilds after unlocking (so
    /// no health callback or rebuild runs while holding it). Edge-triggered: `zero_buffers` once on the
    /// transition to broken, `recovered` once real audio flows again, and a backoff-paced rebuild in
    /// between. `idle` is measured from the later of "audio last flowed" and "this graph started", so a
    /// freshly rebuilt tap gets a full grace window and cannot flap a premature `recovered`; the
    /// stranded check applies the same grace via the graph's age.
    private func watchdogTick() {
        let now = clock.nowNs()
        let lastFlow = flow.lastFlowNs
        let silentNs = silence.silentForNs(nowNs: now)
        // Queried only while already past the silence threshold (so at most once per second, and never
        // in the common case) and never while holding `lock`, since it is a synchronous HAL call.
        let outputRunning =
            silentNs >= liveness.strandedThresholdNs ? outputDeviceIsRunningSomewhere() : false

        lock.lock()
        let base = max(lastFlow, graphStartNs)
        let idle = now > base ? now &- base : 0
        let flowedSinceStart = lastFlow >= graphStartNs
        let graphAge = now > graphStartNs ? now &- graphStartNs : 0
        var emitBroken = false
        var emitRecovered = false
        var wantRebuild = false
        if running {
            if liveness.isBroken(
                idleNs: idle, silentNs: silentNs, graphAgeNs: graphAge,
                outputRunning: outputRunning)
            {
                if !broken {
                    broken = true
                    emitBroken = true
                    nextRebuildAtNs = 0  // rebuild immediately on first detection
                }
                if now >= nextRebuildAtNs {
                    wantRebuild = true
                    rebuildBackoffNs =
                        rebuildBackoffNs == 0
                        ? 1_000_000_000 : min(rebuildBackoffNs * 2, maxBackoffNs)
                    nextRebuildAtNs = now + rebuildBackoffNs
                }
            } else if broken
                && liveness.isRecovered(flowedSinceStart: flowedSinceStart, silentNs: silentNs)
            {
                broken = false
                rebuildBackoffNs = 0
                nextRebuildAtNs = 0
                emitRecovered = true
            }
        }
        lock.unlock()

        if emitBroken { onHealth("zero_buffers", nil) }
        if wantRebuild { rebuild() }
        if emitRecovered { onHealth("recovered", "rebuilt_tap") }
    }
}
