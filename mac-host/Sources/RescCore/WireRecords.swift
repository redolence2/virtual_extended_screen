import Foundation

/// Fixed-width binary wire records (docs/WIRE.md §2 VideoHello/VideoHelloAck,
/// §4 frame header, §5 UDP move/cursor). Stage-1 candidate (freeze pending
/// the A0.0 gates and independent re-review); any change to a layout or
/// validation rule here requires a dated CONTRACT_ERRATA.md entry
/// first. Every fixed-width integer/float on the wire is little-endian
/// (docs/WIRE.md global rule 1) — this file assembles values byte-by-byte
/// rather than binding a possibly-misaligned pointer to a typed load.
///
/// The Rust twin: ubuntu-client/crates/protocol/src/v3wire.
public enum WireError: Error, Equatable {
    case protocolViolation(String)
    case recordCapViolation(total: UInt64, cap: UInt64)
}

// MARK: - Little-endian byte assembly

// Offsets are relative to `data`'s own indexing, not assumed to start at 0
// — a `Data` produced by slicing another `Data` keeps its parent's indices.

private func u8(_ data: Data, _ offset: Int) -> UInt8 {
    data[data.startIndex + offset]
}

private func u16LE(_ data: Data, _ offset: Int) -> UInt16 {
    let i = data.startIndex + offset
    return UInt16(data[i]) | (UInt16(data[i + 1]) << 8)
}

private func u32LE(_ data: Data, _ offset: Int) -> UInt32 {
    let i = data.startIndex + offset
    return UInt32(data[i])
        | (UInt32(data[i + 1]) << 8)
        | (UInt32(data[i + 2]) << 16)
        | (UInt32(data[i + 3]) << 24)
}

private func u64LE(_ data: Data, _ offset: Int) -> UInt64 {
    let i = data.startIndex + offset
    var v: UInt64 = 0
    for byte in 0..<8 {
        v |= UInt64(data[i + byte]) << (8 * byte)
    }
    return v
}

private func i32LE(_ data: Data, _ offset: Int) -> Int32 {
    Int32(bitPattern: u32LE(data, offset))
}

private func f32LE(_ data: Data, _ offset: Int) -> Float {
    Float(bitPattern: u32LE(data, offset))
}

private func slice(_ data: Data, _ offset: Int, _ length: Int) -> Data {
    let start = data.startIndex + offset
    return data.subdata(in: start..<(start + length))
}

private let magicVideoHello: [UInt8] = [0x52, 0x53, 0x43, 0x56]      // "RSCV"
private let magicVideoHelloAck: [UInt8] = [0x52, 0x53, 0x43, 0x41]   // "RSCA"
private let magicFrameHeader: [UInt8] = [0x56, 0x46]                 // "VF"
private let magicUDPPrefix: [UInt8] = [0x52, 0x45, 0x53, 0x43]       // "RESC"

private func checkMagic(_ data: Data, _ expected: [UInt8], _ what: String) throws {
    for (offset, want) in expected.enumerated() where u8(data, offset) != want {
        throw WireError.protocolViolation("\(what): bad magic")
    }
}

// MARK: - VideoHello / VideoHelloAck (docs/WIRE.md §2)

public struct VideoHello: Equatable {
    public let sessionRunId: UInt64
    /// 8 raw bytes, opaque.
    public let profileHash: Data
}

public enum VideoHelloAckStatus: UInt8, Equatable {
    case ok = 0
    case mismatch = 1
    case busy = 2
    case `internal` = 3
}

public struct VideoHelloAck: Equatable {
    public let sessionRunId: UInt64
    public let status: VideoHelloAckStatus
}

/// `VideoHello` (exactly 32 B, host→client; docs/WIRE.md §2).
public func parseVideoHello(_ data: Data) throws -> VideoHello {
    guard data.count == 32 else {
        throw WireError.protocolViolation("VideoHello: expected 32 B, got \(data.count)")
    }
    try checkMagic(data, magicVideoHello, "VideoHello")
    guard u8(data, 4) == 3 else {
        throw WireError.protocolViolation("VideoHello: ver != 3")
    }
    guard u8(data, 5) == 0 else {
        throw WireError.protocolViolation("VideoHello: reserved u8 (offset 5) != 0")
    }
    guard u16LE(data, 6) == 32 else {
        throw WireError.protocolViolation("VideoHello: len field != 32")
    }
    let sessionRunId = u64LE(data, 8)
    let profileHash = slice(data, 16, 8)
    guard u64LE(data, 24) == 0 else {
        throw WireError.protocolViolation("VideoHello: reserved u64 (offset 24) != 0")
    }
    return VideoHello(sessionRunId: sessionRunId, profileHash: profileHash)
}

/// `VideoHelloAck` (exactly 16 B, client→host; docs/WIRE.md §2). Full
/// magic/version/length validation mirrors the "Host Ack-checklist" in
/// docs/WIRE.md §2, not just the status byte.
public func parseVideoHelloAck(_ data: Data) throws -> VideoHelloAck {
    guard data.count == 16 else {
        throw WireError.protocolViolation("VideoHelloAck: expected 16 B, got \(data.count)")
    }
    try checkMagic(data, magicVideoHelloAck, "VideoHelloAck")
    guard u8(data, 4) == 3 else {
        throw WireError.protocolViolation("VideoHelloAck: ver != 3")
    }
    guard u16LE(data, 6) == 16 else {
        throw WireError.protocolViolation("VideoHelloAck: len field != 16")
    }
    guard let status = VideoHelloAckStatus(rawValue: u8(data, 5)) else {
        throw WireError.protocolViolation("VideoHelloAck: unknown status byte")
    }
    let sessionRunId = u64LE(data, 8)
    return VideoHelloAck(sessionRunId: sessionRunId, status: status)
}

