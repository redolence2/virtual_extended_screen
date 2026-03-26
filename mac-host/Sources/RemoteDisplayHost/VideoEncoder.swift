import Foundation
import VideoToolbox
import CoreMedia
import CoreVideo

/// VideoToolbox H.264 hardware encoder with low-latency streaming settings.
/// Consumes CVPixelBuffers from LatestFrameSlot, outputs Annex B NAL units.
final class VideoEncoder {

    // MARK: - Configuration

    struct Config {
        var width: Int32
        var height: Int32
        var fps: Double = 60.0
        var bitrateBps: UInt32 = 20_000_000  // 20Mbps default (1080p)
        var keyframeIntervalSeconds: Double = 1.0
        var maxReferenceFrames: Int32 = 4
        var profile: CFString = kVTProfileLevel_H264_High_AutoLevel

        /// Computes appropriate bitrate based on resolution.
        static func defaultBitrate(width: Int32, height: Int32) -> UInt32 {
            if width >= 3840 || height >= 2160 {
                return 50_000_000  // 50Mbps for 4K
            }
            return 20_000_000  // 20Mbps for 1080p and below
        }
    }

    /// Called for each encoded frame. (annexBData, isKeyframe, presentationTimestamp, encodeDurationMs)
    typealias OutputCallback = (Data, Bool, CMTime, Double) -> Void

    // MARK: - Properties

    private var session: VTCompressionSession?
    private let config: Config
    private let outputCallback: OutputCallback
    private var frameCount: UInt64 = 0
    private var keyframeCount: UInt64 = 0
    private var totalEncodeTimeMs: Double = 0
    private var lastPeriodicParamSetTime: CFAbsoluteTime = 0
    private var pendingForceKeyframe = false

    // MARK: - Init

    init(config: Config, outputCallback: @escaping OutputCallback) {
        self.config = config
        self.outputCallback = outputCallback
    }

    deinit {
        stop()
    }

    // MARK: - Start / Stop

    func start() throws {
        var session: VTCompressionSession?
        let status = VTCompressionSessionCreate(
            allocator: kCFAllocatorDefault,
            width: config.width,
            height: config.height,
            codecType: kCMVideoCodecType_H264,
            encoderSpecification: [
                kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder: true
            ] as CFDictionary,
            imageBufferAttributes: [
                kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
            ] as CFDictionary,
            compressedDataAllocator: nil,
            outputCallback: nil,  // we use the block-based API below
            refcon: nil,
            compressionSessionOut: &session
        )

        guard status == noErr, let session = session else {
            throw EncoderError.sessionCreationFailed(status)
        }

        // --- Low-latency streaming settings ---

        // Real-time encoding
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)

