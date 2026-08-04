//! JSONL diagnostics logger (IMPLEMENTATION_PLAN_V11.md §11.1).
//!
//! Writes one JSON object per line to `client.jsonl` under the RESC state directory
//! (`~/.local/state/resc`, or `$RESC_LOG_DIR` when overridden — see `crate::state_dir`).
//! The active file rotates to `client.jsonl.1..4` past 10 MiB, keeping 5 files total
//! with the oldest deleted. Records are buffered and flushed every 32 records; `fatal()`
//! additionally flushes and `fsync`s before returning so the record survives a crash.
//!
//! Callers must never pass secrets or raw frame bytes in `fields` — this log is for
//! lifecycle/failure/aggregate diagnostics only, never per-packet data (V11 §11.1).

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

/// Active file size at which the *next* write triggers rotation.
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// Numbered backups kept (`client.jsonl.1` .. `client.jsonl.4`) — 5 files total.
const MAX_ROTATIONS: u32 = 4;
/// Buffered records flushed to the OS at least this often even without a fatal record.
const FLUSH_EVERY: u32 = 32;
const FILE_NAME: &str = "client.jsonl";

static GLOBAL: OnceLock<RescLog> = OnceLock::new();
/// Process-start monotonic anchor. `Instant` has no fixed epoch, so every `ts_mono_us`
/// is measured relative to this, established on first use.
static START: OnceLock<Instant> = OnceLock::new();

/// `pub(crate)` so [`crate::mono_us`] can expose the same anchor to the rest
/// of the diagnostics crate (and, through it, every other crate).
pub(crate) fn ts_mono_us() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// The JSONL diagnostics logger. Obtain the process-wide instance via [`RescLog::global`].
pub struct RescLog {
    writer: Mutex<LogWriter>,
    session_run_id: OnceLock<u64>,
    profile_hash: OnceLock<String>,
}

impl RescLog {
    fn new(writer: LogWriter) -> Self {
        RescLog {
            writer: Mutex::new(writer),
            session_run_id: OnceLock::new(),
            profile_hash: OnceLock::new(),
        }
    }

    /// Returns the process-wide logger, creating the log directory and opening
    /// `client.jsonl` on first use. If the real log file cannot be opened at all
    /// (e.g. an unwritable home directory), falls back to discarding records — noted
    /// once on stderr — rather than panicking the whole client over logging.
    pub fn global() -> &'static RescLog {
        GLOBAL.get_or_init(|| {
            let dir = crate::state_dir();
            match LogWriter::open(&dir) {
                Ok(writer) => RescLog::new(writer),
                Err(e) => {
                    eprintln!(
                        "RESC diagnostics: failed to open {:?} ({e}); logging to /dev/null",
                        dir.join(FILE_NAME)
                    );
                    RescLog::new(LogWriter::open_null().expect("/dev/null must open"))
                }
            }
        })
    }

    /// Sets the global run/profile context stamped onto every subsequent record.
    /// Intended to be called exactly once, right after the host's `ProfileResult`
    /// promotes `candidateRun -> activeRun`. A second call is logged and ignored.
    pub fn set_context(&self, session_run_id: u64, profile_hash: &str) {
        if self.session_run_id.set(session_run_id).is_err() {
            log::warn!("RescLog::set_context: session_run_id already set; ignoring update");
        }
        if self.profile_hash.set(profile_hash.to_string()).is_err() {
            log::warn!("RescLog::set_context: profile_hash already set; ignoring update");
        }
    }

    /// Logs one lifecycle/diagnostic record. `fields` carries event-specific structured
    /// data (never secrets or frame bytes) nested under the `fields` key.
    pub fn event(&self, event: &str, component: &str, fields: Value) {
        let mut record = self.base_record(event, component);
        record.insert("fields".into(), fields);
        self.write(record, false);
    }

    /// Logs a `FatalReport`-shaped record and synchronously flushes + `fsync`s before
    /// returning, so the record survives even an immediate process exit.
    pub fn fatal(
        &self,
        code: u32,
        component: &str,
        native_domain: Option<&str>,
        native_code: Option<i64>,
        summary: &str,
        fields: Value,
    ) {
        let mut record = self.base_record("fatal", component);
        record.insert("code".into(), json!(code));
        record.insert("native_domain".into(), json!(native_domain));
        record.insert("native_code".into(), json!(native_code));
        record.insert("summary".into(), json!(summary));
        record.insert("fields".into(), fields);
        self.write(record, true);
    }

    fn base_record(&self, event: &str, component: &str) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("ts_mono_us".into(), json!(ts_mono_us()));
        m.insert("ts_wall".into(), json!(ts_wall()));
        m.insert("event".into(), json!(event));
        m.insert("component".into(), json!(component));
        if let Some(id) = self.session_run_id.get() {
            m.insert("session_run_id".into(), json!(id));
        }
        if let Some(hash) = self.profile_hash.get() {
            m.insert("profile_hash".into(), json!(hash));
        }
        m
    }

    fn write(&self, record: Map<String, Value>, is_fatal: bool) {
        let line = match serde_json::to_string(&Value::Object(record)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("RESC diagnostics: failed to serialize log record: {e}");
                return;
            }
        };
        let mut guard = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        guard.write_line(&line, is_fatal);
    }

    /// Flushes any buffered records to disk without writing a new one.
    /// `event()` only flushes every [`FLUSH_EVERY`] records, which a
    /// short-lived process that logs just a handful of events and then
    /// calls `std::process::exit` (which skips `Drop`, so the `BufWriter`
    /// never gets a chance to flush on its own) can undershoot entirely —
    /// currently the client doctor, which logs only a couple of records per
    /// run. Callers in that shape should call this right before exiting.
    pub fn flush(&self) {
        let mut guard = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        guard.flush_now();
    }
}

