//! RESC protocol v3 two-layer inbound dispatch (remediation item R5).
//!
//! Normative sources: `docs/WIRE.md` §1 "Control framing" (length-prefix
//! gate, per-field caps, oneof semantics, direction/state table);
//! `CONTRACT_ERRATA.md` ERR-01 (cross-TCP activation barrier — the reason
//! host-bound input/heartbeats require `Phase::Active` specifically, not
//! merely "post-video-Ack").
//!
//! Two independent, pure layers:
//!
//! - Layer 1 ([`frame_body_len`]): the 4-byte length-prefix gate. No I/O,
//!   no allocation — a pure function of 4 bytes. The caller allocates a
//!   body buffer only after this returns `Ok`.
//! - Layer 2 ([`validate_inbound`], [`note_outbound`], [`note_video_ack`]):
//!   the typed phase/direction validator and router. No sockets, no
//!   logging, no side effects — a pure function of (role, phase, envelope).
//!
//! This module is INACTIVE: nothing under `ubuntu-client/src/` wires it up
//! yet. The Swift twin lives at `mac-host/Sources/RescCore/V3Dispatch.swift`;
//! both are graded against the same oracle-generated cases in
//! `proto/fixtures/dispatch_cases.json` (see `tools/gen_dispatch_fixtures.py`,
//! which encodes the tables below exactly once as the ground truth).

use crate::resc_v3;

// ===========================================================================
// Layer 1 — framing-length gate (docs/WIRE.md §1 "Bounds")
// ===========================================================================

/// `u32` length-prefix domain accepted before frame allocation: `> 64 KiB`
/// is rejected pre-allocation (docs/WIRE.md §1 "Bounds"). Length `0` is
/// allowed through this gate — an empty body then fails at the
/// absent-payload check in [`validate_inbound`], not here.
const MAX_FRAME_BODY_LEN: u32 = 65536;

/// Layer 1: gate the 4-byte little-endian length prefix that precedes every
/// `Envelope` on the wire (docs/WIRE.md §1 "Control framing": `u32_le`
/// length + protobuf `Envelope`). Pure function of 4 bytes — no I/O, no
/// allocation; the caller allocates a body buffer of the returned length
/// only after this returns `Ok`.
pub fn frame_body_len(prefix: [u8; 4]) -> Result<usize, resc_v3::FatalCode> {
    let len = u32::from_le_bytes(prefix);
    if len > MAX_FRAME_BODY_LEN {
        Err(resc_v3::FatalCode::MalformedFraming)
    } else {
        Ok(len as usize)
    }
}

// ===========================================================================
// Layer 2 — typed validator/router (docs/WIRE.md §1 direction/state table;
// CONTRACT_ERRATA.md ERR-01)
// ===========================================================================

/// The receiving endpoint for [`validate_inbound`] — i.e. "who is
/// validating this inbound envelope", not who sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Host,
    Client,
}

/// Session phase shared by both roles (docs/WIRE.md §1; CONTRACT_ERRATA.md
/// ERR-01 for the `VideoAckAccepted` -> `Active` activation-barrier step).
/// Canonical vector strings (used by the shared JSON fixtures) are noted
/// per variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// "bootstrap" — control TCP connected, announce not yet exchanged.
    Bootstrap,
    /// "announced" — announce sent (host) / received (client).
    Announced,
    /// "profile_accepted"
    ProfileAccepted,
    /// "profile_rejected" — terminal; only `FatalReport` legal inbound.
    ProfileRejected,
    /// "video_ack_accepted" — video handshake done.
    VideoAckAccepted,
    /// "active" — ERR-01 activation barrier passed.
    Active,
}

/// Result of a successful [`validate_inbound`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Accepted {
    pub next: Phase,
    /// Set only when `expected_run_id` was `None` and the envelope's id
    /// became the candidate (docs/WIRE.md §1: legal only for the two
    /// client-bootstrap cases where no run id is known yet).
    pub learned_run_id: Option<u64>,
}

