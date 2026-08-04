//! A0.0 decoder experiment (ERR-03 backend selection evidence).
//!
//! Feeds an Annex-B HEVC elementary stream into exactly one of the two closed
//! `decoder_backend` configurations from `docs/WIRE.md` §7, constructed
//! byte-for-byte per that table (no fallback between backends — an init
//! failure is a hard, structured exit). For every emitted frame it recovers
//! the decoder-reported ordinal (`AVFrame.pts`, falling back to
//! `best_effort_timestamp`) and checks it against the expected strictly
//! increasing 1..=N submission sequence, per `CONTRACT_ERRATA.md` ERR-03 /
//! `docs/WIRE.md` §8. Submission and drain go through the shared
//! `backend_construct` retain-drain-resubmit engine
//! (`backend_construct::submit_with_retry`/`drain_fully`/`flush_tail`), so
//! this binary and `harness-receiver` no longer duplicate that state
//! machine.
//!
//! Four modes, selected by CLI flag:
//! - **default** (no flag; optionally `--stall-every`/`--stall-ms` for an
//!   induced-delay run) — the original ERR-03 evidence run.
//! - **`--characterize`** — the `A00_REMEDIATION_PLAN.md` §3 D2 *bounded*
//!   characterization protocol (§5 R6): Phase A deliberately submits without
//!   draining, bounded by `--max-packets`/`--timeout-secs`, to try to force
//!   real send-side EAGAIN (never assumed — recorded as `forced`/
//!   `not_forced`, exit 0 either way); Phase B continues normally to end of
//!   sample plus an EOF/tail drain, proving `emitted == submitted` with
//!   exact ordinal coverage on the real backend.
//! - **`--clean`** — Phase C: a stall-free full-sample run measuring
//!   per-frame submit→emit lag and output latency (p50/p95/p99) for the
//!   `decoder_lag_bound`/`output_deadline_ms` freeze inputs.
//! - **`--force-zero-output`** — a bounded REAL zero-output-packet forcing
//!   attempt on the selected backend's exact `open_decoder` configuration
//!   (`A00_COMPLETION_REPORT_AMENDED_review.md` finding 5 /
//!   `..._response_review.md` amendment 5; ERR-03 / §3 D2's escape hatch).
//!   R6 (`evidence/a00/wip/r6-summary.md`) recorded zero real zero-output
//!   packets on either backend under ordinary streaming submission — this
//!   sample has no B-frames, so every accepted AU's own post-accept drain
//!   already emits a frame. Rather than accept the scripted `MockDecoder`
//!   double for that branch without either a forced real case or a dated
//!   equivalence erratum, this mode crafts a parameter-set-only packet (no
//!   VCL data at all — see `annexb::extract_param_sets`) and submits it
//!   through the real decoder, falling back to an immediate first-AU drain
//!   check if that crafted packet is rejected outright. Exit 0 iff exact
//!   ordinal coverage / exactly-once / no-phantom-ordinal-0 hold,
//!   regardless of whether forcing itself succeeded — `strategy_used:
//!   "not_forced"` with clean invariants is a valid, honest outcome.
//!
//! This binary has no dependency on the `protocol` crate and is not wired
//! into the main client binary — it is standalone A0.0 measurement tooling.

mod annexb;

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
}

struct Args {
    input: PathBuf,
    backend: Backend,
    frames: Option<u64>,
    stall_every: u64,
    stall_ms: u64,
    json_out: Option<PathBuf>,
    /// `--characterize`: A00_REMEDIATION_PLAN.md §3 D2 bounded
    /// characterization (Phase A forcing + Phase B normal-to-EOF).
    characterize: bool,
    /// `--clean`: §3 D2 Phase C, stall-free full-sample latency baseline.
    clean: bool,
    /// `--force-zero-output`: bounded REAL zero-output-packet forcing
    /// attempt on the selected backend (`A00_COMPLETION_REPORT_AMENDED_
    /// review.md` finding 5 / `..._response_review.md` amendment 5; ERR-03).
    force_zero_output: bool,
    /// Bound shared by `--characterize`/`--force-zero-output`: max packets
    /// submitted, default 480 (the fixed sample's full AU count).
    max_packets: u64,
    /// Bound shared by `--characterize`/`--force-zero-output`: wall-clock
    /// ceiling in seconds — default 90 for `--characterize` (pre-existing),
    /// 60 for `--force-zero-output`, resolved in [`parse_args`] once the
    /// active mode is known.
    timeout_secs: u64,
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
    let mut characterize = false;
    let mut clean = false;
    let mut force_zero_output = false;
    let mut max_packets: u64 = 480;
    // Default depends on mode, resolved after parsing once `force_zero_output`
    // is known: 90s for `--characterize` (pre-existing), 60s for
    // `--force-zero-output` (A00_COMPLETION_REPORT_AMENDED_review.md
    // finding 5 / ..._response_review.md amendment 5).
    let mut timeout_secs: Option<u64> = None;

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
            "--characterize" => characterize = true,
            "--clean" => clean = true,
            "--force-zero-output" => force_zero_output = true,
            "--max-packets" => {
                max_packets = next_val(&mut it, "--max-packets")?
                    .parse()
                    .context("--max-packets must be a non-negative integer")?
            }
            "--timeout-secs" => {
                timeout_secs = Some(
                    next_val(&mut it, "--timeout-secs")?
                        .parse()
                        .context("--timeout-secs must be a non-negative integer")?,
                )
            }
            other => bail!("unknown argument: {other}"),
        }
    }

    if characterize as u8 + clean as u8 + force_zero_output as u8 > 1 {
        bail!("--characterize, --clean, and --force-zero-output are mutually exclusive (each is a separate invocation)");
    }

    let timeout_secs = timeout_secs.unwrap_or(if force_zero_output { 60 } else { 90 });

    Ok(Args {
        input: input.context("--input is required")?,
        backend: backend.context("--backend is required")?,
        frames,
        stall_every,
        stall_ms,
        json_out,
        characterize,
        clean,
        force_zero_output,
        max_packets,
        timeout_secs,
    })
}

