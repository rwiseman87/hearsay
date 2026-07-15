import AVFoundation
import Foundation

/// Shared stdio plumbing for the streaming sidecars (`hearsay-live`, `hearsay-me`, `hearsay-asr`).
///
/// Each sidecar reads length-prefixed PCM frames on stdin and writes NDJSON on stdout; this target
/// holds the framing, the JSON emit (with the dead-core SIGPIPE handling), the stderr logger, and the
/// PCM-buffer builder so they live in one place — in particular the stdin length cap, which must be
/// applied identically by all three. Deliberately dependency-free (only Foundation + AVFoundation, both
/// system frameworks) so linking it never pulls FluidAudio/CoreML into a lean binary.

/// Upper bound on a single stdin frame's sample count. A desynced stream can present a garbage 4-byte
/// length prefix (up to ~4.3e9 -> a ~17 GB reserve); capping bounds a bad read to ~19 MB. The bound is
/// the core's `MAX_SILENCE_PAD_SAMPLES` (`rust/crates/hearsay-orchestrator/src/pipeline.rs`), the
/// largest frame the pipeline legitimately sends (a silence pad during a timeline resync), so real
/// frames always pass. Keep the two in sync.
public let maxInputSamples = 5 * 60 * 16_000

/// The outcome of reading one stdin frame.
public enum FrameResult {
    case samples([Float])  // a full `[u32 LE n][n x f32 LE]` frame
    case empty  // n == 0 (keepalive / empty utterance)
    case eof  // stdin closed — the meeting ended, exit cleanly
    case oversize(UInt32)  // n exceeds `maxInputSamples` — a protocol desync, exit with an error
}

/// Read exactly `count` bytes from stdin, or nil on EOF (stream closed -> the meeting ended).
public func readExactly(_ count: Int) -> Data? {
    var buffer = Data()
    buffer.reserveCapacity(count)
    while buffer.count < count {
        let chunk = FileHandle.standardInput.readData(ofLength: count - buffer.count)
        if chunk.isEmpty { return nil }
        buffer.append(chunk)
    }
    return buffer
}

/// Read one `[u32 LE n][n x f32 LE]` audio frame from stdin, rejecting a length past `maxSamples`
/// before allocating for it (item: bound the length prefix so one byte of desync can't drive a huge
/// reserve). `n == 0` is a valid empty frame; EOF returns `.eof`.
public func readAudioFrame(maxSamples: Int = maxInputSamples) -> FrameResult {
    guard let header = readExactly(4) else { return .eof }
    let n = Int(header.withUnsafeBytes { $0.loadUnaligned(as: UInt32.self) })
    if n == 0 { return .empty }
    guard n <= maxSamples else { return .oversize(UInt32(n)) }
    guard let body = readExactly(n * 4) else { return .eof }
    let samples = body.withUnsafeBytes { Array($0.bindMemory(to: Float.self)) }
    return .samples(samples)
}

/// Encode `value` as one NDJSON line to stdout. A dead core closes our stdout mid-write; SIGPIPE is
/// ignored (each sidecar calls `signal(SIGPIPE, SIG_IGN)`), so the write throws and we treat it as
/// stdin EOF (nothing more to stream) and exit cleanly.
public func emitLine<T: Encodable>(_ value: T, snakeCase: Bool = false) {
    let encoder = JSONEncoder()
    if snakeCase { encoder.keyEncodingStrategy = .convertToSnakeCase }
    guard var data = try? encoder.encode(value) else { return }
    data.append(0x0A)
    do {
        try FileHandle.standardOutput.write(contentsOf: data)
    } catch {
        exit(0)
    }
}

private struct ReadyMarker: Encodable { let ready: Bool }

/// Signal on the stdout protocol that model loading finished and the sidecar is serving. The core's
/// read loop recognizes `{"ready":true}` (it is not a transcript segment) and logs it, so a
/// multi-minute first-run model download is distinguishable from a hung sidecar.
public func emitReady() {
    emitLine(ReadyMarker(ready: true))
}

/// NDJSON-free stderr diagnostic, prefixed with the sidecar name. stderr is the log channel; stdout
/// carries only the protocol. Best-effort: a dead stderr pipe is ignored (SIGPIPE stays ignored).
public func writeError(_ prefix: String, _ message: String) {
    try? FileHandle.standardError.write(contentsOf: Data("\(prefix): \(message)\n".utf8))
}

/// Wrap 16 kHz mono Float samples in an AVAudioPCMBuffer (what the streaming ASR consumes).
public func makeBuffer(_ samples: [Float]) -> AVAudioPCMBuffer {
    let format = AVAudioFormat(
        commonFormat: .pcmFormatFloat32, sampleRate: 16_000, channels: 1, interleaved: false)!
    let buffer = AVAudioPCMBuffer(
        pcmFormat: format, frameCapacity: AVAudioFrameCount(max(1, samples.count)))!
    buffer.frameLength = AVAudioFrameCount(samples.count)
    samples.withUnsafeBufferPointer { src in
        if let base = src.baseAddress {
            buffer.floatChannelData![0].update(from: base, count: samples.count)
        }
    }
    return buffer
}
