# R3b evidence summary — locally verified (ladder state 3)

Date: 2026-08-04 · Executor: root reviewer (scripted, `tools/r3b_runs.sh`).
Scope: `A00_REMEDIATION_PLAN.md` §5 R3b — repeated selected-profile doctor + harness evidence
on the R6-selected backend (`sw1-lowdelay`).

## Runs and results (all retained as `r3b-*.json` in this directory)

- **Host doctor ×3** (Mac): exit 0 / 0 / 0; `doctor_complete` 20 → 23 (+3 — every run persisted;
  the historical intermittent loss remains dead under repetition).
- **Client doctor ×3** (box, `sw1-lowdelay`, full 480-AU sample, texture-update path): exit
  0 / 0 / 0; `doctor_complete` 19 → 22 (+3).
- **Harness pair ×3** (sender Mac ↔ hardened v2 receiver on box, 10 s each, window 1):
  sender exits 0/0/0 with `sustained_60hz` (the pure integrity verdict) true on all three —
  sent==acked 350/350, 587/587, 592/592, zero ACK-order violations, zero write errors, zero
  outstanding; receiver `pass: true`, `report_v: 2` on all three. Run 1's lower throughput is
  first-run warmup (decoder open + TCP ramp); runs 2–3 sustain ~59 fps. The formal window
  trial is A0 work, per the plan's phase boundaries.

## Ladder state

R3b → **locally verified (state 3)**. Commit at R7.