fn mode_name(args: &Args) -> &'static str {
    if args.characterize {
        "characterize"
    } else if args.clean {
        "clean"
    } else if args.force_zero_output {
        "force_zero_output"
    } else {
        "default"
    }
}

/// Running ordinal-faithfulness / timing state accumulated across the whole
/// run (ERR-03 bookkeeping). Shared by the default and `--clean` modes via
/// [`run_streaming`].
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

/// Records one recovered ordinal (`backend_construct` has already run it
/// through the hw transfer when applicable — see
/// `DecoderLoopBackend::receive_frame`'s `DecoderHandle` impl): checks it
/// against the expected strictly-increasing sequence and, when a send
/// timestamp is on record for it, appends its submit→emit delay.
fn record_emission(recovered_ordinal: Option<i64>, send_times: &HashMap<i64, Instant>, state: &mut RunState) {
    let now = Instant::now();
    state.emitted += 1;

    match recovered_ordinal {
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
}

/// A drain's output classified for the characterization report:
/// zero-output drains and multi-output drains are both real-backend
/// evidence the bounded protocol exists to capture (A00_REMEDIATION_PLAN.md
/// §3 D2) — neither is a failure.
fn classify_drain(
    drain: &[Option<i64>],
    zero_output_drains: &mut u64,
    multi_output_drains: &mut u64,
    multi_output_max: &mut usize,
) {
    if drain.is_empty() {
        *zero_output_drains += 1;
    } else if drain.len() > 1 {
        *multi_output_drains += 1;
    }
    *multi_output_max = (*multi_output_max).max(drain.len());
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

/// Init-failure report for the default/`--clean` modes — unchanged shape
/// from the pre-`--characterize` report (back-compat: this is the schema
/// retained evidence from the original ERR-03 runs already used).
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

/// Init-failure report for `--characterize` — its own schema (bounds +
/// environment are meaningful even on an init failure).
fn init_failure_report_characterize(args: &Args, failing_call: &str, detail: &str) -> serde_json::Value {
    serde_json::json!({
        "mode": "characterize",
        "backend": args.backend.id(),
        "pass": false,
        "bounds": { "max_packets": args.max_packets, "timeout_secs": args.timeout_secs },
        "environment": environment_report(),
        "fail_reasons": [format!("{failing_call} failed: {detail}")],
        "error_code": "REQUIRED_NATIVE_API",
        "failing_call": failing_call,
    })
}

/// Init-failure report for `--force-zero-output` — mirrors
/// `init_failure_report_characterize`'s shape (bounds + environment are
/// meaningful even on an init failure); `strategy_used` is always
/// `"not_forced"` here since nothing was ever submitted.
fn init_failure_report_force_zero_output(args: &Args, failing_call: &str, detail: &str) -> serde_json::Value {
    serde_json::json!({
        "mode": "force_zero_output",
        "backend": args.backend.id(),
        "pass": false,
        "strategy_used": "not_forced",
        "bounds": { "max_packets": args.max_packets, "timeout_secs": args.timeout_secs },
        "environment": environment_report(),
        "fail_reasons": [format!("{failing_call} failed: {detail}")],
        "error_code": "REQUIRED_NATIVE_API",
        "failing_call": failing_call,
    })
}

/// Dispatches to the right init-failure report shape for the active mode —
/// used by all three of `main`'s early-exit sites (decoder open,
/// `avformat_open_input`, `av_find_best_stream`) so a third mode only needs
/// a third arm here, not a third arm at every call site.
fn init_failure_report_for_mode(args: &Args, failing_call: &str, detail: &str) -> serde_json::Value {
    if args.characterize {
        init_failure_report_characterize(args, failing_call, detail)
    } else if args.force_zero_output {
        init_failure_report_force_zero_output(args, failing_call, detail)
    } else {
        init_failure_report(args.backend, failing_call, detail)
    }
}

fn classify_open_error(e: &anyhow::Error) -> (String, String) {
    match e.downcast_ref::<backend_construct::BackendOpenError>() {
        Some(be) => (be.failing_call.clone(), be.detail.clone()),
        None => ("open_decoder".to_string(), e.to_string()),
    }
}

// ---------------------------------------------------------------------
// Environment evidence (A00_REMEDIATION_PLAN.md §3 D2: "Run both hevc and
// hevc_cuvid under recorded machine/driver/FFmpeg versions").
// ---------------------------------------------------------------------

fn kernel_release() -> String {
    match std::process::Command::new("uname").arg("-r").output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Ok(out) => format!("uname -r exited with status {}", out.status),
        Err(e) => format!("uname -r not runnable: {e}"),
    }
}

fn nvidia_driver_version() -> String {
    match std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=driver_version", "--format=csv,noheader"])
        .output()
    {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if text.is_empty() {
                "unavailable".to_string()
            } else {
                text
            }
        }
        Ok(out) => format!("nvidia-smi exited with status {}", out.status),
        Err(e) => format!("nvidia-smi not runnable: {e}"),
    }
}

