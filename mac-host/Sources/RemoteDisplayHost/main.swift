import Foundation
import CoreGraphics
import CoreMedia
import CoreVideo
import VirtualDisplayBridge
import RescCore

// Remote Extended Screen — Mac Host
// Phase 1: Virtual Display + Decoupled Capture Pipeline
// Phase 2: H.264 Encoding + Local Validation
// Phase 3: Protocol + Transport + Control Channel

// Line-buffer stdout even when redirected to a file (nohup runs): fully
// buffered print() hid every periodic stats line and died unflushed on
// SIGTERM, repeatedly blinding live diagnosis (native-4K night, 2026-08-06).
setvbuf(stdout, nil, _IOLBF, 0)

print("[RESC] Remote Extended Screen Host starting...")

// Diagnostics bootstrap + per-profile instance lock (IMPLEMENTATION_PLAN_V11.md §3, §11).
// Replaces the old pgrep/kill "stale process" sweep: per §3, a second instance
// must exit cleanly on a held lock — nothing is ever killed.
_ = RescLog.shared
EnvironmentRecord.emit()
guard InstanceLock.acquire(profileId: "moyunfei-desk-1") else {
    print("RESC host: another instance holds the profile lock — exiting")
    // Exit-path flush (A00_REMEDIATION_PLAN.md §5 R3a): every deliberate
    // exit gets a synchronous evidence record before the process actually
    // exits. fatal() is synchronous end-to-end (queue.sync + an in-band
    // flush()), so this record is durable before exit(20) runs below.
    RescLog.shared.fatal(code: 20, component: "instance_lock", nativeDomain: nil, nativeCode: nil,
                          summary: "another instance holds the profile lock (INSTANCE_LOCK_HELD)")
    exit(20) // 20 = INSTANCE_LOCK_HELD in the v3 FatalCode enum (proto/control_v3.proto)
}

// `--doctor` mode (IMPLEMENTATION_PLAN_V11.md §11.4): probe-only run, never
// starts real streaming. Exits here — nothing below this point executes.
if CommandLine.arguments.contains("--doctor") { exit(HostDoctor.run()) }

ProtocolConstants.logAndVerify()
print("[RESC] macOS build: \(CGVirtualDisplayBridge.osBuildVersion())")

// Parse command-line arguments
let args = CommandLine.arguments
// Portrait-native defaults: the single user's 32" client monitor is
// physically vertical, and the canonical profile is 1080x1920@60 portrait —
// so the virtual display is created vertical and streamed unrotated.
let width = Int(args.dropFirst().first ?? "1080") ?? 1080
let height = Int(args.dropFirst(2).first ?? "1920") ?? 1920
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

// Create virtual display — Retina-supersampled (settled 2026-08-05): the
// display is created at 2x the stream size as a TRUE HiDPI display (macOS
// renders Retina-crisp glyphs into a width*2 x height*2 backing; logical
// size stays width x height points), and SCK downscales into the unchanged
// width x height stream below. Supersampled text over the proven 1080p
// pipeline: wire, encoder, cursor, and input mapping all remain at stream
// size (the coordinate mappers are proportional against CGDisplayBounds,
// which reports POINTS = stream size, so mapping is identity-scaled).
// Native-4K launch (`2160 3840 60 ...`): the display's Retina backing IS the
// stream — 2160x3840 pixels, logical 1080x1920 — captured 1:1 with no
// downscale (sharp end-to-end). Default 1080p launch keeps the supersampled
// arrangement (display at 2x the stream, SCK downscales).
let (displayPxW, displayPxH) = width >= 2160 ? (width, height) : (width * 2, height * 2)
let displayManager = VirtualDisplayManager()
let displayHandle: VirtualDisplayManager.DisplayHandle
do {
    displayHandle = try displayManager.create(width: displayPxW, height: displayPxH, refreshRate: refreshRate, hiDPI: true)
    print("[RESC] Virtual display: displayID=\(displayHandle.lastKnownDisplayID) (Retina \(displayPxW)x\(displayPxH) px, looks like \(displayPxW / 2)x\(displayPxH / 2); stream \(width)x\(height))")
} catch {
    print("[RESC] ERROR: \(error)"); exit(1)
}

// Frame pacer: forces compositor to deliver steady 60fps
let framePacer = FramePacer()
framePacer.start(displayID: displayHandle.lastKnownDisplayID, fps: Double(refreshRate))

