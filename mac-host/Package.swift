// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "RemoteDisplayHost",
    platforms: [
        .macOS(.v14)  // macOS 14+ (Sonoma) on Apple Silicon
    ],
    products: [
        .executable(name: "remote-display-host", targets: ["RemoteDisplayHost"]),
        .executable(name: "resc-fixture-check", targets: ["FixtureCheck"]),
        .executable(name: "resc-harness-sender", targets: ["HarnessSender"])
    ],
    dependencies: [
        // Pinned exactly: generator (tools/generate_proto.sh) and runtime must
        // match (plan v11 §11.6). Bump both together, never one alone.
        .package(url: "https://github.com/apple/swift-protobuf.git", exact: "1.36.1"),
    ],
    targets: [
        // Generated protobuf (tools/generate_proto.sh → Sources/Protocol).
        // Target is named RescProto because a module named `Protocol` collides
        // with the ObjC runtime type of the same name.
        .target(
            name: "RescProto",
            dependencies: [
                .product(name: "SwiftProtobuf", package: "swift-protobuf"),
            ],
            path: "Sources/Protocol"
        ),
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
        // Shared logic: canonical profile (CryptoKit) + the VideoToolbox
        // encoder/NALU packager (VideoToolbox, CoreMedia, CoreVideo — moved
        // here from RemoteDisplayHost so the standalone HarnessSender
        // executable can link them too). No explicit linkerSettings needed:
        // frameworks auto-link via `import` on macOS (already proven by
        // CryptoKit above working with no linkerSettings block).
        .target(
            name: "RescCore",
            dependencies: [
                "RescProto",
                .product(name: "SwiftProtobuf", package: "swift-protobuf"),
            ],
            path: "Sources/RescCore"
        ),
        // Fixture checks run as an executable because this Mac has only
        // CommandLineTools (no XCTest). Same assertions as the Rust tests.
        // Depends on RescProto + SwiftProtobuf (in addition to RescCore) for
        // the envelope round-trip checks (proto/fixtures/envelopes/*.bin).
        .executableTarget(
            name: "FixtureCheck",
            dependencies: [
                "RescCore",
                "RescProto",
                .product(name: "SwiftProtobuf", package: "swift-protobuf"),
            ],
            path: "Sources/FixtureCheck"
        ),
        // Main executable
        .executableTarget(
            name: "RemoteDisplayHost",
            dependencies: [
                "VirtualDisplayBridge",
                "RescProto",
                "RescCore",
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
        // A0 measurement harness (plan v11 §12 A0.0): disposable stop-and-wait
        // TCP sender rig driving the real encoder, used to measure whether
        // flow_window_frames=1 sustains 60Hz. Depends only on RescCore (not
        // RemoteDisplayHost, which is an executable target and cannot be
        // depended on) — that is why VideoEncoder/NALUPackager moved to
        // RescCore above.
        .executableTarget(
            name: "HarnessSender",
            dependencies: ["RescCore"],
            path: "Sources/HarnessSender",
            linkerSettings: [
                .linkedFramework("CoreMedia"),
                .linkedFramework("CoreVideo"),
            ]
        ),
    ]
)
