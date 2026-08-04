//! Trace-mode diagnostics logger (IMPLEMENTATION_PLAN_V11.md §10 "Clock &
//! trace", §12 "trace joining on the untouched v1 wire").
//!
//! Writes one JSON object per line to `client-trace.jsonl` under the RESC
//! state directory ([`crate::state_dir`] — `~/.local/state/resc`, or
//! `$RESC_LOG_DIR` when overridden), active only when the environment
//! variable `RESC_TRACE=1` is set at process start; otherwise every method
//! is a cheap no-op and no file is ever created. Records are buffered and
//! flushed every 64 records or 1 second, whichever comes first — except
//! [`ClientTrace::finish`]'s footer, which flushes AND `fsync`s synchronously
//! (A00_COMPLETION_REPORT_AMENDED_response_review.md amendment 3: "a clean/
//! aborted terminal protocol" — a footer that doesn't survive a crash/kill
//! defeats its own purpose, since `tools/join_trace.py` requires exactly one
//! clean footer per side).
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
    /// Per-process 16-hex run identity, generated once when the trace file
    /// opens (A00_COMPLETION_REPORT_AMENDED_response_review.md amendment 3:
    /// "one `trace_complete` footer containing a run token"). Empty and
    /// never read when tracing is disabled.
    run_token: String,
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
                return ClientTrace { writer: None, run_token: String::new() };
            }
            let dir = crate::state_dir();
            match TraceWriter::open(&dir) {
                Ok(w) => ClientTrace { writer: Some(Mutex::new(w)), run_token: generate_run_token() },
                Err(e) => {
                    eprintln!(
                        "RESC diagnostics: RESC_TRACE=1 but failed to open {:?} ({e}); trace disabled",
                        dir.join(FILE_NAME)
                    );
                    ClientTrace { writer: None, run_token: String::new() }
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

    /// Records one decode-side receipt-ledger identity failure
    /// (A00_COMPLETION_REPORT_AMENDED_review.md finding 1: duplicate submit,
    /// ledger cap overflow, unrecovered PTS, or a missing ledger entry at
    /// emission). Callers also invalidate the run's footer status — see
    /// [`Self::finish`] — never exit mid-loop over one of these.
    pub fn identity_failure(&self, fields: Value) {
        self.write("identity_failure", fields);
    }

    /// Records one failed texture upload (A00_COMPLETION_REPORT_AMENDED_review.md
    /// finding 1's presentation-identity bug: `update_frame` failing must
    /// never advance the presented identity). Does not by itself invalidate
    /// the trace footer — it is a presentation-path failure, not a ledger
    /// identity failure.
    pub fn render_failure(&self, fields: Value) {
        self.write("render_failure", fields);
    }

    /// Appends the trace-run footer and synchronously flushes + `fsync`s the
    /// file before returning (A00_COMPLETION_REPORT_AMENDED_response_review.md
    /// amendment 3 / `A00_COMPLETION_REPORT_AMENDED_response.md` v2 §2 F4.3):
    /// one `{"kind":"trace_complete",...}` record carrying this run's
    /// `run_token`, `status` (`"clean"` or `"aborted"`), and whatever
    /// caller-supplied `fields` (pending/failure/drop counts). A footer that
    /// didn't survive a crash/kill defeats its own purpose — `tools/
    /// join_trace.py` requires exactly one clean footer per side. No-op
    /// (including no partial record) when tracing is disabled. `fields` must
    /// be a JSON object (or `Null`); any other shape is nested under `"value"`
    /// rather than silently discarded.
    pub fn finish(&self, status: &str, fields: Value) {
        if !self.enabled() {
            return;
        }
        let mut merged = match fields {
            Value::Object(map) => map,
            Value::Null => Map::new(),
            other => {
                let mut m = Map::new();
                m.insert("value".into(), other);
                m
            }
        };
        merged.insert("run_token".into(), json!(self.run_token));
        merged.insert("status".into(), json!(status));
        self.write_inner("trace_complete", Value::Object(merged), true);
    }

    fn write(&self, kind: &str, fields: Value) {
        self.write_inner(kind, fields, false);
    }

    /// Shared record-building + write path for every trace record. `sync`
    /// forces an immediate flush + `fsync` regardless of the periodic
    /// cadence — set only by [`Self::finish`]'s footer.
    fn write_inner(&self, kind: &str, fields: Value, sync: bool) {
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
        guard.write_line(&line, sync);
    }
}

/// A 16-hex per-process run identity, generated once when the trace file
/// opens. Not a security token — just enough entropy (pid + monotonic +
/// wall-clock reading) to tell two runs' footers apart when reconciling
/// evidence; reuses [`crate::profile::hash8`] rather than adding a
/// randomness dependency.
fn generate_run_token() -> String {
    let seed = format!(
        "{}-{}-{}",
        std::process::id(),
        crate::mono_us(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    crate::profile::hash8(seed.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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

    /// `sync` forces an immediate flush + `fsync` regardless of the periodic
    /// cadence below — used only by the [`ClientTrace::finish`] footer.
    fn write_line(&mut self, line: &str, sync: bool) {
        if let Err(e) = self.file.write_all(line.as_bytes()).and_then(|_| self.file.write_all(b"\n")) {
            eprintln!("RESC diagnostics: trace write failed: {e}");
            return;
        }
        self.records_since_flush += 1;
        if sync || self.records_since_flush >= FLUSH_EVERY_RECORDS || self.last_flush.elapsed() >= FLUSH_EVERY {
            if let Err(e) = self.file.flush() {
                eprintln!("RESC diagnostics: trace flush failed: {e}");
            }
            self.records_since_flush = 0;
            self.last_flush = Instant::now();
        }
        if sync {
            if let Err(e) = self.file.get_ref().sync_all() {
                eprintln!("RESC diagnostics: trace fsync failed: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A00_COMPLETION_REPORT_AMENDED_response_review.md amendment 3: the
    /// footer must carry a 16-hex run token, the given status, and the
    /// caller's fields — and actually land on disk (this is the box smoke
    /// test's fallback verification path per the C3 spec, for environments
    /// where a real no-connection client smoke isn't reachable).
    #[test]
    fn finish_writes_a_clean_footer_with_16_hex_run_token() {
        let dir = std::env::temp_dir().join(format!(
            "resc-trace-test-finish-{}-{}",
            std::process::id(),
            crate::mono_us()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // Private instance, talking to `TraceWriter::open` directly (mirrors
        // jsonl.rs's tempdir test) — `global()` is a process-wide `OnceLock`
        // keyed off `RESC_TRACE`/`RESC_LOG_DIR` and would race other tests.
        let writer = TraceWriter::open(&dir).expect("trace file must open");
        let trace = ClientTrace { writer: Some(Mutex::new(writer)), run_token: generate_run_token() };

        assert!(trace.enabled());
        assert_eq!(trace.run_token.len(), 16, "run_token must be 16 hex chars, got {:?}", trace.run_token);
        assert!(
            trace.run_token.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "run_token must be lowercase hex, got {:?}",
            trace.run_token
        );

        trace.frame(json!({"recovered_frame_id": 1, "decode_trigger_frame_id": 1, "ts_recv_us": 100, "ts_decode_done_us": 200}));
        trace.finish("clean", json!({"pending_identities": 0, "identity_failures": 0}));

        let path = dir.join(FILE_NAME);
        let contents = std::fs::read_to_string(&path).expect("trace file must be readable");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2, "expected exactly 2 lines (frame + footer), got {:?}", lines);

        let footer: Value = serde_json::from_str(lines.last().unwrap()).expect("footer must be valid JSON");
        assert_eq!(footer["kind"], "trace_complete");
        let fields = &footer["fields"];
        assert_eq!(fields["status"], "clean");
        assert_eq!(fields["pending_identities"], 0);
        assert_eq!(fields["identity_failures"], 0);
        let token = fields["run_token"].as_str().expect("run_token must be a string");
        assert_eq!(token.len(), 16);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A disabled trace's `finish` must stay a total no-op — no file, no
    /// panic — exactly like every other method on a disabled `ClientTrace`.
    #[test]
    fn finish_on_disabled_trace_is_a_no_op() {
        let trace = ClientTrace { writer: None, run_token: String::new() };
        assert!(!trace.enabled());
        trace.finish("clean", json!({"pending_identities": 0}));
        // No panic, nothing to assert on disk — the point is this doesn't crash.
    }

    #[test]
    fn generate_run_token_is_16_lowercase_hex_chars() {
        let token = generate_run_token();
        assert_eq!(token.len(), 16);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
