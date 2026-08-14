import Foundation
import HearsayIPC

private func warn(_ s: String) {
    FileHandle.standardError.write(Data((s + "\n").utf8))
}

/// Pure encode/decode round-trip + error checks (the old XCTest cases).
private func internalRoundTripChecks() -> Bool {
    var ok = true
    do {
        let audio = MediaFrame(
            type: .audio, stream: .them, format: .int16, seq: 3, hostTs: 42,
            payload: [0, 0, 1, 0, 0xff, 0xff, 0xff, 0x7f]
        )
        if try FrameCodec.decode(FrameCodec.encode(audio)) != audio {
            ok = false
            warn("  round-trip audio mismatch")
        }
        let hello = MediaFrame(type: .hello, stream: .me, format: .int16, seq: 0, hostTs: 0)
        if try FrameCodec.decode(FrameCodec.encode(hello)) != hello {
            ok = false
            warn("  round-trip hello mismatch")
        }
        var bad = try FrameCodec.encode(hello)
        bad[0] = 0x00
        do {
            _ = try FrameCodec.decode(bad)
            ok = false
            warn("  bad magic did not throw")
        } catch {
            // expected
        }
    } catch {
        ok = false
        warn("  internal check error: \(error)")
    }
    return ok
}

/// Control-channel (NDJSON) encode/decode round-trips, plus a golden wire-format
/// assertion so the Rust core (hearsay-ipc) and the Swift helper agree byte-for-byte.
private func controlRoundTripChecks() -> Bool {
    var ok = true
    func check(_ cond: Bool, _ what: String) {
        if !cond {
            ok = false
            warn("  control: \(what)")
        }
    }
    func decodeBody<T>(_ line: Data, _ decode: (Data) throws -> T) throws -> T {
        try decode(Data(line.dropLast()))  // strip the trailing '\n' as LineReader does
    }
    do {
        let cmd = Command(
            id: 7, cmd: "start_capture",
            args: ["tap_mode": .string("global_except_self"), "sample_rate": .int(16000)])
        check(try decodeBody(ControlCodec.line(cmd), ControlCodec.decodeCommand) == cmd,
            "command round-trip")

        let ping = try ControlCodec.decodeCommand(Data(#"{"id":1,"cmd":"ping"}"#.utf8))
        check(ping.args.isEmpty, "absent args decode to empty")

        let okReply = Reply.ok(1, ["pong": true])
        let okLine = try ControlCodec.line(okReply)
        check(
            String(decoding: okLine, as: UTF8.self) == "{\"id\":1,\"ok\":true,\"result\":{\"pong\":true}}\n",
            "reply golden wire format")
        check(try decodeBody(okLine, ControlCodec.decodeReply) == okReply, "reply ok round-trip")

        let failReply = Reply.fail(2, code: "no_permission", message: "microphone denied")
        check(try decodeBody(ControlCodec.line(failReply), ControlCodec.decodeReply) == failReply,
            "reply fail round-trip")

        let event = Event(
            event: "tap_health", ts: 123_456_789,
            data: ["state": .string("recovered"), "action": .string("rebuilt_tap")])
        check(try decodeBody(ControlCodec.line(event), ControlCodec.decodeEvent) == event,
            "event round-trip")
    } catch {
        ok = false
        warn("  control check error: \(error)")
    }
    return ok
}

/// Decode + re-encode every committed control golden fixture (`shared/fixtures/control.jsonl`),
/// mirroring the Rust `control_golden_fixtures` test. Rust generates the file, so this is the Swift
/// half of the cross-language NDJSON contract check: any drift in key ordering, slash-escaping, or
/// number formatting between the two codecs fails here.
private func controlFixtureChecks(path: String) -> Bool {
    var ok = true
    func fail(_ what: String) {
        ok = false
        warn("  control fixture: \(what)")
    }
    guard let data = FileManager.default.contents(atPath: path),
        let text = String(data: data, encoding: .utf8)
    else {
        warn("cannot read control fixtures at \(path)")
        return false
    }
    var count = 0
    for line in text.split(separator: "\n") {
        guard let lineData = line.data(using: .utf8),
            let obj = try? JSONSerialization.jsonObject(with: lineData) as? [String: Any],
            let kind = obj["kind"] as? String,
            let encoded = obj["encoded"] as? String
        else {
            fail("bad fixture line")
            continue
        }
        let desc = obj["desc"] as? String ?? "?"
        let encodedData = Data(encoded.utf8)
        do {
            let reencoded: Data
            switch kind {
            case "command":
                reencoded = try ControlCodec.line(ControlCodec.decodeCommand(encodedData))
            case "reply_ok", "reply_fail":
                reencoded = try ControlCodec.line(ControlCodec.decodeReply(encodedData))
            case "event":
                reencoded = try ControlCodec.line(ControlCodec.decodeEvent(encodedData))
            default:
                fail("\(desc): unknown kind \(kind)")
                continue
            }
            // ControlCodec.line appends '\n'; the fixture stores the bare line.
            if String(decoding: reencoded, as: UTF8.self) == encoded + "\n" {
                count += 1
            } else {
                fail("\(desc): re-encode drift\n    fixture: \(encoded)")
            }
        } catch {
            fail("\(desc): decode error \(error)")
        }
    }
    print("swift control-fixture check: \(count) control fixtures, \(ok ? "PASS" : "FAIL")")
    return ok
}

/// Deterministic checks for the lock-free-throughput SPSC ring that carries raw audio from the tap's
/// real-time IOProc to its drain worker — FIFO order, physical wrap-around, and newest-over-capacity
/// drop accounting. Pure logic, so it runs off-device in the self-test.
private func spscRingChecks() -> Bool {
    var ok = true
    func check(_ cond: Bool, _ what: String) {
        if !cond {
            ok = false
            warn("  spsc: \(what)")
        }
    }
    func writeRing(_ ring: SPSCFloatRing, _ xs: [Float]) {
        xs.withUnsafeBufferPointer { ring.write($0.baseAddress!, count: $0.count) }
    }
    func readRing(_ ring: SPSCFloatRing, _ maxCount: Int) -> [Float] {
        var out = [Float](repeating: 0, count: maxCount)
        let n = out.withUnsafeMutableBufferPointer { ring.read(into: $0.baseAddress!, max: $0.count) }
        return Array(out[0..<n])
    }

    let r1 = SPSCFloatRing(capacity: 8)
    writeRing(r1, [1, 2, 3])
    check(readRing(r1, 8) == [1, 2, 3] as [Float], "basic fifo")
    writeRing(r1, [4, 5])
    check(readRing(r1, 8) == [4, 5] as [Float], "fifo after drain")
    check(readRing(r1, 8).isEmpty, "empty read")

    // Wrap-around: the second write straddles the physical end of storage.
    let r2 = SPSCFloatRing(capacity: 4)
    writeRing(r2, [1, 2, 3])
    check(readRing(r2, 2) == [1, 2] as [Float], "partial drain")
    writeRing(r2, [4, 5, 6])
    check(readRing(r2, 4) == [3, 4, 5, 6] as [Float], "wrap fifo")

    // Overrun keeps the oldest `capacity` samples and counts the dropped newest.
    let r3 = SPSCFloatRing(capacity: 4)
    writeRing(r3, [1, 2, 3, 4, 5, 6])
    check(r3.droppedSamples == 2, "overrun drop count")
    check(readRing(r3, 8) == [1, 2, 3, 4] as [Float], "overrun keeps oldest four")
    return ok
}

/// The tap watchdog's trip / recovery rules. The regression these pin: a tap that keeps delivering
/// buffers at full cadence but only exact zeros is dead, and cadence alone reports it healthy.
private func tapLivenessChecks() -> Bool {
    var ok = true
    func check(_ cond: Bool, _ what: String) {
        if !cond {
            ok = false
            warn("  tap liveness: \(what)")
        }
    }
    let s = UInt64(1_000_000_000)
    let policy = TapLivenessPolicy(stuckThresholdNs: 5 * s, strandedThresholdNs: 60 * s)

    // Stuck: no samples reach the ring at all.
    check(
        policy.isBroken(idleNs: 6 * s, silentNs: 0, graphAgeNs: 600 * s, outputRunning: false),
        "idle past the stuck threshold is broken")
    check(
        !policy.isBroken(idleNs: 4 * s, silentNs: 0, graphAgeNs: 600 * s, outputRunning: false),
        "brief idle is not broken")

    // Stranded: full cadence, all exact zeros, something playing.
    check(
        policy.isBroken(idleNs: 0, silentNs: 90 * s, graphAgeNs: 600 * s, outputRunning: true),
        "sustained zeros while output runs is broken")
    check(
        !policy.isBroken(idleNs: 0, silentNs: 90 * s, graphAgeNs: 600 * s, outputRunning: false),
        "sustained zeros with nothing playing is legitimately quiet")
    check(
        !policy.isBroken(idleNs: 0, silentNs: 30 * s, graphAgeNs: 600 * s, outputRunning: true),
        "short silence is not broken")
    // A fresh graph carries the silence that triggered its rebuild; the grace window covers it.
    check(
        !policy.isBroken(idleNs: 0, silentNs: 90 * s, graphAgeNs: 10 * s, outputRunning: true),
        "young graph is inside its grace window")

    check(policy.isRecovered(flowedSinceStart: true, silentNs: 0), "non-zero audio recovers")
    check(
        !policy.isRecovered(flowedSinceStart: false, silentNs: 0),
        "no flow on this graph is not recovered")
    check(
        !policy.isRecovered(flowedSinceStart: true, silentNs: 90 * s),
        "cadence alone does not recover a stranded tap")
    return ok
}

/// Internal round-trip checks plus decoding + re-encoding every committed golden
/// fixture. This is the Swift side of the cross-language IPC contract check.
private func runSelfTest(path: String) -> Bool {
    var ok =
        internalRoundTripChecks() && controlRoundTripChecks() && spscRingChecks()
        && tapLivenessChecks()
    // control.jsonl is the NDJSON golden; it sits beside the frames fixtures passed in `path`.
    let controlDir = (path as NSString).deletingLastPathComponent
    let controlPath = (controlDir as NSString).appendingPathComponent("control.jsonl")
    if !controlFixtureChecks(path: controlPath) {
        ok = false
    }
    guard let data = FileManager.default.contents(atPath: path),
        let text = String(data: data, encoding: .utf8)
    else {
        warn("cannot read fixtures at \(path)")
        return false
    }
    var count = 0
    for line in text.split(separator: "\n") {
        guard let lineData = line.data(using: .utf8),
            let obj = try? JSONSerialization.jsonObject(with: lineData) as? [String: Any],
            let encodedHex = obj["encoded_hex"] as? String,
            let header = obj["header"] as? [String: Any],
            let encoded = Hex.decode(encodedHex)
        else {
            warn("bad fixture line")
            ok = false
            continue
        }
        func check(_ cond: Bool, _ field: String) {
            if !cond {
                ok = false
                warn("  \(obj["desc"] ?? "?"): mismatch on \(field)")
            }
        }
        do {
            let frame = try FrameCodec.decode(encoded)
            check(frame.type.wire == header["type"] as? String, "type")
            check(frame.stream.wire == header["stream"] as? String, "stream")
            check(frame.format.wire == header["format"] as? String, "format")
            check(UInt64(frame.seq) == (header["seq"] as? NSNumber)?.uint64Value, "seq")
            check(frame.hostTs == (header["host_ts"] as? NSNumber)?.uint64Value, "host_ts")
            check(
                UInt64(frame.nSamples) == (header["n_samples"] as? NSNumber)?.uint64Value,
                "n_samples")
            let reencoded = try FrameCodec.encode(frame)
            check(Hex.encode(reencoded) == encodedHex, "re-encode")
            count += 1
        } catch {
            ok = false
            warn("  decode error: \(error)")
        }
    }
    print(
        "swift self-test: internal + control + spsc + tap-liveness checks + \(count) fixtures, "
            + "\(ok ? "PASS" : "FAIL")"
    )
    return ok
}

/// Parse `serve --socket-dir DIR [--synthetic]` and run the orchestrator.
private func runServe(_ args: [String]) -> Never {
    var dir: String?
    var synthetic = false
    var i = 2
    while i < args.count {
        switch args[i] {
        case "--socket-dir":
            i += 1
            if i < args.count { dir = args[i] }
        case "--synthetic":
            synthetic = true
        default:
            warn("serve: ignoring unknown argument \(args[i])")
        }
        i += 1
    }
    guard let dir else {
        warn("usage: hearsay-helper serve --socket-dir DIR [--synthetic]")
        exit(2)
    }
    Serve(socketDir: dir, synthetic: synthetic).run()
    exit(0)  // unreachable: run() exits on shutdown / EOF
}

let args = CommandLine.arguments
let cmd = args.count > 1 ? args[1] : "help"

switch cmd {
case "version":
    print("hearsay-helper \(helperVersion)")
case "selftest":
    let path = args.count > 2 ? args[2] : "shared/fixtures/frames.jsonl"
    exit(runSelfTest(path: path) ? 0 : 1)
case "serve":
    runServe(args)
default:
    warn("usage: hearsay-helper [version | selftest <fixtures.jsonl> | serve --socket-dir DIR [--synthetic]]")
    exit(2)
}
