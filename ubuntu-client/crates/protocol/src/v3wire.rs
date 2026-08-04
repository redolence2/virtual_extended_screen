//! RESC protocol v3 fixed-binary wire parsers, comparators, scroll
//! transform, and Annex-B NAL verification.
//!
//! Normative sources: `docs/WIRE.md` §2 (video handshake), §4 (frame
//! records), §5 (UDP records + sequence comparator), §6 (input/cursor
//! semantics, scroll); `IMPLEMENTATION_PLAN_V11.md` §4 (`FatalCode` +
//! failure classes), §5 (wire layouts), §6 (host pipeline RA verification);
//! `CONTRACT_ERRATA.md` ERR-04 (exact scroll injection) and ERR-05 (UDP
//! records carry no reserved field).
//!
//! Pure std, no I/O: every parser here takes an already-received `&[u8]`
//! (or, for the frame header, a fixed-size `&[u8; 32]`) and returns a parsed
//! record or a [`WireError`]. Callers own the socket/file I/O.

use crate::resc_v3;

// ===========================================================================
// Wire error
// ===========================================================================

/// Classification per `proto/fixtures/README.md`: every malformed Stage-1
/// fixture except the frame-cap overflow is a `ProtocolViolation`; the
/// overflow case is the sole `RecordCapViolation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    ProtocolViolation(&'static str),
    RecordCapViolation { total: u64, cap: u64 },
}

// ===========================================================================
// Little-endian field readers (offsets per the docs/WIRE.md tables below).
// Mirrors `tools/gen_fixtures.py`'s `u16le`/`u32le`/`u64le`/`i32le`/`f32le`
// naming so each call site reads like the WIRE.md row it implements.
// ===========================================================================

fn u16le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn u32le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn u64le(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        b[off],
        b[off + 1],
        b[off + 2],
        b[off + 3],
        b[off + 4],
        b[off + 5],
        b[off + 6],
        b[off + 7],
    ])
}

fn i32le(b: &[u8], off: usize) -> i32 {
    i32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn f32le(b: &[u8], off: usize) -> f32 {
    f32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

// ===========================================================================
// 1a. Video handshake — VideoHello / VideoHelloAck (docs/WIRE.md §2)
// ===========================================================================

const VIDEO_HELLO_MAGIC: [u8; 4] = *b"RSCV";
const VIDEO_HELLO_ACK_MAGIC: [u8; 4] = *b"RSCA";

/// Parsed `VideoHello` (docs/WIRE.md §2), 32 B, host->client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoHello {
    pub session_run_id: u64,
    pub profile_hash: [u8; 8],
}

/// `parse_video_hello`: exactly 32 B; magic `52 53 43 56`; `ver == 3`;
/// reserved `u8 == 0`; `len` field `== 32`; reserved `u64 == 0`.
pub fn parse_video_hello(buf: &[u8]) -> Result<VideoHello, WireError> {
    if buf.len() != 32 {
        return Err(WireError::ProtocolViolation("video_hello: length must be 32 B"));
    }
    if !buf.starts_with(&VIDEO_HELLO_MAGIC) {
        return Err(WireError::ProtocolViolation("video_hello: bad magic"));
    }
    if buf[4] != 3 {
        return Err(WireError::ProtocolViolation("video_hello: ver must be 3"));
    }
    if buf[5] != 0 {
        return Err(WireError::ProtocolViolation("video_hello: reserved u8 must be 0"));
    }
    if u16le(buf, 6) != 32 {
        return Err(WireError::ProtocolViolation("video_hello: len field must be 32"));
    }
    let session_run_id = u64le(buf, 8);
    let mut profile_hash = [0u8; 8];
    profile_hash.copy_from_slice(&buf[16..24]);
    if u64le(buf, 24) != 0 {
        return Err(WireError::ProtocolViolation("video_hello: reserved u64 must be 0"));
    }
    Ok(VideoHello { session_run_id, profile_hash })
}

/// `VideoHelloAck` status byte (docs/WIRE.md §2 "Ack status bytes
/// (frozen)"). Any byte outside 0..=3 is a `ProtocolViolation` at the
/// parser, not a fifth variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckStatus {
    Ok,
    Mismatch,
    Busy,
    Internal,
}

/// Parsed `VideoHelloAck` (docs/WIRE.md §2), 16 B, client->host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoHelloAck {
    pub status: AckStatus,
    pub session_run_id: u64,
}

/// `parse_video_hello_ack`: exactly 16 B; magic `52 53 43 41`; `ver == 3`;
/// `len == 16`; status must be `0..=3` (else `ProtocolViolation` — the WIRE
/// status-byte rule).
pub fn parse_video_hello_ack(buf: &[u8]) -> Result<VideoHelloAck, WireError> {
    if buf.len() != 16 {
        return Err(WireError::ProtocolViolation("video_hello_ack: length must be 16 B"));
    }
    if !buf.starts_with(&VIDEO_HELLO_ACK_MAGIC) {
        return Err(WireError::ProtocolViolation("video_hello_ack: bad magic"));
    }
    if buf[4] != 3 {
        return Err(WireError::ProtocolViolation("video_hello_ack: ver must be 3"));
    }
    if u16le(buf, 6) != 16 {
        return Err(WireError::ProtocolViolation("video_hello_ack: len field must be 16"));
    }
    let status = match buf[5] {
        0 => AckStatus::Ok,
        1 => AckStatus::Mismatch,
        2 => AckStatus::Busy,
        3 => AckStatus::Internal,
        _ => return Err(WireError::ProtocolViolation("video_hello_ack: unknown status byte")),
    };
    let session_run_id = u64le(buf, 8);
    Ok(VideoHelloAck { status, session_run_id })
}

