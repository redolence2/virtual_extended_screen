import Foundation
import ScreenCaptureKit
import CoreMedia
import CoreVideo
import RescCore

/// Captures frames from a specific display using ScreenCaptureKit.
/// Writes to a GenerationalFrameSlot for decoupled encoder consumption.
/// Each capture run's SCStreamOutput (CaptureRunOutput below) is bound to
/// its own generation token at creation — see CONTRACT_ERRATA.md
/// "Implementation proofs required" › late capture callbacks.
final class DisplayCapturer: NSObject {

    // MARK: - Properties

    private var stream: SCStream?
    /// Retained for the run's lifetime — DisplayCapturer no longer conforms
    /// to SCStreamOutput itself (see CaptureRunOutput below).
    private var runOutput: CaptureRunOutput?
    private let frameSlot: GenerationalFrameSlot<CVPixelBuffer>
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

    init(displayID: CGDirectDisplayID, width: Int, height: Int, frameSlot: GenerationalFrameSlot<CVPixelBuffer>) {
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

        // §4 item 1 / CONTRACT_ERRATA.md late-capture-callbacks proof: mint
        // this run's generation token BEFORE creating its SCStreamOutput,
        // so the output can bind to it at creation. beginGeneration() also
        // atomically invalidates any earlier run's token, so a callback
        // still in flight from a just-torn-down stream (the -3805 restart
        // path in the SCStreamDelegate extension below reuses this same
        // DisplayCapturer instance) carries an old token and
        // GenerationalFrameSlot.store rejects it.
        let generation = frameSlot.beginGeneration()
        let runOutput = CaptureRunOutput(generation: generation, frameSlot: frameSlot, parent: self)
        try stream.addStreamOutput(runOutput, type: .screen, sampleHandlerQueue: captureQueue)

        try await stream.startCapture()
        self.stream = stream
        self.runOutput = runOutput
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
        // Release this run's output binding and end its generation: any
        // frame still sitting in the slot from this run is discarded
        // (dropCount+1) and the token is invalidated, so a callback that
        // still fires afterward (the just-retired CaptureRunOutput, still
        // referenced by the now-dead SCStream) can only ever produce a
        // .rejectedStale store.
        self.runOutput = nil
        frameSlot.endGeneration()
        print("[RESC] Capture stopped. Total frames: \(totalFrames), dropped: \(frameSlot.dropCount)")
    }

    // MARK: - Stats (called by the active run's CaptureRunOutput)

    /// Aggregate stats spanning the whole `DisplayCapturer` lifetime
    /// (including -3805 restarts) — same scope `totalFrames` always had;
    /// just relocated here now that the SCStreamOutput callback itself
    /// lives on `CaptureRunOutput` instead of on `DisplayCapturer`.
    fileprivate func noteFrameCaptured(pixelFormat: OSType) {
        // Check for BGRA fallback (log once if unexpected format)
        if totalFrames == 0 {
            let formatStr: String
            switch pixelFormat {
            case kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange: formatStr = "NV12"
            case kCVPixelFormatType_420YpCbCr8BiPlanarFullRange: formatStr = "NV12-full"
            case kCVPixelFormatType_32BGRA: formatStr = "BGRA (will convert on encoder thread)"
            default: formatStr = String(format: "0x%08X", pixelFormat)
            }
            print("[RESC] First frame pixel format: \(formatStr)")
        }

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

// MARK: - SCStreamDelegate

extension DisplayCapturer: SCStreamDelegate {
    func stream(_ stream: SCStream, didStopWithError error: Error) {
        let errMsg = "\(error)"
        print("[RESC] Capture stream stopped with error: \(error)")
        // Auto-restart on -3805 (interrupted connection from stale session).
        // Safe by construction (CONTRACT_ERRATA.md late-capture-callbacks
        // proof): start() below mints a new generation and a fresh
        // CaptureRunOutput before starting the new stream, so any callback
        // still arriving from the old, torn-down stream carries the OLD
        // generation and GenerationalFrameSlot.store rejects it instead of
        // populating this new run's slot.
        if errMsg.contains("3805") {
            print("[RESC] Attempting capture restart in 2 seconds...")
            DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) { [weak self] in
                guard let self = self else { return }
                Task {
                    do {
                        try await self.start()
                        print("[RESC] Capture restarted successfully")
                    } catch {
                        print("[RESC] Capture restart failed: \(error)")
                    }
                }
            }
        }
    }
}

// MARK: - CaptureRunOutput (per-run SCStreamOutput)

/// One instance per capture run, created in `start()` immediately after
/// `frameSlot.beginGeneration()` and bound to that generation for its
/// entire lifetime (A00_REMEDIATION_PLAN.md §4 item 1). `DisplayCapturer`
/// no longer conforms to `SCStreamOutput` itself — every run gets its own
/// output object so a callback surviving past its run's teardown still
/// carries that run's (now stale) generation and cannot be mistaken for
/// output from whichever run is current. See CONTRACT_ERRATA.md
/// "Implementation proofs required" › late capture callbacks.
private final class CaptureRunOutput: NSObject, SCStreamOutput {
    private let generation: UInt64
    private let frameSlot: GenerationalFrameSlot<CVPixelBuffer>
    private weak var parent: DisplayCapturer?

