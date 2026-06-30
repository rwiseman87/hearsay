import FluidAudio
import Foundation

// Persistent live-ASR sidecar: FluidAudio Parakeet TDT v3 on the Apple Neural Engine.
//
// Loads the model once, then serves a request/response loop over stdio:
//   stdin  (binary): [UInt32 LE n][n x Float32 LE samples]  -- one VAD utterance, 16 kHz mono
//   stdout (text):   {"text":"..."}\n                       -- the transcript for that utterance
// EOF on stdin -> exit. All diagnostics go to stderr. The Python core (ParakeetBackend) owns
// this process for the life of a meeting, so the model stays warm across utterances.

struct Response: Codable {
    let text: String
}

func emit(_ response: Response) {
    guard let data = try? JSONEncoder().encode(response) else { return }
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data([0x0A]))
}

func note(_ message: String) {
    FileHandle.standardError.write(Data("hearsay-asr: \(message)\n".utf8))
}

/// Read exactly `count` bytes from stdin, or nil on EOF (stream closed -> the meeting ended).
func readExactly(_ count: Int) -> Data? {
    var buffer = Data()
    buffer.reserveCapacity(count)
    while buffer.count < count {
        let chunk = FileHandle.standardInput.readData(ofLength: count - buffer.count)
        if chunk.isEmpty { return nil }
        buffer.append(chunk)
    }
    return buffer
}

let manager: AsrManager
do {
    let models = try await AsrModels.downloadAndLoad(version: .v3)
    manager = AsrManager(config: .default, models: models)
    note("model loaded")
} catch {
    note("failed to load Parakeet model: \(error)")
    exit(1)
}

while true {
    guard let header = readExactly(4) else { break }  // EOF -> clean exit
    let n = Int(header.withUnsafeBytes { $0.loadUnaligned(as: UInt32.self) })
    if n == 0 {
        emit(Response(text: ""))
        continue
    }
    guard let body = readExactly(n * 4) else { break }
    let samples = body.withUnsafeBytes { Array($0.bindMemory(to: Float.self)) }
    do {
        var state = try TdtDecoderState()  // fresh state per utterance (independent clips)
        let result = try await manager.transcribe(samples, decoderState: &state)
        emit(Response(text: result.text))
    } catch {
        note("transcribe failed: \(error)")
        emit(Response(text: ""))
    }
}
