# C1 oracle review — documented line-by-line check of `tools/gen_dispatch_fixtures.py`
# against IMPLEMENTATION_PLAN_V11.md §4 and docs/WIRE.md §1

Date: 2026-08-04 · Reviewer: root reviewer (required by `A00_COMPLETION_REPORT_AMENDED_review.md`
finding 2: "The Python oracle must be reviewed against V11/WIRE before using two implementations
to validate it"). Oracle version: the C1 regeneration (dispatch_cases.json sha256-16
`a132a6d41ce1717d`, 391 rows).

## Rule-by-rule verdicts

| # | Oracle rule (location) | Normative source | Verdict |
|---|---|---|---|
| 1 | `facts_consistent` (l.217): bootstrap ⇒ client no_run / host candidate; announced+profile_rejected ⇒ candidate; else active; zero id inside candidate/active rejected | Frozen C1 design (implements V11 §4's session-run lifecycle; host owns its id from start) | ✓ exact |
| 2 | Step order 0 consistency → 1 version → 2 run → 4 caps → 5 direction/phase → 6 semantics → 7 routing (l.417–454) | Frozen order (WIRE §1 caps "immediately post-decode") | ✓ exact |
| 3 | `env_run_id == 0` ⇒ violation, all payloads all phases (l.432) | V11 §4 nonzero run ids | ✓ |
| 4 | Learn only possible in no_run, and only announce survives step 5 there (l.434–437 + l.249–250: announce legal solely in bootstrap; every other bootstrap payload rejected) | WIRE §1 "first bootstrap message only"; response-review amendment: only a valid nonzero announce moves NoRun→Candidate | ✓ (learn-from-fatal removed: l.264–267) |
| 5 | Bootstrap FatalReport rejected both roles (l.267, l.292) | WIRE §1 "once a candidate run ID is known" | ✓ |
| 6 | Clock kinds require `trace_or_doctor` AND phase ≥ profile_accepted, inbound both roles (l.259–263, 287–290) and outbound (l.318–321, 340–343) | V11 §10 / WIRE §1 "trace/doctor mode after profile acceptance"; response-review: both directions | ✓ |
| 7 | FrameAck ordinal: `oldest_outstanding is None` or mismatch ⇒ violation (l.391–395) | V11 §4 / WIRE §1 "must name the oldest outstanding ordinal (both window sizes)" | ✓ |
| 8 | Rejected ProfileResult: code known AND class == deterministic; 0 caught by accepted-iff-zero; unknown/transient/terminal rejected (l.396–404) | V11 §4 known-nonzero-deterministic reject rule | ✓ |
| 9 | FatalReport.code `not in FATAL_CLASS` ⇒ violation (l.410–413); `FATAL_CLASS` (l.164) covers exactly 1–22, excludes 0, classes identical to `v3wire::classify` / `FatalCodeClass.classify` (11 det / 10 transient / 2 terminal) | V11 §4 zero/unknown ⇒ PROTOCOL_VIOLATION; classification table frozen in `fatal_code_classes.json` | ✓ |
| 10 | Valid FatalReport routes `remote_fatal:<class>` — no phase advance (l.450–453) | V11 §4 det/terminal→Failed, transient→Backoff as session dispositions (mapping done by the session actor; classes, not phases, cross this boundary) | ✓ |
| 11 | Outbound: fatal send requires run ≠ no_run (l.344–347); host path unconditional with step-0 guaranteeing non-no_run (l.322–323) | Response-review amendment 4 | ✓ |
| 12 | ERR-01 rows unchanged: host inbound input/heartbeat Active-only; activation transitions | CONTRACT_ERRATA ERR-01 | ✓ unchanged |

## Cross-checks

- The 36-row verdict delta vs the R5 vectors is exactly the predicted set (24 clock-normal,
  10 fatal-routing, 1 bootstrap-fatal, 1 pre-existing fatal special) — no unexplained changes.
- Both implementations pass all 391 rows (Rust 66+4, Swift 610/0), so implementation fidelity is
  vector-proven against this reviewed oracle rather than assumed.
- Generator idempotent; README row counts generator-owned.

## Accepted narrow scopes (recorded, not defects)

1. Outbound clock diagnostics specials cover both kinds × both roles at `active` only (8 rows);
   the phase×mode interaction is fully matrixed on the inbound side (24 rows) and the outbound
   base matrix covers Normal-mode rejection at every phase. The outbound gate's diagnostics check
   is a single mode conditional ahead of the shared phase table; residual risk accepted.
2. `RunFact` uses plain integers with a boundary zero-check instead of `NonZeroU64`, keeping the
   Swift/Rust shapes symmetric — matches the frozen design's "enforce at the boundary anyway".
3. Facts fields are flattened onto rows (existing schema convention) rather than nested.

## Verdict

**The oracle faithfully encodes V11 §4 + WIRE §1 as amended by the two reviews. C1 vectors are
fit to grade both implementations.** No conflicts between the frozen design and normative text
were found by the worker or by this review.
