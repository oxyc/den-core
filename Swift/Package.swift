// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "DenCore",
    platforms: [.macOS(.v15), .tvOS(.v18), .iOS(.v18)],
    products: [.library(name: "DenCore", targets: ["DenCore"])],
    targets: [
        .binaryTarget(name: "DenCoreFFI", path: "Artifacts/DenCoreFFI.xcframework"),
        .target(name: "DenCore", dependencies: ["DenCoreFFI"]),
    ]
)
