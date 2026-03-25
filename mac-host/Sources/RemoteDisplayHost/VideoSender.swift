import Foundation
import Network

/// Sends encoded H.264 frames as chunked UDP packets to the receiver.
/// Implements the VideoChunkHeader binary protocol from the spec.
final class VideoSender {

    // MARK: - Types

    struct StreamConfig {
        let streamID: UInt32
        let configID: UInt32
        let width: UInt16
        let height: UInt16
        let codec: UInt8 // 0=H.264, 1=HEVC
    }

    // MARK: - Properties

    private var connection: NWConnection?
    private let queue = DispatchQueue(label: "com.resc.video-sender", qos: .userInteractive)
    private var config: StreamConfig
    private var frameID: UInt32 = 0
    private var totalBytesSent: UInt64 = 0
    private var totalPacketsSent: UInt64 = 0

    // MARK: - Init

    init(config: StreamConfig) {
        self.config = config
    }

    // MARK: - Connect / Disconnect

    func connect(host: String, port: UInt16) {
        let endpoint = NWEndpoint.hostPort(
            host: NWEndpoint.Host(host),
            port: NWEndpoint.Port(rawValue: port)!
        )
        let params = NWParameters.udp
        params.serviceClass = .interactiveVideo
        // Set Don't Fragment to catch MTU issues early
        params.requiredInterfaceType = .wiredEthernet

        let conn = NWConnection(to: endpoint, using: params)
        conn.stateUpdateHandler = { state in
            switch state {
            case .ready:
                print("[RESC] Video UDP connected to \(host):\(port)")
            case .failed(let err):
                print("[RESC] Video UDP failed: \(err)")
            default:
                break
            }
        }
        conn.start(queue: queue)
        self.connection = conn
    }

    func disconnect() {
        connection?.cancel()
        connection = nil
        print("[RESC] Video sender: \(totalPacketsSent) packets, \(totalBytesSent / 1024)KB sent")
    }

    // MARK: - Send Frame

    /// Send an encoded frame as chunked UDP packets.
    /// - Parameters:
    ///   - data: Annex B encoded frame data (payload only, no UDP headers)
    ///   - isKeyframe: Whether this is a keyframe
    ///   - timestampUs: Microseconds since session start
    func sendFrame(data: Data, isKeyframe: Bool, timestampUs: UInt64) {
        guard let connection = connection else { return }

        let currentFrameID = frameID
        frameID &+= 1

        let maxPayload = ProtocolConstants.maxVideoPayloadBytes
        let totalChunks = (data.count + maxPayload - 1) / maxPayload
        guard totalChunks <= Int(UInt16.max) else {
            print("[RESC] Frame too large: \(data.count) bytes, \(totalChunks) chunks")
            return
        }

        for chunkIdx in 0..<totalChunks {
            let payloadOffset = chunkIdx * maxPayload
            let payloadSize = min(maxPayload, data.count - payloadOffset)
            let chunkPayload = data[payloadOffset..<payloadOffset + payloadSize]

            // Build packet: PacketPrefix(6) + VideoChunkHeader(32) + payload
            var packet = Data(capacity: ProtocolConstants.videoTotalHeaderBytes + payloadSize)

            // === PacketPrefix (6 bytes) ===
            packet.append(contentsOf: ProtocolConstants.magic)
            packet.append(ProtocolConstants.protocolVersion)
            packet.append(ProtocolConstants.packetTypeVideoChunk)

            // === Per-packet fields (12 bytes, always valid) ===
            appendLE(&packet, config.streamID)
            appendLE(&packet, config.configID)
            appendLE(&packet, currentFrameID)
            appendLE(&packet, UInt16(chunkIdx))
            appendLE(&packet, UInt16(payloadSize))

            // === Per-frame fields (20 bytes, valid when chunk_id==0, zero otherwise) ===
            if chunkIdx == 0 {
                appendLE(&packet, timestampUs)
                packet.append(isKeyframe ? 1 : 0)       // is_keyframe: u8
                packet.append(config.codec)               // codec: u8
                appendLE(&packet, config.width)
                appendLE(&packet, config.height)
                appendLE(&packet, UInt16(totalChunks))
                appendLE(&packet, UInt32(data.count))     // total_bytes (payload only)
            } else {
                // Zero-fill per-frame fields
                packet.append(contentsOf: [UInt8](repeating: 0, count: 20))
            }

            // === Payload ===
            packet.append(chunkPayload)

            // Send
            connection.send(content: packet, completion: .contentProcessed { error in
                if let error = error {
                    print("[RESC] UDP send error: \(error)")
                }
            })

            totalBytesSent += UInt64(packet.count)
            totalPacketsSent += 1
        }
    }

    // MARK: - Helpers

    private func appendLE(_ data: inout Data, _ value: UInt16) {
        var v = value.littleEndian
        data.append(Data(bytes: &v, count: 2))
    }

    private func appendLE(_ data: inout Data, _ value: UInt32) {
        var v = value.littleEndian
        data.append(Data(bytes: &v, count: 4))
    }

    private func appendLE(_ data: inout Data, _ value: UInt64) {
        var v = value.littleEndian
        data.append(Data(bytes: &v, count: 8))
    }

    // MARK: - Stats

    var stats: (packets: UInt64, bytes: UInt64) {
        (totalPacketsSent, totalBytesSent)
    }
}
