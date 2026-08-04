# R3a (host half) evidence summary — locally verified (ladder state 3)

Date: 2026-08-04 · Executor: doctor/harness-implementer worker + root-reviewer verification and
one inline hardening fix · Base: `ce2d693` + R1 + R5 + R2a + R2b + R4(code).
Scope: `A00_REMEDIATION_PLAN.md` §5 R3a, Mac-host portion (F5 host doctor, F7 exit-flush/locks,
F6 sender predicates). The client-half (client doctor texture-path validation, receiver predicate)
is **pending the box** — R3a stays open until it lands.

## Delivered

- **Checked-status encoder** (F5, pulled forward from T1): every `VTSessionSetProperty` in
  `VideoEncoder.start()` routed through a throwing `setProperty` helper (session invalidated
  before throw; exact key + OSStatus in the error); `PrepareToEncodeFrames` checked;
  `PrioritizeEncodingSpeedOverQuality` is the one documented WARN-and-continue exception.
  Surfaced at all three call sites (main.swift encoder thread now exits nonzero; doctor records
  key/status; harness already exited nonzero).
- **Fail-closed host doctor**: profile-true settings incl. the ~60 s safety keyframe interval
  (MaxKeyFrameInterval 3600 / Duration 60) with read-backs for both; NV12 pixel-format in the
  exit condition (real FourCC 420v verified); every load-bearing check in the exit code;
  CoreBrightness stays recorded-only by design. `RESC_DOCTOR_INJECT=<8 check-ids>` failure
  seam — each id independently produces exit 3 (all eight spot-checked by the worker), proving
  every predicate can actually fail. Report file fsynced; `doctor_complete` + synchronous
  `RescLog.flushNow()` on every return path.
- **Exit-flush + lock hygiene** (F7): lock-denial path now writes a synchronous fatal record
  (INSTANCE_LOCK_HELD, code 20) before `exit(20)`; `fatal()` verified synchronous end-to-end
  (queue.sync through synchronizeFile — not modified). The real 0600 gap was `InstanceLock`'s
  `open(O_CREAT, 0o600)` whose mode is kernel-ignored for pre-existing files — explicit
  `chmod` on every open now (RescLog/doctor already reasserted). Two-process flock contention
  proof: FixtureCheck re-spawns itself as a `RESC_LOCK_CONTENTION_CHILD`, child exits 20 while
  parent holds and 0 after release.
- **Sender predicates** (F6): `frames_sent` counted only after confirmed full write; first write
  error fail-stops the run; `harness_report_v:2` adds `ack_order_violation`/`write_errors`/
  `outstanding_at_end`; exit code driven by pure `RescCore/HarnessVerdict.evaluate`, with
  per-argument flip proofs in FixtureCheck.

## Root-reviewer findings on the delivery

- Accepted: the `Config.forcePrepareFailure` typed seam (cleaner than env-reads inside RescCore);
  the honest "already satisfied, changed nothing" notes; the InstanceLock 0600 root-cause.
- **Rejected and fixed inline**: `HarnessVerdict.evaluate` passed a zero-frame run vacuously
  (0==0, all zeros ⇒ sustained_60hz true) and the worker had codified that as an intended check.
  Overruled: `sent > 0` is now required, mirroring the receiver's nonzero-frames rule; the check
  flipped to assert the vacuous run FAILS. This is the fail-open-through-vacuity class F6 exists
  to close.

## Verification (root reviewer, independent)

- `resc-fixture-check`: **554 ok / 0 FAIL / exit 0** (539 + 15 new (j) checks), re-verified after
  the vacuity fix.
- Host doctor: exit 0 (keyframe read-backs 3600/60 match, NV12 true);
  `RESC_DOCTOR_INJECT=readback` → exit 3 with the forced mismatch visible in the report.
- **Evidence-persistence race closed**: `doctor_complete` count 18 → 20 across one normal + one
  injected run — every run persists exactly one record (worker's clean-build pass showed the
  same every-run arithmetic; the historical 2-of-3 loss is gone).
- Worker's final numbers came from a from-scratch rebuild (`rm -rf .build`, 329.79 s).

## Ladder state

R3a host half → **locally verified (state 3)**. R3a overall remains open pending the client half
(box required: ffmpeg-linked crates only compile there).
