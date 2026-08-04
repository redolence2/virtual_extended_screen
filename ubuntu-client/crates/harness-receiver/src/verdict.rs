//! Pure pass-verdict for the A0 harness receiver (`A00_REMEDIATION_PLAN.md`
//! §3 D2 / §5 R3a's receiver-predicate hardening). Mirrors the Mac-side
//! `mac-host/Sources/RescCore/HarnessVerdict.swift` pattern: every predicate
//! term lives here as one field, so a unit test can flip exactly one term
//! and prove it alone flips the verdict, and the same boolean drives both
//! the JSON report's `pass` field and the process exit code.
//!
//! Full predicate (`A00_REMEDIATION_PLAN.md` §5 R3a, "Receiver pass
//! predicate"): accepted==acked AND emitted==submitted AND unknown_pts==0
//! AND nonzero frames AND zero duplicates AND zero reorders/skips AND zero
//! ACK-order violations AND zero protocol/fatal decoder errors AND clean
//! EOF/tail drain AND zero outstanding frames at exit.

/// Every input term the pass predicate reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VerdictInputs {
    pub frames_accepted: u64,
    pub frames_acked: u64,
    pub frames_emitted: u64,
    pub frames_submitted: u64,
    pub unknown_pts: u64,
    pub duplicates: u64,
    pub reorders: u64,
    pub skips: u64,
    pub ack_order_violations: u64,
    pub protocol_errors: u64,
    pub fatal_decoder_errors: u64,
    pub clean_eof_tail_drain: bool,
    pub outstanding_at_exit: u64,
}

impl VerdictInputs {
    /// Each argument alone can flip the result from pass to fail — see the
    /// `tests` module below for the per-term proof. `frames_emitted > 0` is
    /// the explicit nonzero-frames term: a zero-frame run (e.g. a
    /// connection that opens and closes immediately) would otherwise
    /// satisfy every equality and zero-count term below vacuously — the
    /// exact fail-open-through-vacuity class this hardening exists to
    /// close (mirrors `HarnessVerdict`'s `sent > 0`).
    pub fn evaluate(&self) -> bool {
        self.frames_accepted == self.frames_acked
            && self.frames_emitted == self.frames_submitted
            && self.unknown_pts == 0
            && self.frames_emitted > 0
            && self.duplicates == 0
            && self.reorders == 0
            && self.skips == 0
            && self.ack_order_violations == 0
            && self.protocol_errors == 0
            && self.fatal_decoder_errors == 0
            && self.clean_eof_tail_drain
            && self.outstanding_at_exit == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A verdict input that passes every term — each test below takes this
    /// baseline and perturbs exactly one field.
    fn passing() -> VerdictInputs {
        VerdictInputs {
            frames_accepted: 10,
            frames_acked: 10,
            frames_emitted: 10,
            frames_submitted: 10,
            unknown_pts: 0,
            duplicates: 0,
            reorders: 0,
            skips: 0,
            ack_order_violations: 0,
            protocol_errors: 0,
            fatal_decoder_errors: 0,
            clean_eof_tail_drain: true,
            outstanding_at_exit: 0,
        }
    }

    #[test]
    fn passing_baseline_passes() {
        assert!(passing().evaluate());
    }

    #[test]
    fn accepted_ne_acked_fails() {
        let mut v = passing();
        v.frames_acked = 9;
        assert!(!v.evaluate());
    }

    #[test]
    fn emitted_ne_submitted_fails() {
        let mut v = passing();
        v.frames_emitted = 9;
        assert!(!v.evaluate());
    }

    #[test]
    fn nonzero_unknown_pts_fails() {
        let mut v = passing();
        v.unknown_pts = 1;
        assert!(!v.evaluate());
    }

    #[test]
    fn zero_frames_fails_even_with_every_other_term_clean() {
        // The vacuous-pass case this hardening exists to close: a
        // zero-frame run trivially satisfies every equality/zero-count
        // term above (0==0 everywhere) without this explicit check.
        let mut v = passing();
        v.frames_accepted = 0;
        v.frames_acked = 0;
        v.frames_emitted = 0;
        v.frames_submitted = 0;
        assert!(!v.evaluate());
    }

    #[test]
    fn nonzero_duplicates_fails() {
        let mut v = passing();
        v.duplicates = 1;
        assert!(!v.evaluate());
    }

    #[test]
    fn nonzero_reorders_fails() {
        let mut v = passing();
        v.reorders = 1;
        assert!(!v.evaluate());
    }

    #[test]
    fn nonzero_skips_fails() {
        let mut v = passing();
        v.skips = 1;
        assert!(!v.evaluate());
    }

    #[test]
    fn nonzero_ack_order_violations_fails() {
        let mut v = passing();
        v.ack_order_violations = 1;
        assert!(!v.evaluate());
    }

    #[test]
    fn nonzero_protocol_errors_fails() {
        let mut v = passing();
        v.protocol_errors = 1;
        assert!(!v.evaluate());
    }

    #[test]
    fn nonzero_fatal_decoder_errors_fails() {
        let mut v = passing();
        v.fatal_decoder_errors = 1;
        assert!(!v.evaluate());
    }

    #[test]
    fn unclean_eof_tail_drain_fails() {
        let mut v = passing();
        v.clean_eof_tail_drain = false;
        assert!(!v.evaluate());
    }

    #[test]
    fn nonzero_outstanding_at_exit_fails() {
        let mut v = passing();
        v.outstanding_at_exit = 1;
        assert!(!v.evaluate());
    }
}
