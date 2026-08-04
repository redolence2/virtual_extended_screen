//! Client-side four-timestamp clock sync (IMPLEMENTATION_PLAN_V11.md §10
//! "Clock & trace (diagnostics/A0 mode only)").
//!
//! Pure math + state — no I/O. Callers own the transport (the control
//! channel's `ClockPing`/`ClockPong`, `crates/net-transport`) and the
//! logging ([`crate::trace::ClientTrace`]).
//!
//! `offset = ((t2-t1) + (t3-t4)) / 2`, `delay = (t4-t1) - (t3-t2)`, both
//! computed in `i128` so that two clocks anchored at different, arbitrary
//! zero points (client and host each measure "mono microseconds since their
//! own process start") never risk a `u64` underflow/overflow — only the
//! *differences* are meaningful, and those can be negative even though every
//! individual timestamp is an unsigned mono count. Samples with `delay < 0`
//! (physically impossible — indicates a clock step or bad sample) or
//! `delay >= 5000` (5 ms — too much queuing/scheduling noise to trust the
//! offset) are rejected outright. Among accepted samples, the minimum-delay
//! one is retained as [`ClockSync::best`] — it has the tightest uncertainty
//! bound (`uncertainty_us = delay_us / 2`).

/// One accepted clock-sync sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub seq: u32,
    pub offset_us: i64,
    pub delay_us: i64,
    pub uncertainty_us: u32,
}

/// Samples with `delay_us >= 5000` (5 ms round-trip) are rejected (plan §10).
const MAX_ACCEPTED_DELAY_US: i128 = 5000;

/// Accumulates clock-sync samples across one session, retaining the
/// minimum-delay (tightest-uncertainty) accepted sample.
#[derive(Debug, Default)]
pub struct ClockSync {
    best: Option<Sample>,
}

impl ClockSync {
    pub fn new() -> Self {
        ClockSync { best: None }
    }

    /// Feeds one `ClockPong` reply. `t1`/`t4` are the requester's (this
    /// client's) mono microseconds at send/receive; `t2`/`t3` are the
    /// responder's (host's) mono microseconds at receive/send, as echoed
    /// back in the pong. Returns `Some(sample)` when accepted (`delay_us`
    /// in `0..5000`); `None` when rejected. Accepting a sample here does not
    /// guarantee it becomes [`Self::best`] — only the minimum-delay sample
    /// seen so far is kept there.
    pub fn on_pong(&mut self, t1: u64, t2: u64, t3: u64, t4: u64, seq: u32) -> Option<Sample> {
        let (t1, t2, t3, t4) = (t1 as i128, t2 as i128, t3 as i128, t4 as i128);

        let offset_i128 = ((t2 - t1) + (t3 - t4)) / 2;
        let delay_i128 = (t4 - t1) - (t3 - t2);

        if delay_i128 < 0 || delay_i128 >= MAX_ACCEPTED_DELAY_US {
            return None;
        }

        // Lossless for any realistic mono-microsecond uptime (both bounded
        // well within i64/u32 range for any real session) — only pathological
        // synthetic inputs near u64::MAX on all four timestamps could make
        // `offset_i128` exceed i64's range, and that can't happen here since
        // `delay_i128` (which shares t1/t4 and t2/t3 terms) is already capped
        // above at < 5000.
        let sample = Sample {
            seq,
            offset_us: offset_i128 as i64,
            delay_us: delay_i128 as i64,
            uncertainty_us: (delay_i128 / 2) as u32,
        };

        let is_new_best = match &self.best {
            None => true,
            Some(cur) => sample.delay_us < cur.delay_us,
        };
        if is_new_best {
            self.best = Some(sample);
        }

        Some(sample)
    }

