import Darwin
import Foundation
import HearsayIPC

/// The `serve` orchestrator: the long-running helper process the core spawns.
///
/// Connects to the two sockets the core listens on (`control.sock`, `media.sock`),
/// announces itself with a `hello` event, then runs the control loop on the main
/// thread while a dedicated uplink thread drains the per-stream ring buffers into
/// framed PCM on `media.sock`. See `shared/protocol/ipc.md` for the wire contract.
///
/// Wire choice: audio frames are **float32**. The capture graph already produces
/// 16 kHz mono `Float`, so the payload is a zero-cost, lossless reinterpret; the
/// Python reader converts to int16 when it writes `.wav` files.
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
    private let helperVersion = "0.1.0"

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
    private var sources: [AudioSource] = []
    private var streams: [(kind: StreamKind, ring: RingBuffer)] = []
    private var uplink: Thread?
    private var uplinkDone: DispatchSemaphore?

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
                    message: "command '\(cmd.cmd)' is not implemented in this phase"))
        }
    }

    private func replyPermissions(_ id: Int) {
        var result: [String: JSONValue] = [:]
        for (key, value) in Permissions.snapshot() { result[key] = .string(value) }
        reply(.ok(id, result))
    }

    // MARK: - Capture start / stop

    private func startCapture(_ cmd: Command) {
        stateLock.lock()
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

        streams = [(.me, meRing), (.them, themRing)]
        sources = [meSource, themSource]
        let done = DispatchSemaphore(value: 0)
        uplinkDone = done
        capturing = true
        let thread = Thread { [weak self] in self?.uplinkLoop() }
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
        sources = []
        streams = []
        uplink = nil
        uplinkDone = nil
        stateLock.unlock()

        for source in stoppingSources { source.stop() }  // no more producers
        _ = done?.wait(timeout: .now() + 2)  // uplink observes the flag and exits

        // The uplink has exited, so this thread is now the sole ring consumer.
        let now = clock.nowNs()
        for (kind, ring) in stoppingStreams { drainStream(kind: kind, ring: ring, nowNs: now) }
        for (kind, _) in stoppingStreams {
            try? sendMedia(type: .eos, stream: kind, hostTs: clock.nowNs())
        }
        return true
    }

    // MARK: - Uplink

    private func uplinkLoop() {
        let streams = self.streams
        let done = self.uplinkDone
        defer { done?.signal() }
        while isCapturing() {
            let now = clock.nowNs()
            for (kind, ring) in streams { drainStream(kind: kind, ring: ring, nowNs: now) }
            emitMetersAndHeartbeats(streams, now: now)
            Thread.sleep(forTimeInterval: tickInterval)
        }
    }

    private func isCapturing() -> Bool {
        stateLock.lock()
        defer { stateLock.unlock() }
        return capturing
    }

    /// Drain one stream's ring into framed PCM. `host_ts` is stamped from the shared
    /// monotonic clock, corrected for the buffered backlog: `payload[0]` is the
    /// oldest queued sample, so it was captured ~`backlog / 16 kHz` before `now`.
    /// This keeps the two streams aligned and makes `host_ts` strictly increase even
    /// when one tick emits several frames.
    private func drainStream(kind: StreamKind, ring: RingBuffer, nowNs: UInt64) {
        let i = Int(kind.rawValue)
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
        FileHandle.standardError.write(line)
    }
}