// ===========================================================================
// 1b. Frame record header (docs/WIRE.md §4)
// ===========================================================================

const FRAME_HEADER_MAGIC: [u8; 2] = *b"VF";
const KEYFRAME_CLAIM_BIT: u8 = 0x01;

/// Parsed frame-record header (docs/WIRE.md §4), 32 B. The payload itself
/// (`payload_len` bytes of Annex-B HEVC AU) is not part of this type; the
/// caller reads it separately once the cap check below has passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub keyframe_claim: bool,
    pub frame_ordinal: u64,
    pub capture_seq: u32,
    pub content_capture_ts_us: u64,
    pub payload_len: u32,
}

/// `parse_frame_header`: magic `56 46`; `headerLen == 32`; `flags`: any bit
/// other than bit0 (keyframe-claim) set ⇒ `ProtocolViolation`;
/// `frameOrdinal` in `1..=i64::MAX` (as `u64`) else `ProtocolViolation`;
/// reserved `u32 == 0`; `total = 32u64 + payload_len as u64` (widened,
/// checked) `> max_record_bytes` ⇒ `RecordCapViolation{total, cap}`.
pub fn parse_frame_header(
    buf: &[u8; 32],
    max_record_bytes: u64,
) -> Result<FrameHeader, WireError> {
    if !buf.starts_with(&FRAME_HEADER_MAGIC) {
        return Err(WireError::ProtocolViolation("frame_header: bad magic"));
    }
    if buf[2] != 32 {
        return Err(WireError::ProtocolViolation("frame_header: headerLen must be 32"));
    }
    let flags = buf[3];
    if flags & !KEYFRAME_CLAIM_BIT != 0 {
        return Err(WireError::ProtocolViolation("frame_header: unknown flag bit set"));
    }
    let keyframe_claim = flags & KEYFRAME_CLAIM_BIT != 0;

    let frame_ordinal = u64le(buf, 4);
    const FRAME_ORDINAL_MAX: u64 = i64::MAX as u64;
    if !(1..=FRAME_ORDINAL_MAX).contains(&frame_ordinal) {
        return Err(WireError::ProtocolViolation("frame_header: frameOrdinal out of domain"));
    }

    let capture_seq = u32le(buf, 12);
    let content_capture_ts_us = u64le(buf, 16);

    if u32le(buf, 24) != 0 {
        return Err(WireError::ProtocolViolation("frame_header: reserved u32 must be 0"));
    }

    let payload_len = u32le(buf, 28);
    // Both operands are u64 before the add, so a u32 payload_len (max
    // 0xFFFF_FFFF) can never make this overflow u64 — but we still route it
    // through checked_add per the WIRE.md "checked, widened arithmetic" rule
    // rather than relying on that headroom argument at the call site.
    let total = 32u64
        .checked_add(payload_len as u64)
        .expect("32 + u32-range payload_len cannot overflow u64");
    if total > max_record_bytes {
        return Err(WireError::RecordCapViolation { total, cap: max_record_bytes });
    }

    Ok(FrameHeader {
        keyframe_claim,
        frame_ordinal,
        capture_seq,
        content_capture_ts_us,
        payload_len,
    })
}

// ===========================================================================
// 1c. UDP records — Move / Cursor (docs/WIRE.md §5; CONTRACT_ERRATA.md
//     ERR-05: neither UDP layout has a reserved field)
// ===========================================================================

const UDP_PREFIX_MAGIC: [u8; 4] = *b"RESC";
const UDP_TYPE_CURSOR: u8 = 1;
const UDP_TYPE_MOVE: u8 = 2;

/// Parsed `Move` UDP record (docs/WIRE.md §5), 26 B, client->host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveEvent {
    pub session_run_id: u64,
    pub seq: u32,
    pub x: i32,
    pub y: i32,
}

/// `parse_move`: exactly 26 B; prefix magic `52 45 53 43`, `ver == 3`,
/// `type == 2`. Exact-length rule: any other length ⇒ `ProtocolViolation`
/// (ERR-05 — no reserved field exists in UDP records; none is checked here).
pub fn parse_move(buf: &[u8]) -> Result<MoveEvent, WireError> {
    if buf.len() != 26 {
        return Err(WireError::ProtocolViolation("move: length must be 26 B"));
    }
    if !buf.starts_with(&UDP_PREFIX_MAGIC) {
        return Err(WireError::ProtocolViolation("move: bad magic"));
    }
    if buf[4] != 3 {
        return Err(WireError::ProtocolViolation("move: ver must be 3"));
    }
    if buf[5] != UDP_TYPE_MOVE {
        return Err(WireError::ProtocolViolation("move: type must be 2"));
    }
    let session_run_id = u64le(buf, 6);
    let seq = u32le(buf, 14);
    let x = i32le(buf, 18);
    let y = i32le(buf, 22);
    Ok(MoveEvent { session_run_id, seq, x, y })
}