/// `avcodec_version()`, decoded per FFmpeg's `AV_VERSION_MAJOR/MINOR/MICRO`
/// convention (mirrors `src/doctor.rs`'s `ffmpeg_version_value` — that
/// module is out of scope this session, so this is a small, deliberate
/// three-line duplication rather than a new cross-binary dependency).
fn ffmpeg_version_banner() -> String {
    let raw = ffmpeg_next::codec::version();
    let major = (raw >> 16) & 0xFF;
    let minor = (raw >> 8) & 0xFF;
    let micro = raw & 0xFF;
    format!("{major}.{minor}.{micro} (raw {raw})")
}

fn environment_report() -> serde_json::Value {
    serde_json::json!({
        "ffmpeg_version": ffmpeg_version_banner(),
        "nvidia_driver_version": nvidia_driver_version(),
        "kernel_release": kernel_release(),
    })
}

// ---------------------------------------------------------------------
// Default / --clean: sequential submit-drain-to-EOF over the whole sample.
// ---------------------------------------------------------------------

/// Submits the sample sequentially, draining after every accept via the
/// shared `backend_construct` engine, then EOF/tail-drains. Used by both
/// the pre-existing default/`--stall-*` mode and the new `--clean` mode
/// (A00_REMEDIATION_PLAN.md §3 D2 Phase C: "stall-free full-sample run
/// measuring per-frame submit→emit lag ... and output latency").
/// `stall_every`/`stall_ms` are passed explicitly (not read from `args`) so
/// `--clean` is stall-free regardless of what those flags happen to hold.
fn run_streaming(
    args: &Args,
    mut handle: backend_construct::DecoderHandle,
    mut ictx: ffmpeg_next::format::context::Input,
    video_stream_index: usize,
    mode_label: &'static str,
    stall_every: u64,
    stall_ms: u64,
) -> Result<bool> {
    const MAX_EAGAIN_CYCLES: u32 = 64;

    let mut ordinal: i64 = 0;
    let mut submitted: u64 = 0;
    let mut eagain_retries: u64 = 0;
    let mut send_times: HashMap<i64, Instant> = HashMap::new();
    let mut state = RunState::new();
    let mut fatal_reason: Option<String> = None;

    'outer: for (stream, packet) in ictx.packets() {
        if stream.index() != video_stream_index {
            continue;
        }
        if let Some(limit) = args.frames {
            if submitted >= limit {
                break;
            }
        }

        ordinal += 1;
        let data = match packet.data() {
            Some(d) => d,
            None => {
                fatal_reason = Some(format!("ordinal {ordinal}: demuxed packet has no data"));
                break 'outer;
            }
        };
        send_times.insert(ordinal, Instant::now());

        let record = match backend_construct::submit_with_retry(&mut handle, ordinal, data, MAX_EAGAIN_CYCLES) {
            Ok(r) => r,
            Err(reason) => {
                fatal_reason = Some(format!("ordinal {ordinal}: {reason}"));
                break 'outer;
            }
        };
        submitted += 1;
        for attempt in &record.attempts {
            if attempt.again {
                eagain_retries += 1;
            }
        }
        for drain in &record.drains {
            for &recovered in drain {
                record_emission(recovered, &send_times, &mut state);
            }
        }
        state.max_lag = state.max_lag.max(submitted as i64 - state.emitted as i64);

        if stall_every > 0 && submitted % stall_every == 0 && stall_ms > 0 {
            std::thread::sleep(Duration::from_millis(stall_ms));
        }

        match backend_construct::drain_fully(&mut handle) {
            Ok(drain) => {
                for recovered in drain {
                    record_emission(recovered, &send_times, &mut state);
                }
            }
            Err(reason) => {
                fatal_reason = Some(format!("ordinal {ordinal} post-accept drain: {reason}"));
                break 'outer;
            }
        }
        state.max_lag = state.max_lag.max(submitted as i64 - state.emitted as i64);
    }

    if fatal_reason.is_none() {
        match backend_construct::flush_tail(&mut handle, MAX_EAGAIN_CYCLES) {
            Ok(flush) => {
                eagain_retries += flush.eagain_count as u64;
                for recovered in flush.recovered {
                    record_emission(recovered, &send_times, &mut state);
                }
            }
            Err(reason) => {
                fatal_reason = Some(format!("flush: {reason}"));
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
        "p99": percentile(&sorted_delays, 99.0),
        "max": sorted_delays.last().copied().unwrap_or(0.0),
    });

    let report = serde_json::json!({
        "mode": mode_label,
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
        "decoder-experiment done: mode={mode_label} backend={} pass={pass} submitted={submitted} emitted={}",
        args.backend.id(),
        state.emitted
    );

    write_report(&report, args.json_out.as_ref())?;
    Ok(pass)
}

// ---------------------------------------------------------------------
// --characterize: A00_REMEDIATION_PLAN.md §3 D2 bounded protocol,
// Phase A (forcing) + Phase B (normal-to-EOF + tail drain).
// ---------------------------------------------------------------------

fn run_characterize(
    args: &Args,
    mut handle: backend_construct::DecoderHandle,
    mut ictx: ffmpeg_next::format::context::Input,
    video_stream_index: usize,
) -> Result<bool> {
    const MAX_EAGAIN_CYCLES: u32 = 64;

    let start = Instant::now();
    let timeout = Duration::from_secs(args.timeout_secs);

    let mut ordinal: i64 = 0;
    let mut submitted: u64 = 0;
    let mut accepted: u64 = 0;
    // Mid-stream (phase A + B) recovered ordinals; the tail is tracked
    // separately and folded in for the final coverage check.
    let mut mid_stream_emitted: Vec<Option<i64>> = Vec::new();
    let mut zero_output_drains: u64 = 0;
    let mut multi_output_drains: u64 = 0;
    let mut multi_output_max: usize = 0;

    let mut attempt_log: Vec<serde_json::Value> = Vec::new();
    let mut eagain_events: Vec<serde_json::Value> = Vec::new();
    let mut phase_a_again_total: u64 = 0;
    let mut phase_b_again_total: u64 = 0;

    let mut in_phase_a = true;
    // Default: the loop ran out of packets while still forcing — the
    // realistic outcome given the historical eagain_retries=0 evidence.
    let mut phase_a_stop_reason = "input_exhausted";
    let mut fatal_reason: Option<String> = None;
    let mut fatal_phase: Option<&'static str> = None;

    'outer: for (stream, packet) in ictx.packets() {
        if stream.index() != video_stream_index {
            continue;
        }

        if in_phase_a {
            if eagain_events.len() >= 3 {
                phase_a_stop_reason = "eagain_events_reached";
                in_phase_a = false;
            } else if submitted >= args.max_packets {
                phase_a_stop_reason = "max_packets_reached";
                in_phase_a = false;
            } else if start.elapsed() >= timeout {
                phase_a_stop_reason = "timeout_reached";
                in_phase_a = false;
            }
        }
        let this_phase: &'static str = if in_phase_a { "A" } else { "B" };

        ordinal += 1;
        let data = match packet.data() {
            Some(d) => d,
            None => {
                fatal_reason = Some(format!("ordinal {ordinal}: demuxed packet has no data"));
                fatal_phase = Some(this_phase);
                break 'outer;
            }
        };

        let record = match backend_construct::submit_with_retry(&mut handle, ordinal, data, MAX_EAGAIN_CYCLES) {
            Ok(r) => r,
            Err(reason) => {
                fatal_reason = Some(format!("ordinal {ordinal}: {reason}"));
                fatal_phase = Some(this_phase);
                break 'outer;
            }
        };
        submitted += 1;
        accepted += 1;

        let mut drain_idx = 0usize;
        for attempt in &record.attempts {
            attempt_log.push(serde_json::json!({
                "ordinal": ordinal,
                "phase": this_phase,
                "outcome": if attempt.again { "again" } else { "accepted" },
            }));
            if attempt.again {
                if in_phase_a {
                    phase_a_again_total += 1;
                } else {
                    phase_b_again_total += 1;
                }
                let drain = &record.drains[drain_idx];
                drain_idx += 1;
                classify_drain(drain, &mut zero_output_drains, &mut multi_output_drains, &mut multi_output_max);
                mid_stream_emitted.extend(drain.iter().copied());
                if eagain_events.len() < 3 {
                    eagain_events.push(serde_json::json!({
                        "ordinal": ordinal,
                        "outputs_this_drain": drain.len(),
                        "recovered_pts": drain,
                    }));
                }
            }
        }

        if !in_phase_a {
            // Phase B: normal steady-state, drain after every accept.
            match backend_construct::drain_fully(&mut handle) {
                Ok(drain) => {
                    classify_drain(&drain, &mut zero_output_drains, &mut multi_output_drains, &mut multi_output_max);
                    mid_stream_emitted.extend(drain.iter().copied());
                }
                Err(reason) => {
                    fatal_reason = Some(format!("ordinal {ordinal} post-accept drain: {reason}"));
                    fatal_phase = Some("B");
                    break 'outer;
                }
            }
        }
        // Phase A deliberately performs no post-accept drain — it is
        // starving the decoder's output side to try to force EAGAIN; see
        // the module doc comment.
    }

    let mut tail = backend_construct::FlushRecord::default();
    if fatal_reason.is_none() {
        match backend_construct::flush_tail(&mut handle, MAX_EAGAIN_CYCLES) {
            Ok(t) => tail = t,
            Err(reason) => {
                fatal_reason = Some(format!("tail flush: {reason}"));
                fatal_phase = Some("tail");
            }
        }
    }

    // ---- Accounting: real-backend EOF/tail ordinal evidence ----
    let mut combined: Vec<Option<i64>> = mid_stream_emitted.clone();
    combined.extend(tail.recovered.iter().copied());
    let emitted_total = combined.len() as u64;
    let unknown_pts = combined.iter().filter(|o| o.is_none()).count() as u64;
    let mut seen: HashSet<i64> = HashSet::new();
    let mut duplicates = 0u64;
    for v in combined.iter().flatten() {
        if !seen.insert(*v) {
            duplicates += 1;
        }
    }
    let expected: HashSet<i64> = (1..=submitted as i64).collect();
    let ordinal_coverage_exact =
        fatal_reason.is_none() && unknown_pts == 0 && duplicates == 0 && seen == expected;
    let exactly_once_ok = fatal_reason.is_none() && submitted == accepted;

    let phase_a_status = if fatal_phase == Some("A") {
        "error"
    } else if phase_a_again_total > 0 {
        "forced"
    } else {
        "not_forced"
    };
    let phase_b_status = if fatal_phase == Some("B") {
        "error"
    } else if phase_b_again_total > 0 {
        "forced"
    } else {
        "not_forced"
    };
    let tail_status = if fatal_phase == Some("tail") {
        "error"
    } else if tail.eagain_count > 0 {
        "forced"
    } else {
        "not_forced"
    };

    // Exit code per A00_REMEDIATION_PLAN.md §3 D2: 0 iff every INVARIANT
    // held (exactly-once, no loss/dup, exact coverage) — a real backend
    // never actually EAGAINing is evidence, not an invariant failure, so it
    // does not gate `pass`.
    let pass = fatal_reason.is_none() && ordinal_coverage_exact && exactly_once_ok;

    let mut fail_reasons: Vec<String> = Vec::new();
    if let Some(r) = &fatal_reason {
        fail_reasons.push(r.clone());
    }
    if fatal_reason.is_none() && !ordinal_coverage_exact {
        fail_reasons.push(format!(
            "ordinal coverage not exact: submitted={submitted} accepted={accepted} emitted={emitted_total} unknown_pts={unknown_pts} duplicates={duplicates}"
        ));
    }

    let report = serde_json::json!({
        "mode": "characterize",
        "backend": args.backend.id(),
        "pass": pass,
        "bounds": { "max_packets": args.max_packets, "timeout_secs": args.timeout_secs },
        "environment": environment_report(),
        "phase_a": {
            "status": phase_a_status,
            "stop_reason": phase_a_stop_reason,
            "eagain_count": phase_a_again_total,
        },
        "phase_b": {
            "status": phase_b_status,
            "eagain_count": phase_b_again_total,
        },
        "tail": {
            "status": tail_status,
            "eagain_count": tail.eagain_count,
            "outputs": tail.recovered.len(),
        },
        "attempts": attempt_log,
        "eagain_events": {
            "count": phase_a_again_total + phase_b_again_total,
            "detail": eagain_events,
        },
        "zero_output_packets": zero_output_drains,
        "multi_output_drains": {
            "count": multi_output_drains,
            "max_outputs_in_one_drain": multi_output_max,
        },
        "tail_outputs": tail.recovered.len(),
        "submitted": submitted,
        "accepted": accepted,
        "emitted": emitted_total,
        "ordinal_coverage_exact": ordinal_coverage_exact,
        "exactly_once_ok": exactly_once_ok,
        "fail_reasons": fail_reasons,
    });

    log::info!(
        "decoder-experiment --characterize done: backend={} pass={pass} submitted={submitted} accepted={accepted} emitted={emitted_total} phase_a={phase_a_status}({phase_a_stop_reason}) phase_b={phase_b_status} tail={tail_status}",
        args.backend.id()
    );

    write_report(&report, args.json_out.as_ref())?;
    Ok(pass)
}

