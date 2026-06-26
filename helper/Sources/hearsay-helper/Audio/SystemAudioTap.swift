import AVFoundation
import CoreAudio
import Darwin

/// Thread-safe silence accounting shared between the real-time IOProc and the
/// watchdog timer. Uses its own lock so it is never held during `AudioDeviceStop`
/// (which waits for the IOProc to drain) — avoiding the classic tap deadlock.
private final class SilenceMonitor: @unchecked Sendable {
    private let lock = NSLock()
    private var silentSinceNs: UInt64 = 0  // 0 = currently receiving audio

    func record(nonZero: Bool, nowNs: UInt64) {
        lock.lock()
        defer { lock.unlock() }
        if nonZero {
            silentSinceNs = 0
        } else if silentSinceNs == 0 {
            silentSinceNs = nowNs
        }
    }

    /// Nanoseconds of continuous silence, or 0 if audio is currently flowing.
    func silentForNs(nowNs: UInt64) -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        return silentSinceNs == 0 ? 0 : nowNs &- silentSinceNs
    }

    func reset() {
        lock.lock()
        silentSinceNs = 0
        lock.unlock()
    }
}

/// Self-contained IOProc worker captured by the real-time block. Holds only
/// immutables plus thread-safe collaborators, so the audio thread never touches
/// the tap's mutable build state (and needs no lock that teardown also holds).
private final class TapIOEngine: @unchecked Sendable {
    private let format: AVAudioFormat
    private let resampler: Resampler
    private let ring: RingBuffer
    private let silence: SilenceMonitor
    private let clock = MonotonicClock()

    init(format: AVAudioFormat, resampler: Resampler, ring: RingBuffer, silence: SilenceMonitor) {
        self.format = format
        self.resampler = resampler
        self.ring = ring
        self.silence = silence
    }

    func process(_ inData: UnsafePointer<AudioBufferList>) {
        guard let pcm = AVAudioPCMBuffer(pcmFormat: format, bufferListNoCopy: inData, deallocator: nil)
        else { return }
        let samples = resampler.resample(pcm)
        guard !samples.isEmpty else { return }
        var nonZero = false
        for s in samples where abs(s) > 1e-5 {
            nonZero = true
            break
        }
        silence.record(nonZero: nonZero, nowNs: clock.nowNs())
        ring.write(samples)
    }
}

/// System-audio capture ("Them") via a Core Audio process tap.
///
/// Builds a **global-except-self**, mono mixdown tap (dodges the Teams
/// per-process-silent bug and covers browser meeting apps), wraps it in a private
/// aggregate device, and drains its IOProc into a ``RingBuffer``. A watchdog
/// listens for default-output-device and nominal-sample-rate changes — the events
/// that strand the tap on zero buffers — and rebuilds both tap and aggregate,
/// emitting `tap_health`.
final class SystemAudioTap: AudioSource, @unchecked Sendable {
    private let ring: RingBuffer
    private let onHealth: (_ state: String, _ action: String?) -> Void
    private let log: (String) -> Void
    private let clock = MonotonicClock()

    private let lock = NSLock()
    private var running = false
    private var tapID = AudioObjectID(kAudioObjectUnknown)
    private var aggregateID = AudioObjectID(kAudioObjectUnknown)
    private var procID: AudioDeviceIOProcID?
    private var resampler: Resampler?  // retained for the IOProc's lifetime

    private let silence = SilenceMonitor()
    private let watchdogQueue = DispatchQueue(label: "hearsay.tap.watchdog")
    private var listening = false
    private var watchedDevice = AudioObjectID(kAudioObjectUnknown)
    private var defaultDeviceListener: AudioObjectPropertyListenerBlock?
    private var sampleRateListener: AudioObjectPropertyListenerBlock?
    private var silenceTimer: DispatchSourceTimer?
    // A single silence-triggered rebuild per session catches the start-time
    // zero-buffer bug without thrashing during legitimate quiet stretches; the
    // principled recovery is the device/sample-rate listener below.
    private var didSilenceRebuild = false
    private let silenceReportNs: UInt64 = 5_000_000_000

