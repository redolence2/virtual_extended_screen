import Foundation

/// Pure pass-verdict for the A0 harness sender (A00_REMEDIATION_PLAN.md §5
/// R3a, "F6" predicate hardening). Extracted out of HarnessSender/main.swift
/// into RescCore so FixtureCheck — which cannot import HarnessSender (an
/// executableTarget; see Package.swift's existing "cannot be depended on"
/// note on the RemoteDisplayHost target for the same reason) — can prove
/// each predicate independently flips the verdict, not just exercise the
/// composite behavior end to end.
///
/// A harness run passes iff every frame the pump confirmed as sent
/// (`frames_sent`, incremented only after a confirmed full write) was
/// eventually acked, nothing was left outstanding when the run ended, the
/// ACK reader never observed an out-of-order ACK, and no socket write ever
/// failed (HarnessSender's HarnessPump fail-stops the run on the first write
/// error rather than soldiering on). This single boolean drives both the
/// JSON report's `sustained_60hz` field and the process exit code.
public enum HarnessVerdict {
    /// Each argument alone can flip the result from pass to fail — see
    /// FixtureCheck's "(j) R3a" section for the per-argument proof.
    /// `sent > 0` is required: a zero-frame run must not pass vacuously
    /// (fail-open through vacuity is the exact failure class the F6
    /// hardening exists to close; the receiver-side predicate likewise
    /// requires nonzero frames).
    public static func evaluate(sent: Int, acked: Int, outstanding: Int,
                                 orderViolations: Int, writeErrors: Int) -> Bool {
        sent > 0 && sent == acked && outstanding == 0 && orderViolations == 0 && writeErrors == 0
    }
}