// Set up capture pipeline
let frameSlot = GenerationalFrameSlot<CVPixelBuffer>()
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
    // 4K keyframes are ~1-2MB and cost the encoder its realtime budget —
    // space them out (recovery is covered by the client's IDR-request
    // path); 1080p keeps the historical 0.5s cadence.
    keyframeIntervalSeconds: width >= 2160 ? 10.0 : 0.5,
    codec: useHEVC ? .hevc : .h264
)
encoderConfig.bitrateBps = {
    if let idx = args.firstIndex(of: "--bitrate"), idx + 1 < args.count,
       let mbps = UInt32(args[idx + 1]) {
        return mbps * 1_000_000
    }
    return VideoEncoder.Config.defaultBitrate(
        width: Int32(width), height: Int32(height), codec: encoderConfig.codec
    )
}()
print("[RESC] Codec: \(encoderConfig.codec), bitrate: \(encoderConfig.bitrateBps / 1_000_000)Mbps")

let encoder = VideoEncoder(config: encoderConfig) { annexBData, isKeyframe, pts, encodeDurationMs, identity in
    // Encoder-output time, read at VT-callback entry — distinct from the
    // send-time stamp VideoSender takes adjacent to the socket write
    // (A00_REMEDIATION_PLAN.md §4 item 5: two separate events, two stamps).
    let encodeOutTsUs = RescClockBridge.continuousNowUs()
    h264FileHandle?.write(annexBData)
    let timestampUs = UInt64(CMTimeGetSeconds(pts) * 1_000_000)
    streamingState.sendFrame(data: annexBData, isKeyframe: isKeyframe, timestampUs: timestampUs,
                             identity: identity, encodeOutTsUs: encodeOutTsUs)
}

// Encoder thread
let encoderThread = Thread {
    do { try encoder.start() } catch {
        // A00_REMEDIATION_PLAN.md §5 R3a / F5: every VTSessionSetProperty
        // and PrepareToEncodeFrames status is now checked (VideoEncoder.swift);
        // surface a start() failure by exiting nonzero instead of leaving
        // the process running with no encoder and no streaming.
        print("[RESC] ERROR: Encoder start failed: \(error)"); exit(1)
    }
    var frameCount: UInt64 = 0
    while !Thread.current.isCancelled {
        guard let captured = frameSlot.waitAndTake() else { continue }
        frameCount += 1
        let pts = CMTime(value: CMTimeValue(frameCount), timescale: Int32(refreshRate))
        // captured.identity rides the encoder submit-context through to the
        // output callback (A00_REMEDIATION_PLAN.md §4 item 4).
        encoder.encode(pixelBuffer: captured.pixels, presentationTime: pts, identity: captured.identity)
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
/// Stops every subsystem and prints the final summary — shared by SIGINT
/// (always) and, in trace mode, SIGTERM below, so both signals tear down
/// through the identical sequence instead of maintaining two copies of it.
func performGracefulShutdown() async {
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
}

signal(SIGINT) { _ in
    print("\n[RESC] Shutting down...")
    Task {
        await performGracefulShutdown()
        exit(0)
    }
}

// SIGTERM: trace-mode-only clean-termination protocol (C3 —
// A00_COMPLETION_REPORT_AMENDED_response_review.md amendment 3 /
// A00_COMPLETION_REPORT_AMENDED_response.md v2 §2 F4). Untraced mode
// installs no SIGTERM handler at all, so its behavior (default terminate)
// is unchanged.
var sigtermSource: DispatchSourceSignal?
if RescTrace.enabled {
    // DispatchSourceSignal, not the plain `signal(SIGTERM) { ... }`
    // C-handler pattern SIGINT uses above: this handler ends by calling
    // RescTrace.finish(), which does synchronous file I/O (queue.sync +
    // JSON serialize + fsync) — not safe to run inside a real
    // async-signal-handler context. SIG_IGN first disables the default
    // terminate-on-SIGTERM disposition so the signal can't kill the process
    // before GCD's dispatch source delivers the event to the main queue as
    // an ordinary (non-signal-context) block. Scheduled on `.main`: this
    // executable stays alive via RunLoop.main.run() below, which — like
    // DisplayCapturer's own DispatchQueue.main.asyncAfter restart path —
    // keeps pumping the main dispatch queue, so a `.main`-queued event
    // handler fires correctly here too.
    signal(SIGTERM, SIG_IGN)
    let source = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
    source.setEventHandler {
        print("\n[RESC] SIGTERM received (trace mode) — shutting down...")
        Task {
            await performGracefulShutdown()
            let counts = RescTrace.shared.counts
            RescTrace.shared.finish(status: "clean", framesSent: counts.framesSent, pongs: counts.pongs)
            exit(0)
        }
    }
    source.resume()
    sigtermSource = source // retained for the process's lifetime — see DisplayCapturer's runOutput for the same must-retain-or-it-dies pattern
}

print("[RESC] Running. Press Ctrl+C to stop.")
RunLoop.main.run()
