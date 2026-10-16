// swift-tools-version: 6.2
// Resident live-pair helper: Nemotron streaming preview + Parakeet v3 final pass.
import PackageDescription

let package = Package(
    name: "MurmelaFluidWorker",
    platforms: [.macOS(.v14)],
    dependencies: [
        // Pinned to v0.17.5 by exact revision (see manifest.json). The
        // NemoTextProcessing trait links a prebuilt binary and is not needed.
        .package(
            url: "https://github.com/FluidInference/FluidAudio.git",
            revision: "0b1f46289fe27d95b5e66ad8be46e64f5ee02ae7",
            traits: []
        )
    ],
    targets: [
        .executableTarget(
            name: "MurmelaFluidWorker",
            dependencies: [.product(name: "FluidAudio", package: "FluidAudio")],
            swiftSettings: [.swiftLanguageMode(.v6)]
        )
    ]
)
