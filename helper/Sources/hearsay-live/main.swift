import FluidAudio
import Foundation

// Live "Them" processor: FluidAudio streaming diarization + Parakeet ASR on the ANE.
//
// The Python core streams the Them PCM in; this runs a streaming diarizer and, as each speaker
// turn finalizes, slices that turn's audio and transcribes it (Parakeet), emitting a labeled
// segment. So Swift owns diarization + ASR + turn assembly -- the core does no fusion.
//
//   stdin  (binary): repeated [UInt32 LE n][n x Float32 LE]  -- Them audio, 16 kHz mono
//   stdout (text):   {"speaker":N,"text":"...","start_s":f,"end_s":f}\n  -- one per finalized turn
// EOF on stdin -> finalize the diarizer tail, emit any remaining turns, exit. Logs to stderr.

struct Segment: Codable {
    let speaker: Int
    let text: String
    let startS: Double
    let endS: Double
}

func emit(_ segment: Segment) {
    let encoder = JSONEncoder()
    encoder.keyEncodingStrategy = .convertToSnakeCase
    guard let data = try? encoder.encode(segment) else { return }
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data([0x0A]))
}

func note(_ message: String) {
    FileHandle.standardError.write(Data("hearsay-live: \(message)\n".utf8))
}

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

let diarizer: LSEENDDiarizer
let asr: AsrManager
do {
    let model = try await LSEENDModel.loadFromHuggingFace(
        variant: .ami, stepSize: .step500ms, computeUnits: .cpuOnly)
    diarizer = try LSEENDDiarizer(model: model)
    let asrModels = try await AsrModels.downloadAndLoad(version: .v3)
    asr = AsrManager(config: .default, models: asrModels)
    note("models loaded")
} catch {
    note("failed to load models: \(error)")
    exit(1)
}

// All Them audio so far, so a finalized turn can be sliced out by its [startTime, endTime].
var audio: [Float] = []
var emittedTurns: Set<String> = []  // dedupe by time key in case finalizedSegments repeat

@MainActor
func transcribeAndEmit(_ segments: [DiarizerSegment]) async {
    for segment in segments {
        let key = "\(segment.startFrame)-\(segment.endFrame)-\(segment.speakerIndex)"
        if emittedTurns.contains(key) { continue }
        emittedTurns.insert(key)
        let start = max(0, Int(Double(segment.startTime) * 16_000))
        let end = min(audio.count, Int(Double(segment.endTime) * 16_000))
        guard end > start else { continue }
        let clip = Array(audio[start..<end])
        do {
            var state = try TdtDecoderState()
            let result = try await asr.transcribe(clip, decoderState: &state)
            let text = result.text.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty else { continue }
            emit(
                Segment(
                    speaker: segment.speakerIndex, text: text,
                    startS: Double(segment.startTime), endS: Double(segment.endTime)))
        } catch {
            note("transcribe failed: \(error)")
        }
    }
}

while true {
    guard let header = readExactly(4) else { break }
    let n = Int(header.withUnsafeBytes { $0.loadUnaligned(as: UInt32.self) })
    guard n > 0 else { continue }
    guard let body = readExactly(n * 4) else { break }
    let samples = body.withUnsafeBytes { Array($0.bindMemory(to: Float.self)) }
    audio.append(contentsOf: samples)
    do {
        if let update = try diarizer.process(samples: samples, sourceSampleRate: 16_000) {
            await transcribeAndEmit(update.finalizedSegments)
        }
    } catch {
        note("diarize failed: \(error)")
    }
}

// Flush the tail: finalize the streaming session and emit any remaining turns.
if let update = try? diarizer.finalizeSession() {
    await transcribeAndEmit(update.finalizedSegments)
}
