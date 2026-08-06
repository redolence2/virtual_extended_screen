import Foundation
import VideoToolbox
import CoreMedia
import CoreVideo

/// VideoToolbox hardware encoder supporting H.264 and HEVC.
/// Consumes CVPixelBuffers from LatestFrameSlot, outputs Annex B NAL units.
///
/// Lives in RescCore (moved from RemoteDisplayHost) so both the host
/// executable and the standalone HarnessSender executable (A0 measurement
/// rig) can link it without HarnessSender depending on the host's
/// ScreenCaptureKit/CGVirtualDisplay-heavy executable target.
public final class VideoEncoder {

    // MARK: - Codec Selection

    public enum Codec: UInt8, CustomStringConvertible {
        case h264 = 0
        case hevc = 1
        public var description: String { self == .h264 ? "H.264" : "HEVC" }
    }

    // MARK: - Configuration

    public struct Config {
        public var width: Int32
        public var height: Int32
        public var fps: Double = 60.0
        public var bitrateBps: UInt32 = 20_000_000
        public var keyframeIntervalSeconds: Double = 1.0
        public var codec: Codec = .h264
        /// Failure-injection test seam (A00_REMEDIATION_PLAN.md §5 R3a):
        /// when true, `start()` treats `VTCompressionSessionPrepareToEncodeFrames`
        /// as if it failed — with an unmistakably synthetic `OSStatus` — even
        /// though the real call still runs first. Exists because Prepare's
        /// status, unlike every other property this method sets, is
        /// generated and consumed entirely inside `start()`: Doctor.swift
        /// has no externally observable success/fail signal it could
        /// override after the fact the way it does for its own local checks
        /// (`RESC_DOCTOR_INJECT=prepare`). Always `false` outside doctor
        /// mode; never set by the real host or HarnessSender.
        public var forcePrepareFailure: Bool = false

        public init(width: Int32, height: Int32, fps: Double = 60.0, bitrateBps: UInt32 = 20_000_000,
                    keyframeIntervalSeconds: Double = 1.0, codec: Codec = .h264) {
            self.width = width
            self.height = height
            self.fps = fps
            self.bitrateBps = bitrateBps
            self.keyframeIntervalSeconds = keyframeIntervalSeconds
            self.codec = codec
        }

        /// Computes appropriate bitrate based on resolution and codec.
        public static func defaultBitrate(width: Int32, height: Int32, codec: Codec) -> UInt32 {
            let is4K = width >= 3840 || height >= 2160
            switch codec {
            case .h264: return is4K ? 50_000_000 : 20_000_000
            case .hevc: return is4K ? 40_000_000 : 20_000_000
            }
        }
    }

    /// `Data, isKeyframe, presentationTime, encodeDurationMs, identity` — the
    /// last parameter is the capture identity submitted alongside this frame
    /// (`encode(identity:)`), recovered exactly for this exact submit
    /// (A00_REMEDIATION_PLAN.md §4 items 4–5). `nil` for callers with no
    /// capture identity to carry (e.g. HarnessSender's synthetic source).
    public typealias OutputCallback = (Data, Bool, CMTime, Double, FrameIdentity?) -> Void

    // MARK: - Properties

    private var session: VTCompressionSession?
    private let config: Config
    private let outputCallback: OutputCallback
    private var frameCount: UInt64 = 0
    private var keyframeCount: UInt64 = 0
    private var totalEncodeTimeMs: Double = 0
    private var pendingForceKeyframe = false

    // In-flight encode gate (see encode()): submit-thread and VT-callback
    // both touch these, hence the lock.
    private let pendingLock = NSLock()
    private var pendingEncodes = 0
    public private(set) var encodeSkips: UInt64 = 0

    public init(config: Config, outputCallback: @escaping OutputCallback) {
        self.config = config
        self.outputCallback = outputCallback
    }

    deinit { stop() }

    // MARK: - Start / Stop

