import Foundation
import CoreGraphics
import CoreVideo
import VirtualDisplayBridge

// Remote Extended Screen — Mac Host
// Phase 1: Virtual Display + Decoupled Capture Pipeline

print("[RESC] Remote Extended Screen Host starting...")
print("[RESC] macOS build: \(CGVirtualDisplayBridge.osBuildVersion())")

// Parse command-line arguments
let args = CommandLine.arguments
let width = Int(args.dropFirst().first ?? "1920") ?? 1920
let height = Int(args.dropFirst(2).first ?? "1080") ?? 1080
let refreshRate = Int(args.dropFirst(3).first ?? "60") ?? 60

print("[RESC] Requested mode: \(width)x\(height)@\(refreshRate)Hz")

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

// Encoder thread (Phase 1: just logs frame stats; Phase 2 adds actual encoding)
let encoderThread = Thread {
    print("[RESC] Encoder thread started (Phase 1: frame stats only)")
    var frameCount: UInt64 = 0
    let startTime = CFAbsoluteTimeGetCurrent()

    while !Thread.current.isCancelled {
        guard let pixelBuffer = frameSlot.waitAndTake() else { continue }
        frameCount += 1

        // Phase 1: just track that we're getting frames
        if frameCount == 1 {
            let w = CVPixelBufferGetWidth(pixelBuffer)
            let h = CVPixelBufferGetHeight(pixelBuffer)
            let format = CVPixelBufferGetPixelFormatType(pixelBuffer)
            print("[RESC] Encoder received first frame: \(w)x\(h), format=\(String(format: "0x%08X", format))")
        }

        if frameCount % 300 == 0 {
            let elapsed = CFAbsoluteTimeGetCurrent() - startTime
            let fps = Double(frameCount) / elapsed
            print("[RESC] Encoder stats: \(frameCount) frames, \(String(format: "%.1f", fps)) fps avg, \(frameSlot.dropCount) dropped")
        }
    }
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
            print("[RESC] Check System Settings > Displays to verify it appears.")
            print("[RESC] Waiting for Ctrl+C...")
            // Keep running so user can check display appeared
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
        await capturer.stop()
        displayManager.destroy()
        print("[RESC] Cleanup complete. Exiting.")
        exit(0)
    }
}

print("[RESC] Running. Press Ctrl+C to stop.")
print("[RESC] Display should appear in System Settings > Displays")

// Keep the process alive
RunLoop.main.run()