/// Validate one inbound `Envelope` against the receiver's current
/// `(role, phase)`, per the fixed 6-step order below (vectors in
/// `proto/fixtures/dispatch_cases.json` depend on this exact order):
///
/// 1. `protocol_version != 3` -> [`resc_v3::FatalCode::VersionMismatch`].
/// 2. Run id: `expected_run_id` mismatch -> `ProtocolViolation`; if
///    `expected_run_id` is `None`, the envelope's id is learned instead.
/// 3. Absent `payload` (unknown-only oneof decodes as absent — the
///    generated decoder's unknown-field ignoring and last-one-wins oneof
///    resolution are accepted as-is, no raw scanner) -> `ProtocolViolation`.
/// 4. Per-field caps -> `RecordCapViolation`.
/// 5. Direction/phase legality (docs/WIRE.md §1 table, ERR-01 refinement)
///    -> `ProtocolViolation`.
/// 6. Semantic ranges -> `ProtocolViolation`.
pub fn validate_inbound(
    role: Role,
    phase: Phase,
    env: &resc_v3::Envelope,
    expected_run_id: Option<u64>,
) -> Result<Accepted, resc_v3::FatalCode> {
    use resc_v3::FatalCode;

    // 1. protocol version.
    if env.protocol_version != 3 {
        return Err(FatalCode::VersionMismatch);
    }

    // 2. run id.
    let learned_run_id = match expected_run_id {
        Some(id) => {
            if env.session_run_id != id {
                return Err(FatalCode::ProtocolViolation);
            }
            None
        }
        None => Some(env.session_run_id),
    };

    // 3. payload presence.
    let payload = env.payload.as_ref().ok_or(FatalCode::ProtocolViolation)?;

    // 4. per-field caps.
    check_caps(payload)?;

    // 5. direction/phase legality.
    let next = match role {
        Role::Client => client_transition(phase, payload),
        Role::Host => host_transition(phase, payload),
    }
    .ok_or(FatalCode::ProtocolViolation)?;

    // 6. semantic ranges.
    check_semantic(payload)?;

    Ok(Accepted { next, learned_run_id })
}

/// Step 5 for `role = Client` (docs/WIRE.md §1 table). `HostProfileAnnounce`
/// is legal only as the first bootstrap message; `FatalReport` is legal in
/// every phase (the run id becomes known there via the `expected_run_id =
/// None` learn path in `Bootstrap`). Every other payload kind is
/// wrong-direction at the client and never legal inbound.
fn client_transition(phase: Phase, payload: &resc_v3::envelope::Payload) -> Option<Phase> {
    use resc_v3::envelope::Payload::*;
    use Phase::*;
    match payload {
        HostProfileAnnounce(_) => (phase == Bootstrap).then_some(Announced),
        DisplaySettings(_) => matches!(phase, VideoAckAccepted | Active).then_some(phase),
        // ERR-01: receipt of the host's activation Heartbeat is the
        // client's activation signal (VideoAckAccepted -> Active).
        Heartbeat(_) => match phase {
            VideoAckAccepted => Some(Active),
            Active => Some(Active),
            _ => None,
        },
        ClockPing(_) | ClockPong(_) => {
            matches!(phase, ProfileAccepted | VideoAckAccepted | Active).then_some(phase)
        }
        FatalReport(_) => Some(phase),
        // ProfileResult, FrameAck, KeyEvent, ButtonEvent, ScrollEvent,
        // ReleaseInput: client->host only, never legal inbound at client.
        _ => None,
    }
}

/// Step 5 for `role = Host` (docs/WIRE.md §1 table). `expected_run_id` is
/// always `Some` at the host, so `FatalReport` excludes `Bootstrap` (no
/// client can legitimately know the host's run id before the announce).
/// ERR-01: `KeyEvent`/`ButtonEvent`/`ScrollEvent`/`ReleaseInput`/`Heartbeat`
/// require `Phase::Active` specifically — the host has sent its activation
/// Heartbeat (which is what moved it to `Active`) before any input is
/// legal; pre-Ack input is never injected.
fn host_transition(phase: Phase, payload: &resc_v3::envelope::Payload) -> Option<Phase> {
    use resc_v3::envelope::Payload::*;
    use Phase::*;
    match payload {
        ProfileResult(p) => {
            if phase != Announced {
                return None;
            }
            Some(if p.accepted { ProfileAccepted } else { ProfileRejected })
        }
        FrameAck(_) => matches!(phase, VideoAckAccepted | Active).then_some(phase),
        KeyEvent(_) | ButtonEvent(_) | ScrollEvent(_) | ReleaseInput(_) => {
            (phase == Active).then_some(Active)
        }
        Heartbeat(_) => (phase == Active).then_some(Active),
        ClockPing(_) | ClockPong(_) => {
            matches!(phase, ProfileAccepted | VideoAckAccepted | Active).then_some(phase)
        }
        FatalReport(_) => (phase != Bootstrap).then_some(phase),
        // HostProfileAnnounce, DisplaySettings: host->client only, never
        // legal inbound at host.
        _ => None,
    }
}

