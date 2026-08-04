# R6 evidence summary + A0 backend selection — locally verified (ladder state 3)

Date: 2026-08-04 · Executor: decoder/harness-implementer worker; review, independent
re-verification, and the selection decision by root reviewer.
Scope: `A00_REMEDIATION_PLAN.md` §5 R6 / §3 D2 (bounded characterization; ERR-03 closure).

## What was proven (real hardware, box: driver 570.169, ffmpeg 61.19.101, kernel 6.8.0-65)

- **Send-EAGAIN genuinely forced on BOTH real backends** — 3 events each within the bounded
  protocol (no `not_forced` statuses anywhere; ERR-08-style escape hatch never needed). Root
  cause of the historical `eagain_retries=0`: the old runs drained after every accept, so the
  decoder buffer never filled; Phase A's no-drain strategy fills it.
- Each forced EAGAIN's drain recovered exactly 2 frames with correct consecutive PTS — real
  multi-output-drain evidence (4 multi-output drains per run, max 2).
- Real EOF/tail-drain evidence: cuvid's tail flush recovered 1 actual frame; sw1's tail empty
  (all emitted earlier) — both with final `emitted == submitted == 480` and exact ordinal
  coverage, exactly-once acceptance throughout.
- Zero-output packets: 0 observed on this sample/backends (recorded, not asserted — the test
  double covers that branch deterministically).
- **Test double + seam**: `DecoderLoopBackend` trait (send_packet / receive_frame / send_eof)
  with the retain-drain-resubmit engine extracted to `backend-construct/src/loop_engine.rs`;
  BOTH `decoder-experiment` and `harness-receiver` now run the same engine (full unification —
  beyond the spec's minimum). 9 scripted `MockDecoder` tests cover Again→retain-drain-resubmit,
  double-EAGAIN, zero-output, multi-output, tail flush, error propagation.
- **Receiver predicate hardened** (`harness-receiver/src/verdict.rs`, pure, 13 unit tests, each
  term independently flips the verdict, nonzero-frames term included): accepted==acked ∧
  emitted==submitted ∧ unknown_pts==0 ∧ nonzero frames ∧ zero dups ∧ zero reorders/skips ∧
  zero ACK-order violations ∧ zero protocol/fatal errors ∧ clean EOF/tail ∧ zero outstanding.
  Report `report_v: 2`.

## Clean-run measurements (stall-free, full sample)

| Backend | max_lag (frames) | output delay p50 / p95 / p99 / max (ms) |
|---|---|---|
| sw1-lowdelay | **1** | 3.54 / 3.80 / 6.30 / **7.67** |
| cuvid-lowdelay | 2 | 1.78 / 1.94 / 2.03 / **17.01** |

## Selection decision (root reviewer) — **sw1-lowdelay is the A0 backend**

Rationale: at 60 Hz the worst case governs, not the median. sw1's maximum output delay
(7.67 ms) stays under half a frame period with `max_lag 1`; cuvid is faster at p50 but its
observed maximum (17.01 ms) exceeds a full frame period and its pipeline holds `max_lag 2`.
sw1 also avoids the GPU transfer round-trip and its failure modes. This confirms the
long-recorded inclination — now backed by the clean-run data the reviews required.

**Frozen for A0** (Stage-2 writes them into the canonical profile at profile-freeze time):
- `decoder_backend = sw1-lowdelay`
- `decoder_lag_bound_frames = 1` (the observed clean-run maximum)
- `output_deadline_ms = 50` (≈6.5× the observed 7.67 ms max, three frame periods — detects
  genuine stalls without false-positive headroom risk)

## Worker deviations — all reviewed and accepted

`send_eof` as a third trait method (EOF/tail requires it); `FlushRecord` carrying tail EAGAIN
counts; `report_v: 2` naming (implicit v1 predates it); frames_submitted==frames_accepted in
this rig (accept==submit by construction, field kept for predicate vocabulary); ACK-order
operationalized as strictly-increasing past last-acked; drain accounting split
(phases A+B vs tail); streaming-mode unification with additive report fields; dead
`Backend::is_hw()` removed.

## Verification (independent re-run by root reviewer)

All four retained reports inspected (`evidence/a00/wip/r6-*.json` — per-event detail and
environment fields verified present); box `cargo test --workspace` re-run: **21 suites ok,
0 failed** (includes the 9 loop-engine + 13 verdict tests).

## Ladder state

ERR-03 evidence complete (double + real-backend forced paths + tail) and the A0 backend
**selected and fixed** → **locally verified (state 3)**. Commit at R7.
