//! Startup environment record (IMPLEMENTATION_PLAN_V11.md §11.2).
//!
//! Logs one `startup_environment` record describing what this process actually is:
//! build commit/dirty state, the protocol version genuinely in use, and kernel info.
//! `ffmpeg`/SDL/NVIDIA versions are native-call evidence that belongs to the client
//! doctor (§11.4) once it exists, not to this crate — this record reserves their key
//! with a `"pending_doctor"` placeholder so the shape is stable from A0.0 onward.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::RescLog;

const COMPONENT: &str = "environment";

/// Logs the one-shot `startup_environment` record. Call once, early in `main()`.
pub fn emit(log: &RescLog) {
    let repo = workspace_root();

    log.event(
        "startup_environment",
        COMPONENT,
        json!({
            "build_commit": build_commit(&repo),
            "build_dirty": build_dirty(&repo),
            // The generated v3 handshake (protocol::resc_v3, IMPLEMENTATION_PLAN_V11.md
            // §4) is not wired into main.rs yet — the wire this process actually speaks
            // at A0.0 is still v1 (protocol::constants::PROTOCOL_VERSION). Report the
            // truth of what's running, not the target architecture.
            "protocol_version": 1,
            "kernel": kernel_info(),
            "ffmpeg_version": "pending_doctor",
            "sdl_version": "pending_doctor",
            "nvidia_driver_version": "pending_doctor",
        }),
    );
}

/// `CARGO_MANIFEST_DIR` (this crate: `<workspace>/crates/diagnostics`) trimmed up two
/// levels to the Cargo workspace root. `git -C <any-subdir-of-a-repo>` walks up to find
/// the enclosing `.git` on its own, so this need not be the true top-level checkout —
/// the actual RESC repo root, one level further up again, is found the same way.
fn workspace_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent() // .../ubuntu-client/crates
        .and_then(Path::parent) // .../ubuntu-client
        .map(Path::to_path_buf)
        .unwrap_or_else(|| manifest_dir.to_path_buf())
}

/// `RESC_BUILD_COMMIT` env var (checked at *runtime*, not baked in via `env!`, since
/// this isn't set at compile time in the current build) if set and non-empty; else
/// `git -C <repo> rev-parse HEAD`; else the literal string `"unknown"`.
fn build_commit(repo: &Path) -> String {
    if let Ok(v) = std::env::var("RESC_BUILD_COMMIT") {
        if !v.is_empty() {
            return v;
        }
    }
    match run_git(repo, &["rev-parse", "HEAD"]) {
        Some(out) if !out.is_empty() => out,
        _ => "unknown".to_string(),
    }
}

/// Whether `git status --porcelain` reported anything, i.e. a dirty working tree.
/// `None` (recorded as JSON `null`) only when git itself is unavailable/fails — we
/// don't claim clean-vs-dirty when we can't actually tell.
fn build_dirty(repo: &Path) -> Option<bool> {
    run_git(repo, &["status", "--porcelain"]).map(|out| !out.is_empty())
}

/// Runs `git -C <repo> <args>`, returning trimmed stdout on a zero exit; `None` if git
/// can't be run at all or exits non-zero.
fn run_git(repo: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git").arg("-C").arg(repo).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok().map(|s| s.trim().to_string())
}

/// `uname(2)` sysname/release/version/machine — the fields common to both Linux and
/// Darwin's `libc::utsname` (Linux additionally has `domainname`; unused here so this
/// also `cargo check`s on the macOS dev machine per the A0.0 environment note). `pub`
/// so the client doctor's environment check (V11 §11.4) reuses this exact approach
/// instead of duplicating the `uname(2)` unsafe call.
pub fn kernel_info() -> serde_json::Value {
    let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: `uts` is a valid, appropriately-sized out-param for `uname(2)`.
    let rc = unsafe { libc::uname(&mut uts) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        return json!({ "error": err.to_string() });
    }
    json!({
        "sysname": cstr_field(&uts.sysname),
        "release": cstr_field(&uts.release),
        "version": cstr_field(&uts.version),
        "machine": cstr_field(&uts.machine),
    })
}

/// Reads a NUL-terminated `uname(2)` char-array field as a `String`.
fn cstr_field(field: &[libc::c_char]) -> String {
    // SAFETY: `field` comes from a just-populated `libc::utsname`; `uname(2)` guarantees
    // NUL termination within the array bounds.
    unsafe { std::ffi::CStr::from_ptr(field.as_ptr()) }.to_string_lossy().into_owned()
}
