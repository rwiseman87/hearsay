import Darwin
import Foundation
import HearsayIPC

/// Read from the `Info.plist` `Package.swift` embeds (`-sectcreate`), so it cannot drift.
let helperVersion =
    Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "unknown"

/// The `serve` orchestrator: the long-running helper process the core spawns.
///
/// Connects to the two sockets the core listens on (`control.sock`, `media.sock`),
/// announces itself with a `hello` event, then runs the control loop on the main
/// thread while a dedicated uplink thread drains the per-stream ring buffers into
/// framed PCM on `media.sock`. See `shared/protocol/ipc.md` for the wire contract.
///
/// Wire choice: audio frames are **float32**. The capture graph already produces
/// 16 kHz mono `Float`, so the payload is a zero-cost, lossless reinterpret; the
/// core's recorder converts to int16 when it writes `.wav` files.
final class Serve: @unchecked Sendable {
    private let socketDir: String
    private let synthetic: Bool
    private let clock = MonotonicClock()

    private let sampleRate = 16_000.0
    private let maxFrameSamples = 640  // 40 ms at 16 kHz (contract cadence is 320-640)
    private let ringCapacity = 16_000 * 5  // 5 s of slack against scheduling hiccups
    private let tickInterval = 0.02  // 20 ms uplink drain period
    private let heartbeatNs: UInt64 = 1_000_000_000  // emit a heartbeat after 1 s idle
    private let levelThrottleNs: UInt64 = 250_000_000  // meter cadence
    // Bound each media write so a stalled core cannot pin `mediaLock` (and thus the shutdown path)
    // forever; a timed-out write drops the frame (the core's frame reader resyncs to the next magic).
    private let mediaWriteTimeoutSec = 2.0
    // Give the uplink a little longer than one bounded write to observe the stop flag and exit before
    // the control thread does the final drain, so the two never touch the ring / bookkeeping at once.
    private let uplinkJoinTimeoutSec = 3.0

    // Sockets + their write locks. Two threads emit on the control channel (the
    // control loop replies; the uplink + watchdog emit events), so writes are
    // serialized to avoid interleaving partial lines / frames.
    private var control: UnixSocketClient?
    private var media: UnixSocketClient?
    private let mediaLock = NSLock()
    private let controlLock = NSLock()
    private let logLock = NSLock()

    // Capture session state (guarded by `stateLock`).
    private let stateLock = NSLock()
    private var capturing = false
    // Latched by `shutdown` (under `stateLock`) so a `startCapture` whose out-of-lock mic prompt raced
    // a SIGTERM bails instead of starting capture into a process that is already exiting.
    private var shuttingDown = false
    private var sources: [AudioSource] = []
    private var streams: [(kind: StreamKind, ring: RingBuffer)] = []
    private var uplink: Thread?
    private var uplinkDone: DispatchSemaphore?
    // Per-session stop signal. The uplink loops on its own captured flag, not the shared `capturing`,
    // so a stopped-but-still-alive uplink (wedged past the join timeout) can never resume against a
    // *new* session's streams if `capturing` flips back to true.
    private var uplinkStop: StopFlag?

    // Per-stream wire seq (guarded by `mediaLock`), indexed by `StreamKind.rawValue`.
    private var seqByStream: [UInt32] = [0, 0]

    // Per-stream uplink bookkeeping. Touched only by the uplink thread while
    // capturing, then by the control thread's final flush after the uplink exits —
    // never concurrently — so it needs no lock.
    private var lastTs: [UInt64] = [0, 0]
    private var lastFrameNs: [UInt64] = [0, 0]
    private var lastLevelNs: [UInt64] = [0, 0]
    private var levelSumSq: [Double] = [0, 0]
    private var levelCount: [Int] = [0, 0]
    // Per-stream ring `droppedSamples` last observed by the uplink, to detect new overruns.
    private var lastDropped: [UInt64] = [0, 0]

    private let signalQueue = DispatchQueue(label: "hearsay.signals")
    private var signalSources: [DispatchSourceSignal] = []

    init(socketDir: String, synthetic: Bool) {
        self.socketDir = socketDir
        self.synthetic = synthetic
    }