    init(
        ring: RingBuffer,
        onHealth: @escaping (_ state: String, _ action: String?) -> Void,
        log: @escaping (String) -> Void
    ) {
        self.ring = ring
        self.onHealth = onHealth
        self.log = log
    }

    func start() throws {
        lock.lock()
        defer { lock.unlock() }
        guard !running else { return }
        try buildLocked()
        running = true
        didSilenceRebuild = false
        installListenersLocked()
    }

    func stop() {
        lock.lock()
        defer { lock.unlock() }
        guard running else { return }
        running = false
        removeListenersLocked()
        teardownLocked()
    }

    // MARK: - Build / teardown (under `lock`)

    private func buildLocked() throws {
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
            teardownLocked()
            throw CaptureError.tapFormat
        }
        guard let rs = Resampler(from: inFormat) else {
            teardownLocked()
            throw CaptureError.resamplerInit
        }
        resampler = rs

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
            teardownLocked()
            throw CaptureError.createAggregate(aggStatus)
        }
        aggregateID = newAgg

        let engine = TapIOEngine(format: inFormat, resampler: rs, ring: ring, silence: silence)
        var newProc: AudioDeviceIOProcID?
        let procStatus = AudioDeviceCreateIOProcIDWithBlock(&newProc, aggregateID, nil) {
            _, inData, _, _, _ in
            engine.process(inData)
        }
        guard procStatus == noErr, let proc = newProc else {
            teardownLocked()
            throw CaptureError.createIOProc(procStatus)
        }
        procID = proc

        let startStatus = AudioDeviceStart(aggregateID, proc)
        guard startStatus == noErr else {
            teardownLocked()
            throw CaptureError.startDevice(startStatus)
        }
        silence.reset()
    }

    private func teardownLocked() {
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
        resampler = nil
    }

    /// Rebuild both tap and aggregate after a device change or stuck buffers.
    private func rebuild(reason: String, action: String) {
        lock.lock()
        defer { lock.unlock() }
        guard running else { return }
        log("tap watchdog: \(reason) -> rebuilding")
        removeListenersLocked()
        teardownLocked()
        do {
            try buildLocked()
            installListenersLocked()
            onHealth("recovered", action)
        } catch {
            onHealth("zero_buffers", nil)
            log("tap rebuild failed: \(error)")
        }
    }

    // MARK: - Watchdog (under `lock`)

    private func installListenersLocked() {
        guard !listening else { return }
        var devAddr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDefaultOutputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        let devBlock: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
            self?.rebuild(reason: "default output device changed", action: "rebuilt_tap")
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
                self?.rebuild(reason: "output sample rate changed", action: "rebuilt_tap")
            }
            AudioObjectAddPropertyListenerBlock(watchedDevice, &srAddr, watchdogQueue, srBlock)
            sampleRateListener = srBlock
        }

        let timer = DispatchSource.makeTimerSource(queue: watchdogQueue)
        timer.schedule(deadline: .now() + 1.0, repeating: 1.0)
        timer.setEventHandler { [weak self] in self?.checkSilence() }
        timer.resume()
        silenceTimer = timer

        listening = true
    }

    private func removeListenersLocked() {
        guard listening else { return }
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
        silenceTimer?.cancel()
        silenceTimer = nil
        watchedDevice = AudioObjectID(kAudioObjectUnknown)
        listening = false
    }

    private func checkSilence() {
        let silentNs = silence.silentForNs(nowNs: clock.nowNs())
        guard silentNs >= silenceReportNs else { return }
        onHealth("zero_buffers", nil)
        lock.lock()
        let shouldRebuild = running && !didSilenceRebuild
        if shouldRebuild { didSilenceRebuild = true }
        lock.unlock()
        if shouldRebuild {
            rebuild(reason: "sustained zero buffers", action: "rebuilt_tap")
        }
    }
}
