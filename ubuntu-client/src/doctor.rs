//! Client doctor (`--doctor`; IMPLEMENTATION_PLAN_V11.md §11.4 "Doctors").
//!
//! A standalone, non-streaming probe of every load-bearing native
//! dependency the Ubuntu client needs, run on demand
//! (`remote-display-client --doctor`) instead of only being discovered the
//! hard way during a real session. Mirrors `mac-host`'s `HostDoctor.swift`
//! shape and conventions (same `doctor_report_v` envelope, same 0/2/3 exit
//! scheme) with `side: "client"` checks in its place:
//!
//! a. `environment` — kernel, FFmpeg/SDL runtime versions, NVIDIA driver,
//!    `auth_mode` note.
//! b. `backend_open` — opens `--doctor-backend` via `backend_construct`
//!    (CONTRACT_ERRATA.md ERR-02: one explicit candidate, logged, no
//!    fallback).
//! c. `decode_sample` — demuxes `--sample` and decodes its first 60 AUs,
//!    verifying the ERR-03 ordinal-faithfulness conditions.
//! d. `sdl_texture` — hidden window + renderer + IYUV/NV12 streaming
//!    texture probes.
//! e. `input_capability` — informational SDL subsystem/display report.
//!
//! Exit codes (v11 §11.4): 0 = pass, 2 = environment, 3 = native-API. (Code
//! 4, peer-diagnostic failure, belongs to `--diagnose-peer`, which this
//! doctor does not implement.)

use std::collections::HashSet;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use serde_json::{json, Value};

const COMPONENT: &str = "doctor";

const EXIT_PASS: i32 = 0;
const EXIT_ENVIRONMENT: i32 = 2;
const EXIT_NATIVE: i32 = 3;

/// Runs every doctor check in order, writes the report (stdout + compact
/// `~/.local/state/resc/doctor_client.json`), and returns the process exit
/// code.
pub fn run(doctor_backend: &str, sample_path: &Path) -> i32 {
    let log = diagnostics::RescLog::global();

    if let Err(e) = ffmpeg_next::init() {
        // Individual checks below surface their own native failures; this
        // doesn't need to be fatal to the whole doctor run (e.g. the SDL
        // checks don't depend on it).
        eprintln!("RESC doctor: ffmpeg_next::init failed: {e}");
    }

    let mut checks: Vec<Value> = Vec::new();
    let mut exit_code = EXIT_PASS;

    let (entry, contribution) = environment_check();
    checks.push(entry);
    exit_code = exit_code.max(contribution);

    let (entry, contribution, handle) = backend_open_check(doctor_backend, log);
    checks.push(entry);
    exit_code = exit_code.max(contribution);

    let (entry, contribution) = decode_sample_check(handle, sample_path);
    checks.push(entry);
    exit_code = exit_code.max(contribution);

    let probe = probe_sdl();
    let (entry, contribution) = sdl_texture_check(&probe);
    checks.push(entry);
    exit_code = exit_code.max(contribution);

    checks.push(input_capability_check(&probe));

    write_report(log, &checks, exit_code);
    // The doctor logs only a couple of RescLog events per run and its caller
    // (main.rs) calls std::process::exit right after this returns, which
    // skips Drop — force the buffered records out now or they're lost
    // (RescLog::event only auto-flushes every 32 records).
    log.flush();
    exit_code
}

// ---------------------------------------------------------------------
// a. environment
// ---------------------------------------------------------------------

fn environment_check() -> (Value, i32) {
    let kernel = diagnostics::environment::kernel_info();
    // Narrow failure mode (mirrors HostDoctor.swift's environmentCheck):
    // only the kernel read itself gates this check. FFmpeg/SDL versions are
    // infallible native-version queries (no failure mode to report), and
    // NVIDIA driver absence is explicitly environment-level evidence, not a
    // gate — a GPU-less machine is a normal environment fact, not a doctor
    // failure (CONTRACT_ERRATA.md ERR-02 / docs/WIRE.md §7 concern
    // `cuvid-lowdelay` selection, which `backend_open` covers separately).
    let kernel_ok = kernel.get("error").is_none();

    let entry = json!({
        "name": "environment", "ok": kernel_ok,
        "kernel": kernel,
        "ffmpeg": ffmpeg_version_value(),
        "sdl_version": sdl2::version::version().to_string(),
        "nvidia": nvidia_driver_info(),
        "auth_mode": "trusted_lan_none",
    });
    (entry, if kernel_ok { EXIT_PASS } else { EXIT_ENVIRONMENT })
}

