import Foundation
import SwiftProtobuf

/// Manages the host-side session: control channel, mode negotiation, video sending.
/// Implements the Idle → Negotiating → Streaming state progression.
final class HostSession {

    // MARK: - Config

    struct Config {
        var controlPort: UInt16 = 9870
        var displayWidth: Int
        var displayHeight: Int
        var refreshRate: Int = 60
        var bitrateBps: UInt32
    }

    // MARK: - State

    enum State {
        case idle
        case waitingForClient
        case negotiating
        case waitingForStreamingReady
        case streaming
    }

    // MARK: - Properties

    private let config: Config
    private let controlChannel: ControlChannel
    private let discovery: Discovery
    private var videoSender: VideoSender?
    private var state: State = .idle
    private var streamID: UInt32 = 0
    private var configID: UInt32 = 0
    private var sessionID: UInt64 = 0
    private var videoPort: UInt16 = 0

    /// Called when streaming starts — provides VideoSender for the encoder to use.
    var onStreamingStart: ((VideoSender) -> Void)?
    /// Called to force a keyframe (ensures first frame client receives has SPS/PPS).
    var onForceKeyframe: (() -> Void)?

    // MARK: - Init

    init(config: Config) {
        self.config = config
        self.controlChannel = ControlChannel(port: config.controlPort)
        self.discovery = Discovery(controlPort: config.controlPort)
    }

    // MARK: - Start

    func start() throws {
        // Start mDNS advertisement
        discovery.advertise()

        // Start control channel server
        try controlChannel.startServer(
            onMessage: { [weak self] data in self?.handleMessage(data) },
            onClientConnected: { [weak self] endpoint in
                print("[RESC] Client connected from \(endpoint)")
                self?.state = .negotiating
            }
        )

        state = .waitingForClient
        print("[RESC] Host session started, waiting for client on port \(config.controlPort)")
    }

    // MARK: - Message Handling

    private func handleMessage(_ data: Data) {
        switch state {
        case .negotiating:
            // First message from client = ModeRequest → respond with ModeConfirm + StartStreaming
            handleModeRequest(data)
        case .waitingForStreamingReady:
            // Second message = StreamingReady → start sending video
            handleStreamingReady(data)
        default:
            // Stats, RequestIDR, KeyEvent etc. — handle in later phases
            break
        }
    }

    private func handleModeRequest(_ data: Data) {
        // Transition immediately to prevent duplicate handling
        state = .waitingForStreamingReady

        // Generate session parameters
        sessionID = UInt64.random(in: 1...UInt64.max)
        streamID = UInt32.random(in: 1...UInt32.max)
        configID = 1

        // Allocate video UDP port (control port + 1)
        videoPort = config.controlPort + 1

        let bitrate = config.bitrateBps
        let codecLevel = ProtocolConstants.codecLevelIdc(
            width: config.displayWidth, height: config.displayHeight
        )
        let maxFrameBytes = ProtocolConstants.maxFrameBytes(
            bitrateBps: bitrate, fps: Double(config.refreshRate)
        )

        print("[RESC] Sending ModeConfirm: \(config.displayWidth)x\(config.displayHeight), " +
              "stream=\(streamID), config=\(configID), video_port=\(videoPort)")

        // Build ModeConfirm protobuf manually (minimal encoding)
        // In production, use generated swift-protobuf code.
        let modeConfirm = buildModeConfirmEnvelope(
            sessionID: sessionID,
            streamID: streamID,
            configID: configID,
            width: UInt32(config.displayWidth),
            height: UInt32(config.displayHeight),
            refreshMillihz: UInt32(config.refreshRate * 1000),
            codecLevelIdc: codecLevel,
            bitrateBps: bitrate,
            maxFrameBytes: maxFrameBytes,
            videoPort: UInt32(videoPort)
        )
        controlChannel.send(data: modeConfirm)

        // Send StartStreaming
        let startStreaming = buildStartStreamingEnvelope(
            sessionID: sessionID, streamID: streamID, configID: configID
        )
        controlChannel.send(data: startStreaming)

        print("[RESC] Waiting for StreamingReady from client...")
    }

    private func handleStreamingReady(_ data: Data) {
        guard state == .waitingForStreamingReady else { return }
        print("[RESC] StreamingReady received, starting video")
        startStreaming()
    }

    private func startStreaming() {
        state = .streaming

        let senderConfig = VideoSender.StreamConfig(
            streamID: streamID,
            configID: configID,
            width: UInt16(config.displayWidth),
            height: UInt16(config.displayHeight),
            codec: CommandLine.arguments.contains("--hevc") ? 1 : 0
        )
        let sender = VideoSender(config: senderConfig)

        // For Phase 3: connect to client. We need the client's IP.
        // The control channel knows the connected client's address.
        // For simplicity, we send to the same host that connected to us.
        // The video sender sends to the client's video port.
        // NOTE: In production, the client provides its IP in the control channel.
        // For Phase 3 testing: use localhost or pass client IP.

        self.videoSender = sender
        onStreamingStart?(sender)
        // Force keyframe so client's first frame has SPS/PPS
        onForceKeyframe?()
        print("[RESC] Streaming started: stream=\(streamID), config=\(configID)")
    }

