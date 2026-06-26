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
    ]
)
