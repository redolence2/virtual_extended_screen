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
//! c. `decode_sample` — demuxes `--sample` and decodes the COMPLETE bundled
//!    sample with EOF/tail drain (A00_REMEDIATION_PLAN.md §5 R3a). The exit
//!    predicate requires submitted == emitted, exact ordinal recovery (zero
//!    unknown/duplicate/reordered outputs via the backend's
//!    recovered-ordinal mechanism), and nonzero frames — every one of
//!    those clauses gates the exit code.
//! d. `sdl_texture` — hidden window + renderer, then the *candidate's*
//!    actual SDL texture UPDATE path driven with a real decoded frame's
//!    planes/strides (§3 "SDL texture doctor rule": `sw1-lowdelay` →
//!    IYUV/yuv420p via `SDL_UpdateYUVTexture`, `cuvid-lowdelay` → NV12 via
//!    `SDL_UpdateNVTexture`, both per docs/WIRE.md §7). The candidate's
//!    path is exit-affecting; the other format is probed too but recorded
//!    informational-only.
//! e. `input_capability` — informational SDL subsystem/display report.
//!
//! Exit codes (v11 §11.4): 0 = pass, 2 = environment, 3 = native-API. (Code
//! 4, peer-diagnostic failure, belongs to `--diagnose-peer`, which this
//! doctor does not implement.)
//!
//! `RESC_DOCTOR_INJECT=<check-id>` (A00_REMEDIATION_PLAN.md §5 R3a
//! failure-injection test seam, mirroring `mac-host`'s `HostDoctor.swift`
//! `injectedCheck`): forces exactly one named check to report failure
//! through its normal reporting path — same JSON shape, same
//! `RescLog`/report-write, same exit-code contribution a genuine failure
//! would produce, always tagged `"injected": true` so evidence never looks
//! like an unexplained genuine failure. check-ids: `open`, `decode`,
//! `ordinals`, `tail`, `texture`. Doctor-mode only; never read by the real
//! client.

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

    // RESC_DOCTOR_INJECT=<check-id> — read once, threaded through the
    // checks below that support it (see the module doc comment).
    let injected_check_owned = std::env::var("RESC_DOCTOR_INJECT").ok();
    let injected_check = injected_check_owned.as_deref();

    let mut checks: Vec<Value> = Vec::new();
    let mut exit_code = EXIT_PASS;

    let (entry, contribution) = environment_check();
    checks.push(entry);
    exit_code = exit_code.max(contribution);

    let (entry, contribution, handle) = backend_open_check(doctor_backend, log, injected_check);
    checks.push(entry);
    exit_code = exit_code.max(contribution);

    let (entry, contribution, captured) = decode_sample_check(handle, sample_path, injected_check);
    checks.push(entry);
    exit_code = exit_code.max(contribution);

    let probe = probe_sdl(captured.as_ref());
    let (entry, contribution) = sdl_texture_check(&probe, doctor_backend, injected_check);
    checks.push(entry);
    exit_code = exit_code.max(contribution);

    checks.push(input_capability_check(&probe));

    write_report(log, &checks, exit_code);
    // The doctor logs only a couple of RescLog events per run and its caller
    // (main.rs) calls std::process::exit right after this returns, which
    // skips Drop — force the buffered records out now or they're lost
    // (RescLog::event only auto-flushes every 32 records). Every path
    // through this function — success, a genuine failure in any check
    // above, or any RESC_DOCTOR_INJECT case — reaches this same tail; there
    // is no early return anywhere above, so this one call covers every
    // return path (A00_REMEDIATION_PLAN.md §5 R3a).
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
    injected_check: Option<&str>,
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
            if injected_check == Some("open") {
                // Failure injection (A00_REMEDIATION_PLAN.md §5 R3a test
                // seam, mirroring HostDoctor.swift's `api`/`display`
                // idiom): report failure through the exact same shape a
                // genuine backend_open failure would — including handing
                // back `None`, so decode_sample_check takes its existing
                // "skipped: backend_open failed" path — even though the
                // real open_decoder call above succeeded.
                let entry = json!({
                    "name": "backend_open", "ok": false,
                    "candidate": doctor_backend, "is_hw": handle.is_hw,
                    "detail": "RESC_DOCTOR_INJECT=open: forced failure (real backend_open succeeded)",
                    "error_code": "REQUIRED_NATIVE_API",
                    "injected": true,
                });
                return (entry, EXIT_NATIVE, None);
            }
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