struct LogWriter {
    file: BufWriter<File>,
    path: PathBuf,
    bytes_written: u64,
    records_since_flush: u32,
}

impl LogWriter {
    fn open(base_dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(base_dir)?;
        let path = base_dir.join(FILE_NAME);
        let file = open_0600(&path)?;
        let bytes_written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(LogWriter { file: BufWriter::new(file), path, bytes_written, records_since_flush: 0 })
    }

    /// Last-resort sink when the real log file can't be opened at all.
    fn open_null() -> std::io::Result<Self> {
        let file = OpenOptions::new().write(true).open("/dev/null")?;
        Ok(LogWriter {
            file: BufWriter::new(file),
            path: PathBuf::from("/dev/null"),
            bytes_written: 0,
            records_since_flush: 0,
        })
    }

    fn write_line(&mut self, line: &str, is_fatal: bool) {
        if self.bytes_written >= MAX_FILE_BYTES {
            if let Err(e) = self.rotate() {
                eprintln!("RESC diagnostics: log rotation failed: {e}");
            }
        }

        if let Err(e) = self.file.write_all(line.as_bytes()).and_then(|_| self.file.write_all(b"\n")) {
            eprintln!("RESC diagnostics: log write failed: {e}");
            return;
        }
        self.bytes_written += line.len() as u64 + 1;
        self.records_since_flush += 1;

        if is_fatal || self.records_since_flush >= FLUSH_EVERY {
            self.flush_now();
        }
        if is_fatal {
            if let Err(e) = self.file.get_ref().sync_all() {
                eprintln!("RESC diagnostics: log fsync failed: {e}");
            }
        }
    }

    fn flush_now(&mut self) {
        if let Err(e) = self.file.flush() {
            eprintln!("RESC diagnostics: log flush failed: {e}");
        }
        self.records_since_flush = 0;
    }

    /// Shifts `client.jsonl -> .1 -> .2 -> .3 -> .4` (deleting the previous `.4`) and
    /// reopens a fresh `client.jsonl`. The currently-open fd stays valid across its own
    /// rename — POSIX `rename(2)` retargets a directory entry, not the underlying open
    /// file description — so nothing written before this call is lost.
    fn rotate(&mut self) -> std::io::Result<()> {
        self.file.flush()?;

        let oldest = numbered_path(&self.path, MAX_ROTATIONS);
        let _ = std::fs::remove_file(&oldest); // fine if this is the first-ever rotation

        for (from_n, to_n) in shift_pairs(MAX_ROTATIONS) {
            let from = numbered_path(&self.path, from_n);
            if from.exists() {
                std::fs::rename(&from, numbered_path(&self.path, to_n))?;
            }
        }

        std::fs::rename(&self.path, numbered_path(&self.path, 1))?;

        self.file = BufWriter::new(open_0600(&self.path)?);
        self.bytes_written = 0;
        Ok(())
    }
}