    /// Per-run capture sequence — starts at 0, first frame is 1.
    private var captureSeq: UInt64 = 0
    /// Per-run cached host-time↔continuous-time anchor, refreshed whenever
    /// a fresh bracket succeeds; the last good one is kept otherwise (plan
    /// §4's "Nil SCK sync clock" rule: never silently mix a fallback
    /// sample with true SCK-PTS samples).
    private var calibration: RescClockBridge.Calibration?
    private var loggedStaleReject = false

    /// Conservative uncertainty for the callback-time fallback path: one
    /// 60Hz frame period. Labeled, not measured — plan §4's "Nil SCK sync
    /// clock" rule requires a fallback sample to carry a larger,
    /// conservative bound rather than pretend to PTS-level precision.
    private static let fallbackUncertaintyUs: UInt64 = 16_667

    init(generation: UInt64, frameSlot: GenerationalFrameSlot<CVPixelBuffer>, parent: DisplayCapturer) {
        self.generation = generation
        self.frameSlot = frameSlot
        self.parent = parent
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen else { return }

        // Extract pixel buffer — MUST return immediately (no heavy work)
        guard let pixelBuffer = sampleBuffer.imageBuffer else { return }

        captureSeq &+= 1
        let seq = captureSeq

        // Identity timestamp: prefer SCK's own presentation timestamp,
        // bridged into the host continuous-monotonic domain; fall back to
        // a labeled, conservatively-bounded callback-time read when SCK
        // gives no usable PTS or no calibration has ever succeeded
        // (A00_REMEDIATION_PLAN.md §4 item 1; CaptureTsSource).
        if let freshCal = RescClockBridge.bracketedHostTimeCalibration() {
            calibration = freshCal
        }

        let captureTsUs: UInt64
        let tsSource: CaptureTsSource
        let uncertaintyUs: UInt64

        let pts = sampleBuffer.presentationTimeStamp
        // V11 §10's literal SCK pipeline, restored here (C3 — reverts the
        // prior "PTS is already host-domain" shortcut that skipped
        // CMSyncConvertTime entirely. That shortcut shipped without a dated
        // CONTRACT_ERRATA.md entry and was rejected in
        // A00_COMPLETION_REPORT_AMENDED_review.md finding 4.1 — no erratum
        // authorizes deviating from the contracted conversion):
        // stream.synchronizationClock → CMSyncConvertTime(pts, from:, to:
        // CMClockGetHostTimeClock()) → bridge into our continuous domain via
        // the cached bracketed calibration. A nil sync clock, an
        // invalid/zero sample PTS, an invalid conversion result, or no
        // calibration ever having succeeded all fall through to the same
        // labeled callbackFallback path below — V11 §10's nil-clock rule:
        // never silently mix a fallback sample in as a true SCK-PTS one.
        var hostPts = CMTime.invalid
        if let sync = stream.synchronizationClock, CMTIME_IS_VALID(pts), pts != .zero {
            hostPts = CMSyncConvertTime(pts, from: sync, to: CMClockGetHostTimeClock())
        }

        if CMTIME_IS_VALID(hostPts), let cal = calibration {
            let hostTimeUs = UInt64(CMTimeGetSeconds(hostPts) * 1_000_000)
            captureTsUs = RescClockBridge.hostTimeToContinuousUs(hostTimeUs, calibration: cal)
            tsSource = .sckPts
            uncertaintyUs = cal.uncertaintyUs
        } else {
            captureTsUs = RescClockBridge.continuousNowUs()
            tsSource = .callbackFallback
            uncertaintyUs = Self.fallbackUncertaintyUs
        }

        let frame = CapturedFrame(
            pixels: pixelBuffer, generation: generation, captureSeq: seq,
            captureTsUs: captureTsUs, tsSource: tsSource, uncertaintyUs: uncertaintyUs
        )

        let outcome = frameSlot.store(frame)
        switch outcome {
        case .rejectedStale:
            // Evidence of the late-callback path actually firing — see
            // CONTRACT_ERRATA.md late-capture-callbacks proof. Rate-limited:
            // this run's stream can keep delivering stale callbacks for a
            // while after teardown and there is no value in a line per one.
            if !loggedStaleReject {
                loggedStaleReject = true
                print("[RESC] Late capture callback rejected (stale generation \(generation)) — torn-down run's SCStreamOutput still firing")
            }
        case .stored, .storedReplacingDropped:
            parent?.noteFrameCaptured(pixelFormat: CVPixelBufferGetPixelFormatType(pixelBuffer))
        }
    }
}
