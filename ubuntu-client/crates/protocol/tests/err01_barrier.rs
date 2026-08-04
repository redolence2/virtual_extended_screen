//! ERR-01 activation-barrier scheduling proof (R2a).
//!
//! CONTRACT_ERRATA.md ERR-01: `VideoHelloAck(OK)` travels on the video TCP
//! connection; input/heartbeats travel on control. The client must emit no
//! input and no heartbeats until it receives the host's first control
//! `Heartbeat` (sent after the host accepts the Ack); the host must accept
//! the first post-barrier input.
//!
//! These traces prove the property against the R5 phase model — the single
//! state machine both endpoints share (plan-review amendment 1: the barrier
//! reuses the dispatcher's phase model; no second, drifting state machine).
//! Each trace walks the two TCP handlers' events in a specific schedule and
//! asserts the exact verdict at every step, including the reordered
//! schedules where the model must surface the race as PROTOCOL_VIOLATION
//! rather than silently arming input. The single-cell legality of every
//! (role, phase, payload) pair is already vector-graded by
//! `dispatch_cases.json`; what these traces add is the *sequence*
//! dimension: phases actually reached by walking events in order.
//!
//! C1 (corrective-cycle item closing review finding F2): `validate_inbound`
//! and `note_outbound` now take a `DispatchFacts` context. The traces below
//! are unchanged in meaning; each step is threaded with the `DispatchFacts`
//! consistent with the role/phase at that point (run confirmed once
//! `ProfileAccepted` is reached, candidate before that; diagnostics Normal
//! throughout -- these traces never exercise ClockPing/ClockPong; the
//! oldest outstanding ordinal fixed at the `frame_ack()` helper's ordinal
//! wherever a `FrameAck` envelope is validated).

use protocol::resc_v3::{self, envelope::Payload};
use protocol::v3dispatch::{
    note_outbound, note_video_ack, validate_inbound, DiagMode, Dispatch, DispatchFacts, OutboundKind, Phase,
    Role, RunFact,
};

const RUN: u64 = 0x0102_0304_0506_0708;

fn env(payload: Payload) -> resc_v3::Envelope {
    resc_v3::Envelope {
        session_run_id: RUN,
        protocol_version: 3,
        payload: Some(payload),
    }
}

fn heartbeat() -> resc_v3::Envelope {
    env(Payload::Heartbeat(resc_v3::Heartbeat {
        ..Default::default()
    }))
}

fn display_settings() -> resc_v3::Envelope {
    env(Payload::DisplaySettings(resc_v3::DisplaySettings {
        warm_strength: 0.5,
    }))
}

fn key_event() -> resc_v3::Envelope {
    env(Payload::KeyEvent(resc_v3::KeyEvent {
        hid_usage: 4,
        is_down: true,
        modifiers: 0,
    }))
}

fn frame_ack() -> resc_v3::Envelope {
    env(Payload::FrameAck(resc_v3::FrameAck { frame_ordinal: 1 }))
}

fn profile_result_accepted() -> resc_v3::Envelope {
    env(Payload::ProfileResult(resc_v3::ProfileResult {
        accepted: true,
        profile_canonical: vec![b'x'; 20],
        profile_hash: vec![1, 2, 3, 4, 5, 6, 7, 8],
        build_commit: "a".repeat(40),
        build_dirty: false,
        reject_code: 0,
        video_listener_ready: true,
    }))
}

/// Pulls the `next` phase out of a `Dispatch::Accepted`, panicking with the
/// actual value otherwise -- these traces only ever expect a phase
/// transition, never a `RemoteFatal` routing (no trace here sends a
/// `FatalReport`).
fn expect_next(d: Dispatch) -> Phase {
    match d {
        Dispatch::Accepted { next, .. } => next,
        other => panic!("expected Dispatch::Accepted, got {other:?}"),
    }
}

/// The client-side sends ERR-01 forbids before activation, plus FrameAck
/// (allowed earlier by design — decoded frames race the activation signal).
const CLIENT_INPUT_KINDS: [OutboundKind; 5] = [
    OutboundKind::KeyEvent,
    OutboundKind::ButtonEvent,
    OutboundKind::ScrollEvent,
    OutboundKind::ReleaseInput,
    OutboundKind::Heartbeat,
];

fn client_input_armed(phase: Phase, facts: &DispatchFacts) -> bool {
    CLIENT_INPUT_KINDS
        .iter()
        .all(|k| note_outbound(Role::Client, phase, facts, *k) == Ok(Phase::Active))
}

