//! A0 measurement-harness receiver (window-trial rig).
//!
//! A disposable, single-connection TCP receiver used to trial
//! `flow_window_frames` against the real encoder + selected decoder backend
//! (`IMPLEMENTATION_PLAN_V11.md` §12 "A0", §7 flow control, §13 "Harness"
//! gate). It deliberately does NOT depend on the `protocol` crate: it parses
//! the 32-byte v3 frame record inline, exactly per `docs/WIRE.md` §4, and
//! acks with a tiny private 12-byte record (see `build_ack` below for how
//! that differs from the real system).
//!
//! This rig makes no ordering/windowing decisions of its own — the sender
//! is assumed to enforce the window; the receiver only validates, decodes,
//! and acks per contract (exact-once acceptance + drain-to-`Again` before
//! ack; drain `Error` ⇒ teardown, no ack — `IMPLEMENTATION_PLAN_V11.md` §7).
//! Submission and drain go through the shared `backend_construct`
//! retain-drain-resubmit engine (`A00_REMEDIATION_PLAN.md` §3 D2 / §5 R6),
//! so this binary no longer duplicates that state machine locally.
//!
//! The pass predicate is hardened per §5 R3a ("Receiver pass predicate")
//! and extracted into [`verdict::VerdictInputs::evaluate`] — a pure
//! function, unit-tested per term, mirroring the Mac-side `HarnessVerdict`
//! pattern — so it drives both the JSON report's `pass` field and this
//! process's exit code identically.

mod verdict;

use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Instant;

// ---------------------------------------------------------------------
// Backend selection — CLI-facing id only. The actual native construction
// sequence (docs/WIRE.md §7) lives in the shared `backend-construct` crate
// (`backend_construct::open_decoder`) so this rig and `decoder-experiment`
// open byte-identical decoders; this enum stays local because each binary's
// `--backend` flag/error wording is its own CLI surface.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    CuvidLowDelay,
    Sw1LowDelay,
}

impl Backend {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "cuvid-lowdelay" => Ok(Backend::CuvidLowDelay),
            "sw1-lowdelay" => Ok(Backend::Sw1LowDelay),
            other => bail!(
                "unknown --backend '{other}'; must be cuvid-lowdelay or sw1-lowdelay (the two closed WIRE.md §7 IDs)"
            ),
        }
    }

    fn id(&self) -> &'static str {
        match self {
            Backend::CuvidLowDelay => "cuvid-lowdelay",
            Backend::Sw1LowDelay => "sw1-lowdelay",
        }
    }
}

// ---------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------

struct Args {
    listen: SocketAddr,
    backend: Backend,
    json_out: Option<PathBuf>,
}

fn next_val(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    it.next().with_context(|| format!("{flag} requires a value"))
}

fn parse_args() -> Result<Args> {
    let mut listen: Option<SocketAddr> = None;
    let mut backend: Option<Backend> = None;
    let mut json_out: Option<PathBuf> = None;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--listen" => {
                let v = next_val(&mut it, "--listen")?;
                listen = Some(v.parse().with_context(|| format!("invalid --listen address '{v}'"))?);
            }
            "--backend" => backend = Some(Backend::parse(&next_val(&mut it, "--backend")?)?),
            "--json-out" => json_out = Some(PathBuf::from(next_val(&mut it, "--json-out")?)),
            other => bail!("unknown argument: {other}"),
        }
    }

    Ok(Args {
        listen: listen.context("--listen is required")?,
        backend: backend.context("--backend is required")?,
        json_out,
    })
}

// ---------------------------------------------------------------------
// docs/WIRE.md §4 frame record — 32-byte header, parsed inline.
// ---------------------------------------------------------------------

const FRAME_MAGIC: [u8; 2] = [0x56, 0x46];
/// Harness-local hard sanity cap. The real system validates against the
/// active profile's `max_record_bytes`; this disposable rig has no profile,
/// so it uses a fixed 8 MiB ceiling instead.
const MAX_RECORD_BYTES: u64 = 8 * 1024 * 1024;

struct FrameHeader {
    flags: u8,
    frame_ordinal: u64,
    capture_seq: u32,
    content_capture_ts_us: u64,
    payload_len: u32,
}