    public func start() throws {
        let codecType: CMVideoCodecType
        let profileLevel: CFString

        switch config.codec {
        case .h264:
            codecType = kCMVideoCodecType_H264
            profileLevel = kVTProfileLevel_H264_High_AutoLevel
        case .hevc:
            codecType = kCMVideoCodecType_HEVC
            profileLevel = kVTProfileLevel_HEVC_Main_AutoLevel
        }

        var session: VTCompressionSession?
        let status = VTCompressionSessionCreate(
            allocator: kCFAllocatorDefault,
            width: config.width,
            height: config.height,
            codecType: codecType,
            // nil/nil (native-4K experiment, 2026-08-06): the OpenDisplay
            // session — same silicon, same 2160x3840 — encodes in <=20ms
            // where ours took ~41ms; its creation passes NO encoder
            // specification and NO image-buffer attributes, letting VT pick
            // its preferred encoder instance and buffer path. Hardware use
            // is verified at runtime via the UsingHardwareAccelerated query
            // logged below, so the explicit Enable dict adds nothing.
            encoderSpecification: nil,
            imageBufferAttributes: nil,
            compressedDataAllocator: nil,
            outputCallback: nil,
            refcon: nil,
            compressionSessionOut: &session
        )

        guard status == noErr, let session = session else {
            throw EncoderError.sessionCreationFailed(status)
        }

        // Every VTSessionSetProperty call below is checked
        // (A00_REMEDIATION_PLAN.md §5 R3a / F5 — pulled forward from its
        // former T1 slot): on failure the session is invalidated before
        // throwing so no half-configured session ever leaks past this
        // method. `key as String` gives the exact property name (these
        // constants' underlying CFString values are the bare property
        // names, e.g. "AverageBitRate") for EncoderError.propertySetFailed.
        func setProperty(_ key: CFString, _ value: CFTypeRef) throws {
            let status = VTSessionSetProperty(session, key: key, value: value)
            guard status == noErr else {
                VTCompressionSessionInvalidate(session)
                throw EncoderError.propertySetFailed(key: key as String, status: status)
            }
        }

        // Low-latency streaming settings (shared for both codecs)
        try setProperty(kVTCompressionPropertyKey_RealTime, kCFBooleanTrue)
        try setProperty(kVTCompressionPropertyKey_ProfileLevel, profileLevel)
        try setProperty(kVTCompressionPropertyKey_AverageBitRate, config.bitrateBps as CFNumber)

        // Keyframe interval
        let keyframeInterval = Int32(config.fps * config.keyframeIntervalSeconds)
        try setProperty(kVTCompressionPropertyKey_MaxKeyFrameInterval, keyframeInterval as CFNumber)
        try setProperty(kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, config.keyframeIntervalSeconds as CFNumber)

        // NO DataRateLimits (native-4K trace finding, 2026-08-05: capture->
        // encode p50 41.6ms): the hard 2x/0.1s window is overflowed by every
        // 4K keyframe, and fighting it costs the encoder its realtime
        // budget. AverageBitRate alone governs rate — the OpenDisplay-proven
        // configuration on this same silicon.

        // Encoder paces itself for this cadence (also OpenDisplay-proven;
        // was previously unset).
        try setProperty(kVTCompressionPropertyKey_ExpectedFrameRate, config.fps as CFNumber)

        // No B-frames (reduces latency)
        try setProperty(kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse)

        // Zero frame-delay window (native-4K, 2026-08-05, mirroring the
        // OpenDisplay-proven realtime config): the encoder may not hold
        // frames to optimize across a window. OPTIONAL like the speed hint
        // below — this VT H.264 session rejects it with -12900
        // (kVTPropertyNotSupportedErr) on some configurations, and with
        // RealTime + no-reordering already set it is a refinement, not
        // load-bearing.
        let delayStatus = VTSessionSetProperty(session, key: kVTCompressionPropertyKey_MaxFrameDelayCount,
                                               value: 0 as CFNumber)
        if delayStatus != noErr {
            print("[RESC] WARNING: VTSessionSetProperty(MaxFrameDelayCount) failed: status=\(delayStatus) — continuing (optional realtime refinement, not load-bearing)")
        }

        // Entropy mode deliberately UNSET for H.264 (native-4K finding,
        // 2026-08-05): forcing CABAC fights PrioritizeEncodingSpeedOverQuality
        // at 8.3MP — let VT pick the entropy coder that meets the realtime
        // budget. Previous behavior, removed:
        //   if config.codec == .h264 {
        //       try setProperty(kVTCompressionPropertyKey_H264EntropyMode, kVTH264EntropyMode_CABAC)
        //   }

        if #available(macOS 14.0, *) {
            // Deliberate exception (A00_REMEDIATION_PLAN.md §5 R3a): this is
            // a macOS 14+ speed/quality hint, not load-bearing for the
            // encode path itself, so it is the one property allowed to
            // WARN-and-continue instead of aborting session setup on
            // failure. The exact status is named so the warning stays
            // diagnostic rather than silent.
            let speedStatus = VTSessionSetProperty(session, key: kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality,
                                                    value: kCFBooleanFalse)
            if speedStatus != noErr {
                print("[RESC] WARNING: VTSessionSetProperty(PrioritizeEncodingSpeedOverQuality) failed: status=\(speedStatus) — continuing (optional quality hint, not load-bearing)")
            }
        }