/// Step 4: per-field caps (docs/WIRE.md §1 "Per-field caps"). All string
/// fields <= 256 B except `FatalReport.summary` <= 2048 B;
/// `profile_canonical` <= 4096 B; `profile_hash` exactly 8 B. Only the
/// three message kinds below carry variable-length fields.
fn check_caps(payload: &resc_v3::envelope::Payload) -> Result<(), resc_v3::FatalCode> {
    use resc_v3::envelope::Payload::*;
    use resc_v3::FatalCode::RecordCapViolation;
    match payload {
        HostProfileAnnounce(p) => {
            if p.build_commit.len() > 256 {
                return Err(RecordCapViolation);
            }
            if p.profile_canonical.len() > 4096 {
                return Err(RecordCapViolation);
            }
            if p.profile_hash.len() != 8 {
                return Err(RecordCapViolation);
            }
        }
        ProfileResult(p) => {
            if p.build_commit.len() > 256 {
                return Err(RecordCapViolation);
            }
            if p.profile_canonical.len() > 4096 {
                return Err(RecordCapViolation);
            }
            if p.profile_hash.len() != 8 {
                return Err(RecordCapViolation);
            }
        }
        FatalReport(p) => {
            if p.component.len() > 256 {
                return Err(RecordCapViolation);
            }
            if p.native_domain.len() > 256 {
                return Err(RecordCapViolation);
            }
            if p.summary.len() > 2048 {
                return Err(RecordCapViolation);
            }
        }
        _ => {}
    }
    Ok(())
}

/// Step 6: semantic ranges. `ButtonEvent.button` in `{0,1,2}`;
/// `DisplaySettings.warm_strength` finite and in `[0.0, 1.0]`;
/// `ProfileResult.reject_code == FATAL_UNSPECIFIED` iff `accepted`,
/// `video_listener_ready == true` iff `accepted`; `build_commit` (both
/// messages) exactly 40 lowercase hex chars.
fn check_semantic(payload: &resc_v3::envelope::Payload) -> Result<(), resc_v3::FatalCode> {
    use resc_v3::envelope::Payload::*;
    use resc_v3::FatalCode::ProtocolViolation;
    match payload {
        ButtonEvent(p) => {
            if !matches!(p.button, 0 | 1 | 2) {
                return Err(ProtocolViolation);
            }
        }
        DisplaySettings(p) => {
            if !(p.warm_strength.is_finite() && (0.0..=1.0).contains(&p.warm_strength)) {
                return Err(ProtocolViolation);
            }
        }
        ProfileResult(p) => {
            let unspecified = p.reject_code == resc_v3::FatalCode::FatalUnspecified as i32;
            if unspecified != p.accepted {
                return Err(ProtocolViolation);
            }
            if p.video_listener_ready != p.accepted {
                return Err(ProtocolViolation);
            }
            if !build_commit_valid(&p.build_commit) {
                return Err(ProtocolViolation);
            }
        }
        HostProfileAnnounce(p) => {
            if !build_commit_valid(&p.build_commit) {
                return Err(ProtocolViolation);
            }
        }
        _ => {}
    }
    Ok(())
}

/// Exactly 40 lowercase hex characters (a full git object id).
fn build_commit_valid(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Payload kinds an endpoint may *send*, for [`note_outbound`]. Mirrors the
/// `Envelope.payload` oneof but distinguishes the two `ProfileResult`
/// outcomes, since the sender always knows which one it is sending (unlike
/// [`validate_inbound`], which inspects an already-received envelope).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundKind {
    HostProfileAnnounce,
    ProfileResultAccepted,
    ProfileResultRejected,
    FrameAck,
    KeyEvent,
    ButtonEvent,
    ScrollEvent,
    ReleaseInput,
    Heartbeat,
    ClockPing,
    ClockPong,
    DisplaySettings,
    FatalReport,
}