// ---------------------------------------------------------------------
// --force-zero-output: bounded REAL zero-output-packet forcing attempt on
// the selected backend's EXACT `open_decoder` configuration
// (`A00_COMPLETION_REPORT_AMENDED_review.md` finding 5 /
// `..._response_review.md` amendment 5; ERR-03 / `A00_REMEDIATION_PLAN.md`
// §3 D2 escape hatch). R6 (`evidence/a00/wip/r6-summary.md`) recorded zero
// real zero-output packets on either backend under ordinary streaming
// submission (this sample has `bframes=0` — no reordering — so every
// accepted AU's own post-accept drain already emits a frame). The plan's
// D2 escape hatch forbids substituting the scripted `MockDecoder` double
// for that branch without either forcing a real case or a dated
// equivalence erratum, so this mode manufactures a genuine zero-output
// condition through the real decoder instead of asserting one never
// occurs:
//
// - Strategy A (primary): craft a parameter-set-only packet (VPS/SPS/PPS —
//   HEVC types 32/33/34, no VCL data at all — `annexb::extract_param_sets`)
//   from the sample's first AU and submit it as ordinal 0 through the
//   normal `backend_construct::submit_with_retry` path, then immediately
//   drain. A packet with no slice data structurally cannot produce a
//   picture, so an accepted send with an empty immediate drain is genuine
//   real-backend zero-output evidence.
// - Strategy B (fallback, only if strategy A's send is REJECTED rather than
//   accepted-with-zero-output, or AU 1 unexpectedly carries no parameter
//   sets at all): submit the real first AU and drain immediately, before
//   any further sends, and record whatever the real backend does.
//
// Either way the complete, unmodified sample is then submitted normally as
// ordinals 1..N (the same AU 1 whose param sets strategy A borrowed is
// resubmitted in full as ordinal 1 — repeating VPS/SPS/PPS inside a real AU
// is ordinary, legal HEVC), proving the crafted packet corrupted nothing:
// exact ordinal coverage of the VCL-bearing submissions, exactly-once
// acceptance, and ordinal 0 never appearing in any emission. Exit 0 iff
// those invariants hold, regardless of whether forcing itself succeeded —
// `strategy_used: "not_forced"` with clean invariants is a valid, honest
// outcome (per the amended completion report response review's amendment
// 5, that is the input to a possible future ERR-09, not a failure here).
// ---------------------------------------------------------------------