    // MARK: - Lifecycle

    /// Connect, announce, then run the control loop until shutdown or socket EOF.
    func run() {
        signal(SIGPIPE, SIG_IGN)  // writes to a closed socket throw EPIPE, not a crash
        do {
            control = try connectRetry(path: socketDir + "/control.sock")
            emitHello()
            media = try connectRetry(path: socketDir + "/media.sock")
            media?.setWriteTimeout(seconds: mediaWriteTimeoutSec)
        } catch {
            logJSON("error", "failed to connect to core sockets: \(error)")
            exit(1)
        }
        installSignalHandlers()
        logJSON("info", "helper serving (synthetic=\(synthetic)) dir=\(socketDir)")

        let reader = LineReader(control!)
        do {
            while let line = try reader.nextLine() {
                guard let cmd = try? ControlCodec.decodeCommand(line) else {
                    logJSON("warn", "ignoring malformed control line")
                    continue
                }
                handle(cmd)  // a `shutdown` command exits inside
            }
        } catch {
            logJSON("warn", "control read error: \(error)")
        }
        // Control EOF means the core is gone; stop capture and exit so it can respawn.
        shutdown(replyId: nil)
    }

    private func connectRetry(
        path: String, attempts: Int = 50, delayMs: UInt32 = 20
    ) throws -> UnixSocketClient {
        var last: Error = SocketError.connectFailed(0)
        for _ in 0..<attempts {
            do {
                return try UnixSocketClient(path: path)
            } catch {
                last = error
                usleep(delayMs * 1000)
            }
        }
        throw last
    }

    private func installSignalHandlers() {
        for sig in [SIGTERM, SIGINT] {
            signal(sig, SIG_IGN)  // suppress the default action; the source handles it
            let src = DispatchSource.makeSignalSource(signal: sig, queue: signalQueue)
            src.setEventHandler { [weak self] in self?.shutdown(replyId: nil) }
            src.resume()
            signalSources.append(src)
        }
    }

    private func shutdown(replyId: Int?) -> Never {
        if let id = replyId { reply(.ok(id, ["bye": true])) }
        stateLock.lock()
        shuttingDown = true
        stateLock.unlock()
        _ = stopCapture()
        emitStatus("stopped")
        control?.close()
        media?.close()
        exit(0)
    }

    // MARK: - Control dispatch

    private func handle(_ cmd: Command) {
        switch cmd.cmd {
        case "ping":
            reply(.ok(cmd.id, ["pong": true]))
        case "check_permissions":
            replyPermissions(cmd.id)
        case "start_capture":
            startCapture(cmd)
        case "stop_capture":
            _ = stopCapture()
            emitStatus("stopped")
            reply(.ok(cmd.id, ["stopped": true]))
        case "shutdown":
            shutdown(replyId: cmd.id)
        default:
            reply(
                .fail(
                    cmd.id, code: "unsupported",
                    message: "command '\(cmd.cmd)' is not supported"))
        }
    }

    /// Validate `start_capture` args against what the helper implements, returning a message for an
    /// `unsupported` reply when an arg asks for an unsupported mode. Only `global_except_self` at
    /// the contract-fixed 16 kHz is accepted; any other tap mode is rejected, as is a differing
    /// `sample_rate`, rather than silently ignored.
    private func unsupportedStartArg(_ args: [String: JSONValue]) -> String? {
        if let mode = args["tap_mode"]?.stringValue, mode != "global_except_self" {
            return "tap_mode '\(mode)' is not supported (only global_except_self)"
        }
        if let rate = args["sample_rate"]?.intValue, rate != 16_000 {
            return "sample_rate \(rate) is not supported (capture is fixed at 16000 Hz)"
        }
        return nil
    }

    private func replyPermissions(_ id: Int) {
        var result: [String: JSONValue] = [:]
        for (key, value) in Permissions.snapshot() { result[key] = .string(value) }
        reply(.ok(id, result))
    }

    // MARK: - Capture start / stop