fn client_input_fully_disarmed(phase: Phase, facts: &DispatchFacts) -> bool {
    CLIENT_INPUT_KINDS
        .iter()
        .all(|k| note_outbound(Role::Client, phase, facts, *k).is_err())
}

/// Correct schedule, asserted step by step: Ack noted -> (optional host
/// traffic) -> activation heartbeat -> input armed. No input or client
/// heartbeat is permitted at ANY earlier step.
#[test]
fn client_correct_schedule_arms_input_only_at_activation() {
    // The client's run is already confirmed (Active) for this whole trace --
    // it starts at ProfileAccepted and never revisits Bootstrap/Announced.
    let facts = DispatchFacts {
        run: RunFact::Active(RUN),
        diagnostics: DiagMode::Normal,
        oldest_outstanding_ordinal: None,
    };

    // t0: profile accepted, Ack not yet sent.
    let mut phase = Phase::ProfileAccepted;
    assert!(client_input_fully_disarmed(phase, &facts));
    assert!(note_outbound(Role::Client, phase, &facts, OutboundKind::FrameAck).is_err());

    // A: the video thread notes the Ack (must happen before/atomically with
    // writing it — see the reordered trace below).
    phase = note_video_ack(phase).expect("ProfileAccepted -> VideoAckAccepted");
    assert_eq!(phase, Phase::VideoAckAccepted);

    // t1: post-Ack, pre-activation — the barrier window ERR-01 exists for.
    assert!(client_input_fully_disarmed(phase, &facts));
    // FrameAck is deliberately NOT gated by activation: the first decoded
    // frame can race the activation heartbeat.
    assert_eq!(
        note_outbound(Role::Client, phase, &facts, OutboundKind::FrameAck),
        Ok(Phase::VideoAckAccepted)
    );

    // D: non-input host traffic (DisplaySettings) does not arm input.
    let dispatch = validate_inbound(Role::Client, phase, &facts, &display_settings())
        .expect("DisplaySettings legal post-video-Ack");
    let next = expect_next(dispatch);
    assert_eq!(next, Phase::VideoAckAccepted);
    phase = next;
    assert!(client_input_fully_disarmed(phase, &facts));

    // H: the activation heartbeat — the one and only arming event.
    let dispatch = validate_inbound(Role::Client, phase, &facts, &heartbeat())
        .expect("activation heartbeat legal in VideoAckAccepted");
    let next = expect_next(dispatch);
    assert_eq!(next, Phase::Active);
    phase = next;

    // t2: armed. First post-barrier input and client heartbeat are legal.
    assert!(client_input_armed(phase, &facts));
    // Subsequent liveness heartbeats keep the phase.
    let dispatch = validate_inbound(Role::Client, phase, &facts, &heartbeat()).unwrap();
    assert_eq!(expect_next(dispatch), Phase::Active);
}

/// Reordered handlers: the control handler runs before the video thread
/// notes the Ack. The model must surface that schedule as a violation —
/// an implementation that writes the Ack before noting it has a provable
/// race instead of a silent early arm.
#[test]
fn client_reordered_schedule_is_surfaced_not_silently_armed() {
    let phase = Phase::ProfileAccepted;
    let facts = DispatchFacts {
        run: RunFact::Active(RUN),
        diagnostics: DiagMode::Normal,
        oldest_outstanding_ordinal: None,
    };
    assert_eq!(
        validate_inbound(Role::Client, phase, &facts, &heartbeat()).unwrap_err(),
        resc_v3::FatalCode::ProtocolViolation
    );
    assert_eq!(
        validate_inbound(Role::Client, phase, &facts, &display_settings()).unwrap_err(),
        resc_v3::FatalCode::ProtocolViolation
    );
    // And input stays disarmed regardless.
    assert!(client_input_fully_disarmed(phase, &facts));
}

