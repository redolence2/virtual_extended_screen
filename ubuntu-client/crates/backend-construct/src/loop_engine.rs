//! The decode-loop state machine, extracted behind a trait seam so it is
//! testable without ffmpeg (`A00_REMEDIATION_PLAN.md` §3 D2 / §5 R6; the v1
//! claims this replaces — "deterministic by the second packet",
//! "bounded by the surface pool" — are withdrawn per that section: neither
//! is an established API guarantee, so the state machine below only ever
//! *records* what a backend actually does, never assumes it).
//!
//! [`DecoderLoopBackend`] is shaped after the three real FFmpeg calls the
//! previous `decoder-experiment`/`harness-receiver` duplicated inline
//! (`AVCodecContext_send_packet`, `_receive_frame` via [`receive_one`], and
//! `_send_packet(NULL)` for EOF/flush): [`DecoderHandle`] implements it by
//! pure delegation to those same primitives below, so the real decode path
//! is unchanged. [`submit_with_retry`], [`drain_fully`], and [`flush_tail`]
//! are the retain-drain-resubmit / EOF-tail-drain engine itself — generic
//! over the trait, no I/O beyond the trait's own calls — used by both
//! `decoder-experiment` and `harness-receiver` in place of the two
//! near-identical `send_with_eagain_retry`/`drain_frames` copies that used
//! to live in each binary (`CONTRACT_ERRATA.md` ERR-03;
//! `IMPLEMENTATION_PLAN_V11.md` §7's retain/drain/resubmit discipline).
//!
//! The `#[cfg(test)]` [`tests::MockDecoder`] double drives every
//! *hypothetical* branch this state machine allows (EAGAIN loops,
//! zero-output packets, multi-output drains, tail flush, back-to-back
//! EAGAIN, fatal errors) without a GPU or the real `hevc`/`hevc_cuvid`
//! codecs — per §3 D2, it is never claimed as proof of real decoder
//! timestamp behavior; that evidence comes only from the bounded
//! characterization protocol in `decoder-experiment --characterize`
//! running against the real backends.

use crate::{receive_one, transfer_hw_frame, recovered_ordinal, DecoderHandle, ReceiveOutcome};

/// Outcome of one [`DecoderLoopBackend::send_packet`] or
/// [`DecoderLoopBackend::send_eof`] attempt — mirrors
/// `avcodec_send_packet`'s two non-fatal results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    /// The packet (or EOF signal) was accepted.
    Accepted,
    /// `EAGAIN` — the decoder's output side must be drained before it will
    /// accept anything else; callers retain and resubmit the identical
    /// packet after draining.
    Again,
}

/// Outcome of one [`DecoderLoopBackend::receive_frame`] attempt — mirrors
/// `avcodec_receive_frame`'s two non-fatal results, collapsed to what the
/// retain-drain-resubmit engine actually branches on. (A real classified
/// `Eof` — [`ReceiveOutcome::Eof`], only reachable after
/// [`DecoderLoopBackend::send_eof`] has been accepted — maps to `Empty`
/// here: the pre-extraction callers in both binaries never branched on
/// `Again` vs. `Eof` differently, so folding them is behavior-preserving,
/// not a lost distinction.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOutcome {
    /// A frame was produced. `recovered_ordinal` is the ERR-03 recovery
    /// (`AVFrame.pts`, falling back to `best_effort_timestamp`) — `None`
    /// when the backend set neither.
    Frame(Option<i64>),
    /// Nothing ready right now.
    Empty,
}

/// The seam the retain-drain-resubmit engine drives. Every method mirrors
/// one real FFmpeg call byte-for-byte in the [`DecoderHandle`] impl below;
/// [`tests::MockDecoder`] implements the same trait with scripted outcomes
/// instead.
pub trait DecoderLoopBackend {
    /// Submit one packet carrying `ordinal` as its PTS
    /// (`CONTRACT_ERRATA.md` ERR-03: the decoder packet PTS is the
    /// `frameOrdinal`/submission ordinal).
    fn send_packet(&mut self, ordinal: i64, data: &[u8]) -> Result<SendOutcome, String>;

    /// Attempt to receive one decoded frame.
    fn receive_frame(&mut self) -> Result<FrameOutcome, String>;

    /// Signal end of stream (`avcodec_send_packet(ctx, NULL)`). Same
    /// Accepted/Again split as [`Self::send_packet`] — a real decoder can
    /// EAGAIN a flush signal exactly as it can a data packet (the existing
    /// pre-extraction flush loops in both binaries already handled this) —
    /// so callers drain-and-resubmit the EOF signal the same way.
    fn send_eof(&mut self) -> Result<SendOutcome, String>;
}

