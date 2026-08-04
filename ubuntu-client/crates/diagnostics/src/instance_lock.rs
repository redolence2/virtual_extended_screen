//! Per-profile advisory instance lock (IMPLEMENTATION_PLAN_V11.md §3; CONTRACT_ERRATA.md).
//!
//! V11 §3: "Per-profile advisory `flock` ... a second instance exits cleanly
//! (`INSTANCE_LOCK_HELD`); nothing is ever killed." This replaces the old
//! pgrep/kill-stale-processes startup behavior. One exclusive, non-blocking `flock(2)`
//! per profile at `~/.local/state/resc/<profile_id>.lock`, held for the life of the
//! process (released automatically by the kernel on exit/crash).

use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::sync::OnceLock;

use serde_json::json;

use crate::RescLog;

const COMPONENT: &str = "instance_lock";

/// Holds the locked file for the life of the process — its `flock` releases only on
/// close/exit. Never read back; its presence in this `OnceLock` *is* the held lock
/// (the intentional-leak the spec calls for, done via ownership transfer rather than
/// `mem::forget` so a redundant `acquire()` call can still short-circuit safely).
static HELD: OnceLock<File> = OnceLock::new();

/// Attempts to acquire the exclusive advisory lock for `profile_id`. Returns `true` if
/// this process now (or already did) hold it; `false` if another process holds it —
/// callers must exit cleanly on `false` (`FatalCode::INSTANCE_LOCK_HELD` = 20) and must
/// never kill the other holder.
pub fn acquire(profile_id: &str) -> bool {
    if HELD.get().is_some() {
        return true; // already acquired by an earlier call in this process
    }

    let log = RescLog::global();
    let dir = crate::state_dir();
    let path = dir.join(format!("{profile_id}.lock"));

    if let Err(e) = std::fs::create_dir_all(&dir) {
        log.event(
            "instance_lock",
            COMPONENT,
            json!({ "acquired": false, "profile_id": profile_id, "error": e.to_string() }),
        );
        return false;
    }

    let file = match OpenOptions::new().create(true).write(true).mode(0o600).open(&path) {
        Ok(f) => f,
        Err(e) => {
            log.event(
                "instance_lock",
                COMPONENT,
                json!({
                    "acquired": false, "profile_id": profile_id,
                    "path": path.to_string_lossy(), "error": e.to_string(),
                }),
            );
            return false;
        }
    };

    // SAFETY: `file.as_raw_fd()` is a valid, open fd for the duration of this call.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        log.event(
            "instance_lock",
            COMPONENT,
            json!({
                "acquired": false, "profile_id": profile_id,
                "path": path.to_string_lossy(), "error": err.to_string(),
            }),
        );
        return false;
    }

    log.event(
        "instance_lock",
        COMPONENT,
        json!({ "acquired": true, "profile_id": profile_id, "path": path.to_string_lossy() }),
    );

    // Store (rather than std::mem::forget) so the held fd stays reachable — it doesn't
    // need to be *used* again, but keeping it in a static is the same intentional leak
    // the spec asks for, without discarding the value into the void.
    let _ = HELD.set(file);
    true
}
