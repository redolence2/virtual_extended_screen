import Foundation

/// Exact scroll injection (docs/WIRE.md §6; CONTRACT_ERRATA.md ERR-04,
/// which removes the point-conversion sentence in V11 §5.4). Normative
/// rule: rotate the signed SDL wheel-step deltas (axis swap when rotated),
/// multiply each by the 10-pixel quantum using widened (Int64) arithmetic,
/// saturate to `Int32`. No point conversion, no fractional rounding — the
/// result feeds a CoreGraphics pixel-unit scroll event directly.
public func scrollTransform(dx: Int32, dy: Int32, rotated: Bool) -> (dx: Int32, dy: Int32) {
    let (rx, ry): (Int64, Int64) = rotated
        ? (-Int64(dy), Int64(dx))
        : (Int64(dx), Int64(dy))
    let quantum: Int64 = 10
    return (Int32(clamping: rx * quantum), Int32(clamping: ry * quantum))
}