/// Advance `phase` for an endpoint about to *send* `kind` (the mirror image
/// of [`validate_inbound`]'s direction/phase table, from the sender's
/// side). ERR-01: `Heartbeat` sent by the host while `VideoAckAccepted` is
/// the activation send (-> `Active`); client input/heartbeats are legal to
/// send only once the client is already `Active`.
pub fn note_outbound(role: Role, phase: Phase, kind: OutboundKind) -> Result<Phase, resc_v3::FatalCode> {
    use OutboundKind::*;
    use Phase::*;
    let next = match role {
        Role::Host => match kind {
            HostProfileAnnounce => (phase == Bootstrap).then_some(Announced),
            DisplaySettings => matches!(phase, VideoAckAccepted | Active).then_some(phase),
            Heartbeat => match phase {
                VideoAckAccepted => Some(Active),
                Active => Some(Active),
                _ => None,
            },
            ClockPing | ClockPong => {
                matches!(phase, ProfileAccepted | VideoAckAccepted | Active).then_some(phase)
            }
            FatalReport => Some(phase),
            ProfileResultAccepted | ProfileResultRejected | FrameAck | KeyEvent | ButtonEvent
            | ScrollEvent | ReleaseInput => None,
        },
        Role::Client => match kind {
            ProfileResultAccepted => (phase == Announced).then_some(ProfileAccepted),
            ProfileResultRejected => (phase == Announced).then_some(ProfileRejected),
            FrameAck => matches!(phase, VideoAckAccepted | Active).then_some(phase),
            KeyEvent | ButtonEvent | ScrollEvent | ReleaseInput => (phase == Active).then_some(Active),
            // ERR-01: client heartbeats armed only post-activation.
            Heartbeat => (phase == Active).then_some(Active),
            ClockPing | ClockPong => {
                matches!(phase, ProfileAccepted | VideoAckAccepted | Active).then_some(phase)
            }
            FatalReport => (phase != Bootstrap).then_some(phase),
            HostProfileAnnounce | DisplaySettings => None,
        },
    };
    next.ok_or(resc_v3::FatalCode::ProtocolViolation)
}

