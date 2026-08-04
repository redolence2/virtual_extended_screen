//! Trace-mode diagnostics logger (IMPLEMENTATION_PLAN_V11.md §10 "Clock &
//! trace", §12 "trace joining on the untouched v1 wire").
//!
//! Writes one JSON object per line to `client-trace.jsonl` under the RESC
//! state directory ([`crate::state_dir`] — `~/.local/state/resc`, or
//! `$RESC_LOG_DIR` when overridden), active only when the environment
//! variable `RESC_TRACE=1` is set at process start; otherwise every method
//! is a cheap no-op and no file is ever created. Records are buffered and
//! flushed every 64 records or 1 second, whichever comes first.
//!
//! Unlike [`crate::jsonl`] (never per-packet data), this logger exists
//! specifically to record one line per decoded frame plus periodic clock
//! samples — the A0 measurement-mode exemption from that rule. Callers still
//! must never pass secrets or raw frame bytes in `fields`.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

const FILE_NAME: &str = "client-trace.jsonl";
/// Flush cadence: every 64 records, or 1s, whichever is reached first.
const FLUSH_EVERY_RECORDS: u32 = 64;
const FLUSH_EVERY: Duration = Duration::from_secs(1);

static GLOBAL: OnceLock<ClientTrace> = OnceLock::new();

/// The trace-mode logger. Obtain the process-wide instance via
/// [`ClientTrace::global`]. A disabled instance (the common case outside
/// `RESC_TRACE=1`) holds no writer and every method returns immediately.
pub struct ClientTrace {
    writer: Option<Mutex<TraceWriter>>,
}

impl ClientTrace {
    /// Returns the process-wide instance, opening `client-trace.jsonl` on
    /// first use iff `RESC_TRACE=1`. If the file can't be opened even though
    /// tracing was requested, falls back to disabled (noted once on stderr)
    /// rather than panicking the client over trace logging.
    pub fn global() -> &'static ClientTrace {
        GLOBAL.get_or_init(|| {
            let requested = std::env::var("RESC_TRACE").map(|v| v == "1").unwrap_or(false);
            if !requested {
                return ClientTrace { writer: None };
            }
            let dir = crate::state_dir();
            match TraceWriter::open(&dir) {
                Ok(w) => ClientTrace { writer: Some(Mutex::new(w)) },
                Err(e) => {
                    eprintln!(
                        "RESC diagnostics: RESC_TRACE=1 but failed to open {:?} ({e}); trace disabled",
                        dir.join(FILE_NAME)
                    );
                    ClientTrace { writer: None }
                }
            }
        })
    }

    /// Whether trace mode is active (`RESC_TRACE=1` and the file opened
    /// successfully). Callers use this to skip building trace payloads
    /// entirely when tracing is off.
    pub fn enabled(&self) -> bool {
        self.writer.is_some()
    }

    /// Records one per-frame trace line. Per-frame records are allowed here
    /// (A0 measurement-mode exemption from `jsonl`'s "never per-packet"
    /// rule) — callers still must not pass secrets or raw frame bytes.
    pub fn frame(&self, fields: Value) {
        self.write("frame", fields);
    }

    /// Records one presentation trace line (A00_REMEDIATION_PLAN.md §4 item
    /// 8: "the render trace records the recovered frameID immediately
    /// adjacent to the successful presentation call — not when upload is
    /// merely scheduled"). Same per-frame exemption as [`Self::frame`].
    pub fn present(&self, fields: Value) {
        self.write("present", fields);
    }

    /// Records one clock-sync sample (`ClockSync::on_pong`'s accepted
    /// output; IMPLEMENTATION_PLAN_V11.md §10).
    pub fn clock(&self, offset_us: i64, delay_us: i64, uncertainty_us: u32, seq: u32) {
        self.write(
            "clock",
            json!({
                "offset_us": offset_us,
                "delay_us": delay_us,
                "uncertainty_us": uncertainty_us,
                "seq": seq,
            }),
        );
    }

    fn write(&self, kind: &str, fields: Value) {
        let writer = match &self.writer {
            Some(w) => w,
            None => return,
        };
        let mut record = Map::new();
        record.insert("ts_mono_us".into(), json!(crate::mono_us()));
        record.insert("kind".into(), json!(kind));
        record.insert("fields".into(), fields);
        let line = match serde_json::to_string(&Value::Object(record)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("RESC diagnostics: failed to serialize trace record: {e}");
                return;
            }
        };
        let mut guard = writer.lock().unwrap_or_else(|p| p.into_inner());
        guard.write_line(&line);
    }
}

struct TraceWriter {
    file: BufWriter<File>,
    records_since_flush: u32,
    last_flush: Instant,
}

impl TraceWriter {
    fn open(base_dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(base_dir)?;
        let path = base_dir.join(FILE_NAME);
        let file = OpenOptions::new().create(true).append(true).mode(0o600).open(&path)?;
        // 0600 reassertion (R3a lock/log hygiene): open(2)'s mode argument
        // only applies when O_CREAT actually creates the file — reassert on
        // every open so a pre-existing trace file can't keep looser
        // permissions. Best-effort, mirroring jsonl.rs/instance_lock.rs.
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        Ok(TraceWriter { file: BufWriter::new(file), records_since_flush: 0, last_flush: Instant::now() })
    }

    fn write_line(&mut self, line: &str) {
        if let Err(e) = self.file.write_all(line.as_bytes()).and_then(|_| self.file.write_all(b"\n")) {
            eprintln!("RESC diagnostics: trace write failed: {e}");
            return;
        }
        self.records_since_flush += 1;
        if self.records_since_flush >= FLUSH_EVERY_RECORDS || self.last_flush.elapsed() >= FLUSH_EVERY {
            if let Err(e) = self.file.flush() {
                eprintln!("RESC diagnostics: trace flush failed: {e}");
            }
            self.records_since_flush = 0;
            self.last_flush = Instant::now();
        }
    }
}