/// Accumulated accounting across the whole `--force-zero-output` run,
/// mirroring [`RunState`]'s role for the streaming modes.
#[derive(Default)]
struct ForceZeroOutputState {
    /// Count of real (VCL-bearing) AUs submitted — the sample's AUs 1..N,
    /// never the ordinal-0 crafted packet.
    submitted_vcl: u64,
    accepted_vcl: u64,
    /// Every recovered ordinal from every drain across the whole run, in
    /// receive order (includes whatever — if anything — came out of the
    /// ordinal-0 crafted packet's own drain).
    combined_emitted: Vec<Option<i64>>,
    zero_output_detail: Vec<serde_json::Value>,
}

/// One zero-output event for the report's `zero_output_detail` list.
/// `frames_this_drain`/`recovered_pts` are always `0`/`[]` by construction
/// — a non-empty drain is never pushed here.
fn zero_output_event(
    ordinal: i64,
    context: &'static str,
    send_outcome: &'static str,
    drain: &[Option<i64>],
) -> serde_json::Value {
    serde_json::json!({
        "ordinal": ordinal,
        "context": context,
        "send_outcome": send_outcome,
        "frames_this_drain": drain.len(),
        "recovered_pts": drain,
    })
}

fn send_outcome_label(record: &backend_construct::SubmitRecord) -> &'static str {
    if record.attempts.iter().any(|a| a.again) {
        "accepted_after_eagain"
    } else {
        "accepted"
    }
}

