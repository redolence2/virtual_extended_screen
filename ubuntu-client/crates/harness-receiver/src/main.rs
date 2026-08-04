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
// Decode + drain (same EAGAIN/EOF/error discipline as decoder-experiment)
// ---------------------------------------------------------------------

struct RunState {
    emitted: u64,
    unknown_pts: u64,
}

impl RunState {
    fn new() -> Self {
        RunState {
            emitted: 0,
            unknown_pts: 0,
        }
    }
}

enum DrainStop {
    Again,
    Eof,
}

fn record_emission(
    frame: &ffmpeg_next::frame::Video,
    is_hw: bool,
    state: &mut RunState,
) -> std::result::Result<(), String> {
    if is_hw {
        backend_construct::transfer_hw_frame(frame)?;
    }

    state.emitted += 1;
    if backend_construct::recovered_ordinal(frame).is_none() {
        state.unknown_pts += 1;
    }
    Ok(())
}

fn drain_frames(
    decoder: &mut ffmpeg_next::decoder::Video,
    frame: &mut ffmpeg_next::frame::Video,
    is_hw: bool,
    state: &mut RunState,
) -> std::result::Result<DrainStop, String> {
    loop {
        match backend_construct::receive_one(decoder, frame)? {
            backend_construct::ReceiveOutcome::Frame => {
                record_emission(frame, is_hw, state)?;
            }
            backend_construct::ReceiveOutcome::Again => return Ok(DrainStop::Again),
            backend_construct::ReceiveOutcome::Eof => return Ok(DrainStop::Eof),
        }
    }
}

fn send_with_eagain_retry(
    decoder: &mut ffmpeg_next::decoder::Video,
    packet: &ffmpeg_next::Packet,
    frame: &mut ffmpeg_next::frame::Video,
    is_hw: bool,
    state: &mut RunState,
    eagain_retries: &mut u64,
) -> std::result::Result<(), String> {
    const MAX_EAGAIN_CYCLES: u32 = 64;
    let mut cycles = 0u32;
    loop {
        match decoder.send_packet(packet) {
            Ok(()) => return Ok(()),
            Err(ffmpeg_next::Error::Other { errno }) if errno == libc::EAGAIN => {
                *eagain_retries += 1;
                cycles += 1;
                if cycles > MAX_EAGAIN_CYCLES {
                    return Err(format!(
                        "send_packet EAGAIN did not converge after {cycles} drain cycles"
                    ));
                }
                drain_frames(decoder, frame, is_hw, state)?;
            }
            Err(e) => return Err(format!("send_packet fatal error: {e}")),
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

fn init_failure_report(
    args: &Args,
    failing_call: &str,
    detail: &str,
) -> serde_json::Value {
    serde_json::json!({
        "backend": args.backend.id(),
        "listen": args.listen.to_string(),
        "peer": null,
        "pass": false,
        "frames_received": 0,
        "frames_accepted": 0,
        "frames_emitted": 0,
        "frames_acked": 0,
        "unknown_pts": 0,
        "max_decode_lag_frames": 0,
        "receive_to_ack_latency_ms": { "p50": 0.0, "p95": 0.0, "max": 0.0 },
        "eagain_retries": 0,
        "torn_down": true,
        "fail_reasons": [format!("{failing_call} failed: {detail}")],
        "error_code": "REQUIRED_NATIVE_API",
        "failing_call": failing_call,
    })
}

fn handle_connection(
    mut stream: TcpStream,
    handle: &mut backend_construct::DecoderHandle,
    eagain_retries: &mut u64,
) -> (
    u64,           // frames_received
    u64,           // frames_accepted
    u64,           // frames_acked
    RunState,
    i64,           // max_decode_lag_frames
    Vec<f64>,      // receive->ack latencies (ms)
    bool,          // torn_down
    Vec<String>,   // fail_reasons
) {
    let mut frames_received: u64 = 0;
    let mut frames_accepted: u64 = 0;
    let mut frames_acked: u64 = 0;
    let mut max_decode_lag: i64 = 0;
    let mut latencies_ms: Vec<f64> = Vec::new();
    let mut fail_reasons: Vec<String> = Vec::new();
    let mut torn_down = false;

    let mut state = RunState::new();
    let mut frame = ffmpeg_next::frame::Video::empty();
    let is_hw = handle.is_hw;
    let mut header_buf = [0u8; 32];

    loop {
        match stream.read_exact(&mut header_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                log::info!("clean EOF at record boundary after {frames_received} record(s)");
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

        let mut packet = ffmpeg_next::Packet::copy(&payload);
        let ord_i64 = header.frame_ordinal as i64;
        packet.set_pts(Some(ord_i64));

        if let Err(reason) = send_with_eagain_retry(
            &mut handle.decoder,
            &packet,
            &mut frame,
            is_hw,
            &mut state,
            eagain_retries,
        ) {
            let msg = format!("teardown at ordinal {}: {reason}", header.frame_ordinal);
            log::error!("{msg}");
            fail_reasons.push(msg);
            torn_down = true;
            break;
        }
        frames_accepted += 1;
        max_decode_lag = max_decode_lag.max(frames_accepted as i64 - state.emitted as i64);

        match drain_frames(&mut handle.decoder, &mut frame, is_hw, &mut state) {
            Ok(_) => {
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
                torn_down = true;
                break;
            }
        }
    }

    (
        frames_received,
        frames_accepted,
        frames_acked,
        state,
        max_decode_lag,
        latencies_ms,
        torn_down,
        fail_reasons,
    )
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

    let mut eagain_retries: u64 = 0;
    let (
        frames_received,
        frames_accepted,
        frames_acked,
        state,
        max_decode_lag,
        latencies_ms,
        torn_down,
        fail_reasons,
    ) = handle_connection(stream, &mut handle, &mut eagain_retries);

    let mut sorted_latencies = latencies_ms.clone();
    sorted_latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let receive_to_ack_latency_ms = serde_json::json!({
        "p50": percentile(&sorted_latencies, 50.0),
        "p95": percentile(&sorted_latencies, 95.0),
        "max": sorted_latencies.last().copied().unwrap_or(0.0),
    });

    let pass = !torn_down && fail_reasons.is_empty();

    let report = serde_json::json!({
        "backend": args.backend.id(),
        "listen": args.listen.to_string(),
        "peer": peer.to_string(),
        "pass": pass,
        "frames_received": frames_received,
        "frames_accepted": frames_accepted,
        "frames_emitted": state.emitted,
        "frames_acked": frames_acked,
        "unknown_pts": state.unknown_pts,
        "max_decode_lag_frames": max_decode_lag,
        "receive_to_ack_latency_ms": receive_to_ack_latency_ms,
        "eagain_retries": eagain_retries,
        "torn_down": torn_down,
        "fail_reasons": fail_reasons,
    });

    log::info!(
        "harness-receiver done: pass={pass} received={frames_received} accepted={frames_accepted} emitted={} acked={frames_acked}",
        state.emitted
    );

    write_report(&report, args.json_out.as_ref())?;

    if pass {
        Ok(())
    } else {
        std::process::exit(1);
    }
}