/// The complete bundled sample, fed with `pts = ordinal` exactly like
/// `decoder-experiment` (A00_REMEDIATION_PLAN.md §5 R3a: "decodes the
/// complete bundled sample with EOF/tail drain" — no truncated prefix).
/// ERR-03 bookkeeping only (no timing/lag stats — the doctor just needs
/// pass/fail evidence).
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

/// A real decoded frame's plane bytes/strides/dimensions, captured once
/// during the drain loop below — the first successfully decoded frame is
/// enough (already run through `transfer_hw_frame` for `cuvid-lowdelay`, so
/// it's CPU-visible either way). Neither `frame` nor a hw-transfer's output
/// outlives the loop, so the plane bytes are copied out here so
/// `sdl_texture_check` can drive the candidate's actual SDL texture UPDATE
/// call against real data instead of just creating an empty texture
/// (A00_REMEDIATION_PLAN.md §5 R3a: "Creation alone is insufficient — the
/// update call with real plane data/strides must succeed").
struct CapturedPlanes {
    width: u32,
    height: u32,
    /// One entry per plane, in ffmpeg's own plane order: 3 planes (Y, U, V)
    /// for `sw1-lowdelay`'s yuv420p; 2 planes (Y, interleaved UV) for
    /// `cuvid-lowdelay`'s post-transfer NV12 (docs/WIRE.md §7).
    planes: Vec<Vec<u8>>,
    strides: Vec<usize>,
}

impl CapturedPlanes {
    fn from_frame(frame: &ffmpeg_next::frame::Video) -> Self {
        let n = frame.planes();
        let mut planes = Vec::with_capacity(n);
        let mut strides = Vec::with_capacity(n);
        for i in 0..n {
            planes.push(frame.data(i).to_vec());
            strides.push(frame.stride(i));
        }
        CapturedPlanes { width: frame.width(), height: frame.height(), planes, strides }
    }
}