/// `avcodec_version()` via `ffmpeg_next::codec::version()`, decoded per
/// FFmpeg's `AV_VERSION_MAJOR/MINOR/MICRO` convention
/// (`major = v>>16 & 0xFF`, `minor = v>>8 & 0xFF`, `micro = v & 0xFF`).
fn ffmpeg_version_value() -> Value {
    let raw = ffmpeg_next::codec::version();
    let major = (raw >> 16) & 0xFF;
    let minor = (raw >> 8) & 0xFF;
    let micro = raw & 0xFF;
    json!({
        "avcodec_version": format!("{major}.{minor}.{micro}"),
        "avcodec_version_raw": raw,
    })
}

/// `nvidia-smi --query-gpu=name,driver_version --format=csv,noheader`.
/// `ok:false` if absent — that's a normal environment fact on a GPU-less
/// machine, not escalated past this check's own environment-level cap (see
/// `environment_check`'s comment).
fn nvidia_driver_info() -> Value {
    match std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=name,driver_version", "--format=csv,noheader"])
        .output()
    {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            json!({ "ok": !text.is_empty(), "output": text })
        }
        Ok(out) => json!({
            "ok": false,
            "detail": format!("nvidia-smi exited with status {}", out.status),
            "stderr": String::from_utf8_lossy(&out.stderr).trim(),
        }),
        Err(e) => json!({ "ok": false, "detail": format!("nvidia-smi not runnable: {e}") }),
    }
}

// ---------------------------------------------------------------------
// b. backend_open
// ---------------------------------------------------------------------

fn backend_open_check(
    doctor_backend: &str,
    log: &diagnostics::RescLog,
) -> (Value, i32, Option<backend_construct::DecoderHandle>) {
    // ERR-02: "the decoder doctor takes one explicit candidate and logs it."
    log.event(
        "doctor_backend_candidate",
        COMPONENT,
        json!({
            "candidate": doctor_backend,
            "note": "ERR-02: the decoder doctor takes one explicit candidate and logs it",
        }),
    );

    match backend_construct::open_decoder(doctor_backend) {
        Ok(handle) => {
            let entry = json!({
                "name": "backend_open", "ok": true,
                "candidate": doctor_backend, "is_hw": handle.is_hw,
            });
            (entry, EXIT_PASS, Some(handle))
        }
        Err(e) => {
            let (failing_call, detail) = downcast_backend_error(&e);
            let entry = json!({
                "name": "backend_open", "ok": false,
                "candidate": doctor_backend,
                "failing_call": failing_call, "detail": detail,
                "error_code": "REQUIRED_NATIVE_API",
            });
            (entry, EXIT_NATIVE, None)
        }
    }
}

fn downcast_backend_error(e: &anyhow::Error) -> (String, String) {
    match e.downcast_ref::<backend_construct::BackendOpenError>() {
        Some(be) => (be.failing_call.clone(), be.detail.clone()),
        None => ("open_decoder".to_string(), e.to_string()),
    }
}

// ---------------------------------------------------------------------
// c. decode_sample
// ---------------------------------------------------------------------

/// First 60 AUs of `--sample`, fed with `pts = ordinal` exactly like
/// `decoder-experiment`. ERR-03 bookkeeping only (no timing/lag stats — the
/// doctor just needs pass/fail evidence).
struct Err03Counts {
    emitted: u64,
    unknown_pts: u64,
    duplicates: u64,
    reorders: u64,
    expected_next: i64,
    seen: HashSet<i64>,
}

