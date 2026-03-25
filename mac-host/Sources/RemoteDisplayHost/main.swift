import Foundation
import CoreGraphics
import CoreMedia
import CoreVideo
import VirtualDisplayBridge

// Remote Extended Screen — Mac Host
// Phase 1: Virtual Display + Decoupled Capture Pipeline
// Phase 2: H.264 Encoding + Local Validation

print("[RESC] Remote Extended Screen Host starting...")
print("[RESC] macOS build: \(CGVirtualDisplayBridge.osBuildVersion())")

// Parse command-line arguments
let args = CommandLine.arguments
let width = Int(args.dropFirst().first ?? "1920") ?? 1920
let height = Int(args.dropFirst(2).first ?? "1080") ?? 1080
let refreshRate = Int(args.dropFirst(3).first ?? "60") ?? 60
// --dump-h264 <path>: write raw H.264 stream to file for validation
let dumpH264Path: String? = {
    if let idx = args.firstIndex(of: "--dump-h264"), idx + 1 < args.count {
        return args[idx + 1]
    }
    return nil
}()

print("[RESC] Requested mode: \(width)x\(height)@\(refreshRate)Hz")
if let path = dumpH264Path {
    print("[RESC] H.264 dump: \(path)")
}

// Check OS version
let osGate = VirtualDisplayManager.checkOSVersion()
switch osGate {
case .allowed:
    print("[RESC] OS version: allowed")
case .denied(let build):
    print("[RESC] ERROR: OS build \(build) is on deny list. Exiting.")
    exit(1)
case .unknown(let build):
    print("[RESC] WARNING: OS build \(build) not in allowlist. Proceeding with caution.")
}

// Check API availability
guard CGVirtualDisplayBridge.isAPIAvailable() else {
    print("[RESC] ERROR: CGVirtualDisplay API not available. Exiting.")
    exit(1)
}

// Create virtual display
let displayManager = VirtualDisplayManager()
let displayHandle: VirtualDisplayManager.DisplayHandle
do {
    displayHandle = try displayManager.create(width: width, height: height, refreshRate: refreshRate)
    print("[RESC] Virtual display created successfully: displayID=\(displayHandle.lastKnownDisplayID)")
} catch {
    print("[RESC] ERROR: \(error)")
    exit(1)
}

// Set up decoupled capture pipeline
let frameSlot = LatestFrameSlot()
let capturer = DisplayCapturer(
    displayID: displayHandle.lastKnownDisplayID,
    width: width,
    height: height,
    frameSlot: frameSlot
)

// Set up H.264 encoder
let h264FileHandle: FileHandle? = {
    guard let path = dumpH264Path else { return nil }
    FileManager.default.createFile(atPath: path, contents: nil)
    return FileHandle(forWritingAtPath: path)
}()

var encoderConfig = VideoEncoder.Config(
    width: Int32(width),
    height: Int32(height),
    fps: Double(refreshRate)
)
encoderConfig.bitrateBps = VideoEncoder.Config.defaultBitrate(width: Int32(width), height: Int32(height))

let encoder = VideoEncoder(config: encoderConfig) { annexBData, isKeyframe, pts, encodeDurationMs in
    // Write to .h264 file if dumping
    h264FileHandle?.write(annexBData)

    // Log first frame and keyframes
    if isKeyframe {
        let kbSize = annexBData.count / 1024
        // Only log occasionally to avoid spam
        let stats = encoder.stats
        if stats.keyframes <= 3 || stats.keyframes % 10 == 0 {
            print("[RESC] Keyframe #\(stats.keyframes): \(kbSize)KB, encode \(String(format: "%.1f", encodeDurationMs))ms")
        }
    }
}

// Encoder thread: reads from LatestFrameSlot → encodes with VideoToolbox
var presentationTimeBase: CMTime?
let encoderThread = Thread {
    print("[RESC] Encoder thread started")
    var frameCount: UInt64 = 0

    do {
        try encoder.start()
    } catch {
        print("[RESC] ERROR: Encoder start failed: \(error)")
        return
    }

    while !Thread.current.isCancelled {
        guard let pixelBuffer = frameSlot.waitAndTake() else { continue }
        frameCount += 1

        if frameCount == 1 {
            let w = CVPixelBufferGetWidth(pixelBuffer)
            let h = CVPixelBufferGetHeight(pixelBuffer)
            let fmt = CVPixelBufferGetPixelFormatType(pixelBuffer)
            print("[RESC] Encoder first frame: \(w)x\(h), format=\(String(format: "0x%08X", fmt))")
        }

        // Generate presentation timestamp (monotonic, based on frame count)
        let pts = CMTime(value: CMTimeValue(frameCount), timescale: Int32(refreshRate))
        encoder.encode(pixelBuffer: pixelBuffer, presentationTime: pts)
    }

    encoder.stop()
}
encoderThread.name = "com.resc.encoder"
encoderThread.qualityOfService = QualityOfService.userInteractive
encoderThread.start()

// Start capture (with permission retry)
Task {
    do {
        try await capturer.start()
    } catch {
        let errMsg = "\(error)"
        if errMsg.contains("3801") || errMsg.contains("TCC") || errMsg.contains("declined") {
            print("[RESC] Screen Recording permission not granted.")
            print("[RESC] Please grant permission:")
            print("[RESC]   System Settings → Privacy & Security → Screen Recording")
            print("[RESC]   Enable 'remote-display-host' (or Terminal if running from terminal)")
            print("[RESC] Then restart this program.")
            print("[RESC]")
            print("[RESC] Virtual display is alive (displayID=\(displayHandle.lastKnownDisplayID)).")
            print("[RESC] Waiting for Ctrl+C...")
        } else {
            print("[RESC] ERROR: Failed to start capture: \(error)")
            displayManager.destroy()
            exit(1)
        }
    }
}

// Handle graceful shutdown
signal(SIGINT) { _ in
    print("\n[RESC] Shutting down...")
    Task {
        encoder.stop()
        h264FileHandle?.closeFile()
        await capturer.stop()
        displayManager.destroy()

        let stats = encoder.stats
        print("[RESC] Final: \(stats.frames) frames encoded, \(stats.keyframes) keyframes, avg encode \(String(format: "%.1f", stats.avgEncodeMs))ms")
        if let path = dumpH264Path {
            let size = (try? FileManager.default.attributesOfItem(atPath: path)[.size] as? UInt64) ?? 0
            print("[RESC] H.264 dump: \(path) (\(size / 1024)KB)")
            print("[RESC] Verify with: ffplay \(path)")
        }
        print("[RESC] Cleanup complete. Exiting.")
        exit(0)
    }
}

print("[RESC] Running. Press Ctrl+C to stop.")
print("[RESC] Display should appear in System Settings > Displays")

// Keep the process alive
RunLoop.main.run()
