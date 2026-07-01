import AVFoundation
import FluidAudio
import Foundation

// Live "Me" processor: FluidAudio streaming VAD + streaming Parakeet ASR on the ANE.
//
// The Python core streams the local-mic ("Me") PCM in. The VAD marks utterance boundaries; a
// StreamingUnifiedAsrManager transcribes each utterance, emitting growing *partial* transcripts
// as you speak and a *final* when the utterance closes. Me is always the local speaker, so there
// is no diarization and no speaker label.
//
//   stdin  (binary): repeated [UInt32 LE n][n x Float32 LE]  -- Me audio, 16 kHz mono
//   stdout (text):   {"kind":"partial|final","text":...,"start_s":f,"end_s":f}\n
// EOF on stdin -> finalize any open utterance, exit. Logs to stderr.

struct Segment: Codable {
    let kind: String
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

let vad: VadManager
var vadState: VadStreamState
let asr: StreamingUnifiedAsrManager
do {
    vad = try await VadManager()
    vadState = await vad.makeStreamState()
    asr = StreamingUnifiedAsrManager()
    try await asr.loadModels()
    note("models loaded")
} catch {
    note("failed to load models: \(error)")
    exit(1)
}

// All Me audio so far (sample indices from the VAD reference into it). `fedUpTo` tracks how much
// of the current utterance we have handed to the streaming ASR.
var audio: [Float] = []
var speechStart: Int?  // utterance start sample, nil between utterances
var fedUpTo = 0

@MainActor
func streamPartial(start: Int, feed: [Float], endSample: Int) async {
    do {
        if !feed.isEmpty {
            try await asr.appendAudio(makeBuffer(feed))
            try await asr.processBufferedAudio()
        }
        let text = await asr.getPartialTranscript().trimmingCharacters(in: .whitespacesAndNewlines)
        if !text.isEmpty {
            emit(
                Segment(
                    kind: "partial", text: text,
                    startS: Double(start) / 16_000, endS: Double(endSample) / 16_000))
        }
    } catch {
        note("stream partial failed: \(error)")
    }
}

@MainActor
func finalizeUtterance(start: Int, end: Int, feed: [Float]) async {
    do {
        if !feed.isEmpty {
            try await asr.appendAudio(makeBuffer(feed))
            try await asr.processBufferedAudio()
        }
        let text = (try await asr.finish()).trimmingCharacters(in: .whitespacesAndNewlines)
        if !text.isEmpty {
            emit(
                Segment(
                    kind: "final", text: text,
                    startS: Double(start) / 16_000, endS: Double(end) / 16_000))
        }
    } catch {
        note("finalize failed: \(error)")
    }
}

// Silero expects fixed-size frames; buffer the stream and process exactly chunkSize at a time so
// the VAD's sample indices line up with `audio` (each sample is fed through the VAD once, in order).
let frame = VadManager.chunkSize
var pending: [Float] = []

// Tuned for live meeting speech vs FluidAudio's defaults (0.75 s / 0.1 s): close an utterance
// after a shorter silence so quick turn-ends finalize promptly, and pad the speech edges more so
// the ASR sees full word onsets/tails. minSpeechDuration is raised to 0.2 to keep FluidAudio's
// invariant speechPadding <= minSpeechDuration (a debug assert otherwise).
let vadConfig = VadSegmentationConfig(
    minSpeechDuration: 0.2, minSilenceDuration: 0.45, speechPadding: 0.2)

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
        guard
            let result = try? await vad.processStreamingChunk(
                chunk, state: vadState, config: vadConfig)
        else {
            note("vad failed")
            continue
        }
        vadState = result.state
        if let event = result.event {
            if event.kind == .speechStart {
                speechStart = event.sampleIndex
                fedUpTo = event.sampleIndex
                try? await asr.reset()
            } else if let start = speechStart {  // speechEnd
                let feed = fedUpTo < audio.count ? Array(audio[fedUpTo..<audio.count]) : []
                fedUpTo = audio.count
                await finalizeUtterance(start: start, end: event.sampleIndex, feed: feed)
                speechStart = nil
            }
        }
        if let start = speechStart {  // still in speech -> stream a partial for the new audio
            let feed = fedUpTo < audio.count ? Array(audio[fedUpTo..<audio.count]) : []
            fedUpTo = audio.count
            await streamPartial(start: start, feed: feed, endSample: audio.count)
        }
    }
}

// Flush: run the trailing partial frame, then finalize any open utterance.
if !pending.isEmpty,
    let result = try? await vad.processStreamingChunk(pending, state: vadState, config: vadConfig)
{
    vadState = result.state
    if let event = result.event, event.kind == .speechStart {
        speechStart = event.sampleIndex
        fedUpTo = event.sampleIndex
        try? await asr.reset()
    }
}
if let start = speechStart {
    let feed = fedUpTo < audio.count ? Array(audio[fedUpTo..<audio.count]) : []
    await finalizeUtterance(start: start, end: audio.count, feed: feed)
}
