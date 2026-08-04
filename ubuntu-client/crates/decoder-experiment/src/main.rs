//! A0.0 decoder experiment (ERR-03 backend selection evidence).
//!
//! Feeds an Annex-B HEVC elementary stream into exactly one of the two closed
//! `decoder_backend` configurations from `docs/WIRE.md` §7, constructed
//! byte-for-byte per that table (no fallback between backends — an init
//! failure is a hard, structured exit). For every emitted frame it recovers
//! the decoder-reported ordinal (`AVFrame.pts`, falling back to
//! `best_effort_timestamp`) and checks it against the expected strictly
//! increasing 1..=N submission sequence, per `CONTRACT_ERRATA.md` ERR-03 /
//! `docs/WIRE.md` §8. Send-side EAGAIN is handled per
//! `IMPLEMENTATION_PLAN_V11.md` §7: retain the packet, drain, resubmit the
//! same packet until accepted exactly once, then drain again to `Again`.
//!
//! This binary has no dependency on the `protocol` crate and is not wired
//! into the main client binary — it is standalone A0.0 measurement tooling.

use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// One of the two closed `decoder_backend` configuration IDs
/// (`docs/WIRE.md` §7). There is no third option and no fallback between
/// them.
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

    fn is_hw(&self) -> bool {
        matches!(self, Backend::CuvidLowDelay)
    }
}

struct Args {
    input: PathBuf,
    backend: Backend,
    frames: Option<u64>,
    stall_every: u64,
    stall_ms: u64,
    json_out: Option<PathBuf>,
}

fn next_val(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    it.next().with_context(|| format!("{flag} requires a value"))
}

fn parse_args() -> Result<Args> {
    let mut input: Option<PathBuf> = None;
    let mut backend: Option<Backend> = None;
    let mut frames: Option<u64> = None;
    let mut stall_every: u64 = 0;
    let mut stall_ms: u64 = 0;
    let mut json_out: Option<PathBuf> = None;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--input" => input = Some(PathBuf::from(next_val(&mut it, "--input")?)),
            "--backend" => backend = Some(Backend::parse(&next_val(&mut it, "--backend")?)?),
            "--frames" => {
                frames = Some(
                    next_val(&mut it, "--frames")?
                        .parse()
                        .context("--frames must be a non-negative integer")?,
                )
            }
            "--stall-every" => {
                stall_every = next_val(&mut it, "--stall-every")?
                    .parse()
                    .context("--stall-every must be a non-negative integer")?
            }
            "--stall-ms" => {
                stall_ms = next_val(&mut it, "--stall-ms")?
                    .parse()
                    .context("--stall-ms must be a non-negative integer")?
            }
            "--json-out" => json_out = Some(PathBuf::from(next_val(&mut it, "--json-out")?)),
            other => bail!("unknown argument: {other}"),
        }
    }

    Ok(Args {
        input: input.context("--input is required")?,
        backend: backend.context("--backend is required")?,
        frames,
        stall_every,
        stall_ms,
        json_out,
    })
}

/// Running ordinal-faithfulness / timing state accumulated across the whole
/// run (ERR-03 bookkeeping).
struct RunState {
    emitted: u64,
    unknown_pts: u64,
    duplicates: u64,
    reorders: u64,
    expected_next: i64,
    seen: HashSet<i64>,
    delays_ms: Vec<f64>,
    max_lag: i64,
    fail_notes: Vec<String>,
}

impl RunState {
    fn new() -> Self {
        RunState {
            emitted: 0,
            unknown_pts: 0,
            duplicates: 0,
            reorders: 0,
            expected_next: 1,
            seen: HashSet::new(),
            delays_ms: Vec::new(),
            max_lag: 0,
            fail_notes: Vec::new(),
        }
    }

    fn note(&mut self, msg: String) {
        if self.fail_notes.len() < 50 {
            self.fail_notes.push(msg);
        } else if self.fail_notes.len() == 50 {
            self.fail_notes.push("...additional anomalies suppressed...".to_string());
        }
    }
}

enum DrainStop {
    Again,
    Eof,
}