/// Parsed `Cursor` UDP record (docs/WIRE.md §5), 43 B, host->client. Named
/// with a `3` suffix to disambiguate from the legacy v1
/// [`crate::binary::CursorUpdate`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CursorUpdate3 {
    pub session_run_id: u64,
    pub seq: u32,
    pub timestamp_us: u64,
    pub x_px: i32,
    pub y_px: i32,
    pub shape_id: u8,
    pub hotspot_x_px: u16,
    pub hotspot_y_px: u16,
    pub cursor_scale: f32,
}

/// `parse_cursor`: exactly 43 B; `type == 1`; `shape_id` `0..=15` else
/// `ProtocolViolation`; `cursor_scale` must be finite and `> 0` else
/// `ProtocolViolation`. Exact-length rule as [`parse_move`] (ERR-05).
pub fn parse_cursor(buf: &[u8]) -> Result<CursorUpdate3, WireError> {
    if buf.len() != 43 {
        return Err(WireError::ProtocolViolation("cursor: length must be 43 B"));
    }
    if !buf.starts_with(&UDP_PREFIX_MAGIC) {
        return Err(WireError::ProtocolViolation("cursor: bad magic"));
    }
    if buf[4] != 3 {
        return Err(WireError::ProtocolViolation("cursor: ver must be 3"));
    }
    if buf[5] != UDP_TYPE_CURSOR {
        return Err(WireError::ProtocolViolation("cursor: type must be 1"));
    }
    let session_run_id = u64le(buf, 6);
    let seq = u32le(buf, 14);
    let timestamp_us = u64le(buf, 18);
    let x_px = i32le(buf, 26);
    let y_px = i32le(buf, 30);
    let shape_id = buf[34];
    if shape_id > 15 {
        return Err(WireError::ProtocolViolation("cursor: shape_id out of 0..=15"));
    }
    let hotspot_x_px = u16le(buf, 35);
    let hotspot_y_px = u16le(buf, 37);
    let cursor_scale = f32le(buf, 39);
    if !cursor_scale.is_finite() || cursor_scale <= 0.0 {
        return Err(WireError::ProtocolViolation("cursor: cursor_scale must be finite and > 0"));
    }
    Ok(CursorUpdate3 {
        session_run_id,
        seq,
        timestamp_us,
        x_px,
        y_px,
        shape_id,
        hotspot_x_px,
        hotspot_y_px,
        cursor_scale,
    })
}

// ===========================================================================
// 3. FatalCode -> FailureClass classification (IMPLEMENTATION_PLAN_V11.md §4)
// ===========================================================================

/// Failure class attached to each nonzero, known `FatalCode` (plan v11 §4
/// enum comments; `proto/fixtures/fatal_code_classes.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    Deterministic,
    Transient,
    Terminal,
}

/// Classify a raw `FatalCode` numeric. `None` for `0` (`FATAL_UNSPECIFIED`)
/// and any unknown numeric — the v10 rule: a `FatalReport` carrying 0 or an
/// unknown code is a `PROTOCOL_VIOLATION` at the caller, not a failure class
/// in its own right. Written as explicit per-code arms (rather than ranges
/// covering all of `1..=22`) because the class sequence is *not* uniform:
/// `21` (`PERMISSION_DENIED`) breaks back to deterministic and `22`
/// (`WATCHDOG_FIRST_RA`) back to transient after the `19..=20` terminal
/// pair — see `proto/fixtures/fatal_code_classes.json`.
pub fn classify(code: i32) -> Option<FailureClass> {
    use FailureClass::*;
    match code {
        1..=9 => Some(Deterministic),
        10..=18 => Some(Transient),
        19 | 20 => Some(Terminal),
        21 => Some(Deterministic),
        22 => Some(Transient),
        _ => None,
    }
}

// ===========================================================================
// 4. UDP sequence comparators (IMPLEMENTATION_PLAN_V11.md §9; docs/WIRE.md
//    §5 "Sequence comparator")
// ===========================================================================

/// `newer(a, b) <=> d != 0 && d < 2^31`, `d = (a - b) mod 2^32`. Governs
/// `seq` wraparound ordering/liveness for both move and cursor UDP records.
pub fn newer_u32(a: u32, b: u32) -> bool {
    let d = a.wrapping_sub(b);
    d != 0 && d < 0x8000_0000
}

/// Same comparator restricted to the low 24 bits — the T2 packed cursor
/// snapshot's `seq:u24` field (plan v11 §8: "newer <=> d != 0 && d < 2^23").
/// Inputs are masked to 24 bits first so a caller may pass either an
/// already-masked value or a full 32-bit sequence number.
pub fn newer_u24(a: u32, b: u32) -> bool {
    let a = a & 0xFF_FFFF;
    let b = b & 0xFF_FFFF;
    let d = a.wrapping_sub(b) & 0xFF_FFFF;
    d != 0 && d < 0x80_0000
}

