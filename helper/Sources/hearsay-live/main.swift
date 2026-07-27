import AVFoundation
import FluidAudio
import Foundation
import SidecarIO

// Live "Them" processor: FluidAudio streaming diarization + Parakeet ASR.
//
// Compute units: the Parakeet ASR (batch + streaming) runs on the Apple Neural Engine (FluidAudio's
// default). The streaming diarizer is loaded with `computeUnits: .cpuOnly` on purpose — it runs
// concurrently with the ASR, and keeping it off the ANE avoids contending with Parakeet for it.
//
// The core streams the Them PCM in. A streaming diarizer marks speaker turns and, as each
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

// A dead core closes our stdout/stderr mid-write; ignore SIGPIPE so that surfaces as a throwing
// write we can handle (exit) instead of terminating the process with no tail flush.
signal(SIGPIPE, SIG_IGN)

func note(_ message: String) { writeError("hearsay-live", message) }

let diarizer: LSEENDDiarizer
let stream: StreamingUnifiedAsrManager
// The batch Parakeet ASR transcribes a diarizer turn only once it *finalizes* (several seconds into
// speech), so it loads in the background rather than on the critical path: the sidecar signals ready
// — and live partials start flowing — as soon as the streaming ASR + diarizer are up, without also
// waiting for the batch model's ~461 MB to compile on the ANE (which otherwise stacks onto the load
// the user waits through). `transcribeAndEmit` awaits this task before the first final; the task
// warms the model itself so that first final is not slow.
let asrTask: Task<AsrManager, Error>
note("loading models (first run may download from HuggingFace; this can take minutes)")
do {
    // Load the streaming ASR (live partials) and the LS-EEND diarizer concurrently instead of one
    // after another — LS-EEND runs on the CPU (computeUnits: .cpuOnly), so it overlaps the ANE
    // streaming-ASR compile rather than adding to it.
    async let lseendModel = LSEENDModel.loadFromHuggingFace(
        variant: .ami, stepSize: .step500ms, computeUnits: .cpuOnly)
    let streamManager = StreamingUnifiedAsrManager()
    async let streamReady: Void = streamManager.loadModels()
    // Batch ASR: load + ANE-warm in the background; not awaited before ready.
    asrTask = Task {
        let models = try await AsrModels.downloadAndLoad(version: .v3)
        let manager = AsrManager(config: .default, models: models)
        if var state = try? TdtDecoderState() {
            _ = try? await manager.transcribe(
                [Float](repeating: 0, count: 16_000), decoderState: &state)
        }
        return manager
    }
    diarizer = try LSEENDDiarizer(model: try await lseendModel)
    try await streamReady
    stream = streamManager
    note("models loaded (batch ASR loading in the background)")
} catch {
    note("failed to load models: \(error)")
    exit(1)
}

// Warm the ANE-resident streaming Parakeet model before signaling ready: run one silent second so
// Core ML compiles/schedules it now rather than on the first real utterance. The batch model warms
// itself in its background task (above); the diarizer runs on the CPU (computeUnits: .cpuOnly), so it
// has no ANE first-inference cost and is left untouched to keep its stream clock aligned with `audio`.
let warmupSamples = [Float](repeating: 0, count: 16_000)
try? await stream.appendAudio(makeBuffer(warmupSamples))
try? await stream.processBufferedAudio()
_ = await stream.getPartialTranscript()
try? await stream.reset()
note("warmup complete")
emitReady()  // streaming ASR + diarizer loaded + ANE warmed: live partials can flow now; the batch
// ASR finishes loading in the background before the first diarizer turn finalizes.

// The retained tail of Them audio: a finalized turn is sliced out by its [startTime, endTime], and
// the partial is fed from `partialFedTo`. Sample indices stay absolute (meeting-relative, so emitted
// times are correct); `audioBase` is the absolute index of `audio[0]`. Everything before the last
// finalized boundary (`partialAnchor`) is already emitted and never re-sliced, so we drop it instead
// of growing for the whole meeting (~230 MB/hour). A margin cushions diarizer boundary jitter.
var audio: [Float] = []
var audioBase = 0
var emittedTurns: Set<String> = []  // dedupe by time key in case finalizedSegments repeat
// The streaming ASR's partial context starts at the last finalized turn boundary: everything
// before `partialAnchor` is covered by finals, `[partialAnchor, fedTo]` is the in-progress partial.
var partialAnchor = 0  // sample index the current partial started at
var partialFedTo = 0  // how much of `audio` has been handed to the streaming ASR
var lastPartial = ""  // last emitted partial text, to suppress unchanged re-emits
let marginSamples = 32_000  // 2 s cushion below partialAnchor before dropping
// Backstop: a turn that never finalizes (a long uninterrupted monologue) leaves `partialAnchor`
// pinned, so the `partialAnchor - margin` floor alone would let `audio` grow for the whole meeting
// (~230 MB/hour). Cap the retained tail so the live path degrades to a truncated clip on such a turn
// (the post-meeting refine re-transcribes the full audio from audio.wav) instead of growing without
// bound. Mirrors `hearsay-me`'s `maxRetainSamples`.
let maxRetainSamples = 9_600_000  // 10 min at 16 kHz