impl Err03Counts {
    fn new() -> Self {
        Err03Counts {
            emitted: 0,
            unknown_pts: 0,
            duplicates: 0,
            reorders: 0,
            expected_next: 1,
            seen: HashSet::new(),
        }
    }
}

/// Drains via `backend_construct::receive_one` until `Again`/`Eof`,
/// recording ERR-03 counts (docs/WIRE.md §8) — the same
/// transfer/recovered-ordinal helpers `decoder-experiment` uses, reused here
/// per the extraction's purpose.
fn drain_and_count(
    decoder: &mut ffmpeg_next::decoder::Video,
    frame: &mut ffmpeg_next::frame::Video,
    is_hw: bool,
    counts: &mut Err03Counts,
) -> std::result::Result<(), String> {
    loop {
        match backend_construct::receive_one(decoder, frame)? {
            backend_construct::ReceiveOutcome::Frame => {
                if is_hw {
                    backend_construct::transfer_hw_frame(frame)?;
                }
                counts.emitted += 1;
                match backend_construct::recovered_ordinal(frame) {
                    None => counts.unknown_pts += 1,
                    Some(got) => {
                        if got == counts.expected_next {
                            counts.expected_next += 1;
                        } else if counts.seen.contains(&got) {
                            counts.duplicates += 1;
                        } else {
                            counts.reorders += 1;
                            counts.expected_next = got + 1;
                        }
                        counts.seen.insert(got);
                    }
                }
            }
            backend_construct::ReceiveOutcome::Again | backend_construct::ReceiveOutcome::Eof => {
                return Ok(())
            }
        }
    }
}

fn decode_sample_check(handle: Option<backend_construct::DecoderHandle>, sample_path: &Path) -> (Value, i32) {
    if !sample_path.exists() {
        let entry = json!({
            "name": "decode_sample", "ok": false,
            "sample_path": sample_path.display().to_string(),
            "detail": "sample file not found",
            "hint": "run tools/gen_sample_hevc.sh to generate the bundled A0.0 sample",
        });
        return (entry, EXIT_ENVIRONMENT);
    }

    let mut handle = match handle {
        Some(h) => h,
        None => {
            let entry = json!({
                "name": "decode_sample", "ok": false,
                "sample_path": sample_path.display().to_string(),
                "detail": "skipped: backend_open failed",
            });
            // backend_open already escalated the exit code for this run.
            return (entry, EXIT_PASS);
        }
    };

    let mut ictx = match ffmpeg_next::format::input(sample_path) {
        Ok(i) => i,
        Err(e) => {
            let entry = json!({
                "name": "decode_sample", "ok": false,
                "sample_path": sample_path.display().to_string(),
                "detail": format!("avformat_open_input failed: {e}"),
                "error_code": "REQUIRED_NATIVE_API",
            });
            return (entry, EXIT_NATIVE);
        }
    };
    let video_stream_index = match ictx.streams().best(ffmpeg_next::media::Type::Video) {
        Some(s) => s.index(),
        None => {
            let entry = json!({
                "name": "decode_sample", "ok": false,
                "detail": "av_find_best_stream found no video stream",
                "error_code": "REQUIRED_NATIVE_API",
            });
            return (entry, EXIT_NATIVE);
        }
    };

    const SAMPLE_AU_LIMIT: u64 = 60;
    const MAX_EAGAIN_CYCLES: u32 = 64;
    let is_hw = handle.is_hw;
    let mut counts = Err03Counts::new();
    let mut frame = ffmpeg_next::frame::Video::empty();
    let mut ordinal: i64 = 0;
    let mut submitted: u64 = 0;
    let mut eagain_retries: u64 = 0;
    let mut fatal: Option<String> = None;

    'outer: for (stream, mut packet) in ictx.packets() {
        if stream.index() != video_stream_index {
            continue;
        }
        if submitted >= SAMPLE_AU_LIMIT {
            break;
        }

        ordinal += 1;
        packet.set_pts(Some(ordinal));

        // Send-EAGAIN retry per plan §7: retain the packet, drain, resubmit.
        let mut cycles = 0u32;
        loop {
            match handle.decoder.send_packet(&packet) {
                Ok(()) => break,
                Err(ffmpeg_next::Error::Other { errno }) if errno == libc::EAGAIN => {
                    eagain_retries += 1;
                    cycles += 1;
                    if cycles > MAX_EAGAIN_CYCLES {
                        fatal = Some("send_packet EAGAIN did not converge".to_string());
                        break 'outer;
                    }
                    if let Err(e) = drain_and_count(&mut handle.decoder, &mut frame, is_hw, &mut counts) {
                        fatal = Some(e);
                        break 'outer;
                    }
                }
                Err(e) => {
                    fatal = Some(format!("send_packet fatal error: {e}"));
                    break 'outer;
                }
            }
        }
        submitted += 1;

        if let Err(e) = drain_and_count(&mut handle.decoder, &mut frame, is_hw, &mut counts) {
            fatal = Some(e);
            break 'outer;
        }
    }

    if fatal.is_none() {
        loop {
            match handle.decoder.send_eof() {
                Ok(()) => break,
                Err(ffmpeg_next::Error::Other { errno }) if errno == libc::EAGAIN => {
                    eagain_retries += 1;
                    if let Err(e) = drain_and_count(&mut handle.decoder, &mut frame, is_hw, &mut counts) {
                        fatal = Some(e);
                        break;
                    }
                }
                Err(ffmpeg_next::Error::Eof) => break,
                Err(e) => {
                    fatal = Some(format!("send_eof fatal error: {e}"));
                    break;
                }
            }
        }
        if fatal.is_none() {
            if let Err(e) = drain_and_count(&mut handle.decoder, &mut frame, is_hw, &mut counts) {
                fatal = Some(e);
            }
        }
    }

    let ok = fatal.is_none()
        && counts.unknown_pts == 0
        && counts.duplicates == 0
        && counts.reorders == 0
        && submitted > 0
        && counts.emitted > 0;

    let mut entry = json!({
        "name": "decode_sample", "ok": ok,
        "sample_path": sample_path.display().to_string(),
        "frames_submitted": submitted,
        "frames_emitted": counts.emitted,
        "unknown_pts": counts.unknown_pts,
        "duplicates": counts.duplicates,
        "reorders": counts.reorders,
        "eagain_retries": eagain_retries,
    });
    if let Some(reason) = &fatal {
        entry["detail"] = json!(reason);
        entry["error_code"] = json!("REQUIRED_NATIVE_API");
    }

    (entry, if ok { EXIT_PASS } else { EXIT_NATIVE })
}

