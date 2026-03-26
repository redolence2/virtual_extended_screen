import Foundation
import CoreGraphics
import CoreMedia
import CoreVideo
import VirtualDisplayBridge

// Remote Extended Screen — Mac Host
// Phase 1: Virtual Display + Decoupled Capture Pipeline
// Phase 2: H.264 Encoding + Local Validation
// Phase 3: Protocol + Transport + Control Channel

print("[RESC] Remote Extended Screen Host starting...")
print("[RESC] macOS build: \(CGVirtualDisplayBridge.osBuildVersion())")

// Parse command-line arguments
let args = CommandLine.arguments
let width = Int(args.dropFirst().first ?? "1920") ?? 1920
let height = Int(args.dropFirst(2).first ?? "1080") ?? 1080
let refreshRate = Int(args.dropFirst(3).first ?? "60") ?? 60
let controlPort: UInt16 = {
    if let idx = args.firstIndex(of: "--port"), idx + 1 < args.count {
        return UInt16(args[idx + 1]) ?? 9870
    }
    return 9870
}()
let clientHost: String? = {
    if let idx = args.firstIndex(of: "--client"), idx + 1 < args.count {
        return args[idx + 1]
    }
    return nil
}()
let dumpH264Path: String? = {
    if let idx = args.firstIndex(of: "--dump-h264"), idx + 1 < args.count {
        return args[idx + 1]
    }
    return nil
}()

print("[RESC] Mode: \(width)x\(height)@\(refreshRate)Hz, control port: \(controlPort)")

// Check OS version
let osGate = VirtualDisplayManager.checkOSVersion()
switch osGate {
case .allowed: print("[RESC] OS version: allowed")
case .denied(let build):
    print("[RESC] ERROR: OS build \(build) denied."); exit(1)
case .unknown(let build):
    print("[RESC] WARNING: OS build \(build) unknown, proceeding.")
}

guard CGVirtualDisplayBridge.isAPIAvailable() else {
    print("[RESC] ERROR: CGVirtualDisplay API not available."); exit(1)
}

// Create virtual display
let displayManager = VirtualDisplayManager()
let displayHandle: VirtualDisplayManager.DisplayHandle
do {
    displayHandle = try displayManager.create(width: width, height: height, refreshRate: refreshRate)
    print("[RESC] Virtual display: displayID=\(displayHandle.lastKnownDisplayID)")
} catch {
    print("[RESC] ERROR: \(error)"); exit(1)
}

// Set up capture pipeline
let frameSlot = LatestFrameSlot()
let capturer = DisplayCapturer(
    displayID: displayHandle.lastKnownDisplayID,
    width: width, height: height, frameSlot: frameSlot
)

// H.264 dump file (optional)
let h264FileHandle: FileHandle? = {
    guard let path = dumpH264Path else { return nil }
    FileManager.default.createFile(atPath: path, contents: nil)
    return FileHandle(forWritingAtPath: path)
}()

// Video sender (set when client connects and streaming starts)
var activeVideoSender: VideoSender?
// Gate: don't send non-keyframes until first keyframe is sent (ensures SPS/PPS first)
var hasSentKeyframe = false

// Set up encoder (H.264 default, --hevc for HEVC)
let useHEVC = CommandLine.arguments.contains("--hevc")
var encoderConfig = VideoEncoder.Config(
    width: Int32(width), height: Int32(height), fps: Double(refreshRate),
    keyframeIntervalSeconds: 0.5,  // faster recovery from corruption during drag/movement
    codec: useHEVC ? .hevc : .h264
)
encoderConfig.bitrateBps = VideoEncoder.Config.defaultBitrate(
    width: Int32(width), height: Int32(height), codec: encoderConfig.codec
)
print("[RESC] Codec: \(encoderConfig.codec), bitrate: \(encoderConfig.bitrateBps / 1_000_000)Mbps")

let encoder = VideoEncoder(config: encoderConfig) { annexBData, isKeyframe, pts, encodeDurationMs in
    // Write to H.264 dump
    h264FileHandle?.write(annexBData)

    // Send over UDP if streaming
    if let sender = activeVideoSender {
        // Skip non-keyframes until first keyframe is sent (client needs SPS/PPS first)
        if !hasSentKeyframe {
            if isKeyframe {
                hasSentKeyframe = true
                print("[RESC] First keyframe sent to client (\(annexBData.count / 1024)KB)")
            } else {
                return // skip this frame
            }
        }
        let timestampUs = UInt64(CMTimeGetSeconds(pts) * 1_000_000)
        sender.sendFrame(data: annexBData, isKeyframe: isKeyframe, timestampUs: timestampUs)
    }
}