@MainActor func audioEndAbs() -> Int { audioBase + audio.count }

/// Physical index into `audio` for an absolute sample index, clamped to what is still retained so a
/// dropped prefix can never underflow (a clamp only ever trims a little leading audio, never crashes).
@MainActor func physIndex(_ abs: Int) -> Int { min(max(abs - audioBase, 0), audio.count) }

/// Drop everything before the last finalized boundary (minus a margin): those turns are emitted and
/// never re-sliced. Un-finalized turn audio (>= partialAnchor) is retained until it finalizes.
@MainActor func compactAudio() {
    var keepFromAbs = max(audioBase, partialAnchor - marginSamples)
    keepFromAbs = max(keepFromAbs, audioEndAbs() - maxRetainSamples)
    let drop = keepFromAbs - audioBase
    if drop > marginSamples {
        audio.removeFirst(drop)
        audioBase = keepFromAbs
    }
}

@MainActor
func transcribeAndEmit(_ segments: [DiarizerSegment]) async {
    // The batch ASR loads in the background (off the ready path); await it before the first final. It
    // is normally ready well before any turn finalizes, so this rarely blocks; a load failure is
    // logged once and the finals are dropped (partials + the post-meeting refine still cover them).
    let asr: AsrManager
    do {
        asr = try await asrTask.value
    } catch {
        note("batch ASR load failed: \(error)")
        return
    }
    for segment in segments {
        let key = "\(segment.startFrame)-\(segment.endFrame)-\(segment.speakerIndex)"
        if emittedTurns.contains(key) { continue }
        emittedTurns.insert(key)
        let startAbs = max(0, Int(Double(segment.startTime) * 16_000))
        let endAbs = min(audioEndAbs(), Int(Double(segment.endTime) * 16_000))
        let start = physIndex(startAbs)
        let end = physIndex(endAbs)
        guard end > start else { continue }
        let clip = Array(audio[start..<end])
        do {
            var state = try TdtDecoderState()
            let result = try await asr.transcribe(clip, decoderState: &state)
            let text = result.text.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty else { continue }
            emitLine(
                Segment(
                    kind: "final", speaker: segment.speakerIndex, text: text,
                    startS: Double(segment.startTime), endS: Double(segment.endTime)),
                snakeCase: true)
        } catch {
            note("transcribe failed: \(error)")
        }
    }
}

/// Feed any audio past what the streaming ASR has seen and emit the (speaker-less) partial.
@MainActor
func emitPartial() async {
    do {
        if partialFedTo < audioEndAbs() {
            let feed = Array(audio[physIndex(partialFedTo)..<audio.count])
            partialFedTo = audioEndAbs()
            try await stream.appendAudio(makeBuffer(feed))
            try await stream.processBufferedAudio()
        }
        let text = await stream.getPartialTranscript().trimmingCharacters(
            in: .whitespacesAndNewlines)
        if !text.isEmpty && text != lastPartial {
            lastPartial = text
            emitLine(
                Segment(
                    kind: "partial", speaker: -1, text: text,
                    startS: Double(partialAnchor) / 16_000, endS: Double(audioEndAbs()) / 16_000),
                snakeCase: true)
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

reading: while true {
    let samples: [Float]
    switch readAudioFrame() {
    case .eof: break reading
    case .empty: continue reading
    case .oversize(let n):
        note("protocol error: stdin frame length \(n) exceeds cap; exiting")
        exit(3)
    case .samples(let s): samples = s
    }
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
    compactAudio()  // drop finalized-turn audio so `audio` does not grow for the whole meeting
}

// Flush the tail: finalize the streaming diarizer and emit any remaining turns.
if let update = try? diarizer.finalizeSession() {
    await transcribeAndEmit(update.finalizedSegments)
}