// ===========================================================================
// 5. Scroll transform (CONTRACT_ERRATA.md ERR-04; docs/WIRE.md §6 "Scroll")
// ===========================================================================

/// ERR-04, verbatim: rotate the signed SDL steps (axis swap per §5.4),
/// multiply each by the 10-pixel quantum using widened checked arithmetic,
/// saturate to `i32`. All intermediate math happens in `i64` — including
/// the rotation's negation — so `dx == i32::MIN` never overflows on
/// negation (it would, in `i32`, since `i32::MIN` has no positive
/// counterpart) before the final saturating cast back to `i32`.
pub fn scroll_transform(dx: i32, dy: i32, rotated: bool) -> (i32, i32) {
    const QUANTUM: i64 = 10;
    let (x, y): (i64, i64) = if rotated {
        (-(dy as i64), dx as i64)
    } else {
        (dx as i64, dy as i64)
    };
    let out_dx = (x * QUANTUM).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    let out_dy = (y * QUANTUM).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    (out_dx, out_dy)
}

// ===========================================================================
// 6. Annex-B NAL scanning + RA verification (plan v11 §6 "Host pipeline" RA
//    verification; docs/WIRE.md §6 CRA-rejection note)
// ===========================================================================

/// HEVC NAL unit type constants relevant to RA verification (ITU-T H.265
/// Table 7-1). VPS/SPS/PPS are the three parameter-set types; 19/20 are the
/// two IDR types (RA-eligible); 21 is CRA — explicitly RA-rejected even
/// alongside a valid IDR (docs/WIRE.md §6).
const NAL_TYPE_VPS: u8 = 32;
const NAL_TYPE_SPS: u8 = 33;
const NAL_TYPE_PPS: u8 = 34;
const NAL_TYPE_IDR_W_RADL: u8 = 19;
const NAL_TYPE_IDR_N_LP: u8 = 20;
const NAL_TYPE_CRA: u8 = 21;

/// Summary of the NAL unit types seen while scanning one Annex-B byte
/// stream (typically one access unit).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NalSummary {
    pub types: Vec<u8>,
    pub has_vps: bool,
    pub has_sps: bool,
    pub has_pps: bool,
    pub has_idr: bool,
    pub has_cra: bool,
}

/// Scan Annex-B `data` for NAL units delimited by 3- or 4-byte start codes
/// (`00 00 01` / `00 00 00 01`), recording each unit's HEVC NAL type —
/// `(first_byte >> 1) & 0x3F` (ITU-T H.265 §7.3.1.2 `nal_unit_header`: the
/// type occupies bits 6..1 of the first byte).
pub fn scan_annexb(data: &[u8]) -> NalSummary {
    let mut summary = NalSummary::default();
    let mut i = 0usize;
    while i + 2 < data.len() {
        let is_short = data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1;
        let is_long =
            !is_short && i + 3 < data.len() && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1;
        if is_short || is_long {
            let nal_start = i + if is_short { 3 } else { 4 };
            if nal_start < data.len() {
                let nal_type = (data[nal_start] >> 1) & 0x3F;
                summary.types.push(nal_type);
                match nal_type {
                    NAL_TYPE_VPS => summary.has_vps = true,
                    NAL_TYPE_SPS => summary.has_sps = true,
                    NAL_TYPE_PPS => summary.has_pps = true,
                    NAL_TYPE_IDR_W_RADL | NAL_TYPE_IDR_N_LP => summary.has_idr = true,
                    NAL_TYPE_CRA => summary.has_cra = true,
                    _ => {}
                }
            }
            i = nal_start;
        } else {
            i += 1;
        }
    }
    summary
}

/// Session-first AU requirement (plan v11 §6: "session-first AU carries
/// VPS+SPS+PPS"; docs/WIRE.md §6: "RA iff NAL 19/20 (CRA rejected)"). A CRA
/// anywhere in the AU is rejected even alongside a present IDR — the CRA
/// check is unconditional, not merely a fallback when IDR is absent.
pub fn validate_session_first(s: &NalSummary) -> Result<(), &'static str> {
    if s.has_cra {
        return Err("CRA present in session-first AU (rejected — docs/WIRE.md §6)");
    }
    if !(s.has_vps && s.has_sps && s.has_pps) {
        return Err("session-first AU missing a required parameter set (VPS/SPS/PPS)");
    }
    if !s.has_idr {
        return Err("session-first AU missing an IDR (NAL 19/20)");
    }
    Ok(())
}

