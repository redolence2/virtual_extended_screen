import Foundation
import SwiftProtobuf
import RescProto

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

    // MARK: - Properties

    private let config: Config
    private let controlChannel: ControlChannel
    private let discovery: Discovery
    /// Single source of lifecycle state (Item 10: eliminates parallel state enums)
    private let sm = SessionStateMachine(gracePeriodSec: 30.0)
    private var awaitingStreamingReady = false
    private var videoSender: VideoSender?
    private var streamID: UInt32 = 0
    private var configID: UInt32 = 0
    private var sessionID: UInt64 = 0
    private var videoPort: UInt16 = 0
    private var lastIDRRequestTime: Date?

    /// Called when streaming starts — provides VideoSender for the encoder to use.
    var onStreamingStart: ((VideoSender) -> Void)?
    /// Called to force a keyframe (ensures first frame client receives has SPS/PPS).
    var onForceKeyframe: (() -> Void)?
    /// Called on disconnect to release stuck keys/buttons.
    var onReleaseInput: (() -> Void)?
    /// Called when streaming starts to send initial display settings.
    var onSendInitialSettings: (() -> Void)?

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
                if self?.sm.state == .disconnected {
                    self?.sm.handleReconnect()
                } else {
                    self?.sm.transition(to: .negotiating)
                }
                self?.awaitingStreamingReady = false
            }
        )

        sm.transition(to: .waitingForClient)
        print("[RESC] Host session started, waiting for client on port \(config.controlPort)")
    }

    // MARK: - Message Handling

    private func handleMessage(_ data: Data) {
        // A0.0 clock-ping intercept (plan v11 §10, §12): capture t2 as close
        // to entry as possible — before any decode work — so the responder
        // timestamp reflects receipt, not parsing latency. Wrapped in try?:
        // any decode failure (including legitimate legacy v1 payloads that
        // happen not to parse as this exact schema) falls straight through
        // to the unchanged legacy dispatch below.
        let t2 = RescClockBridge.continuousNowUs()
        if let envelope = try? Resc_Control_Envelope(serializedBytes: data), let payload = envelope.payload {
            switch payload {
            case .clockPing(let ping):
                // Intercept completely — do not fall through, so the legacy
                // 0xFA byte-scan in handleStreamingMessage never sees clock
                // traffic.
                handleClockPing(ping, sessionID: envelope.sessionID, t2: t2)
                return
            case .clockPong:
                // Host is responder-only; an inbound pong is unexpected but
                // harmless. Ignore.
                return
            case .requestIdr(let idr):
                // Typed decode — replaces the legacy 0xFA byte-scan, which
                // fired on any control payload containing that byte value.
                // reason is the client's IDRReason discriminant, logged so
                // keyframe storms are attributable (real corruption vs. bug).
                if sm.state == .streaming {
                    forceKeyframeRateLimited(reason: idr.reason)
                }
                return
            default:
                break // not clock traffic — fall through to the legacy path
            }
        }

        if sm.state == .negotiating && awaitingStreamingReady {
            handleStreamingReady(data)
        } else if sm.state == .negotiating {
            handleModeRequest(data)
        } else {
            handleStreamingMessage(data)
        }
    }

    /// Answers a `ClockPing` with a `ClockPong` on the control channel
    /// (IMPLEMENTATION_PLAN_V11.md §10: four-timestamp trace-mode clock
    /// sync). `t1` is echoed from the ping; `t2` was captured at
    /// `handleMessage` entry; `t3` is captured here, immediately before
    /// serializing and sending the reply.
    private func handleClockPing(_ ping: Resc_Control_ClockPing, sessionID: UInt64, t2: UInt64) {
        var pong = Resc_Control_ClockPong()
        pong.t1MonoUs = ping.t1MonoUs
        pong.t2MonoUs = t2
        pong.seq = ping.seq
        pong.t3MonoUs = RescClockBridge.continuousNowUs()

        var outEnvelope = Resc_Control_Envelope()
        outEnvelope.sessionID = sessionID
        outEnvelope.protocolVersion = UInt32(ProtocolConstants.protocolVersion)
        outEnvelope.payload = .clockPong(pong)

        guard let serialized = try? outEnvelope.serializedData() else { return }
        controlChannel.send(data: serialized)

        if RescTrace.enabled {
            RescTrace.shared.clockSample(seq: pong.seq, t1: pong.t1MonoUs, t2: pong.t2MonoUs, t3: pong.t3MonoUs)
        }
    }

    private func handleModeRequest(_ data: Data) {
        // Mark sub-state to prevent duplicate handling
        awaitingStreamingReady = true

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

    /// Handle messages during streaming. RequestIDR is intercepted by the
    /// typed envelope decode in handleMessage; anything reaching here
    /// (Stats, unknown legacy payloads) is consumed silently.
    private func handleStreamingMessage(_ data: Data) {
    }

    private func forceKeyframeRateLimited(reason: Resc_Control_RequestIDR.Reason) {
        let lastIDRTime = lastIDRRequestTime ?? Date.distantPast
        let elapsed = Date().timeIntervalSince(lastIDRTime)
        if elapsed >= 0.25 { // Rate limit: 250ms
            lastIDRRequestTime = Date()
            onForceKeyframe?()
            print("[RESC] IDR requested by client (reason=\(reason), rate-limited)")
        }
    }

    private func handleStreamingReady(_ data: Data) {
        guard awaitingStreamingReady else { return }
        awaitingStreamingReady = false
        print("[RESC] StreamingReady received, starting video")
        startStreaming()
    }

    private func startStreaming() {
        sm.transition(to: .streaming)

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

        // Night Shift state will be sent on next monitor poll (~2s)
    }

    /// Send DisplaySettings (warm_strength) to client via control channel.
    /// Builds protobuf bytes inline — avoids calling private helpers from main queue.
    func sendDisplaySettings(warmStrength: Float) {
        var data = Data()

        // session_id: field 1, varint. Tag = 8
        data.append(0x08)
        var sid = sessionID
        while sid > 0x7F { data.append(UInt8(sid & 0x7F) | 0x80); sid >>= 7 }
        data.append(UInt8(sid))

        // protocol_version: field 2, varint. Tag = 16
        data.append(0x10)
        data.append(UInt8(ProtocolConstants.protocolVersion))

        // display_settings: field 32, length-delimited
        // Tag = (32 << 3) | 2 = 258 → varint [0x82, 0x02]
        data.append(0x82); data.append(0x02)
        data.append(0x05) // inner length = 5
        data.append(0x0D) // warm_strength: field 1, wire type 5 (fixed32). Tag = 13
        var s = warmStrength
        withUnsafeBytes(of: &s) { data.append(contentsOf: $0) }

        controlChannel.send(data: data)
        print("[RESC] Night Shift → client (warm=\(warmStrength))")
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
        // field 10: codec (0=H264, 1=HEVC) — must write explicitly even for 0
        let codecValue: UInt32 = CommandLine.arguments.contains("--hevc") ? 1 : 0
        if codecValue > 0 {
            appendProtoUInt32(&mc, field: 10, value: codecValue)
        }
        let profileValue: UInt32 = codecValue == 1 ? 10 : 3  // HEVC_MAIN=10, H264_HIGH=3
        appendProtoUInt32(&mc, field: 11, value: profileValue)
        appendProtoUInt32(&mc, field: 12, value: codecLevelIdc) // codec_level_idc
        appendProtoUInt32(&mc, field: 13, value: bitrateBps)  // bitrate_bps
        appendProtoUInt32(&mc, field: 14, value: 1400)        // max_datagram_bytes
        appendProtoUInt32(&mc, field: 15, value: UInt32(ProtocolConstants.maxVideoPayloadBytes))
        // 2048, not 512 (native-4K finding, 2026-08-05): real-detail 4K
        // keyframes exceed 512 chunks (~716KB) — at 512 every keyframe was
        // oversize-dropped at the client assembler, reference-starving the
        // decoder into a black screen. 2048 (~2.8MB budget) comfortably
        // covers maxFrameBytes' 2MB ceiling.
        appendProtoUInt32(&mc, field: 16, value: 2048)        // max_total_chunks_per_frame (supports 4K IDR spikes)
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