/// Records one emitted frame: for the hw backend, faithfully exercises the
/// GPU→CPU `av_hwframe_transfer_data` step from the WIRE.md §7 row (a
/// transfer failure means the backend did not actually produce usable
/// output, so it is fatal here); then recovers the ordinal from
/// `frame.pts()` (falling back to `best_effort_timestamp`) and checks it
/// against the expected strictly-increasing sequence.
fn record_emission(
    frame: &ffmpeg_next::frame::Video,
    is_hw: bool,
    send_times: &HashMap<i64, Instant>,
    state: &mut RunState,
) -> std::result::Result<(), String> {
    if is_hw {
        backend_construct::transfer_hw_frame(frame)?;
    }

    let now = Instant::now();
    state.emitted += 1;

    // ERR-03: AVFrame.pts, falling back to best_effort_timestamp.
    let got = backend_construct::recovered_ordinal(frame);
    match got {
        None => {
            state.unknown_pts += 1;
            state.note(format!(
                "emission #{}: unknown pts (neither pts nor best_effort_timestamp set)",
                state.emitted
            ));
        }
        Some(got) => {
            if got == state.expected_next {
                state.expected_next += 1;
            } else if state.seen.contains(&got) {
                state.duplicates += 1;
                state.note(format!(
                    "duplicate: ordinal {got} emitted again (expected {})",
                    state.expected_next
                ));
            } else {
                state.reorders += 1;
                if got > state.expected_next {
                    state.note(format!(
                        "silent skip: expected ordinal {}, got {got} instead ({} ordinal(s) {}..{} never emitted before this)",
                        state.expected_next,
                        got - state.expected_next,
                        state.expected_next,
                        got - 1
                    ));
                } else {
                    state.note(format!(
                        "reorder: expected ordinal {}, got {got} (out of order, not a repeat)",
                        state.expected_next
                    ));
                }
                state.expected_next = got + 1;
            }
            state.seen.insert(got);
            if let Some(&t0) = send_times.get(&got) {
                let delay_ms = now.duration_since(t0).as_secs_f64() * 1000.0;
                state.delays_ms.push(delay_ms);
            }
        }
    }
    Ok(())
}

/// Drains `receive_frame` until classified `Again` or `Eof`. A real error
/// during drain is always fatal to the run (mirrors plan §7's "drain Error
/// ⇒ teardown").
fn drain_frames(
    decoder: &mut ffmpeg_next::decoder::Video,
    frame: &mut ffmpeg_next::frame::Video,
    is_hw: bool,
    send_times: &HashMap<i64, Instant>,
    state: &mut RunState,
) -> std::result::Result<DrainStop, String> {
    loop {
        match backend_construct::receive_one(decoder, frame)? {
            backend_construct::ReceiveOutcome::Frame => {
                record_emission(frame, is_hw, send_times, state)?;
            }
            backend_construct::ReceiveOutcome::Again => return Ok(DrainStop::Again),
            backend_construct::ReceiveOutcome::Eof => return Ok(DrainStop::Eof),
        }
    }
}

