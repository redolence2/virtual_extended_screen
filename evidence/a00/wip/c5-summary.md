# C5 evidence summary — fail-closed run tooling, integrity naming, candidate comments

Date: 2026-08-05 · Executor: root reviewer (inline). Scope: corrective item C5 (review finding
6, as amended by response-review amendment 6, + the factual-corrections list).

## Delivered

- **`tools/r3b_runs.sh` v2, fail-closed**: unique run token; every exit captured immediately
  (sender, ssh, and the remote receiver via an exit-file wrapper — never a trailing echo);
  the host's REAL `doctor_host.json` copied and parsed (`exit_code` asserted 0; stdout retained
  separately as `.log`); per-run `doctor_complete` deltas asserted `== 1` (host) and isolated
  per-run `RESC_LOG_DIR` counts asserted `== 1` (client); copy/missing-artifact failures fatal;
  only the current token's artifacts validated; `sender_integrity_pass` + `harness_report_v: 2`
  + receiver `pass` + `report_v: 2` all required.
- **Integrity naming**: `sender_integrity_pass` is the canonical sender field;
  `sustained_60hz` retained as a documented legacy-misnomer alias (never a rate threshold —
  the formal window decision stays in A0).
- **Candidate comments**: the three "Stage-1 frozen" source comments
  (`control_v3.proto`, `protocol/src/lib.rs`, `WireRecords.swift`) → "Stage-1 CANDIDATE
  (freeze pending the A0.0 gates and independent re-review)"; protobuf regenerated; generated
  Swift verified free of "frozen" wording; regen `--check` clean.

## Process finding (recorded honestly)

The runner's own first execution hung 90 minutes at the harness receiver launch — the SAME
ssh-linger class the C3-client worker had just fixed in `tools/r4_live_gate.sh` (a foreground
ssh whose remote command backgrounds a process never returns despite full fd redirection); this
script was written in parallel and missed the lesson. Third occurrence of the class in the
project. Fixed the same way (launch ssh backgrounded; separate bare-name liveness probe;
comment citing all incidents). Note the fail-closed design's backhanded success: it HUNG
visibly rather than proceeding on phantom success. The doctor phases of the aborted run were
already green (their artifacts retained under token `r3b-20260805023043-76189`).

## Verification — full clean run, token `r3b-20260805040520-78050`

Host doctor ×3: exit 0, report exit_code 0, delta +1 each. Client doctor ×3: exit 0, isolated
doctor_complete == 1 each. Harness ×3: sender exit 0 AND receiver exit 0 each (receiver exits
by its own EOF/tail path — never killed). Predicate validation over this token's six reports:
**ALL GREEN**, runner exit 0.
