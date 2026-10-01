import AVFoundation
import FluidAudio
import Foundation
import SidecarIO

// Live "Me" processor: FluidAudio streaming VAD + streaming Parakeet ASR on the ANE.
//
// The core streams the local-mic ("Me") PCM in. The VAD marks utterance boundaries; a
// StreamingUnifiedAsrManager transcribes each utterance, emitting growing *partial* transcripts
// as you speak and a *final* when the utterance closes. Me is always the local speaker, so there
// is no diarization and no speaker label.
//
//   stdin  (binary): repeated [UInt32 LE n][n x Float32 LE]  -- Me audio, 16 kHz mono
//   stdout (text):   {"kind":"partial|final","text":...,"start_s":f,"end_s":f}\n
// EOF on stdin -> finalize any open utterance, exit. Framing / emit / stderr plumbing is in SidecarIO.

struct Segment: Codable {
    let kind: String
    let text: String
    let startS: Double
    let endS: Double
}

// A dead core closes our stdout/stderr mid-write; ignore SIGPIPE so that surfaces as a throwing
// write we can handle (exit) instead of terminating the process with no tail flush.
signal(SIGPIPE, SIG_IGN)

func note(_ message: String) { writeError("hearsay-me", message) }

let vad: VadManager
var vadState: VadStreamState
let asr: StreamingUnifiedAsrManager
note("loading models (first run may download from HuggingFace; this can take minutes)")
do {
    // Load the VAD and the streaming ASR concurrently instead of one after another, so their model
    // loads overlap rather than stacking before the sidecar can signal ready.
    async let vadLoaded = VadManager()
    let asrManager = StreamingUnifiedAsrManager()
    async let asrReady: Void = asrManager.loadModels()
    vad = try await vadLoaded
    vadState = await vad.makeStreamState()
    try await asrReady
    asr = asrManager
    note("models loaded")
} catch {
    note("failed to load models: \(error)")
    exit(1)
}

// The retained tail of Me audio. Sample indices (VAD events, `speechStart`, `fedUpTo`) stay absolute
// (meeting-relative, so emitted times are correct); `audioBase` is the absolute index of `audio[0]`.
// The streaming ASR accumulates each utterance internally, so we only ever slice `[fedUpTo, end)` —
// never historical audio — which lets us drop the finalized prefix instead of growing for the whole
// meeting (~230 MB/hour). `marginSamples` keeps a cushion below `fedUpTo` because a `speechStart`
// event (with speech padding) can point slightly behind the last-fed sample.
var audio: [Float] = []
var audioBase = 0
var speechStart: Int?  // utterance start sample, nil between utterances
var fedUpTo = 0
let marginSamples = 16_000  // 1 s cushion below fedUpTo before dropping
let maxRetainSamples = 48_000  // 3 s hard cap on retained audio (bounds long inter-utterance silence)

@MainActor func audioEndAbs() -> Int { audioBase + audio.count }

/// Physical index into `audio` for an absolute sample index, clamped to what is still retained so a
/// dropped prefix can never underflow (a clamp only ever adds a little leading audio, never crashes).
@MainActor func physIndex(_ abs: Int) -> Int { min(max(abs - audioBase, 0), audio.count) }

/// The retained audio from absolute `fromAbs` to the end (clamped), i.e. what to feed the ASR next.
@MainActor func feedFrom(_ fromAbs: Int) -> [Float] {
    let lo = physIndex(fromAbs)
    return lo < audio.count ? Array(audio[lo..<audio.count]) : []
}

/// Drop the finalized prefix: keep from `marginSamples` below the last-fed point, never retaining
/// more than `maxRetainSamples`. Only shifts when it can reclaim a meaningful chunk (amortized O(n)).
@MainActor func compactAudio() {
    var keepFromAbs = min(fedUpTo, audioEndAbs()) - marginSamples
    keepFromAbs = max(keepFromAbs, audioEndAbs() - maxRetainSamples)
    keepFromAbs = max(keepFromAbs, audioBase)
    let drop = keepFromAbs - audioBase
    if drop > marginSamples {
        audio.removeFirst(drop)
        audioBase = keepFromAbs
    }
}

@MainActor
func streamPartial(start: Int, feed: [Float], endSample: Int) async {
    do {
        if !feed.isEmpty {
            try await asr.appendAudio(makeBuffer(feed))
            try await asr.processBufferedAudio()
        }
        let text = await asr.getPartialTranscript().trimmingCharacters(in: .whitespacesAndNewlines)
        if !text.isEmpty {
            emitLine(
                Segment(
                    kind: "partial", text: text,
                    startS: Double(start) / 16_000, endS: Double(endSample) / 16_000),
                snakeCase: true)
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
            emitLine(
                Segment(
                    kind: "final", text: text,
                    startS: Double(start) / 16_000, endS: Double(end) / 16_000),
                snakeCase: true)
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

// Warm the ANE before signaling ready: run one silent frame through the VAD + streaming ASR so
// Core ML compiles/schedules the model graph now (at launch) instead of on the first real utterance,
// which otherwise pays the ANE first-inference cost and delays the first transcript. The warmup
// state is discarded (fresh VAD state, ASR reset) so the real stream starts clean.
let warmupChunk = [Float](repeating: 0, count: frame)
_ = try? await vad.processStreamingChunk(warmupChunk, state: vadState, config: vadConfig)
vadState = await vad.makeStreamState()
try? await asr.appendAudio(makeBuffer(warmupChunk))
try? await asr.processBufferedAudio()
_ = await asr.getPartialTranscript()
try? await asr.reset()
note("warmup complete")
emitReady()  // models loaded + ANE warmed: lets the core tell a slow first-run download from a hang
// and lets the prewarm pool know the sidecar is hot.

let inbox = FrameQueue(prefix: "hearsay-me")
inbox.start()

reading: while true {
    let samples: [Float]
    switch inbox.next() {
    case .eof: break reading
    case .empty: continue reading
    case .oversize(let n):
        note("protocol error: stdin frame length \(n) exceeds cap; exiting")
        exit(3)
    case .samples(let s): samples = s
    }
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
                let feed = feedFrom(fedUpTo)
                fedUpTo = audioEndAbs()
                await finalizeUtterance(start: start, end: event.sampleIndex, feed: feed)
                speechStart = nil
            }
        }
        if let start = speechStart {  // still in speech -> stream a partial for the new audio
            let feed = feedFrom(fedUpTo)
            fedUpTo = audioEndAbs()
            await streamPartial(start: start, feed: feed, endSample: audioEndAbs())
        }
    }
    compactAudio()  // drop the finalized prefix so `audio` does not grow for the whole meeting
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
    let feed = feedFrom(fedUpTo)
    await finalizeUtterance(start: start, end: audioEndAbs(), feed: feed)
}