/// Rotation shift order for `max` numbered backups, highest index first so nothing is
/// overwritten before it's read: e.g. `max=4` -> `[(3,4), (2,3), (1,2)]`. The base file
/// becoming `.1` is handled separately by the caller.
fn shift_pairs(max: u32) -> Vec<(u32, u32)> {
    (1..max).rev().map(|i| (i, i + 1)).collect()
}

fn numbered_path(base: &Path, n: u32) -> PathBuf {
    let mut s = base.as_os_str().to_os_string();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}

/// Opens (creating if needed) `path` for append, forcing `0600` at creation via
/// `OpenOptionsExt::mode` — every call site here always creates the file fresh (first
/// ever open, or right after `rotate()` renamed the old one away), so this is
/// sufficient; there's no path where we inherit an already-existing file's permissions.
fn open_0600(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).mode(0o600).open(path)
}

/// Hand-formatted `YYYY-MM-DDTHH:MM:SS.mmmZ`, UTC, millisecond precision — avoids a
/// chrono dependency for one string. `pub` (not just `pub(crate)`) so other crates
/// that need the exact same wall-clock format for their own reports (currently the
/// client doctor's `ts_wall`) reuse it instead of duplicating the calendar math.
pub fn ts_wall() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    format_iso8601(now.as_secs() as i64, now.subsec_millis())
}

fn format_iso8601(unix_secs: i64, millis: u32) -> String {
    let days = unix_secs.div_euclid(86_400);
    let secs_of_day = unix_secs.rem_euclid(86_400);
    let (y, mo, d) = civil_from_days(days);
    let h = secs_of_day / 3600;
    let mi = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{millis:03}Z")
}

/// Civil (proleptic Gregorian) date from a day count since the Unix epoch. Howard
/// Hinnant's `civil_from_days` (public domain;
/// <https://howardhinnant.github.io/date_algorithms.html>) — correct over the full
/// `i64` range with no external calendar dependency.
fn civil_from_days(z_days: i64) -> (i64, u32, u32) {
    let z = z_days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_matches_known_instants() {
        assert_eq!(format_iso8601(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_iso8601(1_700_000_000, 789), "2023-11-14T22:13:20.789Z");
        assert_eq!(format_iso8601(1_600_000_000, 5), "2020-09-13T12:26:40.005Z");
        assert_eq!(format_iso8601(1_754_200_000, 0), "2025-08-03T05:46:40.000Z");
        // Leap-day boundary: 2000 is a /400 leap year, exercising the Hinnant era math.
        assert_eq!(format_iso8601(951_782_400, 0), "2000-02-29T00:00:00.000Z");
        // Year boundary.
        assert_eq!(format_iso8601(946_684_799, 999), "1999-12-31T23:59:59.999Z");
    }

    #[test]
    fn rotation_shift_order_is_highest_first() {
        assert_eq!(shift_pairs(4), vec![(3, 4), (2, 3), (1, 2)]);
        assert_eq!(numbered_path(Path::new("/x/client.jsonl"), 1), PathBuf::from("/x/client.jsonl.1"));
    }

    #[test]
    #[ignore] // touches the filesystem; run explicitly with `cargo test -- --ignored`
    fn writes_and_rotates_under_a_tempdir() {
        let dir = std::env::temp_dir()
            .join(format!("resc-diag-test-{}-{}", std::process::id(), ts_mono_us()));
        std::fs::create_dir_all(&dir).unwrap();

        // Talk to a private RescLog over the tempdir directly, not RescLog::global() —
        // that's a process-wide OnceLock and would race/collide with other tests.
        let log = RescLog::new(LogWriter::open(&dir).unwrap());
        log.set_context(42, "0cc2249662880597");

        // Push well past MAX_FILE_BYTES so at least one rotation fires.
        let padding = "x".repeat(4096);
        let iterations = (MAX_FILE_BYTES / 4096) + 8;
        for i in 0..iterations {
            log.event("test_event", "test", json!({ "i": i, "pad": padding }));
        }
        log.fatal(20, "test", None, None, "forced fatal for rotation test", json!({}));

        let base = dir.join(FILE_NAME);
        let rotated = dir.join(format!("{FILE_NAME}.1"));
        assert!(base.exists(), "active log file must exist");
        assert!(rotated.exists(), "rotation must have produced client.jsonl.1");

        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::metadata(&base).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600, "active log file must be 0600");

        std::fs::remove_dir_all(&dir).ok();
    }
}
