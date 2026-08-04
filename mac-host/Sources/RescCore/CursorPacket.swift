import Foundation

/// The RUNNING v1 wire cursor packet (docs/WIRE.md legacy schema — NOT the
/// v3 43-byte record; do not confuse the two). 6-byte PacketPrefix (magic +
/// version + packet_type) + 29-byte CursorUpdate = 35 bytes total,
/// byte-for-byte identical to what the live host has always sent and what
/// the running Ubuntu client parses.
///
/// Pulled out of `CursorTracker` into a pure function (remediation item
/// R2b) so the byte layout is testable without a live UDP socket. See
/// CONTRACT_ERRATA.md "Implementation proofs required" › cursor
/// `timestamp_us`: sequence number, never the timestamp, governs ordering
/// — this function only assembles bytes from the values it is given, it
/// makes no ordering decisions itself.
public enum CursorPacket {

    /// Builds one 35-byte CursorUpdate packet. `hotspot_x`/`hotspot_y` are
    /// always 0 (arrow-tip hotspot) and `cursor_scale` is always 1.0 —
    /// hardcoded here exactly as `CursorTracker` hardcoded them before this
    /// extraction.
    public static func build(magic: [UInt8], version: UInt8, packetType: UInt8,
                              seq: UInt32, timestampUs: UInt64,
                              x: Int32, y: Int32, shape: UInt8) -> Data {
        var packet = Data(capacity: 35)

        // PacketPrefix
        packet.append(contentsOf: magic)
        packet.append(version)
        packet.append(packetType)

        // CursorUpdate (29 bytes, exact field order from spec)
        appendLE(&packet, seq)                    // seq: u32
        appendLE(&packet, timestampUs)             // timestamp_us: u64
        appendLEi32(&packet, x)                    // x_px: i32
        appendLEi32(&packet, y)                    // y_px: i32
        packet.append(shape)                       // shape_id: u8
        appendLE16(&packet, 0)                      // hotspot_x_px: u16 (0 for arrow tip)
        appendLE16(&packet, 0)                      // hotspot_y_px: u16
        appendLEf32(&packet, 1.0)                   // cursor_scale: f32

        return packet
    }

    // MARK: - Little-endian helpers (copied from the pre-R2b CursorTracker)

    private static func appendLE(_ d: inout Data, _ v: UInt32) { var x = v.littleEndian; d.append(Data(bytes: &x, count: 4)) }
    private static func appendLE(_ d: inout Data, _ v: UInt64) { var x = v.littleEndian; d.append(Data(bytes: &x, count: 8)) }
    private static func appendLEi32(_ d: inout Data, _ v: Int32) { var x = v.littleEndian; d.append(Data(bytes: &x, count: 4)) }
    private static func appendLE16(_ d: inout Data, _ v: UInt16) { var x = v.littleEndian; d.append(Data(bytes: &x, count: 2)) }
    private static func appendLEf32(_ d: inout Data, _ v: Float) { var x = v.bitPattern.littleEndian; d.append(Data(bytes: &x, count: 4)) }
}
