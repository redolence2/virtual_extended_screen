# A0.0 Amended Completion Report

| | |
|---|---|
| **Date** | 2026-08-04 |
| **Status claim** | **A0.0 remediation complete at ladder state 4** — every gate locally verified (state 3), then re-run and passing **from clean checkpoint `fbfbdc99b4350606e1d8cd78665c8396d7e029f3`** on both machines. State 5 (gate closed) requires this document to pass independent re-review; until then A0.0 completion, Stage-1 freeze, and A0 entry remain formally pending, per `A00_REMEDIATION_PLAN.md` §8. |
| **Supersedes** | The withdrawn completion claim of `A00_IMPLEMENTATION_REPORT.md` (still the detailed build record) via the remediation R1–R7 of `A00_REMEDIATION_PLAN.md`. |
| **Evidence root** | `evidence/a00/fbfbdc99…/manifest.json` (13 matrix gates, environments, commands, exit codes, 40 SHA-256-hashed artifacts) + per-item summaries under `evidence/a00/wip/`. |
| **Contract deltas this cycle** | **ERR-07** (ASCII-only profile strings, replacing NFC) and **ERR-08** (clock-sample acceptance: 100 ms ceiling + honest `delay/2` uncertainty replacing the unsatisfiable 5 ms gate) — both dated in `CONTRACT_ERRATA.md` before implementation; a WIRE-status governance note likewise. |

## 1. Gate table (each: what was proven → evidence)

| Item | Ladder state | Proof, in one line |
|---|---|---|
| R1 pins + ERR-07 | 4 | Exact `=7.1.0/=7.1.3` workspace-wide with byte-identical lockfile (hash chain); ASCII gate ordered before canonical-bytes comparison, identical verdicts both languages on raw + escaped fixtures — `r1-summary.md` |
| R5 typed dispatch | 4 | Framing gate (64 KiB pre-allocation, zero-read/zero-alloc proof) + pure validator/router; six-phase model; **335 oracle vectors verdict-identical in both languages** (three independent transcriptions of the outbound tables agreed first-run) — `r5-summary.md` |
| R2a ERR-01 barrier | 4 | Scheduling traces both ends: input armed exactly at activation; reordered handlers surfaced as PROTOCOL_VIOLATION, never silently armed — `r2a-summary.md` |
| R2b capture identity + cursor clock | 4 | GenerationalFrameSlot rejects late callbacks from torn-down runs (counted; -3805 restart safe by construction); cursor timestamps continuous-monotonic with seq-governs-ordering proven under a non-monotonic injected clock — `r2b-summary.md` |
| R4 trace identity + joiner | 4 | Exact identity capture→encoder-closure→frameID→PTS-recovery→present; live gate: **242 and 63 joined frames, zero identity ambiguities**, per-sample offset ± uncertainty flowing post-ERR-08; static-content e2e interpretation recorded — `r4-summary.md` |
| R3a fail-closed doctors/harness | 4 | Every load-bearing check exit-affecting; 8+5 injection ids each force nonzero exit; `doctor_complete` persists every run (the 2-of-3 loss dead); texture-UPDATE paths validated with real decoded planes; two-process flock proofs; zero-frame vacuity closed — `r3a-host-summary.md`, `r3a-client-summary.md` |
| R6 ERR-03 + backend selection | 4 | Real send-EAGAIN forced 3× on BOTH backends (bounded protocol, exactly-once, 480/480/480, real multi-output drains + tail flush); scripted double covers hypothetical branches; **selected: `sw1-lowdelay`, `decoder_lag_bound=1`, `output_deadline_ms=50`** (max observed delay 7.67 ms vs cuvid's 17.01 ms outlier) — `r6-summary.md` |
| R3b repeated evidence | 4 | Doctors 3×0 both ends (+3/+3 `doctor_complete`); harness pairs 3× sent==acked, zero violations, v2 passes — `r3b-summary.md` |
| R7 matrix | 4 | 13 gates PASS from `fbfbdc9` on Mac + box + cross (same-commit verified, box dirty=0, `--locked` builds); one reproducibility defect found by the matrix itself (generator clobbered a hand-edited README section) and fixed as the checkpoint's own corrective commit — the manifest |

## 2. What the process caught this cycle (recorded per the standing acknowledgment)

Workers caught two errors in my frozen specs (joiner offset sign; empty-envelope verdict); my
reviews caught three in worker output (duplicate-event timestamps, missing Swift outbound
coverage, zero-frame vacuity pass); the matrix caught one in the tree (README regeneration
clobber); and the live gate caught one in the contract itself (ERR-08). All fixed with the
finding recorded at the point of correction. Honest failure notes retained: two wedged ssh
launches during the first live-gate attempts, and a permission-blocked capture run — all in
`r4-summary.md`.

## 3. Phase position (per `A00_REMEDIATION_PLAN.md` §8 — unchanged)

1. ✔ R1–R6 + R3b locally verified with retained evidence (state 3)
2. ✔ R7 clean commit + complete applicable-A0.0 matrix pass (state 4)
3. → **independent re-review of this report** (state 5)
4. Then: A0.0 complete, Stage 1 freezes, A0 begins (real-pair histogram under motion, software
   latency baseline + optical spot-check, formal window trial, bitrate confirmation, Stage-2
   final profile/fixture freeze — the profile then gains the R6-selected backend and bounds)
5. Only after V11's separate T1 entry checks and both final-profile doctors: T1.

Every status line in the repo continues to read *candidate* until step 3 returns GO.
