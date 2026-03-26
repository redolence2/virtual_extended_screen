import Foundation
import VideoToolbox
import CoreMedia
import CoreVideo

/// VideoToolbox hardware encoder supporting H.264 and HEVC.
/// Consumes CVPixelBuffers from LatestFrameSlot, outputs Annex B NAL units.
final class VideoEncoder {

    // MARK: - Codec Selection

    enum Codec: UInt8, CustomStringConvertible {
        case h264 = 0
        case hevc = 1
        var description: String { self == .h264 ? "H.264" : "HEVC" }
    }

    // MARK: - Configuration

    struct Config {
        var width: Int32
        var height: Int32
        var fps: Double = 60.0
        var bitrateBps: UInt32 = 20_000_000
        var keyframeIntervalSeconds: Double = 1.0
        var codec: Codec = .h264

        /// Computes appropriate bitrate based on resolution and codec.
        static func defaultBitrate(width: Int32, height: Int32, codec: Codec) -> UInt32 {
            let is4K = width >= 3840 || height >= 2160
            switch codec {
            case .h264: return is4K ? 50_000_000 : 20_000_000
            case .hevc: return is4K ? 30_000_000 : 12_000_000  // HEVC: ~40% better compression
            }
        }
    }

    typealias OutputCallback = (Data, Bool, CMTime, Double) -> Void

    // MARK: - Properties

    private var session: VTCompressionSession?
    private let config: Config
    private let outputCallback: OutputCallback
    private var frameCount: UInt64 = 0
    private var keyframeCount: UInt64 = 0
    private var totalEncodeTimeMs: Double = 0
    private var pendingForceKeyframe = false

    init(config: Config, outputCallback: @escaping OutputCallback) {
        self.config = config
        self.outputCallback = outputCallback
    }

    deinit { stop() }

    // MARK: - Start / Stop

    func start() throws {
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
            encoderSpecification: [
                kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder: true
            ] as CFDictionary,
            imageBufferAttributes: [
                kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
            ] as CFDictionary,
            compressedDataAllocator: nil,
            outputCallback: nil,
            refcon: nil,
            compressionSessionOut: &session
        )

        guard status == noErr, let session = session else {
            throw EncoderError.sessionCreationFailed(status)
        }

        // Low-latency streaming settings (shared for both codecs)
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_ProfileLevel, value: profileLevel)
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_AverageBitRate,
                             value: config.bitrateBps as CFNumber)

        // Keyframe interval
        let keyframeInterval = Int32(config.fps * config.keyframeIntervalSeconds)
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_MaxKeyFrameInterval,
                             value: keyframeInterval as CFNumber)
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration,
                             value: config.keyframeIntervalSeconds as CFNumber)

        // Data rate limits
        let bytesPerSec = Double(config.bitrateBps) / 8.0
        let limits: [Double] = [bytesPerSec * 2.0, 0.1]
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_DataRateLimits,
                             value: limits as CFArray)

        // No B-frames (reduces latency)
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_AllowFrameReordering,
                             value: kCFBooleanFalse)

        // CABAC for H.264
        if config.codec == .h264 {
            VTSessionSetProperty(session, key: kVTCompressionPropertyKey_H264EntropyMode,
                                 value: kVTH264EntropyMode_CABAC)
        }

        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_ExpectedFrameRate,
                             value: config.fps as CFNumber)

        if #available(macOS 14.0, *) {
            VTSessionSetProperty(session, key: kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality,
                                 value: kCFBooleanFalse)
        }

        VTCompressionSessionPrepareToEncodeFrames(session)
        self.session = session

        let bitrateStr = config.bitrateBps >= 1_000_000
            ? "\(config.bitrateBps / 1_000_000)Mbps"
            : "\(config.bitrateBps / 1_000)Kbps"
        print("[RESC] Encoder started: \(config.codec) \(config.width)x\(config.height), \(bitrateStr)")
    }

    func stop() {
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

    func encode(pixelBuffer: CVPixelBuffer, presentationTime: CMTime) {
        guard let session = session else { return }

        let encodeStart = CFAbsoluteTimeGetCurrent()

        var properties: [CFString: Any]? = nil
        if pendingForceKeyframe {
            properties = [kVTEncodeFrameOptionKey_ForceKeyFrame: true]
            pendingForceKeyframe = false
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

            self.outputCallback(annexBData, isKeyframe, presentationTime, encodeDuration)
        }

        if status != noErr {
            print("[RESC] Encode frame failed: \(status)")
        }
    }

    func forceKeyframe() { pendingForceKeyframe = true }

    func updateBitrate(_ newBitrateBps: UInt32) {
        guard let session = session else { return }
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_AverageBitRate,
                             value: newBitrateBps as CFNumber)
    }

    var stats: (frames: UInt64, keyframes: UInt64, avgEncodeMs: Double) {
        let avg = frameCount > 0 ? totalEncodeTimeMs / Double(frameCount) : 0
        return (frameCount, keyframeCount, avg)
    }

    enum EncoderError: Error, CustomStringConvertible {
        case sessionCreationFailed(OSStatus)
        var description: String {
            switch self {
            case .sessionCreationFailed(let s): return "VTCompressionSession creation failed: \(s)"
            }
        }
    }
}