        // H.264 High profile
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_ProfileLevel,
                             value: kVTProfileLevel_H264_High_AutoLevel)

        // Bitrate
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_AverageBitRate,
                             value: config.bitrateBps as CFNumber)

        // Keyframe interval
        let keyframeInterval = Int32(config.fps * config.keyframeIntervalSeconds)
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_MaxKeyFrameInterval,
                             value: keyframeInterval as CFNumber)
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration,
                             value: config.keyframeIntervalSeconds as CFNumber)

        // Limit max frame size to prevent huge keyframes
        // Set data rate limits tighter: max 2x average over 0.1s window
        let tightBytesPerSec = Double(config.bitrateBps) / 8.0
        let tightLimits: [Double] = [tightBytesPerSec * 2.0, 0.1]
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_DataRateLimits,
                             value: tightLimits as CFArray)

        // No B-frames (reduces latency, simplifies decode)
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_AllowFrameReordering,
                             value: kCFBooleanFalse)

        // Max reference frames (conservative for H.264 Level 4.1/5.1)
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_H264EntropyMode,
                             value: kVTH264EntropyMode_CABAC)

        // Expected frame rate
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_ExpectedFrameRate,
                             value: config.fps as CFNumber)

        // Low latency mode (if available)
        if #available(macOS 14.0, *) {
            VTSessionSetProperty(session, key: kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality,
                                 value: kCFBooleanFalse)
        }

        VTCompressionSessionPrepareToEncodeFrames(session)
        self.session = session

        let bitrateStr = config.bitrateBps >= 1_000_000
            ? "\(config.bitrateBps / 1_000_000)Mbps"
            : "\(config.bitrateBps / 1_000)Kbps"
        print("[RESC] Encoder started: H.264 High, \(config.width)x\(config.height), \(bitrateStr), keyframe every \(config.keyframeIntervalSeconds)s")
    }

    func stop() {
        if let session = session {
            VTCompressionSessionCompleteFrames(session, untilPresentationTimeStamp: .invalid)
            VTCompressionSessionInvalidate(session)
            self.session = nil
        }
        if frameCount > 0 {
            let avgMs = totalEncodeTimeMs / Double(frameCount)
            print("[RESC] Encoder stopped: \(frameCount) frames, \(keyframeCount) keyframes, avg encode \(String(format: "%.1f", avgMs))ms")
        }
    }

    // MARK: - Encode

    /// Encode a single frame. Called from encoder thread.
    func encode(pixelBuffer: CVPixelBuffer, presentationTime: CMTime) {
        guard let session = session else { return }

        let encodeStart = CFAbsoluteTimeGetCurrent()

        // Force keyframe if requested
        var properties: [CFString: Any]? = nil
        if pendingForceKeyframe {
            properties = [kVTEncodeFrameOptionKey_ForceKeyFrame: true]
            pendingForceKeyframe = false
        }

        let status = VTCompressionSessionEncodeFrame(
            session,
            imageBuffer: pixelBuffer,
            presentationTimeStamp: presentationTime,
            duration: CMTime(value: 1, timescale: Int32(config.fps)),
            frameProperties: properties as CFDictionary?,
            infoFlagsOut: nil
        ) { [weak self] status, flags, sampleBuffer in
            guard let self = self else { return }
            let encodeDuration = (CFAbsoluteTimeGetCurrent() - encodeStart) * 1000.0

            guard status == noErr, let sampleBuffer = sampleBuffer else {
                if status != noErr {
                    print("[RESC] Encode error: \(status)")
                }
                return
            }

            guard let (annexBData, isKeyframe) = NALUPackager.convertToAnnexB(sampleBuffer: sampleBuffer) else {
                return
            }

            self.frameCount += 1
            self.totalEncodeTimeMs += encodeDuration
            if isKeyframe { self.keyframeCount += 1 }

            // Log periodically
            if self.frameCount % 300 == 0 {
                let avgMs = self.totalEncodeTimeMs / Double(self.frameCount)
                print("[RESC] Encode stats: \(self.frameCount) frames, \(self.keyframeCount) keyframes, avg \(String(format: "%.1f", avgMs))ms, last \(String(format: "%.1f", encodeDuration))ms")
            }

            self.outputCallback(annexBData, isKeyframe, presentationTime, encodeDuration)
        }

        if status != noErr {
            print("[RESC] VTCompressionSessionEncodeFrame failed: \(status)")
        }
    }

    /// Request a keyframe on the next encode call.
    func forceKeyframe() {
        pendingForceKeyframe = true
    }

    /// Update encoder bitrate dynamically (for adaptive bitrate).
    func updateBitrate(_ newBitrateBps: UInt32) {
        guard let session = session else { return }
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_AverageBitRate,
                             value: newBitrateBps as CFNumber)
        print("[RESC] Encoder bitrate updated: \(newBitrateBps / 1_000_000)Mbps")
    }

    // MARK: - Stats

    var stats: (frames: UInt64, keyframes: UInt64, avgEncodeMs: Double) {
        let avg = frameCount > 0 ? totalEncodeTimeMs / Double(frameCount) : 0
        return (frameCount, keyframeCount, avg)
    }

    // MARK: - Errors

    enum EncoderError: Error, CustomStringConvertible {
        case sessionCreationFailed(OSStatus)

        var description: String {
            switch self {
            case .sessionCreationFailed(let s): return "VTCompressionSession creation failed: \(s)"
            }
        }
    }
}