/// One `send_packet` attempt within a single [`submit_with_retry`] call.
/// The last entry is always `again: false` (the call only returns `Ok` once
/// accepted); every entry before it is an `again: true` EAGAIN that
/// triggered a [`drain_fully`] (recorded at the matching index of
/// [`SubmitRecord::drains`]) before the identical packet was resubmitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttemptRecord {
    pub again: bool,
}

/// Full record of what [`submit_with_retry`] observed getting one packet
/// accepted — enough detail for the bounded characterization protocol's
/// Phase A evidence (`A00_REMEDIATION_PLAN.md` §3 D2: "record every
/// attempted ordinal, accepted ordinal, EAGAIN result, drain start/stop,
/// outputs-per-drain, and recovered PTS") as well as plain pass/fail
/// counting.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubmitRecord {
    pub attempts: Vec<AttemptRecord>,
    /// One entry per EAGAIN this submission absorbed (`drains.len() ==
    /// attempts.iter().filter(|a| a.again).count()`), each holding that
    /// drain's recovered ordinals in receive order (empty when the drain
    /// produced zero output).
    pub drains: Vec<Vec<Option<i64>>>,
}

/// Submits one packet, retaining and resubmitting the *identical* `(ordinal,
/// data)` on every EAGAIN — draining fully between attempts — until accepted
/// exactly once (`IMPLEMENTATION_PLAN_V11.md` §7 / `CONTRACT_ERRATA.md`
/// ERR-03). Bounded by `max_attempts` to avoid a true infinite loop if the
/// backend never makes progress; exceeding it is a fatal `Err`, matching the
/// pre-extraction binaries' `MAX_EAGAIN_CYCLES` discipline.
///
/// Exactly-once acceptance holds by construction: the loop only returns
/// `Ok` from the single `SendOutcome::Accepted` arm, and every
/// `SendOutcome::Again` arm drains then resubmits the same arguments — it
/// can neither skip an ordinal nor accept one twice without first erroring.
/// [`tests::MockDecoder`]-driven tests prove this via the mock's recorded
/// call log, which is the actual evidence (a passing assertion here would
/// otherwise just restate the code).
pub fn submit_with_retry<B: DecoderLoopBackend>(
    backend: &mut B,
    ordinal: i64,
    data: &[u8],
    max_attempts: u32,
) -> Result<SubmitRecord, String> {
    let mut record = SubmitRecord::default();
    loop {
        match backend.send_packet(ordinal, data)? {
            SendOutcome::Accepted => {
                record.attempts.push(AttemptRecord { again: false });
                return Ok(record);
            }
            SendOutcome::Again => {
                record.attempts.push(AttemptRecord { again: true });
                if record.attempts.len() as u32 > max_attempts {
                    return Err(format!(
                        "send_packet EAGAIN did not converge after {} attempts for ordinal {ordinal}",
                        record.attempts.len()
                    ));
                }
                record.drains.push(drain_fully(backend)?);
                // loop back and resubmit the identical (ordinal, data)
            }
        }
    }
}

/// Drains `receive_frame` until `Empty`, returning every recovered ordinal
/// in receive order (an empty `Vec` for a zero-output drain). A real error
/// during drain is always fatal to the run (mirrors the pre-extraction
/// binaries' "drain Error ⇒ teardown").
pub fn drain_fully<B: DecoderLoopBackend>(backend: &mut B) -> Result<Vec<Option<i64>>, String> {
    let mut recovered = Vec::new();
    loop {
        match backend.receive_frame()? {
            FrameOutcome::Frame(ordinal) => recovered.push(ordinal),
            FrameOutcome::Empty => return Ok(recovered),
        }
    }
}

/// Everything [`flush_tail`] observed: every recovered ordinal across the
/// whole flush, in receive order, plus how many times `send_eof` itself
/// EAGAINed before being accepted (mirrors [`AttemptRecord::again`], summed,
/// for the flush signal — callers use this to classify the tail phase's
/// own forced/not_forced status the same way [`SubmitRecord`] lets them
/// classify a submission).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FlushRecord {
    pub recovered: Vec<Option<i64>>,
    pub eagain_count: u32,
}

