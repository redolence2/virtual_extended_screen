# R4 evidence summary — locally verified (ladder state 3)

Date: 2026-08-04 · Executors: two capture/trace-implementer workers (Mac half, client half) +
root-reviewer design, review, two inline corrections, ERR-08, and the live evidence gate.
Scope: `A00_REMEDIATION_PLAN.md` §4 (all nine items) + §5 R4.

## Delivered (code)

- **Mac half**: `FrameIdentity` (RescCore) threaded through `VideoEncoder`'s per-submit closure
  (the exact per-submit context — no VT attachments, no latest-wins) → `StreamingState` →
  `VideoSender`'s wire frameID assignment → `RescTrace.frameSent` writing the frozen 11-field
  frameID→capture-identity mapping record. The latest-wins `captureMeta` slot is deleted.
  Root-reviewer inline fix: encoder-output time is stamped at VT-callback entry and threaded
  down; send time stamped separately beside the socket writes (the worker's version read both
  at one site — two fields, one event — rejected).
- **Client half**: `AssembledFrame.recv_ts_us` stamped at assembly completion, ahead of the
  drop-capable `sync_channel(4)`; decoder packet PTS = frameID with per-emission recovery
  (`pts()` → `best_effort_timestamp()`; `recovered_frame_id` per `DecodedFrame` — delayed and
  multi-output emissions carry their OWN id); one trace record per emitted frame; separate
  `present` record at the actual `canvas.present()` call site (upload ≠ present).
- **Joiner** `tools/join_trace.py` (477 lines, stdlib, deterministic, `--selftest`): exact-id
  join, typed rejection reasons, per-sample clock offset ± uncertainty, hard pass predicate
  (exit 0 iff zero identity ambiguities ∧ joined > 0). Root-reviewer fix of a worker-caught
  **sign error in the frozen spec**: client-domain of a host stamp is `host_ts − offset`
  (ClockSync's offset ≈ host − client); selftest fixture corrected accordingly (4200, not 3800).

## ERR-08 (discovered by this gate)

Two live runs (30 s + 90 s, ~12 ping cycles) accepted **zero** clock samples: plan §10's 5 ms
delay gate is unsatisfiable on the real ~7 ms-RTT link. Dated erratum ERR-08 recorded FIRST,
then `clocksync.rs` changed: 100 ms sanity ceiling, per-sample `uncertainty_us = delay/2`
(honest NTP bound), best-sample selection unchanged. Tests updated (ceiling boundaries + the
motivating 7 ms case asserting 3.5 ms uncertainty); 16 + 1 diagnostics tests green Mac + box.

## Live evidence gate (the §4 pass predicate)

Three runs retained; the final one (post-ERR-08, `r4-live-{host,client}-trace.jsonl`,
`r4-live-joined.jsonl`, `r4-live-join-summary.json`):

- *(Reconciled after C7 against the sealed run, per the response review: the corrective cycle's
  final gate run — checkpoint `8e7170f`, footer-gated, receipt-ledger identities, causal-bounded —
  joined **69** frames with **zero identity ambiguities and zero identity failures**, 57
  presented-joined, both footers clean. The figures below describe the pre-corrective interim
  runs and are retained as history.)*
- **PASS: 50 joined, zero identity ambiguities** (earlier 90 s run: 242 joined, zero
  ambiguities — identity chain proven at scale). Only rejections: `never_received` (frames sent
  during client connect — legitimate drops, correctly classified; drops ≠ ambiguities).
- **Offsets flow**: 50/50 samples carry offset ± uncertainty (~7.2 ms — the link's honest
  half-delay); 44/50 carry e2e (those with presents); 3 accepted clock samples,
  median delay 11.9 ms.
- Permission note: the first attempts produced clock-only traces — ScreenCaptureKit delivered
  zero frames until the user granted screen-recording permission to this process context; the
  granted run captured normally. (An ssh stdin-detach bug also wedged run 1's orchestration —
  fixed in the script with the failure documented.)

## Measurement interpretation (recorded so A0 reads the artifact correctly)

Pipeline-only segments are healthy: encode-out→send p50 0.12 ms; recv→decode p50 10.1 ms;
recv→present p50 22.9 ms / max 31.7 ms (vsync-paced). The seconds-scale
`e2e_capture_to_present_us` values are **content age, not pipeline latency**: on a static
virtual display SCK delivers coalesced frames carrying the content's original composition PTS
(and ~2 fps change-driven cadence — 60 Hz needs motion). The trace measures this truthfully;
the A0 baseline runs under real motion where capture PTS is fresh per frame, and the optical
spot-check validates absolute values. `capture_uncertainty_us = 0` reflects sub-microsecond
calibration brackets at µs resolution.

## Ladder state

All nine §4 items implemented and live-proven; joined artifact retained with zero identity
ambiguities and per-sample uncertainties → **locally verified (state 3)**. Commit at R7.