    private func startCapture(_ cmd: Command) {
        // Validate the requested capture args against what the helper implements, rather than
        // silently answering {"started": true} to modes we ignore (`shared/protocol/ipc.md`).
        if let unsupported = unsupportedStartArg(cmd.args) {
            reply(.fail(cmd.id, code: "unsupported", message: unsupported))
            return
        }
        // Surface the TCC mic prompt on a first run (undetermined) so capture does not silently record
        // zeros — but do it BEFORE taking `stateLock`. `requestMicrophone` blocks on the modal dialog,
        // and holding `stateLock` across that wait would wedge a SIGTERM-driven `stopCapture`, leaving
        // the helper unresponsive and unkillable-by-signal. A no-op once the user has granted or denied.
        if !synthetic && Permissions.microphone() == .undetermined {
            _ = Permissions.requestMicrophone()
        }
        stateLock.lock()
        // A stop/shutdown may have raced the prompt above; do not start capture into an exiting process.
        if shuttingDown {
            stateLock.unlock()
            reply(.fail(cmd.id, code: "shutting_down", message: "helper is shutting down"))
            return
        }
        if capturing {
            stateLock.unlock()
            reply(.ok(cmd.id, ["started": true]))  // idempotent
            return
        }

        let meRing = RingBuffer(capacity: ringCapacity)
        let themRing = RingBuffer(capacity: ringCapacity)
        let meSource: AudioSource
        let themSource: AudioSource
        if synthetic {
            // Distinct tones so the two captured `.wav` files are visibly different.
            meSource = SyntheticSource(ring: meRing, frequency: 440)
            themSource = SyntheticSource(ring: themRing, frequency: 660)
        } else {
            meSource = MicCapture(
                ring: meRing,
                onHealth: { [weak self] state, action in self?.emitMicHealth(state, action) },
                log: { [weak self] in self?.logJSON("info", $0) })
            themSource = SystemAudioTap(
                ring: themRing,
                onHealth: { [weak self] state, action in self?.emitTapHealth(state, action) },
                log: { [weak self] in self?.logJSON("info", $0) })
        }

        // Reset wire seq + uplink bookkeeping for this fresh session.
        mediaLock.lock()
        seqByStream = [0, 0]
        mediaLock.unlock()
        let startNs = clock.nowNs()
        lastTs = [0, 0]
        lastFrameNs = [startNs, startNs]
        lastLevelNs = [startNs, startNs]
        levelSumSq = [0, 0]
        levelCount = [0, 0]
        lastDropped = [0, 0]

        // Open each stream with a `hello` frame, then start the producers.
        do {
            try sendMedia(type: .hello, stream: .me, hostTs: startNs)
            try sendMedia(type: .hello, stream: .them, hostTs: startNs)
            try meSource.start()
            try themSource.start()
        } catch {
            meSource.stop()
            themSource.stop()
            stateLock.unlock()
            logJSON("error", "start_capture failed: \(error)")
            emit(
                Event(
                    event: "status", ts: clock.nowNs(),
                    data: ["state": .string("degraded"), "detail": .string("\(error)")]))
            reply(.fail(cmd.id, code: "capture_failed", message: "\(error)"))
            return
        }

        let sessionStreams: [(kind: StreamKind, ring: RingBuffer)] = [(.me, meRing), (.them, themRing)]
        streams = sessionStreams
        sources = [meSource, themSource]
        let done = DispatchSemaphore(value: 0)
        uplinkDone = done
        let stop = StopFlag()
        uplinkStop = stop
        capturing = true
        // Bind this session's streams + done + stop into the thread at creation so the uplink never
        // reads shared state (`self.streams`/`capturing`) a concurrent stopCapture / new session mutates.
        let thread = Thread { [weak self] in
            self?.uplinkLoop(streams: sessionStreams, done: done, stop: stop)
        }
        thread.name = "hearsay-uplink"
        uplink = thread
        stateLock.unlock()

        thread.start()
        emitStatus("capturing")
        reply(.ok(cmd.id, ["started": true]))
    }