/// `claim == has_idr` — the frame-record header's keyframe-claim bit must
/// match whether the parsed AU actually contains an IDR NAL (plan v11 §6:
/// "header claim == parse").
pub fn keyframe_claim_matches(s: &NalSummary, claim: bool) -> bool {
    claim == s.has_idr
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// `proto/fixtures/<rel>` relative to this crate — same pattern as
    /// `diagnostics::profile`'s fixture tests.
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

    // -----------------------------------------------------------------
    // 1. Fixed-binary record parsers — fixture sweep (task group 1)
    // -----------------------------------------------------------------
    mod wire_records {
        use super::*;

        const RUN_ID: u64 = 0x1122334455667788;
        const PROFILE_HASH: [u8; 8] = [0x0c, 0xc2, 0x24, 0x96, 0x62, 0x88, 0x05, 0x97];
        /// docs/WIRE.md §9 placeholder profile's `max_record_bytes`.
        const PLACEHOLDER_MAX_RECORD_BYTES: u64 = 2_097_184;

        #[test]
        fn videohello_golden() {
            let buf = read_fixture("videohello.bin");
            let parsed = parse_video_hello(&buf).expect("videohello.bin must parse");
            assert_eq!(parsed.session_run_id, RUN_ID);
            assert_eq!(parsed.profile_hash, PROFILE_HASH);
        }

        #[test]
        fn videohelloack_golden_statuses() {
            let cases = [
                ("videohelloack_ok.bin", AckStatus::Ok),
                ("videohelloack_mismatch.bin", AckStatus::Mismatch),
                ("videohelloack_busy.bin", AckStatus::Busy),
                ("videohelloack_internal.bin", AckStatus::Internal),
            ];
            for (name, expected_status) in cases {
                let buf = read_fixture(name);
                let parsed =
                    parse_video_hello_ack(&buf).unwrap_or_else(|e| panic!("{name} must parse: {e:?}"));
                assert_eq!(parsed.status, expected_status, "{name}");
                assert_eq!(parsed.session_run_id, RUN_ID, "{name}");
            }
        }

        #[test]
        fn frame_header_min_golden() {
            let buf = read_fixture("frame_header_min.bin");
            let arr: [u8; 32] = buf.as_slice().try_into().expect("frame_header_min.bin must be 32 B");
            let parsed = parse_frame_header(&arr, PLACEHOLDER_MAX_RECORD_BYTES)
                .expect("frame_header_min.bin must parse");
            assert!(parsed.keyframe_claim);
            assert_eq!(parsed.frame_ordinal, 1);
            assert_eq!(parsed.capture_seq, 0);
            assert_eq!(parsed.content_capture_ts_us, 0);
            assert_eq!(parsed.payload_len, 0);
        }

        #[test]
        fn move_golden() {
            let buf = read_fixture("move.bin");
            let parsed = parse_move(&buf).expect("move.bin must parse");
            assert_eq!(parsed.session_run_id, RUN_ID);
            assert_eq!(parsed.seq, 1);
            assert_eq!(parsed.x, 100);
            assert_eq!(parsed.y, 200);
        }

        #[test]
        fn cursor_golden() {
            let buf = read_fixture("cursor.bin");
            let parsed = parse_cursor(&buf).expect("cursor.bin must parse");
            assert_eq!(parsed.session_run_id, RUN_ID);
            assert_eq!(parsed.seq, 1);
            assert_eq!(parsed.timestamp_us, 0);
            assert_eq!(parsed.x_px, 100);
            assert_eq!(parsed.y_px, 200);
            assert_eq!(parsed.shape_id, 0);
            assert_eq!(parsed.hotspot_x_px, 0);
            assert_eq!(parsed.hotspot_y_px, 0);
            assert_eq!(parsed.cursor_scale, 1.0);
        }

        // -- Malformed fixtures: classification per proto/fixtures/README.md --

        #[test]
        fn bad_magic_hello_is_protocol_violation() {
            let buf = read_fixture("malformed/bad_magic_hello.bin");
            assert!(matches!(parse_video_hello(&buf), Err(WireError::ProtocolViolation(_))));
        }

        #[test]
        fn bad_length_hello_is_protocol_violation() {
            let buf = read_fixture("malformed/bad_length_hello.bin");
            assert!(matches!(parse_video_hello(&buf), Err(WireError::ProtocolViolation(_))));
        }

        #[test]
        fn nonzero_reserved_hello_is_protocol_violation() {
            let buf = read_fixture("malformed/nonzero_reserved_hello.bin");
            assert!(matches!(parse_video_hello(&buf), Err(WireError::ProtocolViolation(_))));
        }

        #[test]
        fn unknown_status_ack_is_protocol_violation() {
            let buf = read_fixture("malformed/unknown_status_ack.bin");
            assert!(matches!(parse_video_hello_ack(&buf), Err(WireError::ProtocolViolation(_))));
        }

        #[test]
        fn unknown_flag_frame_is_protocol_violation() {
            let buf = read_fixture("malformed/unknown_flag_frame.bin");
            let arr: [u8; 32] = buf.as_slice().try_into().expect("fixture must be 32 B");
            assert!(matches!(
                parse_frame_header(&arr, PLACEHOLDER_MAX_RECORD_BYTES),
                Err(WireError::ProtocolViolation(_))
            ));
        }

        #[test]
        fn overflow_frame_is_record_cap_violation() {
            let buf = read_fixture("malformed/overflow_frame.bin");
            let arr: [u8; 32] = buf.as_slice().try_into().expect("fixture must be 32 B");
            match parse_frame_header(&arr, PLACEHOLDER_MAX_RECORD_BYTES) {
                Err(WireError::RecordCapViolation { total, cap }) => {
                    assert_eq!(total, 32u64 + 0xFFFF_FFFFu64);
                    assert_eq!(cap, PLACEHOLDER_MAX_RECORD_BYTES);
                }
                other => panic!("expected RecordCapViolation, got {other:?}"),
            }
        }

        #[test]
        fn short_move_is_protocol_violation() {
            let buf = read_fixture("malformed/short_move.bin");
            assert!(matches!(parse_move(&buf), Err(WireError::ProtocolViolation(_))));
        }

        #[test]
        fn long_cursor_is_protocol_violation() {
            let buf = read_fixture("malformed/long_cursor.bin");
            assert!(matches!(parse_cursor(&buf), Err(WireError::ProtocolViolation(_))));
        }
    }

    // -----------------------------------------------------------------
    // 2. Envelope round-trips (task group 2)
    // -----------------------------------------------------------------
    mod envelope_roundtrips {
        use super::*;
        use prost::Message;

        const RUN_ID: u64 = 72_623_859_790_382_856; // 0x0102030405060708
        const PROTOCOL_VERSION: u32 = 3;

        fn envelope_fixture(name: &str) -> Vec<u8> {
            read_fixture(&format!("envelopes/{name}"))
        }

        #[test]
        fn clock_ping_roundtrip() {
            let bytes = envelope_fixture("envelope_clock_ping.bin");
            let env = resc_v3::Envelope::decode(bytes.as_slice()).expect("must decode as Envelope");
            assert_eq!(env.session_run_id, RUN_ID);
            assert_eq!(env.protocol_version, PROTOCOL_VERSION);
            match &env.payload {
                Some(resc_v3::envelope::Payload::ClockPing(p)) => {
                    assert_eq!(p.t1_mono_us, 123_456_789);
                    assert_eq!(p.seq, 7);
                }
                other => panic!("expected ClockPing payload, got {other:?}"),
            }
            assert_eq!(
                env.encode_to_vec(),
                bytes,
                "re-encoded bytes must match the fixture exactly"
            );
        }

        #[test]
        fn fatal_report_roundtrip() {
            let bytes = envelope_fixture("envelope_fatal_report.bin");
            let env = resc_v3::Envelope::decode(bytes.as_slice()).expect("must decode as Envelope");
            assert_eq!(env.session_run_id, RUN_ID);
            assert_eq!(env.protocol_version, PROTOCOL_VERSION);
            match &env.payload {
                Some(resc_v3::envelope::Payload::FatalReport(p)) => {
                    assert_eq!(p.code, resc_v3::FatalCode::ProtocolViolation as i32);
                    assert_eq!(p.component, "test");
                    assert_eq!(p.summary, "unit");
                }
                other => panic!("expected FatalReport payload, got {other:?}"),
            }
            assert_eq!(
                env.encode_to_vec(),
                bytes,
                "re-encoded bytes must match the fixture exactly"
            );
        }

        #[test]
        fn frame_ack_roundtrip() {
            let bytes = envelope_fixture("envelope_frame_ack.bin");
            let env = resc_v3::Envelope::decode(bytes.as_slice()).expect("must decode as Envelope");
            assert_eq!(env.session_run_id, RUN_ID);
            assert_eq!(env.protocol_version, PROTOCOL_VERSION);
            match &env.payload {
                Some(resc_v3::envelope::Payload::FrameAck(p)) => {
                    assert_eq!(p.frame_ordinal, 42);
                }
                other => panic!("expected FrameAck payload, got {other:?}"),
            }
            assert_eq!(
                env.encode_to_vec(),
                bytes,
                "re-encoded bytes must match the fixture exactly"
            );
        }

        #[test]
        fn heartbeat_roundtrip() {
            let bytes = envelope_fixture("envelope_heartbeat.bin");
            let env = resc_v3::Envelope::decode(bytes.as_slice()).expect("must decode as Envelope");
            assert_eq!(env.session_run_id, RUN_ID);
            assert_eq!(env.protocol_version, PROTOCOL_VERSION);
            match &env.payload {
                Some(resc_v3::envelope::Payload::Heartbeat(p)) => {
                    assert_eq!(p.t_mono_us, 1_722_400_000_000_000);
                }
                other => panic!("expected Heartbeat payload, got {other:?}"),
            }
            assert_eq!(
                env.encode_to_vec(),
                bytes,
                "re-encoded bytes must match the fixture exactly"
            );
        }

        /// Exercises `envelopes_manifest.json` itself (not just the pinned
        /// constants duplicated into the tests above), so a future fixture
        /// regeneration that changes an expected value is caught even if
        /// the per-file tests aren't updated in lockstep.
        #[test]
        fn manifest_matches_pinned_expectations() {
            let m = read_fixture_json("envelopes/envelopes_manifest.json");
            assert_eq!(m["session_run_id"].as_u64().unwrap(), RUN_ID);
            assert_eq!(m["protocol_version"].as_u64().unwrap(), PROTOCOL_VERSION as u64);

            let files = m["files"].as_object().expect("files must be an object");
            assert_eq!(files.len(), 4, "expected exactly 4 envelope fixtures");

            assert_eq!(files["envelope_clock_ping.bin"]["t1_mono_us"], 123_456_789);
            assert_eq!(files["envelope_clock_ping.bin"]["seq"], 7);
            assert_eq!(files["envelope_fatal_report.bin"]["code"], 8);
            assert_eq!(files["envelope_fatal_report.bin"]["component"], "test");
            assert_eq!(files["envelope_fatal_report.bin"]["summary"], "unit");
            assert_eq!(files["envelope_frame_ack.bin"]["frame_ordinal"], 42);
            assert_eq!(files["envelope_heartbeat.bin"]["t_mono_us"], 1_722_400_000_000_000u64);
        }
    }

    // -----------------------------------------------------------------
    // 3. FatalCode classification (task group 3)
    // -----------------------------------------------------------------
    mod fatal_code_classification {
        use super::*;

        fn class_str(c: Option<FailureClass>) -> &'static str {
            match c {
                None => "unspecified",
                Some(FailureClass::Deterministic) => "deterministic",
                Some(FailureClass::Transient) => "transient",
                Some(FailureClass::Terminal) => "terminal",
            }
        }

        #[test]
        fn every_fatal_code_variant_and_the_json_agree() {
            let v = read_fixture_json("fatal_code_classes.json");
            let classes = v["classes"].as_object().expect("classes must be an object");

            // Direction A: every resc_v3::FatalCode variant must appear in
            // the json, and classify() must agree with the json's class for
            // it. Probe a generous numeric range via the prost-generated
            // TryFrom<i32> impl rather than hardcoding the variant count, so
            // a future added code is automatically covered.
            let mut variants_seen = 0;
            for code in 0..256i32 {
                if let Ok(variant) = resc_v3::FatalCode::try_from(code) {
                    variants_seen += 1;
                    let key = code.to_string();
                    let expected = classes
                        .get(&key)
                        .unwrap_or_else(|| {
                            panic!("FatalCode::{variant:?} ({code}) missing from fatal_code_classes.json")
                        })
                        .as_str()
                        .unwrap();
                    assert_eq!(class_str(classify(code)), expected, "code {code} ({variant:?})");
                }
            }
            assert_eq!(variants_seen, classes.len(), "FatalCode variant count vs json entry count");

            // Direction B: classify() must agree with the json for every
            // code listed there (including "0" => unspecified => None).
            for (key, expected) in classes {
                let code: i32 = key.parse().unwrap_or_else(|_| panic!("bad json key {key}"));
                let expected = expected.as_str().unwrap();
                assert_eq!(class_str(classify(code)), expected, "code {code}");
            }
        }

        #[test]
        fn unspecified_is_none() {
            assert_eq!(classify(0), None);
        }

        #[test]
        fn unknown_numeric_is_none() {
            assert_eq!(classify(99), None);
        }
    }

    // -----------------------------------------------------------------
    // 4. UDP sequence comparators (task group 4)
    // -----------------------------------------------------------------
    mod comparators {
        use super::*;

        #[test]
        fn u32_equal_is_not_newer() {
            assert!(!newer_u32(5, 5));
        }

        #[test]
        fn u32_forward_small_is_newer() {
            assert!(newer_u32(6, 5));
        }

        #[test]
        fn u32_wrap_forward_is_newer() {
            // a=5, b=0xFFFF_FFF0: d = (5 - 0xFFFF_FFF0) mod 2^32 = 21 < 2^31.
            assert!(newer_u32(5, 0xFFFF_FFF0));
        }

        #[test]
        fn u32_stale_reverse_is_not_newer() {
            assert!(!newer_u32(5, 6));
            assert!(!newer_u32(0xFFFF_FFF0, 5));
        }

        #[test]
        fn u32_half_range_boundary() {
            // d == 0x8000_0000 exactly => false.
            assert!(!newer_u32(0x8000_0000, 0));
            // d == 0x7FFF_FFFF => true.
            assert!(newer_u32(0x7FFF_FFFF, 0));
        }

        #[test]
        fn u24_equal_is_not_newer() {
            assert!(!newer_u24(5, 5));
        }

        #[test]
        fn u24_forward_small_is_newer() {
            assert!(newer_u24(6, 5));
        }

        #[test]
        fn u24_wrap_forward_is_newer() {
            // a=5, b=0xFF_FFF0: d = (5 - 0xFF_FFF0) mod 2^24 = 21 < 2^23.
            assert!(newer_u24(5, 0xFF_FFF0));
        }

        #[test]
        fn u24_stale_reverse_is_not_newer() {
            assert!(!newer_u24(5, 6));
            assert!(!newer_u24(0xFF_FFF0, 5));
        }

        #[test]
        fn u24_half_range_boundary() {
            // d == 0x80_0000 exactly => false.
            assert!(!newer_u24(0x80_0000, 0));
            // d == 0x7F_FFFF => true.
            assert!(newer_u24(0x7F_FFFF, 0));
        }

        #[test]
        fn u24_masks_high_bits_of_its_inputs() {
            // Only the low 24 bits participate; a caller passing a full
            // 32-bit seq must get the same answer as the pre-masked value.
            assert_eq!(newer_u24(0x0100_0005, 0x0100_0000), newer_u24(5, 0));
        }
    }

    // -----------------------------------------------------------------
    // 5. Scroll transform (task group 5)
    // -----------------------------------------------------------------
    mod scroll {
        use super::*;

        #[test]
        fn scroll_cases_fixture() {
            let v = read_fixture_json("scroll_cases.json");
            assert_eq!(v["quantum"].as_i64().unwrap(), 10);

            let cases = v["cases"].as_array().expect("cases must be an array");
            assert_eq!(cases.len(), 12, "expected 12 scroll reference cases");

            for case in cases {
                let name = case["name"].as_str().unwrap();
                let dx = case["dx"].as_i64().unwrap() as i32;
                let dy = case["dy"].as_i64().unwrap() as i32;
                let rotated = case["rotated"].as_bool().unwrap();
                let expected_dx = case["out_dx"].as_i64().unwrap() as i32;
                let expected_dy = case["out_dy"].as_i64().unwrap() as i32;

                let (out_dx, out_dy) = scroll_transform(dx, dy, rotated);
                assert_eq!(out_dx, expected_dx, "case {name}: out_dx");
                assert_eq!(out_dy, expected_dy, "case {name}: out_dy");
            }
        }
    }

    // -----------------------------------------------------------------
    // 6. Annex-B NAL scanning + RA verification (task group 6)
    // -----------------------------------------------------------------
    mod nal {
        use super::*;

        /// Build an Annex-B byte stream from `nal_types`: one NAL unit per
        /// type, each a single-byte header (`type << 1`, forbidden_zero_bit
        /// and the nuh_layer_id bit both 0) with no further payload,
        /// delimited by 4-byte start codes (`00 00 00 01`).
        fn annexb_4byte(nal_types: &[u8]) -> Vec<u8> {
            let mut out = Vec::new();
            for &t in nal_types {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.push(t << 1);
            }
            out
        }

        #[test]
        fn full_valid_set_idr_w_radl_passes() {
            let s = scan_annexb(&annexb_4byte(&[32, 33, 34, 19]));
            assert_eq!(s.types, vec![32, 33, 34, 19]);
            assert!(s.has_vps && s.has_sps && s.has_pps && s.has_idr && !s.has_cra);
            assert_eq!(validate_session_first(&s), Ok(()));
            assert!(keyframe_claim_matches(&s, true));
            assert!(!keyframe_claim_matches(&s, false));
        }

        #[test]
        fn full_valid_set_idr_n_lp_passes() {
            let s = scan_annexb(&annexb_4byte(&[32, 33, 34, 20]));
            assert_eq!(s.types, vec![32, 33, 34, 20]);
            assert!(s.has_vps && s.has_sps && s.has_pps && s.has_idr && !s.has_cra);
            assert_eq!(validate_session_first(&s), Ok(()));
            assert!(keyframe_claim_matches(&s, true));
        }

        #[test]
        fn missing_pps_fails() {
            let s = scan_annexb(&annexb_4byte(&[32, 33, 19])); // no 34 (PPS)
            assert!(s.has_vps && s.has_sps && !s.has_pps && s.has_idr);
            assert!(validate_session_first(&s).is_err());
        }

        #[test]
        fn cra_instead_of_idr_fails() {
            let s = scan_annexb(&annexb_4byte(&[32, 33, 34, 21])); // CRA, no IDR
            assert!(s.has_vps && s.has_sps && s.has_pps && s.has_cra && !s.has_idr);
            assert!(validate_session_first(&s).is_err());
        }

        #[test]
        fn cra_alongside_idr_fails() {
            // CRA present in the same AU as a valid IDR: still rejected
            // (docs/WIRE.md §6 CRA-rejection note).
            let s = scan_annexb(&annexb_4byte(&[32, 33, 34, 21, 19]));
            assert!(s.has_vps && s.has_sps && s.has_pps && s.has_cra && s.has_idr);
            assert!(validate_session_first(&s).is_err());
        }

        #[test]
        fn keyframe_claim_mismatch_both_directions() {
            let idr = scan_annexb(&annexb_4byte(&[32, 33, 34, 19]));
            assert!(!keyframe_claim_matches(&idr, false)); // claim false, has_idr true

            let non_idr = scan_annexb(&annexb_4byte(&[32, 33, 34]));
            assert!(!keyframe_claim_matches(&non_idr, true)); // claim true, has_idr false
            assert!(keyframe_claim_matches(&non_idr, false));
        }

        #[test]
        fn three_byte_start_code_parsing() {
            let mut data = Vec::new();
            for &t in &[32u8, 33, 34, 19] {
                data.extend_from_slice(&[0, 0, 1]); // 3-byte start code
                data.push(t << 1);
            }
            let s = scan_annexb(&data);
            assert_eq!(s.types, vec![32, 33, 34, 19]);
            assert_eq!(validate_session_first(&s), Ok(()));
        }

        #[test]
        fn empty_input() {
            let s = scan_annexb(&[]);
            assert_eq!(s, NalSummary::default());
            assert!(validate_session_first(&s).is_err());
        }
    }
}
