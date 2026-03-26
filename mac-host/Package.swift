// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "RemoteDisplayHost",
    platforms: [
        .macOS(.v14)  // macOS 14+ (Sonoma) on Apple Silicon
    ],
    products: [
        .executable(name: "remote-display-host", targets: ["RemoteDisplayHost"])
    ],
    dependencies: [
        .package(url: "https://github.com/apple/swift-protobuf.git", from: "1.28.0"),
    ],
    targets: [
        // Obj-C bridge for CGVirtualDisplay private API
        .target(
            name: "VirtualDisplayBridge",
            path: "Sources/VirtualDisplay",
            sources: ["CGVirtualDisplayBridge.m"],
            publicHeadersPath: "include",
            cSettings: [
                .headerSearchPath("include"),
                .unsafeFlags(["-fmodules"]),
            ],
            linkerSettings: [
                .linkedFramework("CoreGraphics"),
                .linkedFramework("IOSurface"),
            ]
        ),
        // Main executable
        .executableTarget(
            name: "RemoteDisplayHost",
            dependencies: [
                "VirtualDisplayBridge",
                .product(name: "SwiftProtobuf", package: "swift-protobuf"),
            ],
            path: "Sources/RemoteDisplayHost",
            linkerSettings: [
                .linkedFramework("CoreGraphics"),
                .linkedFramework("ScreenCaptureKit"),
                .linkedFramework("VideoToolbox"),
                .linkedFramework("CoreMedia"),
                .linkedFramework("CoreVideo"),
                .linkedFramework("AppKit"),
            ]
        ),
    ]
)