fn parse_header(buf: &[u8; 32]) -> std::result::Result<FrameHeader, String> {
    if buf[0] != FRAME_MAGIC[0] || buf[1] != FRAME_MAGIC[1] {
        return Err(format!(
            "bad magic: {:02x} {:02x} (want 56 46)",
            buf[0], buf[1]
        ));
    }
    let header_len = buf[2];
    if header_len != 32 {
        return Err(format!("bad headerLen: {header_len} (want 32)"));
    }
    let flags = buf[3];
    if flags & !0x01 != 0 {
        return Err(format!("unknown flag bits set: {flags:#04x}"));
    }
    let frame_ordinal = u64::from_le_bytes(buf[4..12].try_into().unwrap());
    if frame_ordinal == 0 || frame_ordinal > i64::MAX as u64 {
        return Err(format!(
            "frameOrdinal {frame_ordinal} out of domain 1..i64::MAX"
        ));
    }
    let capture_seq = u32::from_le_bytes(buf[12..16].try_into().unwrap());
    let content_capture_ts_us = u64::from_le_bytes(buf[16..24].try_into().unwrap());
    let reserved = u32::from_le_bytes(buf[24..28].try_into().unwrap());
    if reserved != 0 {
        return Err(format!("reserved field nonzero: {reserved}"));
    }
    let payload_len = u32::from_le_bytes(buf[28..32].try_into().unwrap());

    // Checked, widened arithmetic before allocating the payload.
    let total: u64 = (header_len as u64)
        .checked_add(payload_len as u64)
        .ok_or_else(|| "headerLen+payloadLen overflowed u64".to_string())?;
    if total > MAX_RECORD_BYTES {
        return Err(format!(
            "record cap exceeded: headerLen+payloadLen={total} > {MAX_RECORD_BYTES} (8 MiB hard sanity cap)"
        ));
    }

    Ok(FrameHeader {
        flags,
        frame_ordinal,
        capture_seq,
        content_capture_ts_us,
        payload_len,
    })
}

/// This rig's private ACK framing: magic "AK" + u16 reserved(0) +
/// u64 LE frame_ordinal, written back on the same video TCP socket.
///
/// This is NOT the production wire format. In the real system `FrameAck` is
/// a protobuf `Envelope` payload sent on the *control* TCP connection
/// (`docs/WIRE.md` §1 direction table; `proto/control_v3.proto`), separate
/// from the video socket this rig acks on. This harness has no control
/// channel, so it acks inline here purely to measure receive→ack latency
/// and drive the A0 window trial.
fn build_ack(frame_ordinal: u64) -> [u8; 12] {
    let mut buf = [0u8; 12];
    buf[0] = 0x41; // 'A'
    buf[1] = 0x4B; // 'K'
    // buf[2..4] reserved = 0 (already zeroed)
    buf[4..12].copy_from_slice(&frame_ordinal.to_le_bytes());
    buf
}

// ---------------------------------------------------------------------
// Decode + drain via the shared backend_construct engine; ordinal-fidelity
// bookkeeping (A00_REMEDIATION_PLAN.md §5 R3a's duplicates/reorders/skips
// terms — mirrors decoder-experiment's classification).
// ---------------------------------------------------------------------

struct RunState {
    emitted: u64,
    unknown_pts: u64,
    duplicates: u64,
    reorders: u64,
    skips: u64,
    expected_next: i64,
    seen: std::collections::HashSet<i64>,
}

impl RunState {
    fn new() -> Self {
        RunState {
            emitted: 0,
            unknown_pts: 0,
            duplicates: 0,
            reorders: 0,
            skips: 0,
            expected_next: 1,
            seen: std::collections::HashSet::new(),
        }
    }
}

