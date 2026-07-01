import FluidAudio
import Foundation

// Live "Me" processor: FluidAudio streaming VAD + Parakeet ASR on the ANE.
//
// The Python core streams the local-mic ("Me") PCM in; this runs the streaming VAD to find
// speech boundaries and, as each utterance ends, slices that span's audio and transcribes it
// (Parakeet), emitting a segment. So Swift owns VAD + ASR for Me -- the core does no chunking.
// Me is always the local speaker, so there is no diarization and no speaker label.
//
//   stdin  (binary): repeated [UInt32 LE n][n x Float32 LE]  -- Me audio, 16 kHz mono
//   stdout (text):   {"text":"...","start_s":f,"end_s":f}\n  -- one per finalized utterance
// EOF on stdin -> flush the trailing frame, close any open utterance, exit. Logs to stderr.

struct Segment: Codable {
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
    FileHandle.standardError.write(Data("hearsay-me: \(message)\n".utf8))
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

let vad: VadManager
var vadState: VadStreamState
let asr: AsrManager
do {
    vad = try await VadManager()
    vadState = await vad.makeStreamState()
    let asrModels = try await AsrModels.downloadAndLoad(version: .v3)
    asr = AsrManager(config: .default, models: asrModels)
    note("models loaded")
} catch {
    note("failed to load models: \(error)")
    exit(1)
}

// All Me audio so far, so a finalized utterance can be sliced out by its [start, end] samples.
var audio: [Float] = []
var speechStart: Int?  // sample index of the in-progress utterance, nil between utterances

@MainActor
func transcribeAndEmit(_ start: Int, _ end: Int) async {
    let s = max(0, start)
    let e = min(audio.count, end)
    guard e > s else { return }
    let clip = Array(audio[s..<e])
    do {
        var decoder = try TdtDecoderState()
        let result = try await asr.transcribe(clip, decoderState: &decoder)
        let text = result.text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        emit(Segment(text: text, startS: Double(s) / 16_000, endS: Double(e) / 16_000))
    } catch {
        note("transcribe failed: \(error)")
    }
}

// Silero expects fixed-size frames; buffer the stream and process exactly chunkSize at a time so
// the VAD's sample indices line up with `audio` (each sample is fed through the VAD once, in order).
let frame = VadManager.chunkSize
var pending: [Float] = []

while true {
    guard let header = readExactly(4) else { break }
    let n = Int(header.withUnsafeBytes { $0.loadUnaligned(as: UInt32.self) })
    guard n > 0 else { continue }
    guard let body = readExactly(n * 4) else { break }
    let samples = body.withUnsafeBytes { Array($0.bindMemory(to: Float.self)) }
    audio.append(contentsOf: samples)
    pending.append(contentsOf: samples)
    while pending.count >= frame {
        let chunk = Array(pending.prefix(frame))
        pending.removeFirst(frame)
        guard let result = try? await vad.processStreamingChunk(chunk, state: vadState) else {
            note("vad failed")
            continue
        }
        vadState = result.state
        guard let event = result.event else { continue }
        if event.kind == .speechStart {
            speechStart = event.sampleIndex
        } else if let start = speechStart {  // speechEnd
            await transcribeAndEmit(start, event.sampleIndex)
            speechStart = nil
        }
    }
}

// Flush: run the trailing partial frame (it pads internally), then close any open utterance.
if !pending.isEmpty, let result = try? await vad.processStreamingChunk(pending, state: vadState) {
    vadState = result.state
    if let event = result.event, event.kind == .speechStart {
        speechStart = event.sampleIndex
    }
}
if let start = speechStart {
    await transcribeAndEmit(start, audio.count)
}