// MARK: - Frame record header (docs/WIRE.md §4)

public struct FrameHeader: Equatable {
    public let flags: UInt8
    public let frameOrdinal: UInt64
    public let captureSeq: UInt32
    public let contentCaptureTsUs: UInt64
    public let payloadLen: UInt32

    /// bit0 of `flags` — keyframe claim.
    public var isKeyframeClaim: Bool { flags & 0x01 != 0 }
}

private let frameHeaderSize: UInt64 = 32

/// Frame record header (exactly 32 B; docs/WIRE.md §4). `maxRecordBytes` is
/// the active profile's `max_record_bytes`; the cap check uses widened,
/// checked arithmetic on `headerLen + payloadLen` and runs before any
/// payload would be allocated by a caller.
public func parseFrameHeader(_ data: Data, maxRecordBytes: UInt64) throws -> FrameHeader {
    guard data.count == 32 else {
        throw WireError.protocolViolation("FrameHeader: expected 32 B, got \(data.count)")
    }
    try checkMagic(data, magicFrameHeader, "FrameHeader")
    guard u8(data, 2) == 32 else {
        throw WireError.protocolViolation("FrameHeader: headerLen != 32")
    }
    let flags = u8(data, 3)
    guard flags & ~UInt8(0x01) == 0 else {
        throw WireError.protocolViolation("FrameHeader: unknown flag bit set (flags=\(flags))")
    }
    let frameOrdinal = u64LE(data, 4)
    guard frameOrdinal >= 1 && frameOrdinal <= UInt64(Int64.max) else {
        throw WireError.protocolViolation("FrameHeader: frameOrdinal \(frameOrdinal) outside 1...Int64.max")
    }
    let captureSeq = u32LE(data, 12)
    let contentCaptureTsUs = u64LE(data, 16)
    guard u32LE(data, 24) == 0 else {
        throw WireError.protocolViolation("FrameHeader: reserved u32 (offset 24) != 0")
    }
    let payloadLen = u32LE(data, 28)
    let total = frameHeaderSize + UInt64(payloadLen)
    guard total <= maxRecordBytes else {
        throw WireError.recordCapViolation(total: total, cap: maxRecordBytes)
    }
    return FrameHeader(flags: flags, frameOrdinal: frameOrdinal, captureSeq: captureSeq,
                        contentCaptureTsUs: contentCaptureTsUs, payloadLen: payloadLen)
}

// MARK: - UDP records (docs/WIRE.md §5; ERR-05 — no reserved field on UDP)

public struct MoveEvent: Equatable {
    public let sessionRunId: UInt64
    public let seq: UInt32
    public let x: Int32
    public let y: Int32
}

public struct CursorUpdate3: Equatable {
    public let sessionRunId: UInt64
    public let seq: UInt32
    public let timestampUs: UInt64
    public let xPx: Int32
    public let yPx: Int32
    public let shapeId: UInt8
    public let hotspotXPx: UInt16
    public let hotspotYPx: UInt16
    public let cursorScale: Float
}

private let udpTypeCursor: UInt8 = 1
private let udpTypeMove: UInt8 = 2

/// Move datagram (exactly 26 B, client→host; docs/WIRE.md §5). Exact-length
/// rule: any length other than 26 ⇒ `protocolViolation`. No reserved-field
/// check — neither UDP layout has one (ERR-05).
public func parseMove(_ data: Data) throws -> MoveEvent {
    guard data.count == 26 else {
        throw WireError.protocolViolation("Move: expected 26 B, got \(data.count)")
    }
    try checkMagic(data, magicUDPPrefix, "Move")
    guard u8(data, 4) == 3 else {
        throw WireError.protocolViolation("Move: ver != 3")
    }
    guard u8(data, 5) == udpTypeMove else {
        throw WireError.protocolViolation("Move: type != 2")
    }
    let sessionRunId = u64LE(data, 6)
    let seq = u32LE(data, 14)
    let x = i32LE(data, 18)
    let y = i32LE(data, 22)
    return MoveEvent(sessionRunId: sessionRunId, seq: seq, x: x, y: y)
}

/// Cursor datagram (exactly 43 B, host→client; docs/WIRE.md §5).
public func parseCursor(_ data: Data) throws -> CursorUpdate3 {
    guard data.count == 43 else {
        throw WireError.protocolViolation("Cursor: expected 43 B, got \(data.count)")
    }
    try checkMagic(data, magicUDPPrefix, "Cursor")
    guard u8(data, 4) == 3 else {
        throw WireError.protocolViolation("Cursor: ver != 3")
    }
    guard u8(data, 5) == udpTypeCursor else {
        throw WireError.protocolViolation("Cursor: type != 1")
    }
    let sessionRunId = u64LE(data, 6)
    let seq = u32LE(data, 14)
    let timestampUs = u64LE(data, 18)
    let xPx = i32LE(data, 26)
    let yPx = i32LE(data, 30)
    let shapeId = u8(data, 34)
    guard shapeId <= 15 else {
        throw WireError.protocolViolation("Cursor: shape_id \(shapeId) outside 0...15")
    }
    let hotspotXPx = u16LE(data, 35)
    let hotspotYPx = u16LE(data, 37)
    let cursorScale = f32LE(data, 39)
    guard cursorScale.isFinite && cursorScale > 0 else {
        throw WireError.protocolViolation("Cursor: cursor_scale not finite/positive (\(cursorScale))")
    }
    return CursorUpdate3(sessionRunId: sessionRunId, seq: seq, timestampUs: timestampUs,
                          xPx: xPx, yPx: yPx, shapeId: shapeId,
                          hotspotXPx: hotspotXPx, hotspotYPx: hotspotYPx, cursorScale: cursorScale)
}
