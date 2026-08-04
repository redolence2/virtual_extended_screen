//! ERR-01 write-level barrier proof (corrective item C2).
//!
//! `A00_COMPLETION_REPORT_AMENDED_review.md` finding 3: the R2a phase-model
//! traces prove that `note_outbound` REJECTS pre-activation sends, but the
//! normative requirement (docs/WIRE.md §"activation barrier") is about the
//! write boundary — "no client control payload is written before activation
//! and the first post-barrier input is accepted." This file closes that gap
//! with a deterministic scheduler and a writer SPY around the shared
//! outbound gate: every send *attempt* is recorded (kind, written?), so the
//! retained attempt traces prove the gate — not scheduling luck — prevented
//! every pre-activation write, across named delayed/reordered cross-TCP
//! schedules. Since the live V3 cutover is T1 work, the gate+spy model the
//! client's only legal write path; no sockets are involved
//! (`A00_COMPLETION_REPORT_AMENDED_response_review.md` amendment: no
//! production-network integration needed).
//!
//! Swift twin: FixtureCheck section (h2).

use protocol::resc_v3::{self, envelope::Payload};
use protocol::v3dispatch::{
    note_outbound, note_video_ack, validate_inbound, DiagMode, Dispatch, DispatchFacts,
    OutboundKind, Phase, Role,
};

const RUN: u64 = 0x0102_0304_0506_0708;

fn facts() -> DispatchFacts {
    DispatchFacts {
        run: protocol::v3dispatch::RunFact::Active(RUN),
        diagnostics: DiagMode::Normal,
        oldest_outstanding_ordinal: None,
    }
}

fn heartbeat_env() -> resc_v3::Envelope {
    resc_v3::Envelope {
        session_run_id: RUN,
        protocol_version: 3,
        payload: Some(Payload::Heartbeat(resc_v3::Heartbeat::default())),
    }
}

/// One recorded send attempt: the kind, and whether the gate let it reach
/// the writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Attempt {
    kind: OutboundKind,
    written: bool,
}

fn a(kind: OutboundKind, written: bool) -> Attempt {
    Attempt { kind, written }
}

/// The modeled client control writer: the ONLY path that can produce a
/// control write. The gate consults `note_outbound` first; the spy records
/// every attempt, written or refused.
struct ClientGate {
    phase: Phase,
    attempts: Vec<Attempt>,
}

impl ClientGate {
    fn new() -> Self {
        ClientGate { phase: Phase::ProfileAccepted, attempts: Vec::new() }
    }

    /// A send attempt from the input/heartbeat/ack machinery.
    fn try_send(&mut self, kind: OutboundKind) {
        match note_outbound(Role::Client, self.phase, &facts(), kind) {
            Ok(next) => {
                self.attempts.push(a(kind, true)); // the write happens
                self.phase = next;
            }
            Err(_) => self.attempts.push(a(kind, false)), // refused: no write
        }
    }

    /// Video-thread event: the client noted its own VideoHelloAck(OK).
    fn on_video_ack_noted(&mut self) {
        self.phase = note_video_ack(self.phase).expect("ack noted from ProfileAccepted");
    }

    /// Control-thread event: an inbound heartbeat arrived. Returns whether
    /// the dispatcher accepted it (the reordered schedule expects rejection).
    fn on_inbound_heartbeat(&mut self) -> bool {
        match validate_inbound(Role::Client, self.phase, &facts(), &heartbeat_env()) {
            Ok(Dispatch::Accepted { next, .. }) => {
                self.phase = next;
                true
            }
            _ => false,
        }
    }

    fn writes(&self) -> Vec<OutboundKind> {
        self.attempts.iter().filter(|at| at.written).map(|at| at.kind).collect()
    }
}

use OutboundKind::{FrameAck, Heartbeat, KeyEvent};

/// Schedule S1 — "correct": video thread first, control activation second,
/// with input pressure at every stage. The retained attempt trace proves
/// exactly which attempts were refused and that the FIRST written
/// input-class payload follows activation.
#[test]
fn s1_correct_schedule_first_write_is_post_activation() {
    let mut g = ClientGate::new();
    g.try_send(KeyEvent); //   refused: ProfileAccepted
    g.try_send(Heartbeat); //  refused
    g.on_video_ack_noted();
    g.try_send(KeyEvent); //   refused: barrier window
    g.try_send(FrameAck); //   WRITTEN: acks race activation by design
    g.try_send(Heartbeat); //  refused
    assert!(g.on_inbound_heartbeat(), "activation accepted in VideoAckAccepted");
    g.try_send(KeyEvent); //   WRITTEN: first post-barrier input
    g.try_send(Heartbeat); //  WRITTEN

    // Full retained attempt trace, asserted exactly.
    assert_eq!(
        g.attempts,
        vec![
            a(KeyEvent, false),
            a(Heartbeat, false),
            a(KeyEvent, false),
            a(FrameAck, true),
            a(Heartbeat, false),
            a(KeyEvent, true),
            a(Heartbeat, true),
        ]
    );
    // The write boundary: nothing but FrameAck before activation; the first
    // post-activation write is the input.
    assert_eq!(g.writes(), vec![FrameAck, KeyEvent, Heartbeat]);
}

