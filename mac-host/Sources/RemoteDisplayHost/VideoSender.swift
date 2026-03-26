import Foundation

/// Sends encoded H.264 frames as chunked UDP packets to the receiver.
/// Uses raw POSIX sockets for reliability (NWConnection UDP had silent failures).
final class VideoSender {

    struct StreamConfig {
        let streamID: UInt32
        let configID: UInt32
        let width: UInt16
        let height: UInt16
        let codec: UInt8 // 0=H.264
    }

    private var fd: Int32 = -1
    private var destAddr: sockaddr_in?
    private var config: StreamConfig
    private var frameID: UInt32 = 0
    private var totalBytesSent: UInt64 = 0
    private var totalPacketsSent: UInt64 = 0

    init(config: StreamConfig) {
        self.config = config
    }

    func connect(host: String, port: UInt16) {
        fd = socket(AF_INET, SOCK_DGRAM, 0)
        guard fd >= 0 else {
            print("[RESC] UDP socket() failed: \(errno)")
            return
        }

        var addr = sockaddr_in()
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = port.bigEndian
        inet_pton(AF_INET, host, &addr.sin_addr)
        self.destAddr = addr

        print("[RESC] Video UDP sender ready → \(host):\(port)")
    }

    func disconnect() {
        if fd >= 0 {
            close(fd)
            fd = -1
        }
        print("[RESC] Video sender: \(totalPacketsSent) packets, \(totalBytesSent / 1024)KB sent")
    }

    /// Send an encoded frame as chunked UDP packets.
    func sendFrame(data: Data, isKeyframe: Bool, timestampUs: UInt64) {
        guard fd >= 0, var addr = destAddr else { return }

        let currentFrameID = frameID
        frameID &+= 1

        let maxPayload = ProtocolConstants.maxVideoPayloadBytes
        let totalChunks = (data.count + maxPayload - 1) / maxPayload
        guard totalChunks > 0, totalChunks <= Int(UInt16.max) else { return }

        for chunkIdx in 0..<totalChunks {
            let payloadOffset = chunkIdx * maxPayload
            let payloadSize = min(maxPayload, data.count - payloadOffset)

            // Build packet: PacketPrefix(6) + VideoChunkHeader(32) + payload
            var packet = Data(capacity: ProtocolConstants.videoTotalHeaderBytes + payloadSize)

            // PacketPrefix (6 bytes)
            packet.append(contentsOf: ProtocolConstants.magic)
            packet.append(ProtocolConstants.protocolVersion)
            packet.append(ProtocolConstants.packetTypeVideoChunk)

            // Per-packet fields (12 bytes)
            appendLE(&packet, config.streamID)
            appendLE(&packet, config.configID)
            appendLE(&packet, currentFrameID)
            appendLE(&packet, UInt16(chunkIdx))
            appendLE(&packet, UInt16(payloadSize))

            // Per-frame fields (20 bytes)
            if chunkIdx == 0 {
                appendLE(&packet, timestampUs)
                packet.append(isKeyframe ? 1 : 0)
                packet.append(config.codec)
                appendLE(&packet, config.width)
                appendLE(&packet, config.height)
                appendLE(&packet, UInt16(totalChunks))
                appendLE(&packet, UInt32(data.count))
            } else {
                packet.append(contentsOf: [UInt8](repeating: 0, count: 20))
            }

            // Payload
            packet.append(data[payloadOffset..<payloadOffset + payloadSize])

            // Send via POSIX sendto
            let sent = packet.withUnsafeBytes { bufPtr in
                withUnsafePointer(to: &addr) { addrPtr in
                    addrPtr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                        sendto(fd, bufPtr.baseAddress, packet.count, 0, sa, socklen_t(MemoryLayout<sockaddr_in>.size))
                    }
                }
            }

            if sent < 0 {
                if totalPacketsSent == 0 {
                    print("[RESC] UDP sendto error: \(errno) (\(String(cString: strerror(errno))))")
                }
            } else {
                totalBytesSent += UInt64(sent)
                totalPacketsSent += 1
            }
        }
    }

    var stats: (packets: UInt64, bytes: UInt64) {
        (totalPacketsSent, totalBytesSent)
    }

    private func appendLE(_ data: inout Data, _ value: UInt16) {
        var v = value.littleEndian; data.append(Data(bytes: &v, count: 2))
    }
    private func appendLE(_ data: inout Data, _ value: UInt32) {
        var v = value.littleEndian; data.append(Data(bytes: &v, count: 4))
    }
    private func appendLE(_ data: inout Data, _ value: UInt64) {
        var v = value.littleEndian; data.append(Data(bytes: &v, count: 8))
    }
}