    /// Stop the producers, let the uplink drain its final tick, flush the tail, and
    /// send `eos` on each stream. Returns whether a session was actually running.
    @discardableResult
    private func stopCapture() -> Bool {
        stateLock.lock()
        guard capturing else {
            stateLock.unlock()
            return false
        }
        capturing = false
        let stoppingSources = sources
        let stoppingStreams = streams
        let done = uplinkDone
        let stop = uplinkStop
        sources = []
        streams = []
        uplink = nil
        uplinkDone = nil
        uplinkStop = nil
        stateLock.unlock()

        stop?.set()  // this session's uplink exits its loop regardless of a later `capturing` flip
        for source in stoppingSources { source.stop() }  // no more producers
        // Wait for the uplink to observe the flag and exit. If it does not (wedged mid-write), it is
        // still the ring consumer, so skip the final drain/eos rather than race it on the ring and
        // the per-stream bookkeeping — a lost tail on a dying core is better than a data race.
        let joined = done?.wait(timeout: .now() + uplinkJoinTimeoutSec)
        guard joined == .success else {
            logJSON("warn", "uplink did not exit within \(uplinkJoinTimeoutSec)s; skipping final flush")
            return true
        }

        // The uplink has exited, so this thread is now the sole ring consumer.
        let now = clock.nowNs()
        for (kind, ring) in stoppingStreams { drainStream(kind: kind, ring: ring, nowNs: now) }
        for (kind, _) in stoppingStreams {
            try? sendMedia(type: .eos, stream: kind, hostTs: clock.nowNs())
        }
        return true
    }

    // MARK: - Uplink

    private func uplinkLoop(
        streams: [(kind: StreamKind, ring: RingBuffer)], done: DispatchSemaphore, stop: StopFlag
    ) {
        defer { done.signal() }
        while !stop.isSet {
            let now = clock.nowNs()
            for (kind, ring) in streams { drainStream(kind: kind, ring: ring, nowNs: now) }
            emitMetersAndHeartbeats(streams, now: now)
            Thread.sleep(forTimeInterval: tickInterval)
        }
    }

    /// Drain one stream's ring into framed PCM. `host_ts` is stamped from the shared
    /// monotonic clock, corrected for the buffered backlog: `payload[0]` is the
    /// oldest queued sample, so it was captured ~`backlog / 16 kHz` before `now`.
    /// This keeps the two streams aligned and makes `host_ts` strictly increase even
    /// when one tick emits several frames.
    private func drainStream(kind: StreamKind, ring: RingBuffer, nowNs: UInt64) {
        let i = Int(kind.rawValue)
        noteRingDrops(kind, index: i, ring: ring)
        var remaining = ring.available
        while remaining > 0 {
            let want = min(remaining, maxFrameSamples)
            let samples = ring.read(upTo: want)
            let got = samples.count
            if got == 0 { break }

            let backlogNs = UInt64((Double(remaining) / sampleRate) * 1_000_000_000)
            var ts = nowNs > backlogNs ? nowNs - backlogNs : 0
            if ts <= lastTs[i] { ts = lastTs[i] + 1 }  // strict monotonic per stream
            lastTs[i] = ts

            let payload = samples.withUnsafeBytes { Array($0) }  // float32 LE on arm64
            do {
                try sendMedia(type: .audio, stream: kind, hostTs: ts, payload: payload)
            } catch {
                logJSON("warn", "media write failed: \(error)")
                return
            }
            lastFrameNs[i] = nowNs
            for sample in samples { levelSumSq[i] += Double(sample) * Double(sample) }
            levelCount[i] += got
            remaining -= got
        }
    }

    /// Surface ring overruns as a wire `seq` gap. The ring discards oldest-first on overflow —
    /// *before* framing — so the per-frame `seq` would otherwise never gap and the advertised loss
    /// signal (ipc.md: seq gaps = dropped frames) could never fire. Advance `seq` by the
    /// dropped-frame equivalent so the core detects + logs the loss; the core's host_ts-based resync
    /// separately keeps transcript time aligned across the gap.
    private func noteRingDrops(_ kind: StreamKind, index i: Int, ring: RingBuffer) {
        let dropped = ring.droppedSamples
        guard dropped > lastDropped[i] else { return }
        let delta = dropped - lastDropped[i]
        lastDropped[i] = dropped
        let lostFrames = UInt32((delta + UInt64(maxFrameSamples) - 1) / UInt64(maxFrameSamples))
        mediaLock.lock()
        seqByStream[i] = seqByStream[i] &+ lostFrames
        mediaLock.unlock()
        logJSON("warn", "ring overrun on \(kind.wire): dropped \(delta) samples (~\(lostFrames) frames)")
    }