/// Schedule S2 — "control-first (reordered)": the control handler delivers
/// the activation heartbeat BEFORE the video thread notes the Ack. The
/// dispatcher must reject it (surfaced race), input stays unwritten, and
/// activation only lands after the Ack note.
#[test]
fn s2_reordered_control_first_never_writes_early() {
    let mut g = ClientGate::new();
    assert!(!g.on_inbound_heartbeat(), "activation before ack-note must be rejected");
    g.try_send(KeyEvent); // refused
    g.on_video_ack_noted();
    g.try_send(KeyEvent); // refused: still pre-activation
    assert!(g.on_inbound_heartbeat(), "activation accepted after ack-note");
    g.try_send(KeyEvent); // WRITTEN

    assert_eq!(
        g.attempts,
        vec![a(KeyEvent, false), a(KeyEvent, false), a(KeyEvent, true)]
    );
    assert_eq!(g.writes(), vec![KeyEvent]);
}

/// Schedule S3 — "delayed activation sweep": for every position of the
/// activation event within a fixed input-pressure stream, zero input/
/// heartbeat writes occur before it and the first write after it is the
/// next attempted input.
#[test]
fn s3_delayed_activation_sweep_zero_writes_before() {
    // Event stream: ack-note at 0, then N input attempts; activation is
    // inserted at every possible later position.
    const ATTEMPTS: usize = 5;
    for activation_at in 0..=ATTEMPTS {
        let mut g = ClientGate::new();
        g.on_video_ack_noted();
        let mut activated = false;
        for i in 0..ATTEMPTS {
            if i == activation_at {
                assert!(g.on_inbound_heartbeat());
                activated = true;
            }
            g.try_send(KeyEvent);
        }
        if activation_at == ATTEMPTS {
            assert!(!activated);
        }
        let writes = g.writes();
        let expected_writes = ATTEMPTS.saturating_sub(activation_at.min(ATTEMPTS));
        assert_eq!(
            writes.len(),
            expected_writes,
            "activation_at={activation_at}: writes must equal post-activation attempts"
        );
        assert!(writes.iter().all(|k| *k == KeyEvent));
        // And the refused prefix is exactly the pre-activation attempts.
        let refused = g.attempts.iter().filter(|at| !at.written).count();
        assert_eq!(refused, ATTEMPTS - expected_writes, "activation_at={activation_at}");
    }
}

/// Host side: the activation SEND is the host's one barrier write — the spy
/// proves it is attempted exactly once, only after the Ack is noted, and
/// that the first inbound input afterward is accepted.
#[test]
fn s4_host_activation_write_after_ack_note_then_first_input_accepted() {
    let mut attempts: Vec<Attempt> = Vec::new();
    let mut phase = Phase::ProfileAccepted;
    let host_facts = DispatchFacts {
        run: protocol::v3dispatch::RunFact::Active(RUN),
        diagnostics: DiagMode::Normal,
        oldest_outstanding_ordinal: None,
    };

    // Attempted activation before the Ack note: the gate refuses (no write).
    match note_outbound(Role::Host, phase, &host_facts, Heartbeat) {
        Ok(_) => attempts.push(a(Heartbeat, true)),
        Err(_) => attempts.push(a(Heartbeat, false)),
    }
    // Video thread notes the accepted Ack.
    phase = note_video_ack(phase).unwrap();
    // Activation send: written.
    match note_outbound(Role::Host, phase, &host_facts, Heartbeat) {
        Ok(next) => {
            attempts.push(a(Heartbeat, true));
            phase = next;
        }
        Err(_) => attempts.push(a(Heartbeat, false)),
    }
    assert_eq!(attempts, vec![a(Heartbeat, false), a(Heartbeat, true)]);
    assert_eq!(phase, Phase::Active);

    // First post-barrier input is accepted.
    let key = resc_v3::Envelope {
        session_run_id: RUN,
        protocol_version: 3,
        payload: Some(Payload::KeyEvent(resc_v3::KeyEvent {
            hid_usage: 4,
            is_down: true,
            modifiers: 0,
        })),
    };
    match validate_inbound(Role::Host, phase, &host_facts, &key) {
        Ok(Dispatch::Accepted { next, .. }) => assert_eq!(next, Phase::Active),
        other => panic!("first post-barrier input must be accepted, got {other:?}"),
    }
}