/// End-of-stream flush: `send_eof` under the same retain/EAGAIN-retry
/// discipline as [`submit_with_retry`] (bounded the same way by
/// `max_attempts`), folding in any frames recovered from intermediate
/// EAGAIN-driven drains, followed by a final [`drain_fully`] for the
/// remaining buffered tail. This is the real-backend EOF/tail-drain ordinal
/// evidence `A00_REMEDIATION_PLAN.md` §3 D2 requires ("prove every
/// real-backend EOF/tail drain ends with emitted == submitted and exact
/// ordinal coverage" — callers compare [`FlushRecord::recovered`] plus the
/// mid-stream ordinals against that expectation).
pub fn flush_tail<B: DecoderLoopBackend>(
    backend: &mut B,
    max_attempts: u32,
) -> Result<FlushRecord, String> {
    let mut result = FlushRecord::default();
    loop {
        match backend.send_eof()? {
            SendOutcome::Accepted => break,
            SendOutcome::Again => {
                result.eagain_count += 1;
                if result.eagain_count > max_attempts {
                    return Err(format!(
                        "send_eof EAGAIN did not converge after {} attempts",
                        result.eagain_count
                    ));
                }
                result.recovered.extend(drain_fully(backend)?);
            }
        }
    }
    result.recovered.extend(drain_fully(backend)?);
    Ok(result)
}

// ---------------------------------------------------------------------
// Real backend: pure delegation, zero behavior change.
// ---------------------------------------------------------------------

impl DecoderLoopBackend for DecoderHandle {
    /// Delegates to `self.decoder.send_packet`, classified exactly as the
    /// pre-extraction `send_with_eagain_retry` did.
    fn send_packet(&mut self, ordinal: i64, data: &[u8]) -> Result<SendOutcome, String> {
        let mut packet = ffmpeg_next::Packet::copy(data);
        packet.set_pts(Some(ordinal));
        match self.decoder.send_packet(&packet) {
            Ok(()) => Ok(SendOutcome::Accepted),
            Err(ffmpeg_next::Error::Other { errno }) if errno == libc::EAGAIN => {
                Ok(SendOutcome::Again)
            }
            Err(e) => Err(format!("send_packet fatal error: {e}")),
        }
    }

    /// Delegates to [`receive_one`]; for `is_hw` backends, faithfully
    /// exercises the GPU→CPU [`transfer_hw_frame`] step before classifying
    /// the result as emitted (docs/WIRE.md §7) — a transfer failure means
    /// the backend did not actually produce usable output, so it is fatal
    /// here exactly as it was in the pre-extraction `record_emission`.
    fn receive_frame(&mut self) -> Result<FrameOutcome, String> {
        let mut frame = ffmpeg_next::frame::Video::empty();
        match receive_one(&mut self.decoder, &mut frame)? {
            ReceiveOutcome::Frame => {
                if self.is_hw {
                    transfer_hw_frame(&frame)?;
                }
                Ok(FrameOutcome::Frame(recovered_ordinal(&frame)))
            }
            ReceiveOutcome::Again | ReceiveOutcome::Eof => Ok(FrameOutcome::Empty),
        }
    }

    /// Delegates to `self.decoder.send_eof`. A real `Eof` result here means
    /// the decoder was already fully flushed (e.g. a repeated call) — the
    /// pre-extraction flush loop treated that identically to a fresh
    /// `Ok(())`, so it maps to `Accepted` here too.
    fn send_eof(&mut self) -> Result<SendOutcome, String> {
        match self.decoder.send_eof() {
            Ok(()) => Ok(SendOutcome::Accepted),
            Err(ffmpeg_next::Error::Other { errno }) if errno == libc::EAGAIN => {
                Ok(SendOutcome::Again)
            }
            Err(ffmpeg_next::Error::Eof) => Ok(SendOutcome::Accepted),
            Err(e) => Err(format!("send_eof fatal error: {e}")),
        }
    }
}