// ---------------------------------------------------------------------
// d. sdl_texture / e. input_capability
// ---------------------------------------------------------------------

/// Facts gathered by one SDL probe pass, shared by `sdl_texture_check`
/// (which turns them into a pass/fail check) and `input_capability_check`
/// (informational; `ok` mirrors `sdl_texture`).
struct SdlProbe {
    init_ok: bool,
    video_ok: bool,
    num_displays: i32,
    window_ok: bool,
    canvas_ok: bool,
    iyuv_ok: bool,
    nv12_ok: bool,
    error: Option<String>,
}

fn probe_sdl() -> SdlProbe {
    let sdl = match sdl2::init() {
        Ok(s) => s,
        Err(e) => {
            return SdlProbe {
                init_ok: false, video_ok: false, num_displays: 0,
                window_ok: false, canvas_ok: false, iyuv_ok: false, nv12_ok: false,
                error: Some(format!("sdl2::init failed: {e}")),
            }
        }
    };
    let video = match sdl.video() {
        Ok(v) => v,
        Err(e) => {
            return SdlProbe {
                init_ok: true, video_ok: false, num_displays: 0,
                window_ok: false, canvas_ok: false, iyuv_ok: false, nv12_ok: false,
                error: Some(format!("SDL video subsystem init failed (headless/no DISPLAY?): {e}")),
            }
        }
    };
    let num_displays = video.num_video_displays().unwrap_or(0);

    let window = match video.window("RESC Doctor", 64, 64).hidden().build() {
        Ok(w) => w,
        Err(e) => {
            return SdlProbe {
                init_ok: true, video_ok: true, num_displays,
                window_ok: false, canvas_ok: false, iyuv_ok: false, nv12_ok: false,
                error: Some(format!("hidden window creation failed: {e}")),
            }
        }
    };

    let canvas = match window.into_canvas().accelerated().build() {
        Ok(c) => c,
        Err(e) => {
            return SdlProbe {
                init_ok: true, video_ok: true, num_displays,
                window_ok: true, canvas_ok: false, iyuv_ok: false, nv12_ok: false,
                error: Some(format!("renderer/canvas creation failed: {e}")),
            }
        }
    };
    let texture_creator = canvas.texture_creator();
    let iyuv_ok = texture_creator
        .create_texture_streaming(sdl2::pixels::PixelFormatEnum::IYUV, 1080, 1920)
        .is_ok();
    let nv12_ok = texture_creator
        .create_texture_streaming(sdl2::pixels::PixelFormatEnum::NV12, 1080, 1920)
        .is_ok();

    SdlProbe {
        init_ok: true, video_ok: true, num_displays,
        window_ok: true, canvas_ok: true, iyuv_ok, nv12_ok,
        error: None,
    }
}

