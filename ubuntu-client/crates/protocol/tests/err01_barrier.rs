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

use protocol::resc_v3::{self, envelope::Payload};
use protocol::v3dispatch::{
    note_outbound, note_video_ack, validate_inbound, OutboundKind, Phase, Role,
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

/// The client-side sends ERR-01 forbids before activation, plus FrameAck
/// (allowed earlier by design — decoded frames race the activation signal).
const CLIENT_INPUT_KINDS: [OutboundKind; 5] = [
    OutboundKind::KeyEvent,
    OutboundKind::ButtonEvent,
    OutboundKind::ScrollEvent,
    OutboundKind::ReleaseInput,
    OutboundKind::Heartbeat,
];

fn client_input_armed(phase: Phase) -> bool {
    CLIENT_INPUT_KINDS
        .iter()
        .all(|k| note_outbound(Role::Client, phase, *k) == Ok(Phase::Active))
}

fn client_input_fully_disarmed(phase: Phase) -> bool {
    CLIENT_INPUT_KINDS
        .iter()
        .all(|k| note_outbound(Role::Client, phase, *k).is_err())
}

/// Correct schedule, asserted step by step: Ack noted -> (optional host
/// traffic) -> activation heartbeat -> input armed. No input or client
/// heartbeat is permitted at ANY earlier step.
#[test]
fn client_correct_schedule_arms_input_only_at_activation() {
    // t0: profile accepted, Ack not yet sent.
    let mut phase = Phase::ProfileAccepted;
    assert!(client_input_fully_disarmed(phase));
    assert!(note_outbound(Role::Client, phase, OutboundKind::FrameAck).is_err());

    // A: the video thread notes the Ack (must happen before/atomically with
    // writing it — see the reordered trace below).
    phase = note_video_ack(phase).expect("ProfileAccepted -> VideoAckAccepted");
    assert_eq!(phase, Phase::VideoAckAccepted);

    // t1: post-Ack, pre-activation — the barrier window ERR-01 exists for.
    assert!(client_input_fully_disarmed(phase));
    // FrameAck is deliberately NOT gated by activation: the first decoded
    // frame can race the activation heartbeat.
    assert_eq!(
        note_outbound(Role::Client, phase, OutboundKind::FrameAck),
        Ok(Phase::VideoAckAccepted)
    );

    // D: non-input host traffic (DisplaySettings) does not arm input.
    let accepted = validate_inbound(Role::Client, phase, &display_settings(), Some(RUN))
        .expect("DisplaySettings legal post-video-Ack");
    assert_eq!(accepted.next, Phase::VideoAckAccepted);
    phase = accepted.next;
    assert!(client_input_fully_disarmed(phase));

    // H: the activation heartbeat — the one and only arming event.
    let accepted = validate_inbound(Role::Client, phase, &heartbeat(), Some(RUN))
        .expect("activation heartbeat legal in VideoAckAccepted");
    assert_eq!(accepted.next, Phase::Active);
    phase = accepted.next;

    // t2: armed. First post-barrier input and client heartbeat are legal.
    assert!(client_input_armed(phase));
    // Subsequent liveness heartbeats keep the phase.
    let accepted = validate_inbound(Role::Client, phase, &heartbeat(), Some(RUN)).unwrap();
    assert_eq!(accepted.next, Phase::Active);
}

/// Reordered handlers: the control handler runs before the video thread
/// notes the Ack. The model must surface that schedule as a violation —
/// an implementation that writes the Ack before noting it has a provable
/// race instead of a silent early arm.
#[test]
fn client_reordered_schedule_is_surfaced_not_silently_armed() {
    let phase = Phase::ProfileAccepted;
    assert_eq!(
        validate_inbound(Role::Client, phase, &heartbeat(), Some(RUN)).unwrap_err(),
        resc_v3::FatalCode::ProtocolViolation
    );
    assert_eq!(
        validate_inbound(Role::Client, phase, &display_settings(), Some(RUN)).unwrap_err(),
        resc_v3::FatalCode::ProtocolViolation
    );
    // And input stays disarmed regardless.
    assert!(client_input_fully_disarmed(phase));
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

    for cut in 0..=schedule.len() {
        let mut phase = Phase::ProfileAccepted;
        let mut activated = false;
        for ev in &schedule[..cut] {
            phase = match ev {
                Ev::A => note_video_ack(phase).unwrap(),
                Ev::D => {
                    validate_inbound(Role::Client, phase, &display_settings(), Some(RUN))
                        .unwrap()
                        .next
                }
                Ev::H => {
                    activated = true;
                    validate_inbound(Role::Client, phase, &heartbeat(), Some(RUN))
                        .unwrap()
                        .next
                }
            };
        }
        assert_eq!(
            client_input_armed(phase),
            activated,
            "prefix length {cut}: armed must equal activated"
        );
        if !activated {
            assert!(client_input_fully_disarmed(phase), "prefix length {cut}");
        }
    }
}

/// Host side: pre-activation input/heartbeats are rejected (never
/// injected), FrameAck racing the activation is legal, and the first input
/// after the activation send is accepted.
#[test]
fn host_rejects_early_input_and_accepts_first_post_barrier_input() {
    // Bootstrap -> Announced (host sends the announce).
    let phase = note_outbound(Role::Host, Phase::Bootstrap, OutboundKind::HostProfileAnnounce)
        .expect("announce from Bootstrap");
    assert_eq!(phase, Phase::Announced);

    // Client accepts the profile.
    let phase = validate_inbound(Role::Host, phase, &profile_result_accepted(), Some(RUN))
        .expect("ProfileResult(accepted) legal in Announced")
        .next;
    assert_eq!(phase, Phase::ProfileAccepted);

    // No host heartbeat exists before the video handshake completes.
    assert!(note_outbound(Role::Host, phase, OutboundKind::Heartbeat).is_err());

    // Video handshake completes (host accepts the client's Ack).
    let phase = note_video_ack(phase).unwrap();
    assert_eq!(phase, Phase::VideoAckAccepted);

    // Pre-activation: rogue early input and client heartbeats are
    // violations ("pre-Ack input is never injected" extends through the
    // barrier window); a FrameAck racing the activation is legal.
    assert_eq!(
        validate_inbound(Role::Host, phase, &key_event(), Some(RUN)).unwrap_err(),
        resc_v3::FatalCode::ProtocolViolation
    );
    assert_eq!(
        validate_inbound(Role::Host, phase, &heartbeat(), Some(RUN)).unwrap_err(),
        resc_v3::FatalCode::ProtocolViolation
    );
    assert_eq!(
        validate_inbound(Role::Host, phase, &frame_ack(), Some(RUN))
            .unwrap()
            .next,
        Phase::VideoAckAccepted
    );

    // The activation send is what moves the host to Active.
    let phase = note_outbound(Role::Host, phase, OutboundKind::Heartbeat)
        .expect("activation heartbeat send");
    assert_eq!(phase, Phase::Active);

    // First post-barrier input, client heartbeat, and FrameAck all accepted.
    for envlp in [key_event(), heartbeat(), frame_ack()] {
        assert_eq!(
            validate_inbound(Role::Host, phase, &envlp, Some(RUN))
                .expect("post-barrier control traffic accepted")
                .next,
            Phase::Active
        );
    }
}