    private func emitMetersAndHeartbeats(
        _ streams: [(kind: StreamKind, ring: RingBuffer)], now: UInt64
    ) {
        for (kind, _) in streams {
            let i = Int(kind.rawValue)
            if now &- lastLevelNs[i] >= levelThrottleNs {
                let rms =
                    levelCount[i] > 0 ? (levelSumSq[i] / Double(levelCount[i])).squareRoot() : 0
                emit(
                    Event(
                        event: "level", ts: now,
                        data: ["stream": .string(kind.wire), "rms": .double(rms)]))
                levelSumSq[i] = 0
                levelCount[i] = 0
                lastLevelNs[i] = now
            }
            if now &- lastFrameNs[i] >= heartbeatNs {
                try? sendMedia(type: .heartbeat, stream: kind, hostTs: now)
                lastFrameNs[i] = now
            }
        }
    }

    // MARK: - Media + control senders

    private func sendMedia(
        type: FrameType, stream: StreamKind, hostTs: UInt64, payload: [UInt8] = []
    ) throws {
        mediaLock.lock()
        defer { mediaLock.unlock() }
        guard let media else { return }
        let i = Int(stream.rawValue)
        let seq = seqByStream[i]
        seqByStream[i] = seq &+ 1
        let frame = MediaFrame(
            type: type, stream: stream, format: .float32, seq: seq, hostTs: hostTs,
            payload: payload)
        try media.writeAll(try FrameCodec.encode(frame))
    }

    private func emit(_ event: Event) {
        guard let control, let line = try? ControlCodec.line(event) else { return }
        controlLock.lock()
        defer { controlLock.unlock() }
        do {
            try control.writeAll(line)
        } catch {
            logJSON("warn", "event write failed: \(error)")
        }
    }

    private func reply(_ reply: Reply) {
        guard let control, let line = try? ControlCodec.line(reply) else { return }
        controlLock.lock()
        defer { controlLock.unlock() }
        do {
            try control.writeAll(line)
        } catch {
            logJSON("warn", "reply write failed: \(error)")
        }
    }

    private func emitStatus(_ state: String) {
        emit(Event(event: "status", ts: clock.nowNs(), data: ["state": .string(state)]))
    }

    private func emitTapHealth(_ state: String, _ action: String?) {
        var data: [String: JSONValue] = ["state": .string(state)]
        if let action { data["action"] = .string(action) }
        emit(Event(event: "tap_health", ts: clock.nowNs(), data: data))
    }

    private func emitMicHealth(_ state: String, _ action: String?) {
        var data: [String: JSONValue] = ["state": .string(state)]
        if let action { data["action"] = .string(action) }
        emit(Event(event: "mic_health", ts: clock.nowNs(), data: data))
    }

    private func emitHello() {
        emit(
            Event(
                event: "hello", ts: clock.nowNs(),
                data: [
                    "helper_version": .string(helperVersion),
                    "protocol_version": .int(1),
                    "pid": .int(Int64(getpid())),
                ]))
    }

    /// NDJSON log line to stderr (stdout/stderr carry logs only, never protocol data).
    private func logJSON(_ level: String, _ message: String) {
        let record: [String: JSONValue] = [
            "level": .string(level),
            "msg": .string(message),
            "ts": .int(Int64(bitPattern: clock.nowNs())),
        ]
        guard let line = try? ControlCodec.line(record) else { return }
        logLock.lock()
        defer { logLock.unlock() }
        // `write(contentsOf:)` throws on a dead stderr pipe; the legacy `write(_:)` raises an
        // uncatchable ObjC exception instead. SIGPIPE is already ignored in `run()`.
        try? FileHandle.standardError.write(contentsOf: line)
    }
}