    /// The minimum-delay accepted sample seen so far, if any.
    pub fn best(&self) -> Option<Sample> {
        self.best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responder_clock_far_behind_still_yields_small_delay() {
        // Requester's mono anchor has been running much longer than the
        // responder's own independent anchor (e.g. the host process started
        // long after the client did) — t2/t3 read far smaller than t1/t4
        // even though the real one-way delays are tiny. Naive u64
        // subtraction of t2-t1 or t3-t4 here would underflow; i128 must not.
        let mut cs = ClockSync::new();
        let t1 = 10_000_000u64; // requester: 10s uptime at send
        let t2 = 500u64; // responder: 0.5ms uptime at receive
        let t3 = 550u64; // responder: send 50us later
        let t4 = 10_000_100u64; // requester: receive 100us after send
        let sample = cs.on_pong(t1, t2, t3, t4, 1).expect("valid sample");
        assert_eq!(sample.delay_us, 50); // (t4-t1)-(t3-t2) = 100-50
        assert_eq!(
            sample.offset_us,
            ((t2 as i64 - t1 as i64) + (t3 as i64 - t4 as i64)) / 2
        );
        assert!(sample.offset_us < -9_000_000); // huge negative offset, but accepted
    }

    #[test]
    fn responder_clock_far_ahead_still_yields_small_delay() {
        let mut cs = ClockSync::new();
        let t1 = 500u64;
        let t2 = 10_000_050u64;
        let t3 = 10_000_100u64;
        let t4 = 600u64;
        let sample = cs.on_pong(t1, t2, t3, t4, 2).expect("valid sample");
        assert_eq!(sample.delay_us, 50); // (600-500)-(10000100-10000050) = 100-50
        assert!(sample.offset_us > 9_000_000);
    }

    #[test]
    fn negative_delay_rejected() {
        let mut cs = ClockSync::new();
        // (t4-t1)=100, (t3-t2)=190 => delay = 100-190 = -90 < 0.
        let sample = cs.on_pong(0, 10, 200, 100, 1);
        assert_eq!(sample, None);
        assert_eq!(cs.best(), None);
    }

    #[test]
    fn delay_at_or_above_5ms_rejected() {
        let mut cs = ClockSync::new();
        // Zero processing time (t2=t3=0) so delay = t4-t1 = round-trip directly.
        assert_eq!(cs.on_pong(0, 0, 0, 5000, 1), None); // delay == 5000 (threshold) -> rejected
        assert!(cs.on_pong(0, 0, 0, 4999, 2).is_some()); // delay == 4999 -> accepted
    }

    #[test]
    fn min_delay_sample_is_retained_as_best() {
        let mut cs = ClockSync::new();
        cs.on_pong(0, 0, 0, 200, 1); // delay=200
        assert_eq!(cs.best().unwrap().delay_us, 200);
        cs.on_pong(0, 0, 0, 50, 2); // delay=50 (better) -> becomes best
        assert_eq!(cs.best().unwrap().seq, 2);
        assert_eq!(cs.best().unwrap().delay_us, 50);
        cs.on_pong(0, 0, 0, 100, 3); // delay=100 (worse than 50) -> best unchanged
        assert_eq!(cs.best().unwrap().seq, 2);
        assert_eq!(cs.best().unwrap().delay_us, 50);
    }

    #[test]
    fn i128_intermediate_avoids_u64_underflow_near_max() {
        // All four timestamps close to u64::MAX, spaced so plain u64
        // subtraction of (t3 - t4) would underflow (t3 < t4). i128 must
        // handle this as an ordinary negative intermediate, not panic.
        let mut cs = ClockSync::new();
        let t1 = u64::MAX - 1000;
        let t2 = u64::MAX - 900; // t2 - t1 = 100
        let t3 = u64::MAX - 850; // t3 - t4 = -50 (t3 < t4)
        let t4 = u64::MAX - 800;
        let sample = cs.on_pong(t1, t2, t3, t4, 7).expect("valid sample");
        assert_eq!(sample.delay_us, 150); // (t4-t1)=200, (t3-t2)=50 => 150
        assert_eq!(sample.offset_us, 25); // (100 + -50)/2
    }
}