        // Diagnostic (native-4K, 2026-08-06): is this session actually
        // hardware? 41ms/frame at 4K despite the Enable spec smells of a
        // silent software fallback.
        var usingHW: CFTypeRef?
        let hwStatus = VTSessionCopyProperty(session, key: kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
                                             allocator: nil, valueOut: &usingHW)
        print("[RESC] Encoder hardware-accelerated: \(hwStatus == noErr ? String(describing: usingHW) : "query failed (\(hwStatus))")")

        var prepareStatus = VTCompressionSessionPrepareToEncodeFrames(session)
        if config.forcePrepareFailure {
            // Doctor-mode failure-injection seam — see Config.forcePrepareFailure's
            // doc comment. The real call above still ran; only the status
            // this method reacts to is overridden, with an unmistakably
            // synthetic value so it is never confused with a genuine
            // OSStatus in logs/evidence.
            prepareStatus = -123_456_789
        }
        guard prepareStatus == noErr else {
            VTCompressionSessionInvalidate(session)
            throw EncoderError.prepareFailed(prepareStatus)
        }
        self.session = session

        let bitrateStr = config.bitrateBps >= 1_000_000
            ? "\(config.bitrateBps / 1_000_000)Mbps"
            : "\(config.bitrateBps / 1_000)Kbps"
        print("[RESC] Encoder started: \(config.codec) \(config.width)x\(config.height), \(bitrateStr)")
    }

    public func stop() {
        if let session = session {
            VTCompressionSessionCompleteFrames(session, untilPresentationTimeStamp: .invalid)
            VTCompressionSessionInvalidate(session)
            self.session = nil
        }
        if frameCount > 0 {
            let avgMs = totalEncodeTimeMs / Double(frameCount)
            print("[RESC] Encoder stopped: \(frameCount) frames, \(keyframeCount) KF, avg \(String(format: "%.1f", avgMs))ms [\(config.codec)]")
        }
    }

    // MARK: - Encode

    /// `identity`, when non-nil, is the exact capture identity of
    /// `pixelBuffer` — captured here in the per-submit closure exactly like
    /// `presentationTime`/`encodeStart` already are, so the asynchronous
    /// output callback below recovers the identity belonging to this exact
    /// submitted frame, not whatever the latest capture happens to be by the
    /// time the callback fires (A00_REMEDIATION_PLAN.md §4 items 4–5).
    public func encode(pixelBuffer: CVPixelBuffer, presentationTime: CMTime, identity: FrameIdentity? = nil) {
        guard let session = session else { return }

        // At most ONE frame inside VT at a time (native-4K trace finding,
        // 2026-08-05: capture->encode p50 41.6ms at ~58fps THROUGHPUT — the
        // encoder kept up but pipelined ~2.5 frames deep when every capture
        // was submitted unconditionally; the delay was queueing, not encode
        // speed). A capture arriving while one is in flight is dropped
        // BEFORE encode — reference-chain-safe (the next encoded frame
        // references the last ENCODED frame). Mirrors OpenDisplay's proven
        // maxPendingEncodes=1. The completion handler below decrements
        // UNCONDITIONALLY (before its guards), so error frames can't leak
        // the gate.
        pendingLock.lock()
        // Depth 2, not 1 (native-4K, 2026-08-06): 4K encode (~19ms) exceeds
        // the 16.7ms frame period, so a 1-deep gate skipped every frame that
        // arrived mid-encode and halved throughput to ~28fps. Two in flight
        // pipelines to full rate (2 frames / 19ms ≈ 105fps ceiling) while
        // bounding queueing to one extra slot — per-frame encode latency
        // stays ~19ms, wait-for-slot ≤ a few ms. (OpenDisplay's depth-1
        // works only because its encode beats the frame period.)
        if pendingEncodes >= 2 {
            encodeSkips += 1
            pendingLock.unlock()
            return
        }
        pendingEncodes += 1
        // Consume the force-keyframe flag inside the same lock forceKeyframe()
        // takes — an unsynchronized read/clear could lose a request landing
        // between them (keyframe-fix review finding 6). Consumed only after
        // the gate passes, so a gate-skipped frame can't swallow the request.
        let forceKF = pendingForceKeyframe
        pendingForceKeyframe = false
        pendingLock.unlock()

        let encodeStart = CFAbsoluteTimeGetCurrent()

        var properties: [CFString: Any]? = nil
        if forceKF {
            properties = [kVTEncodeFrameOptionKey_ForceKeyFrame: true]
        }

        let codec = config.codec
        let status = VTCompressionSessionEncodeFrame(
            session,
            imageBuffer: pixelBuffer,
            presentationTimeStamp: presentationTime,
            duration: CMTime(value: 1, timescale: Int32(config.fps)),
            frameProperties: properties as CFDictionary?,
            infoFlagsOut: nil
        ) { [weak self] status, flags, sampleBuffer in
            guard let self = self else { return }
            // Gate release FIRST, before any guard — error/nil-buffer frames
            // must not leak the in-flight slot.
            self.pendingLock.lock()
            self.pendingEncodes -= 1
            self.pendingLock.unlock()
            let encodeDuration = (CFAbsoluteTimeGetCurrent() - encodeStart) * 1000.0

            guard status == noErr, let sampleBuffer = sampleBuffer else { return }

            let result: (Data, Bool)?
            switch codec {
            case .h264:
                result = NALUPackager.convertH264ToAnnexB(sampleBuffer: sampleBuffer)
            case .hevc:
                result = NALUPackager.convertHEVCToAnnexB(sampleBuffer: sampleBuffer)
            }

            guard let (annexBData, isKeyframe) = result else { return }

            self.frameCount += 1
            self.totalEncodeTimeMs += encodeDuration
            if isKeyframe { self.keyframeCount += 1 }

            if self.frameCount % 300 == 0 {
                let avgMs = self.totalEncodeTimeMs / Double(self.frameCount)
                print("[RESC] Encode: \(self.frameCount) frames, \(self.keyframeCount) KF, avg \(String(format: "%.1f", avgMs))ms [\(codec)]")
            }

            self.outputCallback(annexBData, isKeyframe, presentationTime, encodeDuration, identity)
        }

        if status != noErr {
            // Synchronous submission failure: VT does not invoke the output
            // handler for a frame it never accepted — release the slot here.
            pendingLock.lock()
            pendingEncodes -= 1
            pendingLock.unlock()
            print("[RESC] Encode frame failed: \(status)")
        }
    }

    public func forceKeyframe() {
        pendingLock.lock()
        pendingForceKeyframe = true
        pendingLock.unlock()
    }

    public func updateBitrate(_ newBitrateBps: UInt32) {
        guard let session = session else { return }
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_AverageBitRate,
                             value: newBitrateBps as CFNumber)
    }

    public var stats: (frames: UInt64, keyframes: UInt64, avgEncodeMs: Double) {
        let avg = frameCount > 0 ? totalEncodeTimeMs / Double(frameCount) : 0
        return (frameCount, keyframeCount, avg)
    }

    /// Doctor-mode accessor (IMPLEMENTATION_PLAN_V11.md §11.4): exposes the
    /// underlying session so HostDoctor can VTSessionCopyProperty a
    /// requested-vs-observed read-back after start(). Not used by normal
    /// streaming code paths.
    public var vtSession: VTCompressionSession? { session }

    public enum EncoderError: Error, CustomStringConvertible {
        case sessionCreationFailed(OSStatus)
        /// A `VTSessionSetProperty` call failed (A00_REMEDIATION_PLAN.md §5
        /// R3a / F5). `key` is the property's bare name (e.g.
        /// "AverageBitRate"), read back from the CFString constant itself
        /// so it can never drift from what was actually set.
        case propertySetFailed(key: String, status: OSStatus)
        /// `VTCompressionSessionPrepareToEncodeFrames` failed.
        case prepareFailed(OSStatus)
        public var description: String {
            switch self {
            case .sessionCreationFailed(let s): return "VTCompressionSession creation failed: \(s)"
            case .propertySetFailed(let key, let status): return "VTSessionSetProperty(\(key)) failed: \(status)"
            case .prepareFailed(let status): return "VTCompressionSessionPrepareToEncodeFrames failed: \(status)"
            }
        }
    }
}