// Encoder thread
let encoderThread = Thread {
    do { try encoder.start() } catch {
        print("[RESC] ERROR: Encoder start failed: \(error)"); return
    }
    var frameCount: UInt64 = 0
    while !Thread.current.isCancelled {
        guard let pixelBuffer = frameSlot.waitAndTake() else { continue }
        frameCount += 1
        let pts = CMTime(value: CMTimeValue(frameCount), timescale: Int32(refreshRate))
        encoder.encode(pixelBuffer: pixelBuffer, presentationTime: pts)
    }
    encoder.stop()
}
encoderThread.name = "com.resc.encoder"
encoderThread.qualityOfService = QualityOfService.userInteractive
encoderThread.start()

// Host session (control channel + mDNS + mode negotiation)
let sessionConfig = HostSession.Config(
    controlPort: controlPort,
    displayWidth: width, displayHeight: height,
    refreshRate: refreshRate,
    bitrateBps: encoderConfig.bitrateBps
)
let hostSession = HostSession(config: sessionConfig)
// Cursor tracker (Phase 5) + Input receiver (Phase 6)
var cursorTracker: CursorTracker?
var inputReceiver: InputReceiver?

// Check Accessibility permission for input injection
let _ = EventInjector.checkAccessibility()

hostSession.onStreamingStart = { (sender: VideoSender) in
    if let client = clientHost {
        let videoPort = controlPort + 1
        let inputPort = controlPort + 2
        let cursorPort = controlPort + 3
        sender.connect(host: client, port: videoPort)
        activeVideoSender = sender
        print("[RESC] Video sender → \(client):\(videoPort)")

        // Start cursor tracker
        let tracker = CursorTracker(
            displayID: displayHandle.lastKnownDisplayID,
            streamWidth: width, streamHeight: height
        )
        tracker.start(host: client, port: cursorPort)
        cursorTracker = tracker

        // Start input receiver (Phase 6)
        let mapper = CoordinateMapper(
            displayID: displayHandle.lastKnownDisplayID,
            streamWidth: width, streamHeight: height
        )
        let injector = EventInjector(coordinateMapper: mapper)
        let receiver = InputReceiver(port: inputPort, injector: injector)
        receiver.start()
        inputReceiver = receiver
    } else {
        print("[RESC] WARNING: No --client specified")
    }
}
hostSession.onForceKeyframe = {
    encoder.forceKeyframe()
    print("[RESC] Forced keyframe for streaming start")
}

do {
    try hostSession.start()
} catch {
    print("[RESC] ERROR: Host session start failed: \(error)")
}

// Start capture
Task {
    do {
        try await capturer.start()
    } catch {
        let errMsg = "\(error)"
        if errMsg.contains("3801") || errMsg.contains("TCC") || errMsg.contains("declined") {
            print("[RESC] Screen Recording permission needed.")
            print("[RESC]   System Settings → Privacy & Security → Screen Recording")
            print("[RESC] Virtual display is alive. Waiting for Ctrl+C...")
        } else {
            print("[RESC] ERROR: Capture failed: \(error)")
            displayManager.destroy(); exit(1)
        }
    }
}

// Graceful shutdown
signal(SIGINT) { _ in
    print("\n[RESC] Shutting down...")
    Task {
        encoder.stop()
        h264FileHandle?.closeFile()
        cursorTracker?.stop()
        inputReceiver?.stop()
        activeVideoSender?.disconnect()
        hostSession.stop()
        await capturer.stop()
        displayManager.destroy()
        let s = encoder.stats
        print("[RESC] Final: \(s.frames) frames, \(s.keyframes) KF, avg \(String(format: "%.1f", s.avgEncodeMs))ms")
        if let vs = activeVideoSender?.stats {
            print("[RESC] Sent: \(vs.packets) packets, \(vs.bytes / 1024)KB")
        }
        exit(0)
    }
}

print("[RESC] Running. Press Ctrl+C to stop.")
RunLoop.main.run()
