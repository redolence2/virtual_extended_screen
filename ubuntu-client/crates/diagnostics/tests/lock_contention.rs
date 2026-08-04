//! Two-process lock contention test (A00_REMEDIATION_PLAN.md §5 R3a: "a
//! two-process lock-contention test").
//!
//! `flock(2)` locks are scoped to the *open file description*, not the
//! process — a second `File::open` in the *same* process would get its own
//! open file description and could plausibly contend too, so that alone
//! would not distinguish "real cross-process contention" from "an accident
//! of however this test is plumbed." `instance_lock::acquire`/`acquire_at`
//! also short-circuit to `true` on a second call in the same process (see
//! their doc comments), so a naive in-process "acquire twice" test would
//! pass *trivially*, proving nothing. What actually needs proving is the
//! thing `instance_lock` exists for: a genuinely separate *second process*
//! for the same profile must fail cleanly while a *first* process holds the
//! lock, and must succeed once that first process releases it (by exiting,
//! same as a crash releases it).
//!
//! This test re-execs its own compiled test binary (`std::env::current_exe`)
//! as real child processes, distinguished by which of two env vars is set:
//! - `RESC_LOCK_CONTENTION_CHILD=<path>` — "prober" role: attempt
//!   `instance_lock::acquire_at(path, ..)` once, `exit(0)` if acquired,
//!   `exit(1)` if denied.
//! - `RESC_LOCK_CONTENTION_HOLD=<path>` — "holder" role: acquire the lock,
//!   print `acquired` on stdout (the parent's synchronization point — no
//!   sleep-based timing guesswork), then block reading a line from stdin;
//!   on EOF (the parent dropping its end of the pipe) it exits(0),
//!   releasing the lock the same way any process exit does.
//!
//! This file has exactly one `#[test]` — deliberately: when a re-exec'd
//! child process runs under the default (unfiltered) test harness, this is
//! the only test that *can* be selected, so there is no risk of some other
//! test in this binary racing the child's early `std::env::var` check.
//! Neither env var is ever set for the actual `cargo test` invocation
//! itself, so that one instance falls through both checks into the real
//! test body at the bottom.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

const CHILD_PROBE_ENV: &str = "RESC_LOCK_CONTENTION_CHILD";
const CHILD_HOLD_ENV: &str = "RESC_LOCK_CONTENTION_HOLD";

#[test]
fn two_process_lock_contention() {
    // --- Child role: prober (try once, report the outcome via exit code) ---
    if let Ok(path) = std::env::var(CHILD_PROBE_ENV) {
        let acquired = diagnostics::instance_lock::acquire_at(
            std::path::Path::new(&path),
            "lock-contention-test",
        );
        std::process::exit(if acquired { 0 } else { 1 });
    }

    // --- Child role: holder (acquire, signal ready, block until released) ---
    if let Ok(path) = std::env::var(CHILD_HOLD_ENV) {
        let acquired = diagnostics::instance_lock::acquire_at(
            std::path::Path::new(&path),
            "lock-contention-test",
        );
        if !acquired {
            eprintln!("holder: failed to acquire lock (unexpected)");
            std::process::exit(2);
        }
        println!("acquired");
        std::io::stdout().flush().ok();
        // Blocks until the parent closes its end of the pipe (EOF) — that
        // is the release signal; the actual bytes read (if any) don't
        // matter.
        let mut discard = String::new();
        let _ = std::io::stdin().read_line(&mut discard);
        std::process::exit(0);
    }

    // --- True parent: the actual `cargo test` invocation ---
    let exe = std::env::current_exe().expect("current_exe");
    let dir = std::env::temp_dir().join(format!(
        "resc-lock-contention-{}-{}",
        std::process::id(),
        diagnostics::mono_us(),
    ));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    let lock_path = dir.join("contention.lock");

    // Every re-exec below passes `--nocapture`: libtest captures (buffers,
    // then discards-unless-failed) a test's *own* stdout writes by default,
    // and every child role here ends in `std::process::exit`, which tears
    // the process down before libtest's own post-test code would ever flush
    // that capture buffer — without `--nocapture` the holder's "acquired"
    // line (this test's whole synchronization point) never reaches the real
    // pipe the parent is reading.

    // 1. Spawn the holder and block until it confirms acquisition — no
    //    sleep, no timing guess: the "acquired" line on its stdout is the
    //    synchronization point. The re-exec'd holder is itself a fresh
    //    libtest process, so its stdout carries the harness's own preamble
    //    ("\nrunning 1 test\n" etc.) ahead of our "acquired" line even with
    //    `--nocapture` (that preamble is the harness's own status output,
    //    not per-test captured output) — scan lines until the marker
    //    itself, rather than assuming it is the first line.
    let mut holder = Command::new(&exe)
        .env(CHILD_HOLD_ENV, &lock_path)
        .arg("--nocapture")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn holder");
    let mut holder_stdout = BufReader::new(holder.stdout.take().expect("holder stdout"));
    let mut saw_acquired = false;
    loop {
        let mut line = String::new();
        let n = holder_stdout.read_line(&mut line).expect("read from holder stdout");
        if n == 0 {
            break; // EOF: holder exited without ever printing "acquired"
        }
        if line.trim() == "acquired" {
            saw_acquired = true;
            break;
        }
    }
    assert!(saw_acquired, "holder did not confirm lock acquisition before closing its stdout");

    // 2. A prober while the holder still holds the lock must be denied —
    //    this is the actual contention proof: a real second process,
    //    genuinely blocked by flock(2), not a second in-process fd.
    let status = Command::new(&exe)
        .env(CHILD_PROBE_ENV, &lock_path)
        .arg("--nocapture")
        .status()
        .expect("spawn prober (held)");
    assert_eq!(
        status.code(),
        Some(1),
        "prober must be denied while the holder still holds the lock"
    );

    // 3. Release: close the holder's stdin (EOF unblocks its read_line) and
    //    wait for it to actually exit, so the flock is guaranteed released
    //    (not merely "probably released by now") before the next probe.
    drop(holder.stdin.take());
    let holder_status = holder.wait().expect("wait for holder");
    assert!(holder_status.success(), "holder did not exit cleanly: {holder_status:?}");

    // 4. A prober after release must succeed.
    let status = Command::new(&exe)
        .env(CHILD_PROBE_ENV, &lock_path)
        .arg("--nocapture")
        .status()
        .expect("spawn prober (released)");
    assert_eq!(
        status.code(),
        Some(0),
        "prober must succeed once the holder has released the lock"
    );

    std::fs::remove_dir_all(&dir).ok();
}