/// Submits one real (VCL-bearing) AU as `ordinal` through the normal
/// `submit_with_retry` path, drains fully, and folds both into `state`.
/// Returns the `SubmitRecord` (so callers can label the send outcome) plus
/// this submission's own post-accept drain — callers decide what, if
/// anything, to record in `zero_output_detail`, since the context label
/// differs by call site (forced first-AU check vs. ordinary streaming).
fn submit_vcl_and_drain(
    handle: &mut backend_construct::DecoderHandle,
    ordinal: i64,
    data: &[u8],
    max_attempts: u32,
    state: &mut ForceZeroOutputState,
) -> std::result::Result<(backend_construct::SubmitRecord, Vec<Option<i64>>), String> {
    let record = backend_construct::submit_with_retry(handle, ordinal, data, max_attempts)?;
    state.submitted_vcl += 1;
    state.accepted_vcl += 1;
    for d in &record.drains {
        state.combined_emitted.extend(d.iter().copied());
    }
    let drain = backend_construct::drain_fully(handle)?;
    state.combined_emitted.extend(drain.iter().copied());
    Ok((record, drain))
}

fn run_force_zero_output(
    args: &Args,
    mut handle: backend_construct::DecoderHandle,
    mut ictx: ffmpeg_next::format::context::Input,
    video_stream_index: usize,
) -> Result<bool> {
    const MAX_EAGAIN_CYCLES: u32 = 64;
    let start = Instant::now();
    let timeout = Duration::from_secs(args.timeout_secs);

    let mut packets = ictx.packets();

    // ---- Pull AU 1 (frame 0 is always an IDR keyframe, and
    // `tools/gen_sample_hevc.sh`'s libx265 encode carries VPS/SPS/PPS on
    // every AU when muxing to raw Annex-B, so AU 1 definitely has them). ----
    let mut first_au: Option<Vec<u8>> = None;
    while let Some((stream, packet)) = packets.next() {
        if stream.index() != video_stream_index {
            continue;
        }
        first_au = Some(packet.data().context("first AU has no data")?.to_vec());
        break;
    }
    let au1_data = first_au.context("input has no packets on the selected video stream")?;

    let (crafted, crafted_types) = annexb::extract_param_sets(&au1_data);

    let mut state = ForceZeroOutputState::default();
    let mut notes: Vec<String> = Vec::new();
    let mut strategy_used: &'static str = "not_forced";
    let mut fatal_reason: Option<String> = None;
    let mut au1_submitted = false;

    // ---- Strategy A: crafted parameter-set-only packet as ordinal 0 ----
    let mut try_strategy_b = false;
    if crafted.is_empty() {
        notes.push(format!(
            "AU 1 carried no VPS/SPS/PPS NAL units (types found: {crafted_types:?}); strategy A unavailable"
        ));
        try_strategy_b = true;
    } else {
        match backend_construct::submit_with_retry(&mut handle, 0, &crafted, MAX_EAGAIN_CYCLES) {
            Ok(record) => {
                for d in &record.drains {
                    state.combined_emitted.extend(d.iter().copied());
                }
                let outcome = send_outcome_label(&record);
                match backend_construct::drain_fully(&mut handle) {
                    Ok(drain0) => {
                        state.combined_emitted.extend(drain0.iter().copied());
                        if drain0.is_empty() {
                            strategy_used = "param_set_packet";
                            state
                                .zero_output_detail
                                .push(zero_output_event(0, "forced_param_set_packet", outcome, &drain0));
                        } else {
                            notes.push(format!(
                                "ordinal 0 crafted parameter-set-only packet ({crafted_types:?}) was accepted but its immediate drain emitted {} frame(s) unexpectedly ({drain0:?}); strategy A did not force a zero-output condition",
                                drain0.len()
                            ));
                        }
                    }
                    Err(reason) => fatal_reason = Some(format!("ordinal 0 immediate drain: {reason}")),
                }
            }
            Err(reason) => {
                notes.push(format!("ordinal 0 crafted parameter-set-only packet was REJECTED: {reason}"));
                try_strategy_b = true;
            }
        }
    }

    // ---- Strategy B fallback: real first AU, immediate drain, before any
    // further sends (only on strategy A rejection / unavailability). ----
    if try_strategy_b && fatal_reason.is_none() {
        match submit_vcl_and_drain(&mut handle, 1, &au1_data, MAX_EAGAIN_CYCLES, &mut state) {
            Ok((record, drain1)) => {
                au1_submitted = true;
                let outcome = send_outcome_label(&record);
                if drain1.is_empty() {
                    strategy_used = "first_au_immediate_drain";
                    state
                        .zero_output_detail
                        .push(zero_output_event(1, "first_au_immediate_drain", outcome, &drain1));
                } else {
                    notes.push(format!(
                        "strategy B: ordinal 1 (real first AU) accepted, but its immediate drain emitted {} frame(s) ({drain1:?}) — accepted-no-emit-until-next-send did not hold under an immediate drain",
                        drain1.len()
                    ));
                }
            }
            Err(reason) => fatal_reason = Some(format!("ordinal 1 (strategy B real first AU): {reason}")),
        }
    }

    // ---- AU 1 as ordinal 1, if not already submitted by strategy B ----
    // (`au1_submitted` is only ever read above, to decide whether to reach
    // this block at all — nothing downstream needs it again, so this arm
    // does not re-assign it.)
    if fatal_reason.is_none() && !au1_submitted {
        match submit_vcl_and_drain(&mut handle, 1, &au1_data, MAX_EAGAIN_CYCLES, &mut state) {
            Ok((_, drain)) => {
                if drain.is_empty() {
                    state
                        .zero_output_detail
                        .push(zero_output_event(1, "normal_post_accept_drain", "accepted", &drain));
                }
            }
            Err(reason) => fatal_reason = Some(format!("ordinal 1: {reason}")),
        }
    }

    // ---- Ordinals 2..N: the rest of the sample, submitted normally ----
    // `loop_stop_reason` starts "not_entered" so a fatal error during the
    // AU-1 handling above (which skips this loop entirely) is never
    // misreported as a clean "end_of_sample".
    let mut loop_stop_reason = "not_entered";
    if fatal_reason.is_none() {
        loop_stop_reason = "end_of_sample";
        let mut ordinal: i64 = 1;
        'outer: while let Some((stream, packet)) = packets.next() {
            if stream.index() != video_stream_index {
                continue;
            }
            if state.submitted_vcl >= args.max_packets {
                loop_stop_reason = "max_packets_reached";
                break 'outer;
            }
            if start.elapsed() >= timeout {
                loop_stop_reason = "timeout_reached";
                break 'outer;
            }
            ordinal += 1;
            let data = match packet.data() {
                Some(d) => d,
                None => {
                    fatal_reason = Some(format!("ordinal {ordinal}: demuxed packet has no data"));
                    loop_stop_reason = "fatal_error";
                    break 'outer;
                }
            };
            match submit_vcl_and_drain(&mut handle, ordinal, data, MAX_EAGAIN_CYCLES, &mut state) {
                Ok((_, drain)) => {
                    if drain.is_empty() {
                        state
                            .zero_output_detail
                            .push(zero_output_event(ordinal, "normal_post_accept_drain", "accepted", &drain));
                    }
                }
                Err(reason) => {
                    fatal_reason = Some(format!("ordinal {ordinal}: {reason}"));
                    loop_stop_reason = "fatal_error";
                    break 'outer;
                }
            }
        }
    }

    // ---- Tail / EOF flush ----
    let mut tail_outputs: usize = 0;
    if fatal_reason.is_none() {
        match backend_construct::flush_tail(&mut handle, MAX_EAGAIN_CYCLES) {
            Ok(tail) => {
                tail_outputs = tail.recovered.len();
                state.combined_emitted.extend(tail.recovered.iter().copied());
            }
            Err(reason) => fatal_reason = Some(format!("tail flush: {reason}")),
        }
    }

    // Derived from `zero_output_detail` itself (rather than a separately
    // hand-incremented counter) so the two can never diverge — every push
    // above already covers exactly the events this count should include,
    // whether they came from the designated forcing attempt or an
    // incidental zero-output drain elsewhere in the run (e.g. cuvid's
    // higher lag bound means AU 1's own post-accept drain can legitimately
    // come back empty too — real evidence, not a bug, if it happens).
    let zero_output_packets = state.zero_output_detail.len() as u64;

    // ---- Final accounting: exact ordinal coverage of the VCL-bearing
    // submissions (1..=submitted_vcl), zero unknown/duplicate, and ordinal
    // 0 (the crafted packet's own PTS, when strategy A ran) never among the
    // recovered ordinals — proving the injected packet corrupted nothing.
    let emitted = state.combined_emitted.len() as u64;
    let unknown_pts = state.combined_emitted.iter().filter(|o| o.is_none()).count() as u64;
    let mut seen: HashSet<i64> = HashSet::new();
    let mut duplicates = 0u64;
    for v in state.combined_emitted.iter().flatten() {
        if !seen.insert(*v) {
            duplicates += 1;
        }
    }
    let ordinal_zero_never_emitted = !seen.contains(&0);
    let expected: HashSet<i64> = (1..=state.submitted_vcl as i64).collect();
    let ordinal_coverage_exact = fatal_reason.is_none()
        && unknown_pts == 0
        && duplicates == 0
        && ordinal_zero_never_emitted
        && seen == expected;
    let exactly_once_ok = fatal_reason.is_none() && state.submitted_vcl == state.accepted_vcl;

    let pass = fatal_reason.is_none() && ordinal_coverage_exact && exactly_once_ok;

    let mut fail_reasons: Vec<String> = Vec::new();
    if let Some(r) = &fatal_reason {
        fail_reasons.push(r.clone());
    }
    if fatal_reason.is_none() && !ordinal_coverage_exact {
        fail_reasons.push(format!(
            "ordinal coverage not exact: submitted_vcl={} accepted_vcl={} emitted={emitted} unknown_pts={unknown_pts} duplicates={duplicates} ordinal_zero_never_emitted={ordinal_zero_never_emitted}",
            state.submitted_vcl, state.accepted_vcl
        ));
    }
    if fatal_reason.is_none() && !exactly_once_ok {
        fail_reasons.push(format!(
            "exactly-once violated: submitted_vcl={} accepted_vcl={}",
            state.submitted_vcl, state.accepted_vcl
        ));
    }

    let report = serde_json::json!({
        "mode": "force_zero_output",
        "backend": args.backend.id(),
        "pass": pass,
        "strategy_used": strategy_used,
        "zero_output_packets": zero_output_packets,
        "zero_output_detail": state.zero_output_detail,
        "emitted": emitted,
        "submitted_vcl": state.submitted_vcl,
        "accepted_vcl": state.accepted_vcl,
        "unknown_pts": unknown_pts,
        "duplicates": duplicates,
        "ordinal_zero_never_emitted": ordinal_zero_never_emitted,
        "ordinal_coverage_exact": ordinal_coverage_exact,
        "exactly_once_ok": exactly_once_ok,
        "tail_outputs": tail_outputs,
        "loop_stop_reason": loop_stop_reason,
        "crafted_param_set_nal_types": crafted_types,
        "environment": environment_report(),
        "bounds": { "max_packets": args.max_packets, "timeout_secs": args.timeout_secs },
        "notes": notes,
        "fail_reasons": fail_reasons,
    });

    log::info!(
        "decoder-experiment --force-zero-output done: backend={} pass={pass} strategy_used={strategy_used} zero_output_packets={zero_output_packets} submitted_vcl={} emitted={emitted}",
        args.backend.id(),
        state.submitted_vcl
    );

    write_report(&report, args.json_out.as_ref())?;
    Ok(pass)
}

