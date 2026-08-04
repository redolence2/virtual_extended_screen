# C3 evidence summary — identity/clock/termination repair (both halves)

Date: 2026-08-04 · Executors: two capture/trace-implementer workers (Mac half, client half) +
root-reviewer review and independent re-verification. Scope: corrective item C3
(review finding 1 + 4.1/4.2/4.3, as amended by response-review amendments 2 and 3).

## Mac half (accepted; three deviations all accepted)

- **V11 §10's literal conversion restored**: SCK PTS → `CMSyncConvertTime(pts, from:
  stream.synchronizationClock, to: CMClockGetHostTimeClock())` → bracketed calibration bridge;
  labeled callback fallback when the sync clock is nil / PTS invalid / no calibration. Worker
  addition accepted: `CMTIME_IS_VALID` guarded on the conversion *output* too (an invalid
  result would NaN-trap `UInt64(Double)` in the capture callback).
- **Host trace termination**: 16-hex `run_token`; internally tracked frame/pong counters;
  `finish(status:...)` appends the `trace_complete` footer then synchronously flushes +
  `synchronizeFile()`; `finishAborted()` exposed. SIGTERM handled via
  `SIG_IGN` + `DispatchSourceSignal` on `.main` (footer I/O is unsafe in a real async signal
  context), installed only in trace mode; shared `performGracefulShutdown()` extracted verbatim
  from the SIGINT path. Deviation accepted with its reasoning: `finishAborted` left UNWIRED at
  the lock-denial site — a second instance losing the lock would otherwise inject a spurious
  aborted footer into the FIRST instance's live trace.
- Smoke: 408 real frames through the new conversion path, then SIGTERM → verified clean footer
  (16-hex token, status clean).

## Client half (accepted; all disclosed deviations accepted)

- **Decode-side receipt ledger** (cap 1024): insert at submit of each ADMITTED frame; resolve/
  remove by each emission's `recovered_frame_id`; `decode_trigger_frame_id` carried separately;
  duplicate-submit / cap-overflow / unrecovered-PTS / missing-entry each traced as
  `identity_failure` and invalidating the footer status; queue drops excluded by construction
  (`video_receiver.rs` untouched — its assembly-completion stamp stays immutable).
- **Upload-confirmed presentation identity**: `last_uploaded_recovered_id`/`new_video_frame`
  advance only on `update_frame` Ok; failures counted + traced (`render_failure`).
- **Decoder EOF/tail**: `video-decode::flush()` (send_eof/EAGAIN-retry/drain, mirroring
  `loop_engine`'s proven pattern; per-emission PTS recovery identical to `decode()`); shared
  `drain_ready()` extraction. Companion `libc` dependency accepted (`Cargo.lock` one-liner).
- **Client trace termination**: 16-hex run token; `finish()` footer with synchronous
  flush+fsync; shutdown sequence drain-admitted → tail-flush → footer (clean iff zero pending
  identities and zero identity failures) → exit 0/1. Worker-discovered bug FIXED beyond spec
  (accepted): an unconditional SIGTERM handler made the client unkillable while stuck in
  `negotiate_mode()` — alive-guard + 100 ms watchdog now force-exits any window the decode loop
  isn't polling; empirically SIGTERM-to-exit went from ∞ to 103 ms.
- **Joiner rewrite**: fatal on unparseable/non-object lines; exactly-one-clean-footer per side
  (position-independent, both sides' distinct shapes); session-global best clock sample only;
  host-local causal check with `--causal-slack-us`; `decode_trigger_frame_id` informational;
  9-condition PASS predicate. **10 selftest scenarios** incl. trigger≠recovered joining on the
  recovered identity with the ledger-correct receive stamp, footer missing/duplicate/aborted
  fatality, causal violation + slack, parse-error fatality, zero-clock and zero-present
  failures, and (worker bonus, accepted) a lone `identity_failure` record failing the pass.
- **Runner**: SIGTERM + 10 s polled waits, SIGKILL-escalation fails the gate (exit 4, no join).
  Worker-discovered pre-existing bug FIXED (accepted): `pkill -f` over ssh self-matches the
  invoking shell's argv and kills it — every match switched to bare process-name matching.
- Footer-path smoke note: the dead-host/half-open handshake paths architecturally never reach
  `ClientTrace::global()`, so the footer was verified by unit test (sanctioned fallback); the
  REAL end-to-end footer pair is exercised by the C7 live gate.
- Accepted narrow scope: footer `queue_drops` uses the receiver's aggregate `frames_dropped`
  (documented in-code) — the only counter reachable without touching the out-of-scope receiver.

## Root-reviewer verification

Joiner selftest: **all 10 scenarios pass**. Mac `cargo test -p diagnostics`: 19+1 green (3 new
footer/token tests). Box: full workspace **22 suites / 0 failed**. Mac package build + 624
fixture checks green (C3-Mac verified within its own run).