/// Send-EAGAIN retry per plan §7: retain the exact packet, drain (to make
/// room), resubmit the same packet, until accepted exactly once. Bounded to
/// avoid a true infinite loop if the decoder never makes progress.
fn send_with_eagain_retry(
    decoder: &mut ffmpeg_next::decoder::Video,
    packet: &ffmpeg_next::Packet,
    frame: &mut ffmpeg_next::frame::Video,
    is_hw: bool,
    send_times: &HashMap<i64, Instant>,
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
                drain_frames(decoder, frame, is_hw, send_times, state)?;
                // loop back and resubmit the same packet
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

fn init_failure_report(backend: Backend, failing_call: &str, detail: &str) -> serde_json::Value {
    serde_json::json!({
        "backend": backend.id(),
        "pass": false,
        "frames_submitted": 0,
        "frames_emitted": 0,
        "unknown_pts": 0,
        "duplicates": 0,
        "reorders": 0,
        "max_lag_frames": 0,
        "output_delay_ms": { "p50": 0.0, "p95": 0.0, "max": 0.0 },
        "eagain_retries": 0,
        "fail_reasons": [format!("{failing_call} failed: {detail}")],
        "error_code": "REQUIRED_NATIVE_API",
        "failing_call": failing_call,
    })
}

fn main() -> Result<()> {
    env_logger::init();
    let args = parse_args()?;

    ffmpeg_next::init().context("ffmpeg_next::init")?;

    log::info!(
        "decoder-experiment: backend={} input={}",
        args.backend.id(),
        args.input.display()
    );

    let mut handle = match backend_construct::open_decoder(args.backend.id()) {
        Ok(h) => h,
        Err(e) => {
            let (failing_call, detail) = match e.downcast_ref::<backend_construct::BackendOpenError>() {
                Some(be) => (be.failing_call.clone(), be.detail.clone()),
                None => ("open_decoder".to_string(), e.to_string()),
            };
            log::error!("backend init failed: {failing_call}: {detail}");
            let report = init_failure_report(args.backend, &failing_call, &detail);
            write_report(&report, args.json_out.as_ref())?;
            std::process::exit(1);
        }
    };

    let mut ictx = match ffmpeg_next::format::input(&args.input) {
        Ok(i) => i,
        Err(e) => {
            log::error!("avformat_open_input failed: {e}");
            let report = init_failure_report(args.backend, "avformat_open_input", &e.to_string());
            write_report(&report, args.json_out.as_ref())?;
            std::process::exit(1);
        }
    };

    let video_stream_index = match ictx.streams().best(ffmpeg_next::media::Type::Video) {
        Some(s) => s.index(),
        None => {
            log::error!("av_find_best_stream found no video stream");
            let report = init_failure_report(
                args.backend,
                "av_find_best_stream",
                "no video stream in input",
            );
            write_report(&report, args.json_out.as_ref())?;
            std::process::exit(1);
        }
    };

    let is_hw = args.backend.is_hw();
    let mut ordinal: i64 = 0;
    let mut submitted: u64 = 0;
    let mut eagain_retries: u64 = 0;
    let mut send_times: HashMap<i64, Instant> = HashMap::new();
    let mut state = RunState::new();
    let mut frame = ffmpeg_next::frame::Video::empty();
    let mut fatal_reason: Option<String> = None;

    'outer: for (stream, mut packet) in ictx.packets() {
        if stream.index() != video_stream_index {
            continue;
        }
        if let Some(limit) = args.frames {
            if submitted >= limit {
                break;
            }
        }

        ordinal += 1;
        packet.set_pts(Some(ordinal));
        send_times.insert(ordinal, Instant::now());

        if let Err(reason) = send_with_eagain_retry(
            &mut handle.decoder,
            &packet,
            &mut frame,
            is_hw,
            &send_times,
            &mut state,
            &mut eagain_retries,
        ) {
            fatal_reason = Some(format!("ordinal {ordinal}: {reason}"));
            break 'outer;
        }
        submitted += 1;
        state.max_lag = state.max_lag.max(submitted as i64 - state.emitted as i64);

        if args.stall_every > 0 && submitted % args.stall_every == 0 && args.stall_ms > 0 {
            std::thread::sleep(Duration::from_millis(args.stall_ms));
        }

        match drain_frames(&mut handle.decoder, &mut frame, is_hw, &send_times, &mut state) {
            Ok(_) => {}
            Err(reason) => {
                fatal_reason = Some(format!("ordinal {ordinal} post-accept drain: {reason}"));
                break 'outer;
            }
        }
        state.max_lag = state.max_lag.max(submitted as i64 - state.emitted as i64);
    }

    if fatal_reason.is_none() {
        // Flush: send_eof (with the same EAGAIN-retry discipline), then
        // drain fully to Eof.
        loop {
            match handle.decoder.send_eof() {
                Ok(()) => break,
                Err(ffmpeg_next::Error::Other { errno }) if errno == libc::EAGAIN => {
                    eagain_retries += 1;
                    if let Err(reason) =
                        drain_frames(&mut handle.decoder, &mut frame, is_hw, &send_times, &mut state)
                    {
                        fatal_reason = Some(format!("flush drain: {reason}"));
                        break;
                    }
                }
                Err(ffmpeg_next::Error::Eof) => break,
                Err(e) => {
                    fatal_reason = Some(format!("send_eof fatal error: {e}"));
                    break;
                }
            }
        }
        if fatal_reason.is_none() {
            if let Err(reason) =
                drain_frames(&mut handle.decoder, &mut frame, is_hw, &send_times, &mut state)
            {
                fatal_reason = Some(format!("final flush drain: {reason}"));
            }
        }
    }

    if fatal_reason.is_none() && state.emitted < submitted {
        state.note(format!(
            "{} frame(s) submitted but never emitted by end of flush ({} submitted, {} emitted)",
            submitted - state.emitted,
            submitted,
            state.emitted
        ));
    }

    let pass = fatal_reason.is_none()
        && state.unknown_pts == 0
        && state.duplicates == 0
        && state.reorders == 0
        && state.emitted == submitted;

    let mut fail_reasons = state.fail_notes.clone();
    if let Some(reason) = &fatal_reason {
        fail_reasons.push(reason.clone());
    }

    let mut sorted_delays = state.delays_ms.clone();
    sorted_delays.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let output_delay_ms = serde_json::json!({
        "p50": percentile(&sorted_delays, 50.0),
        "p95": percentile(&sorted_delays, 95.0),
        "max": sorted_delays.last().copied().unwrap_or(0.0),
    });

    let report = serde_json::json!({
        "backend": args.backend.id(),
        "pass": pass,
        "frames_submitted": submitted,
        "frames_emitted": state.emitted,
        "unknown_pts": state.unknown_pts,
        "duplicates": state.duplicates,
        "reorders": state.reorders,
        "max_lag_frames": state.max_lag,
        "output_delay_ms": output_delay_ms,
        "eagain_retries": eagain_retries,
        "fail_reasons": fail_reasons,
    });

    log::info!(
        "decoder-experiment done: backend={} pass={} submitted={} emitted={}",
        args.backend.id(),
        pass,
        submitted,
        state.emitted
    );

    write_report(&report, args.json_out.as_ref())?;

    if pass {
        Ok(())
    } else {
        std::process::exit(1);
    }
}