/// Video handshake completion: `ProfileAccepted -> VideoAckAccepted`, the
/// only legal transition (docs/WIRE.md §2/§3; host accepts the Ack /
/// client sends it).
pub fn note_video_ack(phase: Phase) -> Result<Phase, resc_v3::FatalCode> {
    match phase {
        Phase::ProfileAccepted => Ok(Phase::VideoAckAccepted),
        _ => Err(resc_v3::FatalCode::ProtocolViolation),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures_path(rel: &str) -> std::path::PathBuf {
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../proto/fixtures")).join(rel)
    }

    fn read_fixture(rel: &str) -> Vec<u8> {
        std::fs::read(fixtures_path(rel)).unwrap_or_else(|e| panic!("failed to read fixture {rel}: {e}"))
    }

    fn read_fixture_json(rel: &str) -> serde_json::Value {
        let s = std::fs::read_to_string(fixtures_path(rel))
            .unwrap_or_else(|e| panic!("failed to read fixture {rel}: {e}"));
        serde_json::from_str(&s).unwrap_or_else(|e| panic!("failed to parse {rel} as json: {e}"))
    }

    fn hex_bytes(s: &str) -> Vec<u8> {
        assert_eq!(s.len() % 2, 0, "odd hex length: {s}");
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or_else(|e| panic!("bad hex {s}: {e}")))
            .collect()
    }

    // Shared by `dispatch_fixtures` (validate_inbound) and `outbound`
    // (note_outbound / note_video_ack) — both consume rows shaped by the
    // same role/phase/verdict vocabulary.

    fn parse_role(s: &str) -> Role {
        match s {
            "host" => Role::Host,
            "client" => Role::Client,
            other => panic!("unknown role {other}"),
        }
    }

    fn parse_phase(s: &str) -> Phase {
        match s {
            "bootstrap" => Phase::Bootstrap,
            "announced" => Phase::Announced,
            "profile_accepted" => Phase::ProfileAccepted,
            "profile_rejected" => Phase::ProfileRejected,
            "video_ack_accepted" => Phase::VideoAckAccepted,
            "active" => Phase::Active,
            other => panic!("unknown phase {other}"),
        }
    }

    enum ExpectedVerdict {
        Accept { next: Phase, learn: bool },
        Error(resc_v3::FatalCode),
    }

    fn parse_verdict(s: &str) -> ExpectedVerdict {
        if let Some(rest) = s.strip_prefix("accept:") {
            let mut parts = rest.split(':');
            let phase_name = parts.next().unwrap();
            let learn = parts.next() == Some("learn");
            ExpectedVerdict::Accept { next: parse_phase(phase_name), learn }
        } else {
            let code = resc_v3::FatalCode::from_str_name(s)
                .unwrap_or_else(|| panic!("unknown FatalCode name {s}"));
            ExpectedVerdict::Error(code)
        }
    }

    // -----------------------------------------------------------------
    // 1. Layer 1 — frame_body_len
    // -----------------------------------------------------------------
    mod framing {
        use super::*;
        use std::io::Read;

        #[test]
        fn boundary_65536_ok() {
            assert_eq!(frame_body_len(65536u32.to_le_bytes()), Ok(65536));
        }

        #[test]
        fn boundary_65537_err() {
            assert_eq!(
                frame_body_len(65537u32.to_le_bytes()),
                Err(resc_v3::FatalCode::MalformedFraming)
            );
        }

        #[test]
        fn max_u32_err() {
            assert_eq!(
                frame_body_len(0xFFFF_FFFFu32.to_le_bytes()),
                Err(resc_v3::FatalCode::MalformedFraming)
            );
        }

        #[test]
        fn zero_len_ok() {
            assert_eq!(frame_body_len(0u32.to_le_bytes()), Ok(0));
        }

        /// Wraps a `Read` and counts calls/bytes, so the zero-body-reads
        /// test below can prove the reference loop never touches the body
        /// after an oversized prefix.
        struct CountingReader<R> {
            inner: R,
            bytes_read: usize,
            read_calls: usize,
        }

        impl<R: Read> Read for CountingReader<R> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.read_calls += 1;
                let n = self.inner.read(buf)?;
                self.bytes_read += n;
                Ok(n)
            }
        }

        /// The reference read loop callers must follow: read the 4-byte
        /// prefix, gate it through `frame_body_len`, and only then
        /// allocate+read the body. Returns the gate's verdict alongside the
        /// number of body-sized allocations actually performed (0 or 1).
        fn reference_read_loop<R: Read>(r: &mut R) -> (Result<usize, resc_v3::FatalCode>, usize) {
            let mut prefix = [0u8; 4];
            r.read_exact(&mut prefix).expect("prefix read");
            match frame_body_len(prefix) {
                Ok(len) => {
                    let mut body = vec![0u8; len]; // the only body-sized allocation
                    r.read_exact(&mut body).expect("body read");
                    (Ok(len), 1)
                }
                Err(e) => (Err(e), 0),
            }
        }

        #[test]
        fn oversized_prefix_zero_body_reads_zero_allocations() {
            let prefix = 65537u32.to_le_bytes();
            let mut reader = CountingReader { inner: &prefix[..], bytes_read: 0, read_calls: 0 };
            let (result, allocations) = reference_read_loop(&mut reader);
            assert_eq!(result, Err(resc_v3::FatalCode::MalformedFraming));
            assert_eq!(allocations, 0, "must not allocate a body buffer on the error path");
            assert_eq!(reader.bytes_read, 4, "must read only the 4-byte prefix, never the body");
            assert_eq!(reader.read_calls, 1, "must issue exactly one read call (the prefix)");
        }

        #[test]
        fn accepted_prefix_reads_exactly_the_body() {
            let mut data = 3u32.to_le_bytes().to_vec();
            data.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
            let mut reader = CountingReader { inner: &data[..], bytes_read: 0, read_calls: 0 };
            let (result, allocations) = reference_read_loop(&mut reader);
            assert_eq!(result, Ok(3));
            assert_eq!(allocations, 1);
            assert_eq!(reader.bytes_read, 7);
        }

        #[test]
        fn framing_cases_from_json() {
            let v = read_fixture_json("dispatch_cases.json");
            let cases = v["framing"].as_array().expect("framing must be an array");
            assert_eq!(cases.len(), 6);
            for case in cases {
                let name = case["name"].as_str().unwrap();
                let prefix_bytes = hex_bytes(case["prefix_hex"].as_str().unwrap());
                let prefix: [u8; 4] = prefix_bytes.clone().try_into().unwrap_or_else(|_| {
                    panic!("{name}: prefix_hex must decode to 4 bytes, got {prefix_bytes:?}")
                });
                let verdict = case["verdict"].as_str().unwrap();
                let result = frame_body_len(prefix);
                if verdict == "accept" {
                    let expected_len = u32::from_le_bytes(prefix) as usize;
                    assert_eq!(result, Ok(expected_len), "{name}");
                } else {
                    let code = resc_v3::FatalCode::from_str_name(verdict)
                        .unwrap_or_else(|| panic!("{name}: unknown FatalCode name {verdict}"));
                    assert_eq!(result, Err(code), "{name}");
                }
            }
        }
    }

    // -----------------------------------------------------------------
    // 2. Layer 2 — validate_inbound over the shared state/raw fixtures
    // -----------------------------------------------------------------
    mod dispatch_fixtures {
        use super::*;

        fn u_field(fields: &serde_json::Value, key: &str) -> u64 {
            fields[key].as_u64().unwrap_or_else(|| panic!("field {key} missing/not u64 in {fields}"))
        }
        fn i_field(fields: &serde_json::Value, key: &str) -> i64 {
            fields[key].as_i64().unwrap_or_else(|| panic!("field {key} missing/not i64 in {fields}"))
        }
        fn b_field(fields: &serde_json::Value, key: &str) -> bool {
            fields[key].as_bool().unwrap_or_else(|| panic!("field {key} missing/not bool in {fields}"))
        }
        fn s_field<'a>(fields: &'a serde_json::Value, key: &str) -> &'a str {
            fields[key].as_str().unwrap_or_else(|| panic!("field {key} missing/not string in {fields}"))
        }
        fn warm_strength_field(fields: &serde_json::Value) -> f32 {
            let v = &fields["warm_strength"];
            if let Some(s) = v.as_str() {
                match s {
                    "NaN" => f32::NAN,
                    "Infinity" => f32::INFINITY,
                    "-Infinity" => f32::NEG_INFINITY,
                    other => panic!("unknown warm_strength sentinel {other}"),
                }
            } else {
                v.as_f64().unwrap_or_else(|| panic!("warm_strength not a number: {v}")) as f32
            }
        }

        fn build_payload(kind: &str, fields: &serde_json::Value) -> resc_v3::envelope::Payload {
            use resc_v3::envelope::Payload;
            match kind {
                "display_settings" => Payload::DisplaySettings(resc_v3::DisplaySettings {
                    warm_strength: warm_strength_field(fields),
                }),
                "key_event" => Payload::KeyEvent(resc_v3::KeyEvent {
                    hid_usage: u_field(fields, "hid_usage") as u32,
                    is_down: b_field(fields, "is_down"),
                    modifiers: u_field(fields, "modifiers") as u32,
                }),
                "host_profile_announce" => Payload::HostProfileAnnounce(resc_v3::HostProfileAnnounce {
                    profile_canonical: hex_bytes(s_field(fields, "profile_canonical_hex")),
                    profile_hash: hex_bytes(s_field(fields, "profile_hash_hex")),
                    build_commit: s_field(fields, "build_commit").to_string(),
                    build_dirty: b_field(fields, "build_dirty"),
                }),
                "profile_result" => Payload::ProfileResult(resc_v3::ProfileResult {
                    accepted: b_field(fields, "accepted"),
                    profile_canonical: hex_bytes(s_field(fields, "profile_canonical_hex")),
                    profile_hash: hex_bytes(s_field(fields, "profile_hash_hex")),
                    build_commit: s_field(fields, "build_commit").to_string(),
                    build_dirty: b_field(fields, "build_dirty"),
                    reject_code: i_field(fields, "reject_code") as i32,
                    video_listener_ready: b_field(fields, "video_listener_ready"),
                }),
                "frame_ack" => Payload::FrameAck(resc_v3::FrameAck {
                    frame_ordinal: u_field(fields, "frame_ordinal"),
                }),
                "button_event" => Payload::ButtonEvent(resc_v3::ButtonEvent {
                    button: u_field(fields, "button") as u32,
                    is_down: b_field(fields, "is_down"),
                    x_px: i_field(fields, "x_px") as i32,
                    y_px: i_field(fields, "y_px") as i32,
                    modifiers: u_field(fields, "modifiers") as u32,
                }),
                "scroll_event" => Payload::ScrollEvent(resc_v3::ScrollEvent {
                    dx: i_field(fields, "dx") as i32,
                    dy: i_field(fields, "dy") as i32,
                }),
                "clock_ping" => Payload::ClockPing(resc_v3::ClockPing {
                    t1_mono_us: u_field(fields, "t1_mono_us"),
                    seq: u_field(fields, "seq") as u32,
                }),
                "clock_pong" => Payload::ClockPong(resc_v3::ClockPong {
                    t1_mono_us: u_field(fields, "t1_mono_us"),
                    t2_mono_us: u_field(fields, "t2_mono_us"),
                    t3_mono_us: u_field(fields, "t3_mono_us"),
                    seq: u_field(fields, "seq") as u32,
                }),
                "fatal_report" => Payload::FatalReport(resc_v3::FatalReport {
                    code: i_field(fields, "code") as i32,
                    component: s_field(fields, "component").to_string(),
                    native_domain: fields["native_domain"].as_str().unwrap_or("").to_string(),
                    native_code: i_field(fields, "native_code"),
                    summary: s_field(fields, "summary").to_string(),
                }),
                "release_input" => Payload::ReleaseInput(resc_v3::ReleaseInput {}),
                "heartbeat" => Payload::Heartbeat(resc_v3::Heartbeat {
                    t_mono_us: u_field(fields, "t_mono_us"),
                }),
                other => panic!("unknown payload kind {other}"),
            }
        }

        /// Shared assertion for one state/raw case: build inputs from the
        /// JSON row, call `validate_inbound`, and check the result against
        /// the row's `verdict` string.
        fn assert_case(
            name: &str,
            role: Role,
            phase: Phase,
            env: &resc_v3::Envelope,
            expected_run_id: Option<u64>,
            verdict: &str,
        ) {
            let result = validate_inbound(role, phase, env, expected_run_id);
            match parse_verdict(verdict) {
                ExpectedVerdict::Accept { next, learn } => {
                    let accepted =
                        result.unwrap_or_else(|e| panic!("{name}: expected accept, got Err({e:?})"));
                    assert_eq!(accepted.next, next, "{name}: next phase");
                    let expected_learned = if learn { Some(env.session_run_id) } else { None };
                    assert_eq!(accepted.learned_run_id, expected_learned, "{name}: learned_run_id");
                }
                ExpectedVerdict::Error(code) => {
                    assert_eq!(result, Err(code), "{name}");
                }
            }
        }

        #[test]
        fn state_matrix_and_special_rows() {
            let v = read_fixture_json("dispatch_cases.json");
            let cases = v["state"].as_array().expect("state must be an array");
            assert_eq!(cases.len(), 164, "expected 144 matrix + 20 special rows");
            for case in cases {
                let name = case["name"].as_str().unwrap();
                let role = parse_role(case["role"].as_str().unwrap());
                let phase = parse_phase(case["phase"].as_str().unwrap());
                let kind = case["payload"].as_str().unwrap();
                let payload = build_payload(kind, &case["fields"]);
                let env = resc_v3::Envelope {
                    session_run_id: u_field(case, "env_run_id"),
                    protocol_version: u_field(case, "env_version") as u32,
                    payload: Some(payload),
                };
                let expected_run_id = case["expected_run_id"].as_u64();
                assert_case(name, role, phase, &env, expected_run_id, case["verdict"].as_str().unwrap());
            }
        }

        #[test]
        fn raw_byte_vectors() {
            let v = read_fixture_json("dispatch_cases.json");
            let cases = v["raw"].as_array().expect("raw must be an array");
            assert_eq!(cases.len(), 3);
            for case in cases {
                use prost::Message;
                let name = case["name"].as_str().unwrap();
                let file = case["file"].as_str().unwrap();
                let bytes = read_fixture(file);
                let env = resc_v3::Envelope::decode(bytes.as_slice())
                    .unwrap_or_else(|e| panic!("{name}: {file} failed to decode: {e}"));
                let role = parse_role(case["role"].as_str().unwrap());
                let phase = parse_phase(case["phase"].as_str().unwrap());
                let expected_run_id = case["expected_run_id"].as_u64();
                assert_case(name, role, phase, &env, expected_run_id, case["verdict"].as_str().unwrap());
            }
        }
    }

    // -----------------------------------------------------------------
    // 3. note_outbound / note_video_ack unit tests
    // -----------------------------------------------------------------
    mod outbound {
        use super::*;

        const ALL_INPUT_KINDS: [OutboundKind; 4] = [
            OutboundKind::KeyEvent,
            OutboundKind::ButtonEvent,
            OutboundKind::ScrollEvent,
            OutboundKind::ReleaseInput,
        ];
        const PRE_ACTIVE_PHASES: [Phase; 5] = [
            Phase::Bootstrap,
            Phase::Announced,
            Phase::ProfileAccepted,
            Phase::ProfileRejected,
            Phase::VideoAckAccepted,
        ];

        #[test]
        fn host_announce_bootstrap_to_announced() {
            assert_eq!(
                note_outbound(Role::Host, Phase::Bootstrap, OutboundKind::HostProfileAnnounce),
                Ok(Phase::Announced)
            );
        }

        #[test]
        fn client_input_rejected_in_every_phase_except_active() {
            for &phase in &PRE_ACTIVE_PHASES {
                for &kind in &ALL_INPUT_KINDS {
                    assert_eq!(
                        note_outbound(Role::Client, phase, kind),
                        Err(resc_v3::FatalCode::ProtocolViolation),
                        "{phase:?} {kind:?}"
                    );
                }
            }
            for &kind in &ALL_INPUT_KINDS {
                assert_eq!(note_outbound(Role::Client, Phase::Active, kind), Ok(Phase::Active), "{kind:?}");
            }
        }

        #[test]
        fn client_heartbeat_rejected_pre_active() {
            for &phase in &PRE_ACTIVE_PHASES {
                assert_eq!(
                    note_outbound(Role::Client, phase, OutboundKind::Heartbeat),
                    Err(resc_v3::FatalCode::ProtocolViolation),
                    "{phase:?}"
                );
            }
            assert_eq!(
                note_outbound(Role::Client, Phase::Active, OutboundKind::Heartbeat),
                Ok(Phase::Active)
            );
        }

        #[test]
        fn activation_transitions_both_sides() {
            // Host sends its activation Heartbeat while VideoAckAccepted,
            // moving itself to Active (ERR-01 step 2).
            assert_eq!(
                note_outbound(Role::Host, Phase::VideoAckAccepted, OutboundKind::Heartbeat),
                Ok(Phase::Active)
            );
            // Client, on receiving that Heartbeat, also moves
            // VideoAckAccepted -> Active (ERR-01 step 3, the client's
            // activation signal) via validate_inbound.
            let env = resc_v3::Envelope {
                session_run_id: 0x0102030405060708,
                protocol_version: 3,
                payload: Some(resc_v3::envelope::Payload::Heartbeat(resc_v3::Heartbeat { t_mono_us: 1 })),
            };
            let accepted = validate_inbound(
                Role::Client,
                Phase::VideoAckAccepted,
                &env,
                Some(0x0102030405060708),
            )
            .expect("client must accept the host's activation heartbeat");
            assert_eq!(accepted.next, Phase::Active);
        }

        #[test]
        fn note_video_ack_profile_accepted_to_video_ack_accepted() {
            assert_eq!(note_video_ack(Phase::ProfileAccepted), Ok(Phase::VideoAckAccepted));
        }

        #[test]
        fn note_video_ack_every_other_phase_rejected() {
            for phase in [
                Phase::Bootstrap,
                Phase::Announced,
                Phase::ProfileRejected,
                Phase::VideoAckAccepted,
                Phase::Active,
            ] {
                assert_eq!(
                    note_video_ack(phase),
                    Err(resc_v3::FatalCode::ProtocolViolation),
                    "{phase:?}"
                );
            }
        }

        fn parse_outbound_kind(s: &str) -> OutboundKind {
            use OutboundKind::*;
            match s {
                "host_profile_announce" => HostProfileAnnounce,
                "profile_result_accepted" => ProfileResultAccepted,
                "profile_result_rejected" => ProfileResultRejected,
                "frame_ack" => FrameAck,
                "key_event" => KeyEvent,
                "button_event" => ButtonEvent,
                "scroll_event" => ScrollEvent,
                "release_input" => ReleaseInput,
                "heartbeat" => Heartbeat,
                "clock_ping" => ClockPing,
                "clock_pong" => ClockPong,
                "display_settings" => DisplaySettings,
                "fatal_report" => FatalReport,
                other => panic!("unknown OutboundKind name {other}"),
            }
        }

        /// D1 acceptance ("both languages pass shared vectors") requires
        /// note_outbound to be vector-covered like validate_inbound, not
        /// just hand-written spot checks (the tests above stay — they read
        /// well as documentation of specific rules). Covers the full 13
        /// kinds x 6 phases x 2 roles matrix from the same oracle
        /// (tools/gen_dispatch_fixtures.py's `outbound_transition`) the
        /// Swift twin is graded against.
        #[test]
        fn outbound_matrix_from_json() {
            let v = read_fixture_json("dispatch_cases.json");
            let cases = v["outbound"].as_array().expect("outbound must be an array");
            assert_eq!(cases.len(), 156, "expected 13 kinds x 6 phases x 2 roles");
            for case in cases {
                let name = case["name"].as_str().unwrap();
                let role = parse_role(case["role"].as_str().unwrap());
                let phase = parse_phase(case["phase"].as_str().unwrap());
                let kind = parse_outbound_kind(case["kind"].as_str().unwrap());
                let result = note_outbound(role, phase, kind);
                match parse_verdict(case["verdict"].as_str().unwrap()) {
                    ExpectedVerdict::Accept { next, .. } => assert_eq!(result, Ok(next), "{name}"),
                    ExpectedVerdict::Error(code) => assert_eq!(result, Err(code), "{name}"),
                }
            }
        }

        /// Same D1 rationale as `outbound_matrix_from_json`, for
        /// note_video_ack's 6-row table.
        #[test]
        fn video_ack_matrix_from_json() {
            let v = read_fixture_json("dispatch_cases.json");
            let cases = v["video_ack"].as_array().expect("video_ack must be an array");
            assert_eq!(cases.len(), 6);
            for case in cases {
                let name = case["name"].as_str().unwrap();
                let phase = parse_phase(case["phase"].as_str().unwrap());
                let result = note_video_ack(phase);
                match parse_verdict(case["verdict"].as_str().unwrap()) {
                    ExpectedVerdict::Accept { next, .. } => assert_eq!(result, Ok(next), "{name}"),
                    ExpectedVerdict::Error(code) => assert_eq!(result, Err(code), "{name}"),
                }
            }
        }
    }
}
