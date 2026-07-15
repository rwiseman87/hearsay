import FluidAudio
import Foundation
import SidecarIO

// Persistent live-ASR sidecar: FluidAudio Parakeet TDT v3 on the Apple Neural Engine.
//
// Loads the model once, then serves a request/response loop over stdio:
//   stdin  (binary): [UInt32 LE n][n x Float32 LE samples]  -- one VAD utterance, 16 kHz mono
//   stdout (text):   {"text":"..."}\n                       -- the transcript for that utterance
// EOF on stdin -> exit. All diagnostics go to stderr. The core owns this process for the life of a
// meeting, so the model stays warm across utterances. Framing / emit / stderr plumbing is in SidecarIO.

struct Response: Codable {
    let text: String
}

// A dead core closes our stdout/stderr mid-write; ignore SIGPIPE so that surfaces as a throwing
// write we can handle (exit) instead of terminating the process with no tail flush.
signal(SIGPIPE, SIG_IGN)

func note(_ message: String) { writeError("hearsay-asr", message) }

let manager: AsrManager
note("loading models (first run may download from HuggingFace; this can take minutes)")
do {
    let models = try await AsrModels.downloadAndLoad(version: .v3)
    manager = AsrManager(config: .default, models: models)
    note("model loaded")
    emitReady()  // signal readiness so the core can tell a first-run download from a hang
} catch {
    note("failed to load Parakeet model: \(error)")
    exit(1)
}

reading: while true {
    switch readAudioFrame() {
    case .eof:
        break reading  // stdin closed -> clean exit
    case .oversize(let n):
        note("protocol error: stdin frame length \(n) exceeds cap; exiting")
        exit(3)
    case .empty:
        emitLine(Response(text: ""))
    case .samples(let samples):
        do {
            var state = try TdtDecoderState()  // fresh state per utterance (independent clips)
            let result = try await manager.transcribe(samples, decoderState: &state)
            emitLine(Response(text: result.text))
        } catch {
            note("transcribe failed: \(error)")
            emitLine(Response(text: ""))
        }
    }
}
