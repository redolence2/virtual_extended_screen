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

// Kill any stale host processes from previous runs (prevents -3805 capture errors)
do {
    let selfPID = ProcessInfo.processInfo.processIdentifier
    let task = Process()
    task.executableURL = URL(fileURLWithPath: "/usr/bin/pgrep")
    task.arguments = ["-f", "remote-display-host"]
    let pipe = Pipe()
    task.standardOutput = pipe
    try task.run()
    task.waitUntilExit()
    let output = String(data: pipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
    for line in output.split(separator: "\n") {
        if let pid = Int32(line.trimmingCharacters(in: .whitespaces)), pid != selfPID {
            print("[RESC] Killing stale host process (PID \(pid))")
            kill(pid, SIGTERM)
            usleep(200_000) // 200ms for graceful shutdown
            kill(pid, SIGKILL) // force if still alive
        }
    }
    // Give ScreenCaptureKit time to clean up after killing stale processes
    usleep(1_000_000) // 1 second
}

ProtocolConstants.logAndVerify()
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

// Frame pacer: forces compositor to deliver steady 60fps
let framePacer = FramePacer()
framePacer.start(displayID: displayHandle.lastKnownDisplayID, fps: Double(refreshRate))

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

// Thread-safe streaming state (Item 1 from review: eliminates data races)
let streamingState = StreamingState()

// Set up encoder (H.264 default, --hevc for HEVC)
let useHEVC = CommandLine.arguments.contains("--hevc")
var encoderConfig = VideoEncoder.Config(
    width: Int32(width), height: Int32(height), fps: Double(refreshRate),
    keyframeIntervalSeconds: 0.5,
    codec: useHEVC ? .hevc : .h264
)
encoderConfig.bitrateBps = VideoEncoder.Config.defaultBitrate(
    width: Int32(width), height: Int32(height), codec: encoderConfig.codec
)
print("[RESC] Codec: \(encoderConfig.codec), bitrate: \(encoderConfig.bitrateBps / 1_000_000)Mbps")

let encoder = VideoEncoder(config: encoderConfig) { annexBData, isKeyframe, pts, encodeDurationMs in
    h264FileHandle?.write(annexBData)
    let timestampUs = UInt64(CMTimeGetSeconds(pts) * 1_000_000)
    streamingState.sendFrame(data: annexBData, isKeyframe: isKeyframe, timestampUs: timestampUs)
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
        streamingState.startStreaming(sender: sender, streamID: 0, configID: 0)
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
        // Force Night Shift resend to new client on next poll
        nightShiftMonitor.forceResend()
    } else {
        print("[RESC] WARNING: No --client specified")
    }
}
hostSession.onForceKeyframe = {
    encoder.forceKeyframe()
    print("[RESC] Forced keyframe for streaming start")
}

// Night Shift monitor — sends warm filter strength to client
let nightShiftMonitor = NightShiftMonitor()
nightShiftMonitor.onChange = { strength in
    hostSession.sendDisplaySettings(warmStrength: strength)
}
nightShiftMonitor.start()


do {
    try hostSession.start()
} catch {
    print("[RESC] ERROR: Host session start failed: \(error)")
}

// Start capture with retry (ScreenCaptureKit needs time after stale session cleanup)
Task {
    var lastError: Error?
    for attempt in 1...5 {
        do {
            try await capturer.start()
            lastError = nil
            break
        } catch {
            lastError = error
            let errMsg = "\(error)"
            if errMsg.contains("3801") || errMsg.contains("TCC") || errMsg.contains("declined") {
                print("[RESC] Screen Recording permission needed.")
                print("[RESC]   System Settings → Privacy & Security → Screen Recording")
                print("[RESC] Virtual display is alive. Waiting for Ctrl+C...")
                return
            }
            print("[RESC] Capture attempt \(attempt)/5 failed: \(error)")
            if attempt < 5 {
                print("[RESC] Retrying in 2 seconds...")
                try? await Task.sleep(nanoseconds: 2_000_000_000)
            }
        }
    }
    if let error = lastError {
        print("[RESC] ERROR: Capture failed after 5 attempts: \(error)")
        displayManager.destroy(); exit(1)
    }
}

// Graceful shutdown
signal(SIGINT) { _ in
    print("\n[RESC] Shutting down...")
    Task {
        encoder.stop()
        h264FileHandle?.closeFile()
        framePacer.stop()
        cursorTracker?.stop()
        inputReceiver?.stop()
        streamingState.stopStreaming()
        hostSession.stop()
        await capturer.stop()
        displayManager.destroy()
        let s = encoder.stats
        print("[RESC] Final: \(s.frames) frames, \(s.keyframes) KF, avg \(String(format: "%.1f", s.avgEncodeMs))ms")
        let vs = streamingState.stats
        if vs.packets > 0 {
            print("[RESC] Sent: \(vs.packets) packets, \(vs.bytes / 1024)KB")
        }
        exit(0)
    }
}

print("[RESC] Running. Press Ctrl+C to stop.")
RunLoop.main.run()