// ---------------------------------------------------------------------
// Test double + state-machine tests (no ffmpeg calls; runs on box per the
// crate's ffmpeg build requirement, but exercises none of it — see the
// module doc comment).
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A scriptable [`DecoderLoopBackend`] double: three independent
    /// pre-programmed outcome queues (one per trait method), consumed
    /// strictly in call order. Running a queue dry is a test bug, not a
    /// state the engine should hit — it panics loudly rather than guessing
    /// a default. `send_calls` is the retain-and-resubmit evidence: it
    /// records every `send_packet` call's exact `(ordinal, data)`, so tests
    /// can assert retries resubmit byte-identical content and that
    /// acceptance happened exactly once.
    #[derive(Default)]
    struct MockDecoder {
        send_script: VecDeque<Result<SendOutcome, String>>,
        frame_script: VecDeque<Result<FrameOutcome, String>>,
        eof_script: VecDeque<Result<SendOutcome, String>>,
        send_calls: Vec<(i64, Vec<u8>)>,
        receive_calls: u32,
        eof_calls: u32,
    }

    impl MockDecoder {
        fn new() -> Self {
            Self::default()
        }

        fn script_send(mut self, outcomes: impl IntoIterator<Item = Result<SendOutcome, String>>) -> Self {
            self.send_script.extend(outcomes);
            self
        }

        fn script_frames(mut self, outcomes: impl IntoIterator<Item = Result<FrameOutcome, String>>) -> Self {
            self.frame_script.extend(outcomes);
            self
        }

        fn script_eof(mut self, outcomes: impl IntoIterator<Item = Result<SendOutcome, String>>) -> Self {
            self.eof_script.extend(outcomes);
            self
        }
    }

    impl DecoderLoopBackend for MockDecoder {
        fn send_packet(&mut self, ordinal: i64, data: &[u8]) -> Result<SendOutcome, String> {
            self.send_calls.push((ordinal, data.to_vec()));
            self.send_script
                .pop_front()
                .unwrap_or_else(|| panic!("MockDecoder: send_packet script exhausted at call #{}", self.send_calls.len()))
        }

        fn receive_frame(&mut self) -> Result<FrameOutcome, String> {
            self.receive_calls += 1;
            self.frame_script
                .pop_front()
                .unwrap_or_else(|| panic!("MockDecoder: receive_frame script exhausted at call #{}", self.receive_calls))
        }

        fn send_eof(&mut self) -> Result<SendOutcome, String> {
            self.eof_calls += 1;
            self.eof_script
                .pop_front()
                .unwrap_or_else(|| panic!("MockDecoder: send_eof script exhausted at call #{}", self.eof_calls))
        }
    }

    // -- 1. Scripted Again -> retain-drain-resubmit, exactly-once accept --

    #[test]
    fn again_then_accept_retains_and_resubmits_exact_packet_exactly_once() {
        let mut mock = MockDecoder::new()
            .script_send([Ok(SendOutcome::Again), Ok(SendOutcome::Accepted)])
            .script_frames([Ok(FrameOutcome::Empty)]);

        let record = submit_with_retry(&mut mock, 7, b"payload-7", 64).unwrap();

        // Retained + resubmitted byte-identical (ordinal, data).
        assert_eq!(
            mock.send_calls,
            vec![(7, b"payload-7".to_vec()), (7, b"payload-7".to_vec())]
        );
        // Exactly one acceptance, exactly one EAGAIN, in that order.
        assert_eq!(
            record.attempts,
            vec![AttemptRecord { again: true }, AttemptRecord { again: false }]
        );
        assert_eq!(record.drains, vec![Vec::<Option<i64>>::new()]);
    }

    // -- 2. Zero-output packet, recovered by a later drain --

    #[test]
    fn zero_output_packet_is_recovered_by_a_later_drain() {
        let mut mock = MockDecoder::new()
            .script_send([Ok(SendOutcome::Accepted), Ok(SendOutcome::Accepted)])
            .script_frames([
                Ok(FrameOutcome::Empty),          // post-accept drain of packet 1: nothing yet
                Ok(FrameOutcome::Frame(Some(1))), // post-accept drain of packet 2: packet 1 shows up here
                Ok(FrameOutcome::Empty),
            ]);

        submit_with_retry(&mut mock, 1, b"p1", 64).unwrap();
        let first_drain = drain_fully(&mut mock).unwrap();
        assert!(first_drain.is_empty(), "packet 1 must be accepted with zero output on its own drain");

        submit_with_retry(&mut mock, 2, b"p2", 64).unwrap();
        let second_drain = drain_fully(&mut mock).unwrap();
        assert_eq!(second_drain, vec![Some(1)], "packet 1's frame recovered on packet 2's drain");
    }

    // -- 3. Multi-output drain: one drain emits >=3 with distinct ordinals --

    #[test]
    fn multi_output_drain_emits_distinct_ordinals_in_one_drain() {
        let mut mock = MockDecoder::new()
            .script_send([Ok(SendOutcome::Accepted)])
            .script_frames([
                Ok(FrameOutcome::Frame(Some(1))),
                Ok(FrameOutcome::Frame(Some(2))),
                Ok(FrameOutcome::Frame(Some(3))),
                Ok(FrameOutcome::Empty),
            ]);

        submit_with_retry(&mut mock, 9, b"p9", 64).unwrap();
        let drained = drain_fully(&mut mock).unwrap();
        assert_eq!(drained, vec![Some(1), Some(2), Some(3)]);
    }

    // -- 4. Tail flush emits the remainder with emitted == submitted --

    #[test]
    fn tail_flush_emits_remainder_with_emitted_eq_submitted() {
        // Two packets submitted with zero output each (both buffered inside
        // the decoder); flush_tail recovers both. Exactly one `Empty` per
        // mid-stream drain below (2 total) — no surplus, so the later
        // `extend` for the tail isn't corrupted by a stale leftover entry.
        let mut mock = MockDecoder::new()
            .script_send([Ok(SendOutcome::Accepted), Ok(SendOutcome::Accepted)])
            .script_frames([Ok(FrameOutcome::Empty), Ok(FrameOutcome::Empty)])
            .script_eof([Ok(SendOutcome::Accepted)]);

        submit_with_retry(&mut mock, 1, b"p1", 64).unwrap();
        drain_fully(&mut mock).unwrap();
        submit_with_retry(&mut mock, 2, b"p2", 64).unwrap();
        drain_fully(&mut mock).unwrap();

        // Reprogram the frame script for the tail: both buffered frames
        // come out on the post-EOF drain.
        mock.frame_script.extend([
            Ok(FrameOutcome::Frame(Some(1))),
            Ok(FrameOutcome::Frame(Some(2))),
            Ok(FrameOutcome::Empty),
        ]);

        let submitted = 2u64;
        let tail = flush_tail(&mut mock, 64).unwrap();
        assert_eq!(tail.recovered, vec![Some(1), Some(2)]);
        assert_eq!(tail.recovered.len() as u64, submitted, "emitted == submitted after tail flush");
        assert_eq!(tail.eagain_count, 0, "send_eof was accepted first try");
    }

    // -- 5. Double-EAGAIN on resubmit: no loss, no duplication --

    #[test]
    fn double_eagain_on_resubmit_handled_without_loss_or_duplication() {
        let mut mock = MockDecoder::new()
            .script_send([Ok(SendOutcome::Again), Ok(SendOutcome::Again), Ok(SendOutcome::Accepted)])
            .script_frames([Ok(FrameOutcome::Empty), Ok(FrameOutcome::Empty)]);

        let record = submit_with_retry(&mut mock, 5, b"p5", 64).unwrap();

        assert_eq!(mock.send_calls, vec![(5, b"p5".to_vec()); 3]);
        let accepted = record.attempts.iter().filter(|a| !a.again).count();
        let agains = record.attempts.iter().filter(|a| a.again).count();
        assert_eq!(accepted, 1, "accepted exactly once despite two EAGAINs");
        assert_eq!(agains, 2);
        assert_eq!(record.drains.len(), 2, "one drain per EAGAIN, no more");
    }

    // -- 6. Error propagation --

    #[test]
    fn send_packet_error_propagates() {
        let mut mock = MockDecoder::new().script_send([Err("native send failure".to_string())]);
        let err = submit_with_retry(&mut mock, 1, b"p1", 64).unwrap_err();
        assert!(err.contains("native send failure"));
    }

    #[test]
    fn receive_frame_error_propagates_during_drain() {
        let mut mock = MockDecoder::new()
            .script_send([Ok(SendOutcome::Accepted)])
            .script_frames([Err("native receive failure".to_string())]);
        submit_with_retry(&mut mock, 1, b"p1", 64).unwrap();
        let err = drain_fully(&mut mock).unwrap_err();
        assert!(err.contains("native receive failure"));
    }

    #[test]
    fn send_eof_error_propagates() {
        let mut mock = MockDecoder::new().script_eof([Err("native flush failure".to_string())]);
        let err = flush_tail(&mut mock, 64).unwrap_err();
        assert!(err.contains("native flush failure"));
    }

    // -- Bound enforcement: EAGAIN that never converges is a fatal Err,
    //    never a silent infinite loop (the bounded-protocol philosophy
    //    applied to the engine itself).

    #[test]
    fn eagain_exceeding_max_attempts_is_a_fatal_error_not_an_infinite_loop() {
        let mut mock = MockDecoder::new()
            .script_send([Ok(SendOutcome::Again), Ok(SendOutcome::Again), Ok(SendOutcome::Again)])
            .script_frames([Ok(FrameOutcome::Empty), Ok(FrameOutcome::Empty), Ok(FrameOutcome::Empty)]);
        let err = submit_with_retry(&mut mock, 1, b"p1", 2).unwrap_err();
        assert!(err.contains("did not converge"));
    }
}