// ---------------------------------------------------------------------

fn main() -> Result<()> {
    env_logger::init();
    let args = parse_args()?;

    ffmpeg_next::init().context("ffmpeg_next::init")?;

    log::info!(
        "decoder-experiment: backend={} input={} mode={}",
        args.backend.id(),
        args.input.display(),
        mode_name(&args)
    );

    let handle = match backend_construct::open_decoder(args.backend.id()) {
        Ok(h) => h,
        Err(e) => {
            let (failing_call, detail) = classify_open_error(&e);
            log::error!("backend init failed: {failing_call}: {detail}");
            let report = init_failure_report_for_mode(&args, &failing_call, &detail);
            write_report(&report, args.json_out.as_ref())?;
            std::process::exit(1);
        }
    };

    let ictx = match ffmpeg_next::format::input(&args.input) {
        Ok(i) => i,
        Err(e) => {
            log::error!("avformat_open_input failed: {e}");
            let report = init_failure_report_for_mode(&args, "avformat_open_input", &e.to_string());
            write_report(&report, args.json_out.as_ref())?;
            std::process::exit(1);
        }
    };

    let video_stream_index = match ictx.streams().best(ffmpeg_next::media::Type::Video) {
        Some(s) => s.index(),
        None => {
            log::error!("av_find_best_stream found no video stream");
            let report = init_failure_report_for_mode(&args, "av_find_best_stream", "no video stream in input");
            write_report(&report, args.json_out.as_ref())?;
            std::process::exit(1);
        }
    };

    let pass = if args.characterize {
        run_characterize(&args, handle, ictx, video_stream_index)?
    } else if args.clean {
        run_streaming(&args, handle, ictx, video_stream_index, "clean", 0, 0)?
    } else if args.force_zero_output {
        run_force_zero_output(&args, handle, ictx, video_stream_index)?
    } else {
        run_streaming(&args, handle, ictx, video_stream_index, "default", args.stall_every, args.stall_ms)?
    };

    if pass {
        Ok(())
    } else {
        std::process::exit(1);
    }
}
