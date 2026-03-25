import Foundation
import ScreenCaptureKit
import CoreMedia
import CoreVideo

/// Captures frames from a specific display using ScreenCaptureKit.
/// Writes to a LatestFrameSlot for decoupled encoder consumption.
final class DisplayCapturer: NSObject {

    // MARK: - Properties

    private var stream: SCStream?
    private let frameSlot: LatestFrameSlot
    private let targetDisplayID: CGDirectDisplayID
    private let captureWidth: Int
    private let captureHeight: Int
    private let captureQueue = DispatchQueue(label: "com.resc.capture", qos: .userInteractive)

    // Stats
    private var captureStartTime: CFAbsoluteTime = 0
    private var totalFrames: UInt64 = 0
    private var lastFPSLogTime: CFAbsoluteTime = 0
    private var framesSinceLastLog: UInt64 = 0

    // MARK: - Init

    init(displayID: CGDirectDisplayID, width: Int, height: Int, frameSlot: LatestFrameSlot) {
        self.targetDisplayID = displayID
        self.captureWidth = width
        self.captureHeight = height
        self.frameSlot = frameSlot
        super.init()
    }

    // MARK: - Start / Stop

    func start() async throws {
        // Find the SCDisplay matching our target display ID.
        // Virtual displays may take a moment to register with ScreenCaptureKit.
        var scDisplay: SCDisplay?
        for attempt in 1...5 {
            let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
            scDisplay = content.displays.first(where: { $0.displayID == targetDisplayID })
            if scDisplay != nil { break }
            print("[RESC] Display \(targetDisplayID) not yet in SCShareableContent (attempt \(attempt)/5), waiting...")
            try await Task.sleep(nanoseconds: 1_000_000_000) // 1s
        }

        guard let scDisplay = scDisplay else {
            throw CaptureError.displayNotFound(targetDisplayID)
        }

        // Create filter for just this display (no windows excluded)
        let filter = SCContentFilter(display: scDisplay, excludingWindows: [])

        // Configure stream
        let config = SCStreamConfiguration()
        config.width = captureWidth
        config.height = captureHeight
        config.minimumFrameInterval = CMTime(value: 1, timescale: 60) // 60fps target
        config.showsCursor = false  // We render cursor separately
        config.queueDepth = 3

        // Prefer NV12 for direct VideoToolbox input
        config.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange

        let stream = SCStream(filter: filter, configuration: config, delegate: self)
        try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: captureQueue)

        try await stream.startCapture()
        self.stream = stream
        self.captureStartTime = CFAbsoluteTimeGetCurrent()
        self.lastFPSLogTime = captureStartTime

        print("[RESC] Capture started: displayID=\(targetDisplayID), \(captureWidth)x\(captureHeight)@60fps, NV12")
    }

    func stop() async {
        if let stream = stream {
            do {
                try await stream.stopCapture()
            } catch {
                print("[RESC] Warning: stopCapture error: \(error)")
            }
            self.stream = nil
        }
        print("[RESC] Capture stopped. Total frames: \(totalFrames), dropped: \(frameSlot.dropCount)")
    }

    // MARK: - Errors

    enum CaptureError: Error, CustomStringConvertible {
        case displayNotFound(CGDirectDisplayID)

        var description: String {
            switch self {
            case .displayNotFound(let id): return "Display \(id) not found in SCShareableContent"
            }
        }
    }
}

// MARK: - SCStreamOutput

extension DisplayCapturer: SCStreamOutput {
    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen else { return }

        // Extract pixel buffer — MUST return immediately (no heavy work)
        guard let pixelBuffer = sampleBuffer.imageBuffer else { return }

        // Check for BGRA fallback (log once if unexpected format)
        let format = CVPixelBufferGetPixelFormatType(pixelBuffer)
        if totalFrames == 0 {
            let formatStr: String
            switch format {
            case kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange: formatStr = "NV12"
            case kCVPixelFormatType_420YpCbCr8BiPlanarFullRange: formatStr = "NV12-full"
            case kCVPixelFormatType_32BGRA: formatStr = "BGRA (will convert on encoder thread)"
            default: formatStr = String(format: "0x%08X", format)
            }
            print("[RESC] First frame pixel format: \(formatStr)")
        }

        // Store in slot — this is lock-free and returns immediately
        frameSlot.store(pixelBuffer)
        totalFrames += 1
        framesSinceLastLog += 1

        // Log FPS every 5 seconds
        let now = CFAbsoluteTimeGetCurrent()
        if now - lastFPSLogTime >= 5.0 {
            let fps = Double(framesSinceLastLog) / (now - lastFPSLogTime)
            if fps < 50.0 {
                print("[RESC] WARNING: Capture FPS low: \(String(format: "%.1f", fps)) fps (expected ~60)")
            } else {
                print("[RESC] Capture FPS: \(String(format: "%.1f", fps)) (frames=\(totalFrames), dropped=\(frameSlot.dropCount))")
            }
            framesSinceLastLog = 0
            lastFPSLogTime = now
        }
    }
}

// MARK: - SCStreamDelegate

extension DisplayCapturer: SCStreamDelegate {
    func stream(_ stream: SCStream, didStopWithError error: Error) {
        print("[RESC] Capture stream stopped with error: \(error)")
    }
}