/// Drains via `backend_construct::receive_one` until `Again`/`Eof`,
/// recording ERR-03 counts (docs/WIRE.md §8) — the same
/// transfer/recovered-ordinal helpers `decoder-experiment` uses, reused here
/// per the extraction's purpose. Also opportunistically fills `captured`
/// (see `CapturedPlanes`) from the first frame seen, if the caller hasn't
/// already captured one.
fn drain_and_count(
    decoder: &mut ffmpeg_next::decoder::Video,
    frame: &mut ffmpeg_next::frame::Video,
    is_hw: bool,
    counts: &mut Err03Counts,
    captured: &mut Option<CapturedPlanes>,
) -> std::result::Result<(), String> {
    loop {
        match backend_construct::receive_one(decoder, frame)? {
            backend_construct::ReceiveOutcome::Frame => {
                if is_hw {
                    let sw_frame = backend_construct::transfer_hw_frame(frame)?;
                    if captured.is_none() {
                        *captured = Some(CapturedPlanes::from_frame(&sw_frame));
                    }
                } else if captured.is_none() {
                    *captured = Some(CapturedPlanes::from_frame(frame));
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

fn decode_sample_check(
    handle: Option<backend_construct::DecoderHandle>,
    sample_path: &Path,
    injected_check: Option<&str>,
) -> (Value, i32, Option<CapturedPlanes>) {
    if !sample_path.exists() {
        let entry = json!({
            "name": "decode_sample", "ok": false,
            "sample_path": sample_path.display().to_string(),
            "detail": "sample file not found",
            "hint": "run tools/gen_sample_hevc.sh to generate the bundled A0.0 sample",
        });
        return (entry, EXIT_ENVIRONMENT, None);
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
            return (entry, EXIT_PASS, None);
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
            return (entry, EXIT_NATIVE, None);
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
            return (entry, EXIT_NATIVE, None);
        }
    };

    const MAX_EAGAIN_CYCLES: u32 = 64;
    let is_hw = handle.is_hw;
    let mut counts = Err03Counts::new();
    let mut frame = ffmpeg_next::frame::Video::empty();
    let mut captured: Option<CapturedPlanes> = None;
    let mut ordinal: i64 = 0;
    let mut submitted: u64 = 0;
    let mut eagain_retries: u64 = 0;
    let mut fatal: Option<String> = None;

    // Complete bundled sample, not a truncated prefix
    // (A00_REMEDIATION_PLAN.md §5 R3a) — every video AU in the file is
    // submitted.
    'outer: for (stream, mut packet) in ictx.packets() {
        if stream.index() != video_stream_index {
            continue;
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
                    if let Err(e) =
                        drain_and_count(&mut handle.decoder, &mut frame, is_hw, &mut counts, &mut captured)
                    {
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

        if let Err(e) = drain_and_count(&mut handle.decoder, &mut frame, is_hw, &mut counts, &mut captured) {
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
                    if let Err(e) =
                        drain_and_count(&mut handle.decoder, &mut frame, is_hw, &mut counts, &mut captured)
                    {
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
            if let Err(e) = drain_and_count(&mut handle.decoder, &mut frame, is_hw, &mut counts, &mut captured) {
                fatal = Some(e);
            }
        }
    }

    // Every clause of the exit predicate named explicitly
    // (A00_REMEDIATION_PLAN.md §5 R3a): submitted == emitted, exact ordinal
    // recovery (zero unknown/duplicate/reordered), nonzero frames — plus
    // the pre-existing "no fatal error" gate. RESC_DOCTOR_INJECT=decode/
    // ordinals/tail (the failure-injection test seam, mirroring
    // mac-host's HostDoctor.swift) forces one real-passing clause false at
    // a time, proving each actually gates the exit code rather than being
    // vacuously always-true.
    let real_decode_ok = fatal.is_none();
    let real_ordinals_ok = counts.unknown_pts == 0 && counts.duplicates == 0 && counts.reorders == 0;
    let real_tail_ok = submitted == counts.emitted;
    let nonzero_frames = submitted > 0 && counts.emitted > 0;

    let decode_ok = real_decode_ok && injected_check != Some("decode");
    let ordinals_ok = real_ordinals_ok && injected_check != Some("ordinals");
    let tail_ok = real_tail_ok && injected_check != Some("tail");
    let ok = decode_ok && ordinals_ok && tail_ok && nonzero_frames;

    let mut entry = json!({
        "name": "decode_sample", "ok": ok,
        "sample_path": sample_path.display().to_string(),
        "frames_submitted": submitted,
        "frames_emitted": counts.emitted,
        "submitted_eq_emitted": real_tail_ok,
        "unknown_pts": counts.unknown_pts,
        "duplicates": counts.duplicates,
        "reorders": counts.reorders,
        "eagain_retries": eagain_retries,
    });
    if let Some(reason) = &fatal {
        entry["detail"] = json!(reason);
        entry["error_code"] = json!("REQUIRED_NATIVE_API");
    }

    let injected_id = ["decode", "ordinals", "tail"]
        .into_iter()
        .find(|id| injected_check == Some(*id));
    if let Some(id) = injected_id {
        entry["injected"] = json!(true);
        if entry.get("detail").is_none() {
            let real_ok = real_decode_ok && real_ordinals_ok && real_tail_ok && nonzero_frames;
            entry["detail"] = json!(format!(
                "RESC_DOCTOR_INJECT={id}: forced failure (real decode_sample result: {real_ok})"
            ));
            entry["error_code"] = json!("REQUIRED_NATIVE_API");
        }
    }

    (entry, if ok { EXIT_PASS } else { EXIT_NATIVE }, captured)
}

// ---------------------------------------------------------------------
// d. sdl_texture / e. input_capability
// ---------------------------------------------------------------------

/// Facts + outcomes from one SDL probe pass: init/video/window/canvas
/// health, the SDL video driver actually in use, and both candidate
/// formats' texture CREATE + UPDATE outcomes. `sdl_texture_check` turns the
/// *requested `--doctor-backend` candidate's* format into the exit-
/// affecting verdict per §3's "SDL texture doctor rule"; the other format
/// stays informational (see that function). `input_capability_check` only
/// reads the init/video/window/canvas facts (deliberately candidate-
/// agnostic).
struct SdlProbe {
    init_ok: bool,
    video_ok: bool,
    video_driver: Option<&'static str>,
    num_displays: i32,
    window_ok: bool,
    canvas_ok: bool,
    iyuv_create_ok: bool,
    iyuv_update_ok: bool,
    iyuv_detail: Option<String>,
    nv12_create_ok: bool,
    nv12_update_ok: bool,
    nv12_detail: Option<String>,
    error: Option<String>,
}

/// `captured` is the real decoded frame (if any) `decode_sample_check`
/// produced — its planes/strides drive the texture UPDATE calls below, not
/// just their creation (A00_REMEDIATION_PLAN.md §5 R3a).
fn probe_sdl(captured: Option<&CapturedPlanes>) -> SdlProbe {
    // The doctor must work headless (§5 R3a). Default SDL to the "dummy"
    // video driver when the caller hasn't already chosen one, so a bare
    // `--doctor` run on a display-less box still exercises the texture
    // probes below instead of failing SDL init outright; an operator-set
    // SDL_VIDEODRIVER (e.g. a real x11/wayland run, or the verification
    // scripts' own explicit SDL_VIDEODRIVER=dummy) is never overridden.
    //
    // SAFETY: this runs at the very top of the doctor's one SDL entry
    // point, invoked before any of main.rs's streaming threads are spawned
    // (the doctor branch exits the process immediately after `run`
    // returns), so no other code in this process reads/writes the
    // environment concurrently with this call.
    if std::env::var_os("SDL_VIDEODRIVER").is_none() {
        unsafe {
            std::env::set_var("SDL_VIDEODRIVER", "dummy");
        }
    }

    let sdl = match sdl2::init() {
        Ok(s) => s,
        Err(e) => {
            return SdlProbe {
                init_ok: false, video_ok: false, video_driver: None, num_displays: 0,
                window_ok: false, canvas_ok: false,
                iyuv_create_ok: false, iyuv_update_ok: false, iyuv_detail: None,
                nv12_create_ok: false, nv12_update_ok: false, nv12_detail: None,
                error: Some(format!("sdl2::init failed: {e}")),
            }
        }
    };
    let video = match sdl.video() {
        Ok(v) => v,
        Err(e) => {
            return SdlProbe {
                init_ok: true, video_ok: false, video_driver: None, num_displays: 0,
                window_ok: false, canvas_ok: false,
                iyuv_create_ok: false, iyuv_update_ok: false, iyuv_detail: None,
                nv12_create_ok: false, nv12_update_ok: false, nv12_detail: None,
                error: Some(format!("SDL video subsystem init failed (headless/no DISPLAY?): {e}")),
            }
        }
    };
    // Recorded regardless of outcome (§5 R3a: "record which video driver
    // was used") — e.g. "dummy" on a headless box.
    let video_driver = Some(video.current_video_driver());
    let num_displays = video.num_video_displays().unwrap_or(0);

    let window = match video.window("RESC Doctor", 64, 64).hidden().build() {
        Ok(w) => w,
        Err(e) => {
            return SdlProbe {
                init_ok: true, video_ok: true, video_driver, num_displays,
                window_ok: false, canvas_ok: false,
                iyuv_create_ok: false, iyuv_update_ok: false, iyuv_detail: None,
                nv12_create_ok: false, nv12_update_ok: false, nv12_detail: None,
                error: Some(format!("hidden window creation failed: {e}")),
            }
        }
    };

    // Deliberately not `.accelerated()`: the real renderer
    // (`crates/renderer/src/lib.rs`) requests it because it runs on a real
    // display with a real GPU, but the "dummy" driver this doctor defaults
    // to for headless operation has no GPU-backed renderer at all —
    // requesting one would fail `SDL_CreateRenderer` outright ("Couldn't
    // find matching render driver") before ever reaching the texture
    // probes below. This doctor only needs *some* working renderer to
    // create/update streaming textures against, not an accelerated one.
    let canvas = match window.into_canvas().build() {
        Ok(c) => c,
        Err(e) => {
            return SdlProbe {
                init_ok: true, video_ok: true, video_driver, num_displays,
                window_ok: true, canvas_ok: false,
                iyuv_create_ok: false, iyuv_update_ok: false, iyuv_detail: None,
                nv12_create_ok: false, nv12_update_ok: false, nv12_detail: None,
                error: Some(format!("renderer/canvas creation failed: {e}")),
            }
        }
    };
    let texture_creator = canvas.texture_creator();

    // Real dimensions from the captured decoded frame when available (the
    // normal case); the sample's nominal profile dimensions otherwise, so
    // the create-only probe still runs even when decode_sample_check never
    // produced a frame (RESC_DOCTOR_INJECT=open/decode, or a genuine decode
    // failure) — the update calls below separately gate on
    // `captured.is_some()` regardless.
    let (w, h) = captured.map(|c| (c.width, c.height)).unwrap_or((1080, 1920));

    let (iyuv_create_ok, iyuv_update_ok, iyuv_detail) = probe_iyuv_texture(&texture_creator, w, h, captured);
    let (nv12_create_ok, nv12_update_ok, nv12_detail) = probe_nv12_texture(&texture_creator, w, h, captured);

    SdlProbe {
        init_ok: true, video_ok: true, video_driver, num_displays,
        window_ok: true, canvas_ok: true,
        iyuv_create_ok, iyuv_update_ok, iyuv_detail,
        nv12_create_ok, nv12_update_ok, nv12_detail,
        error: None,
    }
}

/// Creates an IYUV (yuv420p, 3-plane) streaming texture and, when a real
/// decoded frame was captured, drives it through `SDL_UpdateYUVTexture`
/// (rust-sdl2's safe `update_yuv` wrapper) using that frame's actual Y/U/V
/// planes and strides — the same call the real renderer uses for
/// `sw1-lowdelay`'s yuv420p output (`crates/renderer/src/lib.rs`'s
/// `PersistentTexture::update_and_copy`, docs/WIRE.md §7). Creation alone
/// does not prove the update call itself works (A00_REMEDIATION_PLAN.md §5
/// R3a: "Creation alone is insufficient"), so `update_ok` — not
/// `create_ok` — is what `sdl_texture_check` treats as this format's real
/// pass/fail signal. Returns `(create_ok, update_ok, detail)`.
fn probe_iyuv_texture(
    tc: &sdl2::render::TextureCreator<sdl2::video::WindowContext>,
    w: u32,
    h: u32,
    captured: Option<&CapturedPlanes>,
) -> (bool, bool, Option<String>) {
    let mut texture = match tc.create_texture_streaming(sdl2::pixels::PixelFormatEnum::IYUV, w, h) {
        Ok(t) => t,
        Err(e) => return (false, false, Some(format!("create_texture_streaming(IYUV) failed: {e}"))),
    };

    // Exact match, not `>=`: a 2-plane NV12 capture must never be reused
    // here as if it were 3-plane yuv420p data (and vice versa in
    // `probe_nv12_texture` below) — that would silently feed one format's
    // decoded bytes into the other's update call and still report
    // `update_ok: true`, which is not evidence the *labeled* format's path
    // actually works. When the candidate is `cuvid-lowdelay`, this IYUV
    // probe is the informational side and correctly reports "skipped"
    // rather than misusing the NV12 capture's planes.
    let c = match captured {
        Some(c) if c.planes.len() == 3 => c,
        Some(c) => {
            return (
                true, false,
                Some(format!("captured frame has {} planes, not the 3 IYUV needs", c.planes.len())),
            )
        }
        None => return (true, false, Some("skipped: no decoded frame available".to_string())),
    };

    match texture.update_yuv(
        None::<sdl2::rect::Rect>,
        &c.planes[0], c.strides[0],
        &c.planes[1], c.strides[1],
        &c.planes[2], c.strides[2],
    ) {
        Ok(()) => (true, true, None),
        Err(e) => (true, false, Some(format!("SDL_UpdateYUVTexture failed: {e}"))),
    }
}

/// Creates an NV12 (2-plane) streaming texture and, when a real decoded
/// frame was captured, drives it through `SDL_UpdateNVTexture` using that
/// frame's actual Y/UV planes and strides — `cuvid-lowdelay`'s path (the
/// transferred frame's two planes, post `av_hwframe_transfer_data`,
/// docs/WIRE.md §7). rust-sdl2 0.37 has no safe wrapper for this 2-plane
/// update (only the 3-plane `update_yuv`), so this reaches into
/// `sdl2::sys` directly — the same pattern `renderer/src/lib.rs` already
/// uses for `SDL_UpdateYUVTexture`. Returns `(create_ok, update_ok,
/// detail)`.
fn probe_nv12_texture(
    tc: &sdl2::render::TextureCreator<sdl2::video::WindowContext>,
    w: u32,
    h: u32,
    captured: Option<&CapturedPlanes>,
) -> (bool, bool, Option<String>) {
    let texture = match tc.create_texture_streaming(sdl2::pixels::PixelFormatEnum::NV12, w, h) {
        Ok(t) => t,
        Err(e) => return (false, false, Some(format!("create_texture_streaming(NV12) failed: {e}"))),
    };

    // Exact match, not `>=` — see `probe_iyuv_texture`'s matching comment:
    // a 3-plane yuv420p capture (`sw1-lowdelay`) must never be reused here
    // as 2-plane NV12 data. Without this, `>= 2` would happily accept a
    // yuv420p capture and feed its separate Y/U planes to
    // `SDL_UpdateNVTexture` as if plane 1 were interleaved UV — the call
    // still "succeeds" (SDL only validates buffer sizes, not semantic
    // content), which would misreport `update_ok: true` for a format that
    // was never actually exercised with real NV12 data.
    let c = match captured {
        Some(c) if c.planes.len() == 2 => c,
        Some(c) => {
            return (
                true, false,
                Some(format!("captured frame has {} planes, not the 2 NV12 needs", c.planes.len())),
            )
        }
        None => return (true, false, Some("skipped: no decoded frame available".to_string())),
    };

    // SAFETY: `texture.raw()` is a valid `SDL_Texture*` for the duration of
    // this call; `c.planes[0]`/`[1]` are real, fully-owned byte buffers at
    // least `stride * plane_height` long (copied verbatim from ffmpeg's own
    // decoded-frame plane data in `CapturedPlanes::from_frame`); a null
    // rect means "update the whole texture" per SDL's own contract.
    let ret = unsafe {
        sdl2::sys::SDL_UpdateNVTexture(
            texture.raw(),
            std::ptr::null(),
            c.planes[0].as_ptr(), c.strides[0] as i32,
            c.planes[1].as_ptr(), c.strides[1] as i32,
        )
    };
    if ret == 0 {
        (true, true, None)
    } else {
        (true, false, Some(format!("SDL_UpdateNVTexture returned {ret}")))
    }
}

/// Turns one `SdlProbe` into the `sdl_texture` check. §3 "SDL texture
/// doctor rule": the *candidate's* required texture UPDATE path is
/// exit-affecting (`sw1-lowdelay` → IYUV, `cuvid-lowdelay` → NV12); the
/// other format is probed too but stays informational-only, clearly
/// labeled via each format's own `exit_affecting` field, never itself
/// exit-affecting.
fn sdl_texture_check(p: &SdlProbe, doctor_backend: &str, injected_check: Option<&str>) -> (Value, i32) {
    let candidate_is_cuvid = doctor_backend == "cuvid-lowdelay";
    let (candidate_format, candidate_create_ok, candidate_update_ok, candidate_detail) = if candidate_is_cuvid {
        ("nv12", p.nv12_create_ok, p.nv12_update_ok, &p.nv12_detail)
    } else {
        ("iyuv", p.iyuv_create_ok, p.iyuv_update_ok, &p.iyuv_detail)
    };

    let injected = injected_check == Some("texture");
    let real_ok = p.init_ok && p.video_ok && p.window_ok && p.canvas_ok && candidate_update_ok;
    let ok = real_ok && !injected;

    let mut entry = json!({
        "name": "sdl_texture", "ok": ok,
        "sdl_init_ok": p.init_ok,
        "sdl_video_ok": p.video_ok,
        "sdl_video_driver": p.video_driver,
        "candidate": doctor_backend,
        "candidate_format": candidate_format,
        "candidate_texture_create_ok": candidate_create_ok,
        "candidate_texture_update_ok": candidate_update_ok,
        "iyuv": {
            "create_ok": p.iyuv_create_ok,
            "update_ok": p.iyuv_update_ok,
            "detail": p.iyuv_detail,
            "exit_affecting": !candidate_is_cuvid,
        },
        "nv12": {
            "create_ok": p.nv12_create_ok,
            "update_ok": p.nv12_update_ok,
            "detail": p.nv12_detail,
            "exit_affecting": candidate_is_cuvid,
        },
    });
    if let Some(e) = &p.error {
        entry["detail"] = json!(e);
    } else if !candidate_update_ok {
        if let Some(d) = candidate_detail {
            entry["detail"] = json!(d);
        }
    }
    if injected {
        entry["injected"] = json!(true);
        if entry.get("detail").is_none() {
            entry["detail"] = json!(format!(
                "RESC_DOCTOR_INJECT=texture: forced failure (real {candidate_format} texture-update result: {real_ok})"
            ));
        }
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
    // Deliberately candidate-agnostic, unlike sdl_texture (which is now
    // parameterized by --doctor-backend, §3 "SDL texture doctor rule") —
    // this check reports SDL subsystem/display facts only, never
    // texture-format specifics.
    let ok = p.init_ok && p.video_ok && p.window_ok && p.canvas_ok;
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
                } else if let Err(e) = f.sync_all() {
                    // Synchronous durability (A00_REMEDIATION_PLAN.md §5
                    // R3a: "fsync the report JSON file write") — without
                    // this, the write above can still be sitting in the
                    // page cache, not stable storage, when the process
                    // exits right after this function returns.
                    eprintln!("RESC doctor: failed to fsync {}: {e}", path.display());
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
