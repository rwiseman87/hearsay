import AVFoundation
import FluidAudio
import Foundation

// Live "Them" processor: FluidAudio streaming diarization + Parakeet ASR on the ANE.
//
// The Python core streams the Them PCM in. A streaming diarizer marks speaker turns and, as each
// turn finalizes, that turn's audio is sliced and transcribed (batch Parakeet) into a labeled
// *final* segment -- so Swift owns diarization + ASR + turn assembly, the core does no fusion.
//
// On top of that, a StreamingUnifiedAsrManager transcribes the current (not-yet-finalized) audio
// into growing *partial* transcripts, so Them text appears live as it is spoken. Partials are
// speaker-less (speaker = -1): the diarizer only assigns a speaker when the turn finalizes, so we
// defer attribution to the final. The streaming ASR is re-anchored to each finalized turn boundary
// so the partial always reflects just the in-progress speech.
//
//   stdin  (binary): repeated [UInt32 LE n][n x Float32 LE]  -- Them audio, 16 kHz mono
//   stdout (text):   {"kind":"partial|final","speaker":N,"text":"...","start_s":f,"end_s":f}\n
//                    partial -> speaker -1 (unknown); final -> the diarizer speaker index (0-based)
// EOF on stdin -> finalize the diarizer tail, emit any remaining turns, exit. Logs to stderr.

struct Segment: Codable {
    let kind: String
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

/// Wrap 16 kHz mono Float samples in an AVAudioPCMBuffer (what the streaming ASR consumes).
func makeBuffer(_ samples: [Float]) -> AVAudioPCMBuffer {
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

let diarizer: LSEENDDiarizer
let asr: AsrManager
let stream: StreamingUnifiedAsrManager
do {
    let model = try await LSEENDModel.loadFromHuggingFace(
        variant: .ami, stepSize: .step500ms, computeUnits: .cpuOnly)
    diarizer = try LSEENDDiarizer(model: model)
    let asrModels = try await AsrModels.downloadAndLoad(version: .v3)
    asr = AsrManager(config: .default, models: asrModels)
    stream = StreamingUnifiedAsrManager()
    try await stream.loadModels()
    note("models loaded")
} catch {
    note("failed to load models: \(error)")
    exit(1)
}

// All Them audio so far, so a finalized turn can be sliced out by its [startTime, endTime].
var audio: [Float] = []
var emittedTurns: Set<String> = []  // dedupe by time key in case finalizedSegments repeat
// The streaming ASR's partial context starts at the last finalized turn boundary: everything
// before `partialAnchor` is covered by finals, `[partialAnchor, fedTo]` is the in-progress partial.
var partialAnchor = 0  // sample index the current partial started at
var partialFedTo = 0  // how much of `audio` has been handed to the streaming ASR
var lastPartial = ""  // last emitted partial text, to suppress unchanged re-emits

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
                    kind: "final", speaker: segment.speakerIndex, text: text,
                    startS: Double(segment.startTime), endS: Double(segment.endTime)))
        } catch {
            note("transcribe failed: \(error)")
        }
    }
}

/// Feed any audio past what the streaming ASR has seen and emit the (speaker-less) partial.
@MainActor
func emitPartial() async {
    do {
        if partialFedTo < audio.count {
            let feed = Array(audio[partialFedTo..<audio.count])
            partialFedTo = audio.count
            try await stream.appendAudio(makeBuffer(feed))
            try await stream.processBufferedAudio()
        }
        let text = await stream.getPartialTranscript().trimmingCharacters(
            in: .whitespacesAndNewlines)
        if !text.isEmpty && text != lastPartial {
            lastPartial = text
            emit(
                Segment(
                    kind: "partial", speaker: -1, text: text,
                    startS: Double(partialAnchor) / 16_000, endS: Double(audio.count) / 16_000))
        }
    } catch {
        note("stream partial failed: \(error)")
    }
}

/// Re-anchor the streaming ASR to a finalized turn boundary so the partial drops the audio the
/// finals already cover and reflects only the in-progress turn from here on.
@MainActor
func reanchorPartial(to sample: Int) async {
    guard sample > partialAnchor else { return }
    partialAnchor = sample
    partialFedTo = sample
    lastPartial = ""
    try? await stream.reset()
}

while true {
    guard let header = readExactly(4) else { break }
    let n = Int(header.withUnsafeBytes { $0.loadUnaligned(as: UInt32.self) })
    guard n > 0 else { continue }
    guard let body = readExactly(n * 4) else { break }
    let samples = body.withUnsafeBytes { Array($0.bindMemory(to: Float.self)) }
    audio.append(contentsOf: samples)
    do {
        if let update = try diarizer.process(samples: samples, sourceSampleRate: 16_000),
            !update.finalizedSegments.isEmpty
        {
            await transcribeAndEmit(update.finalizedSegments)
            let boundary = update.finalizedSegments
                .map { Int(Double($0.endTime) * 16_000) }
                .max() ?? partialAnchor
            await reanchorPartial(to: boundary)
        }
    } catch {
        note("diarize failed: \(error)")
    }
    await emitPartial()
}

// Flush the tail: finalize the streaming diarizer and emit any remaining turns.
if let update = try? diarizer.finalizeSession() {
    await transcribeAndEmit(update.finalizedSegments)
}
