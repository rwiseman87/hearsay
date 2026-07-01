// swift-tools-version: 6.0
import Foundation
import PackageDescription

// Absolute path to the embedded launch info (independent of the build CWD).
let infoPlistPath = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()
    .appendingPathComponent("Info.plist")
    .path

let package = Package(
    name: "hearsay-helper",
    platforms: [.macOS("14.4")],
    dependencies: [
        // On-device AI on the Apple Neural Engine (Apache-2.0). Used only by the
        // batch `hearsay-diarize` tool; the capture executable stays dependency-free.
        .package(url: "https://github.com/FluidInference/FluidAudio.git", exact: "0.15.4")
    ],
    targets: [
        .target(name: "HearsayIPC"),
        .executableTarget(
            name: "hearsay-helper",
            dependencies: ["HearsayIPC"],
            // The capture executable drives real-time Core Audio / AVAudioEngine
            // callbacks across threads with explicit lock discipline and
            // @unchecked Sendable. Swift 6 strict-concurrency flags those
            // synchronous audio closures as false positives, so this target uses
            // the Swift 5 language mode; the HearsayIPC contract library stays
            // in strict Swift 6 mode.
            swiftSettings: [.swiftLanguageMode(.v5)],
            // Embed the TCC usage strings into the bare executable so the system
            // can attribute the Microphone / Audio Capture prompts. (Full bundle
            // attribution is Phase 5; this covers running the spike from source.)
            linkerSettings: [
                .unsafeFlags([
                    "-Xlinker", "-sectcreate",
                    "-Xlinker", "__TEXT",
                    "-Xlinker", "__info_plist",
                    "-Xlinker", infoPlistPath,
                ])
            ]
        ),
        // Unit + cross-language checks run via `hearsay-helper selftest` (works with
        // Command Line Tools; `swift test`/XCTest needs full Xcode).
        //
        // Post-meeting offline diarization on the ANE (FluidAudio's pyannote
        // community-1 CoreML pipeline). A one-shot batch tool: reads a wav, prints
        // JSON speaker turns, exits. The Python core invokes it as a subprocess for
        // `hearsay rediarize` (replaces the torch/pyannote refine). Kept a separate
        // target so the heavy CoreML dep never touches the lean capture binary.
        .executableTarget(
            name: "hearsay-diarize",
            dependencies: [.product(name: "FluidAudio", package: "FluidAudio")]
        ),
        // Persistent live-ASR sidecar (FluidAudio Parakeet TDT on the ANE). Loads the model
        // once, then transcribes VAD utterances the Python core streams over stdin/stdout --
        // replacing whisper.cpp/Metal in the live path (whose Metal backend can enter an
        // unrecoverable error state). Separate target so the capture binary stays lean.
        .executableTarget(
            name: "hearsay-asr",
            dependencies: [.product(name: "FluidAudio", package: "FluidAudio")]
        ),
        // Live "Them" processor: streaming diarization + Parakeet on the ANE. The Python core
        // streams the Them PCM in; as each speaker turn finalizes, this transcribes it and
        // emits a labeled segment -- so Swift owns diarization + ASR + turn assembly and the
        // core does no fusion. Separate target so the capture binary stays lean.
        .executableTarget(
            name: "hearsay-live",
            dependencies: [.product(name: "FluidAudio", package: "FluidAudio")]
        ),
        // Live "Me" processor: streaming VAD + Parakeet on the ANE. The Python core streams the
        // local-mic PCM in; this segments speech and transcribes each utterance, emitting a
        // segment -- so Swift owns VAD + ASR for Me (no Silero/Python in the live path). Me is
        // always the local speaker, so there is no diarization. Separate target, lean capture.
        .executableTarget(
            name: "hearsay-me",
            dependencies: [.product(name: "FluidAudio", package: "FluidAudio")]
        ),
    ]
)
