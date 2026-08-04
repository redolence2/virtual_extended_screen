# R3a (client half) evidence summary — locally verified (ladder state 3)

Date: 2026-08-04 · Executor: doctor/lock-implementer worker + root-reviewer review and two
follow-on inline hygiene fixes. Completes R3a (host half: `r3a-host-summary.md`).

## Delivered

- **Fail-closed client doctor** (`doctor.rs`, 578→922): the audit found real rot — a hardcoded
  `SAMPLE_AU_LIMIT = 60` (doctor decoded 1/8th of the sample) and NO `submitted == emitted`
  clause at all. Now: complete 480-AU decode + EOF/tail drain, predicate = submitted==emitted ∧
  exact-once ordinal recovery ∧ zero unknown/dup/reordered ∧ nonzero frames, all named clauses.
- **Texture-UPDATE-path validation** (was creation-only): a real decoded frame's planes drive
  the candidate's actual update call — sw1 → `SDL_UpdateYUVTexture`, cuvid → `SDL_UpdateNVTexture`
  via FFI with genuinely GPU-decoded, transferred NV12 planes; candidate exit-affecting, other
  format informational with explicit `exit_affecting` field. Worker's guard fix accepted: exact
  plane-count required per format (the old `>=2` let yuv420p planes "pass" the NV12 probe with
  wrong-semantics data). Headless via `SDL_VIDEODRIVER=dummy` (defaulted, never overriding);
  `.accelerated()` dropped from the doctor's throwaway canvas (dummy has no GPU renderer).
- **Failure injection**: `RESC_DOCTOR_INJECT=` open|decode|ordinals|tail|texture — sweep on box:
  all five exit 3 (with correct `open` cascade), baseline exit 0 on BOTH candidates;
  `doctor_complete` arithmetic 5 runs → 5 records (and 14→19 across the final sweep).
- **Lock/log hygiene**: `acquire_at()` path override + 0600 reassertion (same kernel-ignores-
  mode-on-existing bug as the host side); `jsonl.rs` `open_0600` reasserts unconditionally
  (its doc-comment claim that files are "always created fresh" was false for `client.jsonl`) +
  regression test; report JSON fsync added; flush-on-all-paths audited (single linear path).
- **Two-process flock contention test** (`tests/lock_contention.rs`): real `execve` child
  (strace-verified distinct PIDs), denied-while-held → succeeds-after-release; 5× no flakes.

## Root-reviewer follow-ons (after the worker released the crate)

- `trace.rs` had the identical 0600-only-at-creation bug — worker correctly reported it as
  out-of-scope rather than drive-by fixing; fixed inline by root reviewer (reassert on open).
- ERR-08 clocksync change implemented here too (see `r4-summary.md`).

## Verification

Box workspace suite green including the new tests (16 diagnostics + 1 contention); both
candidates' doctors exit 0 with update-path evidence; injection sweep all-exit-3;
`git status` scope exactly the four files claimed.

## Ladder state

R3a complete (both halves) → **locally verified (state 3)**. Commit at R7.
