import AVFoundation
import FluidAudio
import Foundation

// Post-meeting offline speaker diarization on the Apple Neural Engine.
//
// Reads a 16 kHz mono WAV (the recorded "Them" track), runs FluidAudio's
// OfflineDiarizerManager (pyannote community-1, CoreML), and writes the speaker
// turns + per-speaker voiceprints as a single JSON object to stdout:
//
//   {"sample_rate":16000,"duration_s":291.9,"speaker_count":2,
//    "turns":[{"speaker":"S1","start_s":1.2,"end_s":4.5}, ...],
//    "speakers":[{"speaker":"S1","embedding":[0.01, ...]}, ...]}
//
// The per-speaker embeddings (FluidAudio's mean-of-segments speaker database) are
// the cross-meeting voiceprints the Python refine stores + matches, so no separate
// ONNX embedder is needed. All FluidAudio diagnostics go to stderr, so stdout is
// clean JSON. The Python core (`hearsay rediarize`) invokes this as a subprocess.
// CoreML models auto-download from public HuggingFace repos on first run.

struct Turn: Codable {
    let speaker: String
    let startS: Double
    let endS: Double
}

struct SpeakerEmbedding: Codable {
    let speaker: String
    let embedding: [Float]
}

struct Output: Codable {
    let sampleRate: Int
    let durationS: Double
    let speakerCount: Int
    let turns: [Turn]
    let speakers: [SpeakerEmbedding]
}

func emitErrorAndExit(_ message: String) -> Never {
    let payload = ["error": message]
    if let data = try? JSONSerialization.data(withJSONObject: payload) {
        FileHandle.standardError.write(data)
        FileHandle.standardError.write(Data("\n".utf8))
    }
    exit(1)
}

let args = CommandLine.arguments
guard args.count >= 2 else {
    FileHandle.standardError.write(Data("usage: hearsay-diarize <wav>\n".utf8))
    exit(2)
}
let wavPath = args[1]
guard FileManager.default.fileExists(atPath: wavPath) else {
    emitErrorAndExit("no such file: \(wavPath)")
}

do {
    let url = URL(fileURLWithPath: wavPath)
    // Duration from the file header (frames / sample rate) — no need to decode the whole track just
    // to count samples; `manager.process(url)` decodes it once below.
    let file = try AVAudioFile(forReading: url)
    let fileRate = file.fileFormat.sampleRate
    let durationS = fileRate > 0 ? Double(file.length) / fileRate : 0

    let manager = OfflineDiarizerManager()
    let result = try await manager.process(url)

    let turns = result.segments.map {
        Turn(speaker: $0.speakerId, startS: Double($0.startTimeSeconds), endS: Double($0.endTimeSeconds))
    }
    let speakerCount = Set(result.segments.map { $0.speakerId }).count
    // The offline pipeline populates a per-speaker mean embedding (the voiceprint);
    // emit it so the Python refine can store + match it across meetings.
    let speakers = (result.speakerDatabase ?? [:]).map {
        SpeakerEmbedding(speaker: $0.key, embedding: $0.value)
    }
    let output = Output(
        sampleRate: 16_000, durationS: durationS, speakerCount: speakerCount, turns: turns,
        speakers: speakers)

    let encoder = JSONEncoder()
    encoder.keyEncodingStrategy = .convertToSnakeCase
    let data = try encoder.encode(output)
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data("\n".utf8))
} catch {
    emitErrorAndExit("diarization failed: \(error)")
}