    func stop() {
        videoSender?.disconnect()
        controlChannel.stop()
        discovery.stop()
    }

    // MARK: - Protobuf Encoding Helpers
    // Minimal hand-rolled protobuf encoding for Phase 3.
    // Will be replaced with generated swift-protobuf code.

    private func buildModeConfirmEnvelope(
        sessionID: UInt64, streamID: UInt32, configID: UInt32,
        width: UInt32, height: UInt32, refreshMillihz: UInt32,
        codecLevelIdc: UInt32, bitrateBps: UInt32,
        maxFrameBytes: UInt32, videoPort: UInt32
    ) -> Data {
        // Encode ModeConfirm message
        var mc = Data()
        appendProtoUInt64(&mc, field: 1, value: sessionID)    // session_id
        appendProtoUInt32(&mc, field: 2, value: streamID)     // stream_id
        appendProtoUInt32(&mc, field: 3, value: configID)     // config_id
        appendProtoUInt32(&mc, field: 4, value: width)        // actual_width
        appendProtoUInt32(&mc, field: 5, value: height)       // actual_height
        appendProtoUInt32(&mc, field: 6, value: refreshMillihz) // actual_refresh_rate_millihz
        // field 7: rotation = 0 (default, omitted)
        appendProtoUInt32(&mc, field: 8, value: width)        // stream_width
        appendProtoUInt32(&mc, field: 9, value: height)       // stream_height
        // field 10: codec = 0 (H264, default)
        appendProtoUInt32(&mc, field: 11, value: 3)           // codec_profile = H264_HIGH
        appendProtoUInt32(&mc, field: 12, value: codecLevelIdc) // codec_level_idc
        appendProtoUInt32(&mc, field: 13, value: bitrateBps)  // bitrate_bps
        appendProtoUInt32(&mc, field: 14, value: 1400)        // max_datagram_bytes
        appendProtoUInt32(&mc, field: 15, value: 1362)        // max_video_payload_bytes
        appendProtoUInt32(&mc, field: 16, value: 256)         // max_total_chunks_per_frame
        appendProtoUInt32(&mc, field: 17, value: maxFrameBytes) // max_frame_bytes
        appendProtoUInt32(&mc, field: 20, value: videoPort)   // video_port
        appendProtoUInt32(&mc, field: 21, value: videoPort + 1) // input_udp_port
        appendProtoUInt32(&mc, field: 22, value: videoPort + 2) // cursor_udp_port

        // Wrap in Envelope (field 21 = mode_confirm)
        var env = Data()
        appendProtoUInt64(&env, field: 1, value: sessionID)
        appendProtoUInt32(&env, field: 2, value: UInt32(ProtocolConstants.protocolVersion))
        appendProtoBytes(&env, field: 21, value: mc) // oneof payload: mode_confirm

        return env
    }

    private func buildStartStreamingEnvelope(
        sessionID: UInt64, streamID: UInt32, configID: UInt32
    ) -> Data {
        var ss = Data()
        appendProtoUInt32(&ss, field: 1, value: streamID)
        appendProtoUInt32(&ss, field: 2, value: configID)

        var env = Data()
        appendProtoUInt64(&env, field: 1, value: sessionID)
        appendProtoUInt32(&env, field: 2, value: UInt32(ProtocolConstants.protocolVersion))
        appendProtoBytes(&env, field: 23, value: ss) // oneof payload: start_streaming

        return env
    }

    // Minimal protobuf encoding helpers
    private func appendProtoUInt32(_ data: inout Data, field: Int, value: UInt32) {
        if value == 0 { return } // proto3 default
        let tag = UInt8((field << 3) | 0) // varint wire type
        appendVarint(&data, UInt64(tag))
        appendVarint(&data, UInt64(value))
    }

    private func appendProtoUInt64(_ data: inout Data, field: Int, value: UInt64) {
        if value == 0 { return }
        let tag = UInt64((field << 3) | 0)
        appendVarint(&data, tag)
        appendVarint(&data, value)
    }

    private func appendProtoBytes(_ data: inout Data, field: Int, value: Data) {
        let tag = UInt8((field << 3) | 2) // length-delimited wire type
        appendVarint(&data, UInt64(tag))
        appendVarint(&data, UInt64(value.count))
        data.append(value)
    }

    private func appendVarint(_ data: inout Data, _ value: UInt64) {
        var v = value
        while v >= 0x80 {
            data.append(UInt8(v & 0x7F) | 0x80)
            v >>= 7
        }
        data.append(UInt8(v))
    }
}