/// Systematic prefix sweep over the legal client schedule [A, D, H]:
/// after every prefix, input is armed iff the activation heartbeat (H)
/// has been processed.
#[test]
fn client_prefix_sweep_arms_exactly_at_activation() {
    #[derive(Clone, Copy, PartialEq)]
    enum Ev {
        A, // note_video_ack
        D, // inbound DisplaySettings
        H, // inbound activation heartbeat
    }
    let schedule = [Ev::A, Ev::D, Ev::H];
    // Constant across the whole sweep: ProfileAccepted/VideoAckAccepted/
    // Active all require an Active run per the facts<->phase mapping.
    let facts = DispatchFacts {
        run: RunFact::Active(RUN),
        diagnostics: DiagMode::Normal,
        oldest_outstanding_ordinal: None,
    };

    for cut in 0..=schedule.len() {
        let mut phase = Phase::ProfileAccepted;
        let mut activated = false;
        for ev in &schedule[..cut] {
            phase = match ev {
                Ev::A => note_video_ack(phase).unwrap(),
                Ev::D => expect_next(
                    validate_inbound(Role::Client, phase, &facts, &display_settings()).unwrap(),
                ),
                Ev::H => {
                    activated = true;
                    expect_next(validate_inbound(Role::Client, phase, &facts, &heartbeat()).unwrap())
                }
            };
        }
        assert_eq!(
            client_input_armed(phase, &facts),
            activated,
            "prefix length {cut}: armed must equal activated"
        );
        if !activated {
            assert!(client_input_fully_disarmed(phase, &facts), "prefix length {cut}");
        }
    }
}

/// Host side: pre-activation input/heartbeats are rejected (never
/// injected), FrameAck racing the activation is legal, and the first input
/// after the activation send is accepted.
#[test]
fn host_rejects_early_input_and_accepts_first_post_barrier_input() {
    // The oldest outstanding ordinal never changes across this trace --
    // `frame_ack()` always names ordinal 1 -- so it is fixed at Some(1)
    // throughout (irrelevant to every payload kind but FrameAck). The run
    // fact tracks the phase: Candidate before the profile is accepted,
    // Active from ProfileAccepted onward (the host owns its candidate id
    // from process start, so it is never NoRun).
    fn facts(phase: Phase) -> DispatchFacts {
        let run = match phase {
            Phase::Bootstrap | Phase::Announced => RunFact::Candidate(RUN),
            _ => RunFact::Active(RUN),
        };
        DispatchFacts { run, diagnostics: DiagMode::Normal, oldest_outstanding_ordinal: Some(1) }
    }

    // Bootstrap -> Announced (host sends the announce).
    let phase = note_outbound(
        Role::Host,
        Phase::Bootstrap,
        &facts(Phase::Bootstrap),
        OutboundKind::HostProfileAnnounce,
    )
    .expect("announce from Bootstrap");
    assert_eq!(phase, Phase::Announced);

    // Client accepts the profile.
    let phase = expect_next(
        validate_inbound(Role::Host, phase, &facts(phase), &profile_result_accepted())
            .expect("ProfileResult(accepted) legal in Announced"),
    );
    assert_eq!(phase, Phase::ProfileAccepted);

    // No host heartbeat exists before the video handshake completes.
    assert!(note_outbound(Role::Host, phase, &facts(phase), OutboundKind::Heartbeat).is_err());

    // Video handshake completes (host accepts the client's Ack).
    let phase = note_video_ack(phase).unwrap();
    assert_eq!(phase, Phase::VideoAckAccepted);

    // Pre-activation: rogue early input and client heartbeats are
    // violations ("pre-Ack input is never injected" extends through the
    // barrier window); a FrameAck racing the activation is legal.
    assert_eq!(
        validate_inbound(Role::Host, phase, &facts(phase), &key_event()).unwrap_err(),
        resc_v3::FatalCode::ProtocolViolation
    );
    assert_eq!(
        validate_inbound(Role::Host, phase, &facts(phase), &heartbeat()).unwrap_err(),
        resc_v3::FatalCode::ProtocolViolation
    );
    assert_eq!(
        expect_next(validate_inbound(Role::Host, phase, &facts(phase), &frame_ack()).unwrap()),
        Phase::VideoAckAccepted
    );

    // The activation send is what moves the host to Active.
    let phase = note_outbound(Role::Host, phase, &facts(phase), OutboundKind::Heartbeat)
        .expect("activation heartbeat send");
    assert_eq!(phase, Phase::Active);

    // First post-barrier input, client heartbeat, and FrameAck all accepted.
    for envlp in [key_event(), heartbeat(), frame_ack()] {
        assert_eq!(
            expect_next(
                validate_inbound(Role::Host, phase, &facts(phase), &envlp)
                    .expect("post-barrier control traffic accepted"),
            ),
            Phase::Active
        );
    }
}
