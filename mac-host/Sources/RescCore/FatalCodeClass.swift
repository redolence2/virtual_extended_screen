import Foundation

/// Failure-class taxonomy for `Resc_V3_FatalCode` (IMPLEMENTATION_PLAN_V11.md
/// §4). Machine-tested identically in Swift and Rust (v11 gate:
/// "classification passes in both languages"). Code 0 (`FATAL_UNSPECIFIED`)
/// is valid only as `ProfileResult.reject_code` when accepted and is never a
/// legal `FatalReport.code` — it classifies as `nil`, same as any unknown
/// numeric outside the 22 known codes.
public enum FailureClass: Equatable {
    case deterministic
    case transient
    case terminal
}

/// Classify a `FatalCode` raw value (proto/control_v3.proto numbering).
/// `nil` for 0 (unspecified) and any value outside the 22 known codes.
public func classify(_ code: Int32) -> FailureClass? {
    switch code {
    case 1, 2, 3, 4, 5, 6, 7, 8, 9, 21:
        return .deterministic
    case 10, 11, 12, 13, 14, 15, 16, 17, 18, 22:
        return .transient
    case 19, 20:
        return .terminal
    default:
        return nil
    }
}
