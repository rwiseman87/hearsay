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
/// assertion so the Python core and the Swift helper agree byte-for-byte.
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

/// Internal round-trip checks plus decoding + re-encoding every committed golden
/// fixture. This is the Swift side of the cross-language IPC contract check.
private func runSelfTest(path: String) -> Bool {
    var ok = internalRoundTripChecks() && controlRoundTripChecks()
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
    print("swift self-test: internal + control checks + \(count) fixtures, \(ok ? "PASS" : "FAIL")")
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
    print("hearsay-helper 0.1.0")
case "selftest":
    let path = args.count > 2 ? args[2] : "shared/fixtures/frames.jsonl"
    exit(runSelfTest(path: path) ? 0 : 1)
case "serve":
    runServe(args)
default:
    warn("usage: hearsay-helper [version | selftest <fixtures.jsonl> | serve --socket-dir DIR [--synthetic]]")
    exit(2)
}
