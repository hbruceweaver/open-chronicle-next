// swift-tools-version: 5.9

import PackageDescription

let package = Package(
    name: "MacOSWindowResolutionSpike",
    platforms: [.macOS(.v14)],
    products: [
        .executable(
            name: "window-resolution-spike",
            targets: ["WindowResolutionSpike"]
        )
    ],
    targets: [
        .target(name: "WindowResolutionCore"),
        .executableTarget(
            name: "WindowResolutionSpike",
            dependencies: ["WindowResolutionCore"]
        ),
        .testTarget(
            name: "WindowResolutionCoreTests",
            dependencies: ["WindowResolutionCore"]
        )
    ]
)
