//! Diagnostics core for the RESC Ubuntu client — A0.0 scope
//! (IMPLEMENTATION_PLAN_V11.md §11 "Diagnostics & upgradeability", §3 "Lifecycle").
//!
//! Independent pieces, wired together once near the top of `main()`:
//! - [`jsonl`]: `RescLog`, the JSONL diagnostics logger.
//! - [`environment`]: the one-shot `startup_environment` record.
//! - [`instance_lock`]: the per-profile advisory lock that replaces killing stale
//!   processes ("a second instance exits cleanly; nothing is ever killed" — V11 §3).
//! - [`trace`]: `ClientTrace`, the A0.0 trace-mode (`RESC_TRACE=1`) per-frame/clock logger.
//! - [`clocksync`]: `ClockSync`, the pure four-timestamp clock-sync math (§10).

pub mod clocksync;
pub mod environment;
pub mod instance_lock;
pub mod jsonl;
pub mod profile;
pub mod trace;

pub use jsonl::RescLog;

use std::path::{Path, PathBuf};

/// Root of the RESC local-state directory: `~/.local/state/resc` in production, or
/// `$RESC_LOG_DIR` when overridden. The fs-touching tests in this crate use the
/// override so they never write into a developer's real state directory. Public so
/// out-of-crate writers into the same directory (currently the client doctor's
/// `doctor_client.json`) honor the same override.
pub fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RESC_LOG_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    Path::new(&home).join(".local/state/resc")
}

/// Process-wide monotonic microsecond clock, anchored at first use. Shared by
/// every diagnostics sub-module — [`jsonl`]'s `client.jsonl` records,
/// [`trace`]'s trace-mode records, and callers feeding [`clocksync::ClockSync`]
/// — so timestamps recorded through different entry points stay comparable
/// within one process (IMPLEMENTATION_PLAN_V11.md §10/§11).
pub fn mono_us() -> u64 {
    jsonl::ts_mono_us()
}
