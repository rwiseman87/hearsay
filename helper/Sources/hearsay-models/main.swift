import CoreML
import FluidAudio
import Foundation
import SidecarIO

// First-run model preparation: fetches every FluidAudio model the live + refine sidecars load, so
// the installer ships no models and the first meeting is not a silent multi-minute download. Each
// step calls the same loader its sidecar calls (`hearsay-me` VAD, `hearsay-{live,me}` streaming ASR,
// `hearsay-live` batch ASR + LS-EEND, `hearsay-diarize` offline diarizer) with a progress handler, so
// the prepared set cannot drift from the loaded set. Idempotent: a loader with its repo already in
// FluidAudio's cache skips the download.
//
//   stdout (text): {"kind":"plan","steps":[{"id":"vad","label":"Voice activity","weight":1}, ...]}
//                  {"kind":"progress","step":"vad","fraction":0.4,"phase":"downloading"}
//                  {"kind":"step","step":"vad"}                     -- that step finished
//                  {"kind":"done"} | {"kind":"error","step":"vad","message":"..."}
// Exits 0 once every model is present, 1 on the first failure. Logs to stderr.

// A dead core closes our stdout mid-write; ignore SIGPIPE so that surfaces as a throwing write we
// handle (exit) instead of terminating the process.
signal(SIGPIPE, SIG_IGN)

func note(_ message: String) { writeError("hearsay-models", message) }

struct PlanStep: Encodable {
    let id: String
    let label: String
    /// Approximate download size in MB, so the core can weight one overall percentage across steps
    /// that differ by three orders of magnitude.
    let weight: Int
}

struct Plan: Encodable {
    let kind = "plan"
    let steps: [PlanStep]
}

struct Progress: Encodable {
    let kind = "progress"
    let step: String
    let fraction: Double
    let phase: String
}

struct StepDone: Encodable {
    let kind = "step"
    let step: String
}

struct Done: Encodable {
    let kind = "done"
}

struct Failure: Encodable {
    let kind = "error"
    let step: String
    let message: String
}

/// Serializes the progress lines. FluidAudio calls its handler from arbitrary queues, so the write
/// is taken under a lock (two half-written lines would corrupt the NDJSON stream) and throttled to
/// whole-percent changes.
final class Reporter: @unchecked Sendable {
    static let shared = Reporter()
    private let lock = NSLock()
    private var lastPercent: [String: Int] = [:]

    func report(_ step: String, _ progress: DownloadUtils.DownloadProgress) {
        let phase: String
        switch progress.phase {
        case .listing: phase = "listing"
        case .downloading: phase = "downloading"
        case .compiling: phase = "compiling"
        }
        let percent = Int((progress.fractionCompleted * 100).rounded())
        lock.lock()
        defer { lock.unlock() }
        guard lastPercent[step] != percent else { return }
        lastPercent[step] = percent
        emitLine(Progress(step: step, fraction: progress.fractionCompleted, phase: phase))
    }
}

func handler(_ step: String) -> DownloadUtils.ProgressHandler {
    { progress in Reporter.shared.report(step, progress) }
}

// Weights are the on-disk sizes of the cached repos, rounded to MB.
let plan = [
    PlanStep(id: "vad", label: "Voice activity", weight: 1),
    PlanStep(id: "diarizer", label: "Speaker diarization", weight: 34),
    PlanStep(id: "ls-eend", label: "Live speaker turns", weight: 43),
    PlanStep(id: "parakeet-v3", label: "Transcription", weight: 461),
    PlanStep(id: "parakeet-unified", label: "Live transcription", weight: 582),
]
emitLine(Plan(steps: plan))

func run(_ step: String, _ work: () async throws -> Void) async {
    note("preparing \(step)")
    do {
        try await work()
        emitLine(StepDone(step: step))
    } catch {
        emitLine(Failure(step: step, message: "\(error)"))
        note("failed to prepare \(step): \(error)")
        exit(1)
    }
}

await run("vad") {
    _ = try await VadManager(progressHandler: handler("vad"))
}
await run("diarizer") {
    _ = try await OfflineDiarizerModels.load(progressHandler: handler("diarizer"))
}
await run("ls-eend") {
    _ = try await LSEENDModel.loadFromHuggingFace(
        variant: .ami, stepSize: .step500ms, computeUnits: .cpuOnly,
        progressHandler: handler("ls-eend"))
}
await run("parakeet-v3") {
    _ = try await AsrModels.download(version: .v3, progressHandler: handler("parakeet-v3"))
}
await run("parakeet-unified") {
    let manager = StreamingUnifiedAsrManager()
    try await manager.loadModels(progressHandler: handler("parakeet-unified"))
}

emitLine(Done())
note("all models ready")
