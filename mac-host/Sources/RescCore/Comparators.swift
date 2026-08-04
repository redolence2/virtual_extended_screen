import Foundation

/// UDP sequence-number comparators (docs/WIRE.md §5 "Sequence comparator";
/// IMPLEMENTATION_PLAN_V11.md §9, §8). `lastSeq` resets per session; both
/// comparators use wrapping subtraction so a sequence number that has
/// wrapped around still compares as "newer" than its predecessor.

/// `newer(a, b) ⇔ d ≠ 0 ∧ d < 2^31, d = (a − b) mod 2^32` — the move/cursor
/// wire `seq` comparator (32-bit domain).
public func newerU32(_ a: UInt32, _ b: UInt32) -> Bool {
    let d = a &- b
    return d != 0 && d < 0x8000_0000
}

/// Same comparator over a 24-bit domain — the T2 packed cursor snapshot's
/// low-24-bits-of-seq field (IMPLEMENTATION_PLAN_V11.md §8: "newer ⇔ d≠0 ∧
/// d<2²³"). Inputs are masked to 24 bits before comparing.
public func newerU24(_ a: UInt32, _ b: UInt32) -> Bool {
    let am = a & 0xFF_FFFF
    let bm = b & 0xFF_FFFF
    let d = (am &- bm) & 0xFF_FFFF
    return d != 0 && d < 0x80_0000
}
