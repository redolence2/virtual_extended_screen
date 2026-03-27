import Foundation

/// Shared protocol constants (v1). Must match Rust implementation exactly.
enum ProtocolConstants {
    static let protocolVersion: UInt8 = 1
    static let magic: [UInt8] = [0x52, 0x45, 0x53, 0x43] // "RESC"

    // Packet types (for UDP validation)
    static let packetTypeVideoChunk: UInt8 = 0
    static let packetTypeCursorUpdate: UInt8 = 1
    static let packetTypeInputEvent: UInt8 = 2

    // Header sizes (bytes)
    static let packetPrefixBytes = 6       // magic(4) + version(1) + packet_type(1)
    static let videoChunkHeaderBytes = 36  // per-packet(16) + per-frame(20)
    static let videoTotalHeaderBytes = 42  // prefix(6) + chunk header(36)
    static let maxDatagramBytes = 1400
    static let maxVideoPayloadBytes = maxDatagramBytes - videoTotalHeaderBytes // 1358

    // Cursor
    static let cursorUpdateBytes = 29
    static let cursorTotalPacketBytes = packetPrefixBytes + cursorUpdateBytes // 35

    // Input (Phase 6)
    static let inputEventBytes = 22
    static let inputTotalPacketBytes = packetPrefixBytes + inputEventBytes // 28

    // mDNS service type
    static let mdnsServiceType = "_remotedisplay._tcp."
    static let mdnsDomain = "local."

    // Default modes
    static let default1080p = (width: 1920, height: 1080, refreshRateMillihz: UInt32(60000))
    static let default4K = (width: 3840, height: 2160, refreshRateMillihz: UInt32(60000))

    /// Log and verify all protocol constants at startup.
    /// Crashes on mismatch — prevents silent wire-format drift.
    static func logAndVerify() {
        print("[RESC] Protocol constants v\(protocolVersion):")
        print("[RESC]   PACKET_PREFIX_BYTES      = \(packetPrefixBytes)")
        print("[RESC]   VIDEO_CHUNK_HEADER_BYTES  = \(videoChunkHeaderBytes)")
        print("[RESC]   VIDEO_TOTAL_HEADER_BYTES  = \(videoTotalHeaderBytes)")
        print("[RESC]   MAX_VIDEO_PAYLOAD_BYTES   = \(maxVideoPayloadBytes)")
        print("[RESC]   CURSOR_TOTAL_PACKET_BYTES = \(cursorTotalPacketBytes)")
        print("[RESC]   INPUT_TOTAL_PACKET_BYTES  = \(inputTotalPacketBytes)")

        // Self-consistency checks
        assert(packetPrefixBytes == 6, "PacketPrefix must be 6 bytes")
        assert(videoChunkHeaderBytes == 36, "VideoChunkHeader must be 36 bytes")
        assert(videoTotalHeaderBytes == packetPrefixBytes + videoChunkHeaderBytes)
        assert(maxVideoPayloadBytes == maxDatagramBytes - videoTotalHeaderBytes)
        assert(cursorTotalPacketBytes == packetPrefixBytes + cursorUpdateBytes)
        assert(inputTotalPacketBytes == packetPrefixBytes + inputEventBytes)
    }

    // Codec level computation
    static func codecLevelIdc(width: Int, height: Int) -> UInt32 {
        if width >= 3840 || height >= 2160 { return 51 } // Level 5.1 for 4K
        return 41 // Level 4.1 for 1080p
    }

    // Max frame bytes — generous to avoid dropping keyframes.
    // During rapid content changes (drag, video), frames spike well above average.
    static func maxFrameBytes(bitrateBps: UInt32, fps: Double) -> UInt32 {
        let avgFrameBytes = Double(bitrateBps) / 8.0 / fps
        return UInt32(min(avgFrameBytes * 20.0, 2_000_000)) // 20x avg, cap 2MB
    }
}