fn sdl_texture_check(p: &SdlProbe) -> (Value, i32) {
    let ok = p.init_ok && p.video_ok && p.window_ok && p.canvas_ok && p.iyuv_ok;
    let mut entry = json!({
        "name": "sdl_texture", "ok": ok,
        "sdl_init_ok": p.init_ok,
        "sdl_video_ok": p.video_ok,
        "iyuv_streaming_1080x1920": p.iyuv_ok,
        "nv12_streaming_1080x1920": p.nv12_ok,
    });
    if let Some(e) = &p.error {
        entry["detail"] = json!(e);
    }
    // SDL video subsystem failing outright (headless/no DISPLAY) is an
    // environment fact; a live display that then fails a native SDL call
    // (window/canvas/texture) is a native-API failure.
    let exit = if !p.video_ok {
        EXIT_ENVIRONMENT
    } else if ok {
        EXIT_PASS
    } else {
        EXIT_NATIVE
    };
    (entry, exit)
}

fn input_capability_check(p: &SdlProbe) -> Value {
    let ok = p.init_ok && p.video_ok && p.window_ok && p.canvas_ok && p.iyuv_ok; // mirrors sdl_texture
    let mut subsystems: Vec<&str> = Vec::new();
    if p.init_ok {
        subsystems.push("events");
    }
    if p.video_ok {
        subsystems.push("video");
    }
    json!({
        "name": "input_capability", "ok": ok,
        "initialized_subsystems": subsystems,
        "display_connection": p.video_ok && p.num_displays > 0,
        "num_video_displays": p.num_displays,
    })
}

// ---------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------

fn write_report(log: &diagnostics::RescLog, checks: &[Value], exit_code: i32) {
    let report = json!({
        "doctor_report_v": 1,
        "side": "client",
        "profile_id": diagnostics::profile::PROFILE_ID,
        "ts_wall": diagnostics::jsonl::ts_wall(),
        "checks": checks,
        "exit_code": exit_code,
    });

    if let Ok(pretty) = serde_json::to_string_pretty(&report) {
        println!("{pretty}");
    }

    let dir = diagnostics::state_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("RESC doctor: failed to create {}: {e}", dir.display());
    } else {
        let path = dir.join("doctor_client.json");
        let compact = serde_json::to_string(&report).unwrap_or_default();
        match std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut f) => {
                use std::io::Write;
                if let Err(e) = f.write_all(compact.as_bytes()) {
                    eprintln!("RESC doctor: failed to write {}: {e}", path.display());
                }
            }
            Err(e) => eprintln!("RESC doctor: failed to open {}: {e}", path.display()),
        }
    }

    log.event(
        "doctor_complete",
        COMPONENT,
        json!({
            "exit_code": exit_code,
            "checks_summary": checks.iter().map(|c| json!({
                "name": c.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
                "ok": c.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
            })).collect::<Vec<_>>(),
        }),
    );
}
