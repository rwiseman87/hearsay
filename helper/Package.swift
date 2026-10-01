// swift-tools-version: 6.1
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
        // On-device AI on the Apple Neural Engine (Apache-2.0). Used by the audio-AI
        // sidecars (hearsay-{live,me,diarize}); the capture executable stays dependency-free.
        // `traits: []` opts out of the prebuilt NemoTextProcessing binary (inverse text normalization, unused here).
        .package(url: "https://github.com/FluidInference/FluidAudio.git", exact: "0.17.5", traits: [])
    ],
    targets: [
        .target(name: "HearsayIPC"),
        // Shared stdio plumbing for the streaming sidecars (framing + stdin length cap + JSON emit +
        // stderr log + PCM buffer). Dependency-free (system frameworks only) so it never bloats a
        // binary; the length-cap fix lives here once instead of in each sidecar main.
        .target(name: "SidecarIO"),
        .executableTarget(
            name: "hearsay-helper",
            dependencies: ["HearsayIPC", "SidecarIO"],
            // The capture executable drives real-time Core Audio / AVAudioEngine
            // callbacks across threads with explicit lock discipline and
            // @unchecked Sendable. Swift 6 strict-concurrency flags those
            // synchronous audio closures as false positives, so this target uses
            // the Swift 5 language mode; the HearsayIPC contract library stays
            // in strict Swift 6 mode.
            swiftSettings: [.swiftLanguageMode(.v5)],
            // Embed the TCC usage strings into the bare executable so the system
            // can attribute the Microphone / Audio Capture prompts. (Full bundle
            // attribution is handled by the Tauri bundle; this covers running from source.)
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
        // JSON speaker turns, exits. The Rust core invokes it as a subprocess for
        // the offline refine (replaces the torch/pyannote refine). Kept a separate
        // target so the heavy CoreML dep never touches the lean capture binary.
        .executableTarget(
            name: "hearsay-diarize",
            dependencies: [.product(name: "FluidAudio", package: "FluidAudio")]
        ),
        // Live "Them" processor: streaming diarization + Parakeet on the ANE. The Rust core
        // streams the Them PCM in; as each speaker turn finalizes, this transcribes it and
        // emits a labeled segment -- so Swift owns diarization + ASR + turn assembly and the
        // core does no fusion. Separate target so the capture binary stays lean.
        .executableTarget(
            name: "hearsay-live",
            dependencies: [.product(name: "FluidAudio", package: "FluidAudio"), "SidecarIO"]
        ),
        // Live "Me" processor: streaming VAD + Parakeet on the ANE. The Rust core streams the
        // local-mic PCM in; this segments speech and transcribes each utterance, emitting a
        // segment -- so Swift owns VAD + ASR for Me end-to-end. Me is
        // always the local speaker, so there is no diarization. Separate target, lean capture.
        .executableTarget(
            name: "hearsay-me",
            dependencies: [.product(name: "FluidAudio", package: "FluidAudio"), "SidecarIO"]
        ),
        // First-run model preparation: downloads the FluidAudio models the sidecars above load,
        // reporting progress so the core can drive a first-run progress bar. The installer ships
        // no models, so this is what puts them on the machine.
        .executableTarget(
            name: "hearsay-models",
            dependencies: [.product(name: "FluidAudio", package: "FluidAudio"), "SidecarIO"]
        ),
    ]
)
