// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "hearsay-helper",
    platforms: [.macOS("14.4")],
    targets: [
        .target(name: "HearsayIPC"),
        .executableTarget(
            name: "hearsay-helper",
            dependencies: ["HearsayIPC"]
        ),
        // Unit + cross-language checks run via `hearsay-helper selftest` (works with
        // Command Line Tools; `swift test`/XCTest needs full Xcode).
    ]
)