/// Classifies one recovered ordinal (`backend_construct` has already run it
/// through the hw transfer when applicable) against the expected
/// strictly-increasing sequence. A duplicate is an ordinal seen before; a
/// skip is a higher ordinal arriving while a lower expected one is still
/// missing; a reorder is anything else out of place (not a repeat, not
/// ahead of schedule).
fn record_emission(recovered_ordinal: Option<i64>, state: &mut RunState) {
    state.emitted += 1;
    match recovered_ordinal {
        None => state.unknown_pts += 1,
        Some(got) => {
            if got == state.expected_next {
                state.expected_next += 1;
            } else if state.seen.contains(&got) {
                state.duplicates += 1;
            } else if got > state.expected_next {
                state.skips += 1;
                state.expected_next = got + 1;
            } else {
                state.reorders += 1;
                state.expected_next = got + 1;
            }
            state.seen.insert(got);
        }
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (p / 100.0 * sorted.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

fn write_report(value: &serde_json::Value, json_out: Option<&PathBuf>) -> Result<()> {
    let pretty = serde_json::to_string_pretty(value)?;
    println!("{pretty}");
    if let Some(path) = json_out {
        std::fs::write(path, format!("{pretty}\n"))
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// Report schema version. Bumped from the pre-hardening (unversioned)
/// report to 2 with this predicate-hardening pass (A00_REMEDIATION_PLAN.md
/// §5 R3a) — every new field below (`duplicates`, `reorders`, `skips`,
/// `ack_order_violations`, `protocol_errors`, `fatal_decoder_errors`,
/// `clean_eof_tail_drain`, `outstanding_at_exit`) is additive alongside the
/// original fields.
const REPORT_V: u32 = 2;

fn init_failure_report(args: &Args, failing_call: &str, detail: &str) -> serde_json::Value {
    serde_json::json!({
        "report_v": REPORT_V,
        "backend": args.backend.id(),
        "listen": args.listen.to_string(),
        "peer": null,
        "pass": false,
        "frames_received": 0,
        "frames_accepted": 0,
        "frames_emitted": 0,
        "frames_acked": 0,
        "unknown_pts": 0,
        "duplicates": 0,
        "reorders": 0,
        "skips": 0,
        "ack_order_violations": 0,
        "protocol_errors": 0,
        "fatal_decoder_errors": 0,
        "clean_eof_tail_drain": false,
        "outstanding_at_exit": 0,
        "max_decode_lag_frames": 0,
        "receive_to_ack_latency_ms": { "p50": 0.0, "p95": 0.0, "max": 0.0 },
        "eagain_retries": 0,
        "torn_down": true,
        "fail_reasons": [format!("{failing_call} failed: {detail}")],
        "error_code": "REQUIRED_NATIVE_API",
        "failing_call": failing_call,
    })
}

/// Everything one connection produced, for report-building in `main`.
struct ConnectionResult {
    frames_received: u64,
    frames_accepted: u64,
    frames_acked: u64,
    state: RunState,
    max_decode_lag: i64,
    latencies_ms: Vec<f64>,
    torn_down: bool,
    fail_reasons: Vec<String>,
    eagain_retries: u64,
    ack_order_violations: u64,
    protocol_errors: u64,
    fatal_decoder_errors: u64,
    outstanding_at_exit: u64,
    /// True iff the TCP stream closed cleanly at a record boundary *and*
    /// the subsequent decoder `flush_tail` succeeded — the real-backend
    /// EOF/tail-drain evidence A00_REMEDIATION_PLAN.md §3 D2 requires,
    /// applied to the harness rig's own predicate (§5 R3a).
    clean_eof_tail_drain: bool,
}

fn handle_connection(
    mut stream: TcpStream,
    handle: &mut backend_construct::DecoderHandle,
) -> ConnectionResult {
    const MAX_EAGAIN_CYCLES: u32 = 64;

    let mut frames_received: u64 = 0;
    let mut frames_accepted: u64 = 0;
    let mut frames_acked: u64 = 0;
    let mut max_decode_lag: i64 = 0;
    let mut latencies_ms: Vec<f64> = Vec::new();
    let mut fail_reasons: Vec<String> = Vec::new();
    let mut torn_down = false;
    let mut eagain_retries: u64 = 0;
    let mut ack_order_violations: u64 = 0;
    let mut protocol_errors: u64 = 0;
    let mut fatal_decoder_errors: u64 = 0;
    let mut outstanding: u64 = 0;
    let mut last_acked_ordinal: Option<u64> = None;
    let mut clean_close = false;

    let mut state = RunState::new();
    let mut header_buf = [0u8; 32];

    loop {
        match stream.read_exact(&mut header_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                log::info!("clean EOF at record boundary after {frames_received} record(s)");
                clean_close = true;
                break;
            }
            Err(e) => {
                let msg = format!("header read error: {e}");
                log::error!("teardown: {msg}");
                fail_reasons.push(msg);
                torn_down = true;
                break;
            }
        }

        let header = match parse_header(&header_buf) {
            Ok(h) => h,
            Err(reason) => {
                let msg = format!("PROTOCOL_VIOLATION parsing header: {reason}");
                log::error!("teardown: {msg}");
                fail_reasons.push(msg);
                protocol_errors += 1;
                torn_down = true;
                break;
            }
        };

        let mut payload = vec![0u8; header.payload_len as usize];
        if let Err(e) = stream.read_exact(&mut payload) {
            let msg = format!(
                "payload read error (ordinal {}, {} bytes): {e}",
                header.frame_ordinal, header.payload_len
            );
            log::error!("teardown: {msg}");
            fail_reasons.push(msg);
            torn_down = true;
            break;
        }
        let receive_complete = Instant::now();
        frames_received += 1;
        log::debug!(
            "ordinal {} received: {} bytes, flags={:#04x}, captureSeq={}, contentCaptureTs_us={}",
            header.frame_ordinal,
            header.payload_len,
            header.flags,
            header.capture_seq,
            header.content_capture_ts_us
        );

        // ACK-order self-check: the ordinal we're about to accept must
        // strictly increase past the last one we acked (symmetric to the
        // sender's own oldest-outstanding-ordinal validation on the ACKs it
        // reads back — §9.1's `ack_order_violation`). A violation means the
        // sender or wire is confused, not just this frame — fail-closed.
        if let Some(prev) = last_acked_ordinal {
            if header.frame_ordinal <= prev {
                let msg = format!(
                    "ACK-order violation: ordinal {} did not increase past last-acked {prev}",
                    header.frame_ordinal
                );
                log::error!("teardown: {msg}");
                fail_reasons.push(msg);
                ack_order_violations += 1;
                torn_down = true;
                break;
            }
        }

        let ord_i64 = header.frame_ordinal as i64;
        let record = match backend_construct::submit_with_retry(handle, ord_i64, &payload, MAX_EAGAIN_CYCLES) {
            Ok(r) => r,
            Err(reason) => {
                let msg = format!("teardown at ordinal {}: {reason}", header.frame_ordinal);
                log::error!("{msg}");
                fail_reasons.push(msg);
                fatal_decoder_errors += 1;
                torn_down = true;
                break;
            }
        };
        frames_accepted += 1;
        outstanding += 1;
        for attempt in &record.attempts {
            if attempt.again {
                eagain_retries += 1;
            }
        }
        for drain in &record.drains {
            for &ord in drain {
                record_emission(ord, &mut state);
            }
        }
        max_decode_lag = max_decode_lag.max(frames_accepted as i64 - state.emitted as i64);

        match backend_construct::drain_fully(handle) {
            Ok(drain) => {
                for ord in drain {
                    record_emission(ord, &mut state);
                }
                // Exact-once acceptance + drain-to-Again/Eof done — ack now.
                let ack = build_ack(header.frame_ordinal);
                if let Err(e) = stream.write_all(&ack) {
                    let msg = format!("ACK write failed at ordinal {}: {e}", header.frame_ordinal);
                    log::error!("teardown: {msg}");
                    fail_reasons.push(msg);
                    torn_down = true;
                    break;
                }
                let ack_done = Instant::now();
                frames_acked += 1;
                outstanding -= 1;
                last_acked_ordinal = Some(header.frame_ordinal);
                let latency_ms = ack_done.duration_since(receive_complete).as_secs_f64() * 1000.0;
                latencies_ms.push(latency_ms);
                log::debug!(
                    "ordinal {} acked, receive->ack {:.3} ms",
                    header.frame_ordinal,
                    latency_ms
                );
            }
            Err(reason) => {
                // drain Error ⇒ teardown, no ACK.
                let msg = format!("teardown draining ordinal {}: {reason}", header.frame_ordinal);
                log::error!("{msg}");
                fail_reasons.push(msg);
                fatal_decoder_errors += 1;
                torn_down = true;
                break;
            }
        }
    }

    // Clean EOF/tail drain (A00_REMEDIATION_PLAN.md §3 D2 / §5 R3a): only
    // attempted when the stream itself closed cleanly. The TCP connection
    // is already gone by this point, so any frames recovered here can
    // never be individually acked — `frames_acked` stays a pure per-record
    // protocol counter, while `frames_emitted` (via `state.emitted`) grows
    // to catch up with `frames_accepted`, which is exactly what the
    // emitted==submitted predicate term needs.
    let mut clean_eof_tail_drain = false;
    if clean_close {
        match backend_construct::flush_tail(handle, MAX_EAGAIN_CYCLES) {
            Ok(tail) => {
                eagain_retries += tail.eagain_count as u64;
                for ord in tail.recovered {
                    record_emission(ord, &mut state);
                }
                clean_eof_tail_drain = true;
            }
            Err(reason) => {
                let msg = format!("tail flush error: {reason}");
                log::error!("{msg}");
                fail_reasons.push(msg);
                fatal_decoder_errors += 1;
            }
        }
    }

    ConnectionResult {
        frames_received,
        frames_accepted,
        frames_acked,
        state,
        max_decode_lag,
        latencies_ms,
        torn_down,
        fail_reasons,
        eagain_retries,
        ack_order_violations,
        protocol_errors,
        fatal_decoder_errors,
        outstanding_at_exit: outstanding,
        clean_eof_tail_drain,
    }
}

fn main() -> Result<()> {
    env_logger::init();
    let args = parse_args()?;

    ffmpeg_next::init().context("ffmpeg_next::init")?;

    log::info!(
        "harness-receiver: backend={} listen={}",
        args.backend.id(),
        args.listen
    );

    let mut handle = match backend_construct::open_decoder(args.backend.id()) {
        Ok(h) => h,
        Err(e) => {
            let (failing_call, detail) = match e.downcast_ref::<backend_construct::BackendOpenError>() {
                Some(be) => (be.failing_call.clone(), be.detail.clone()),
                None => ("open_decoder".to_string(), e.to_string()),
            };
            log::error!("backend init failed: {failing_call}: {detail}");
            let report = init_failure_report(&args, &failing_call, &detail);
            write_report(&report, args.json_out.as_ref())?;
            std::process::exit(1);
        }
    };

    let listener = match TcpListener::bind(args.listen) {
        Ok(l) => l,
        Err(e) => {
            log::error!("bind failed: {e}");
            let report = init_failure_report(&args, "TcpListener::bind", &e.to_string());
            write_report(&report, args.json_out.as_ref())?;
            std::process::exit(1);
        }
    };

    log::info!("listening on {}, awaiting the one trial connection", args.listen);
    let (stream, peer) = match listener.accept() {
        Ok(v) => v,
        Err(e) => {
            log::error!("accept failed: {e}");
            let report = init_failure_report(&args, "TcpListener::accept", &e.to_string());
            write_report(&report, args.json_out.as_ref())?;
            std::process::exit(1);
        }
    };
    // Exactly one connection is ever accepted; stop listening immediately.
    drop(listener);
    log::info!("accepted connection from {peer}");

    if let Err(e) = stream.set_nodelay(true) {
        log::warn!("set_nodelay failed (continuing anyway): {e}");
    }

    let result = handle_connection(stream, &mut handle);

    let mut sorted_latencies = result.latencies_ms.clone();
    sorted_latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let receive_to_ack_latency_ms = serde_json::json!({
        "p50": percentile(&sorted_latencies, 50.0),
        "p95": percentile(&sorted_latencies, 95.0),
        "max": sorted_latencies.last().copied().unwrap_or(0.0),
    });

    // Every accepted record is submitted to the decoder exactly once before
    // acceptance is counted (no separate "submitted but not yet accepted"
    // state in this rig's synchronous per-record loop), so
    // frames_accepted *is* the "submitted" term the predicate compares
    // frames_emitted against.
    let verdict_inputs = verdict::VerdictInputs {
        frames_accepted: result.frames_accepted,
        frames_acked: result.frames_acked,
        frames_emitted: result.state.emitted,
        frames_submitted: result.frames_accepted,
        unknown_pts: result.state.unknown_pts,
        duplicates: result.state.duplicates,
        reorders: result.state.reorders,
        skips: result.state.skips,
        ack_order_violations: result.ack_order_violations,
        protocol_errors: result.protocol_errors,
        fatal_decoder_errors: result.fatal_decoder_errors,
        clean_eof_tail_drain: result.clean_eof_tail_drain,
        outstanding_at_exit: result.outstanding_at_exit,
    };
    let pass = verdict_inputs.evaluate();

    let report = serde_json::json!({
        "report_v": REPORT_V,
        "backend": args.backend.id(),
        "listen": args.listen.to_string(),
        "peer": peer.to_string(),
        "pass": pass,
        "frames_received": result.frames_received,
        "frames_accepted": result.frames_accepted,
        "frames_emitted": result.state.emitted,
        "frames_acked": result.frames_acked,
        "unknown_pts": result.state.unknown_pts,
        "duplicates": result.state.duplicates,
        "reorders": result.state.reorders,
        "skips": result.state.skips,
        "ack_order_violations": result.ack_order_violations,
        "protocol_errors": result.protocol_errors,
        "fatal_decoder_errors": result.fatal_decoder_errors,
        "clean_eof_tail_drain": result.clean_eof_tail_drain,
        "outstanding_at_exit": result.outstanding_at_exit,
        "max_decode_lag_frames": result.max_decode_lag,
        "receive_to_ack_latency_ms": receive_to_ack_latency_ms,
        "eagain_retries": result.eagain_retries,
        "torn_down": result.torn_down,
        "fail_reasons": result.fail_reasons,
    });

    log::info!(
        "harness-receiver done: pass={pass} received={} accepted={} emitted={} acked={}",
        result.frames_received,
        result.frames_accepted,
        result.state.emitted,
        result.frames_acked
    );

    write_report(&report, args.json_out.as_ref())?;

    if pass {
        Ok(())
    } else {
        std::process::exit(1);
    }
}
