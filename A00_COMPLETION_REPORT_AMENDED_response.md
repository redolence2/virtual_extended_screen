# Response to the Amended Completion Report Review — v2

| | |
|---|---|
| **Date** | 2026-08-04 (v2 — amended same day per `A00_COMPLETION_REPORT_AMENDED_response_review.md`, which conditionally accepted v1 as disposition and required seven amendments before implementation; all seven are incorporated below and the v1 wording they correct is superseded) |
| **Responds to** | `A00_COMPLETION_REPORT_AMENDED_review.md` (state 5 not granted) and its response review (conditional accept; implementation GO once amended) |
| **Position** | **Both reviews accepted in full; nothing contested.** This document is an **execution commitment — no corrective source or contract change exists yet**; every fix named here is future work scheduled in C1–C8. v1's past-tense wording ("rebuilt", "added", "applied") violated exactly the premature-status rule the ladder exists to enforce and is withdrawn. A0.0 remains incomplete; Stage 1 remains candidate; A0 and T1 remain NO-GO. |
| **Attribution** | Unchanged from v1 and reaffirmed: five of the seven blocking findings trace to my artifacts (the R4 record schema, the R5 semantic scope, acceptance of the SCK-conversion deviation without an erratum, the windowed clock selection, the fail-open runner and manifest). The response review then found three more defects **in my proposed corrections themselves** (ledger placement, wrong forcing target, self-referential seal) — accepted below. |
| **Governance corrections** | ERR-09 is **not** authorized or written; it is a possible future dated erratum, triggered only if the bounded SW1 real-forcing attempt fails, and must exist **before** any equivalence evidence is accepted. `DispatchFacts` and the regenerated vectors implement **existing** V11 §4 / WIRE §1 rules — they are not a contract delta. ERR-09 is the only possible new contract entry this cycle. |

## 1. Independent re-verification of the prior review (all held; unchanged from v1)

| Review claim | My check |
|---|---|
| First decoded record pairs frame 0 with frame 4's `ts_recv_us` | Confirmed verbatim at `r4-live-client-trace.jsonl:2` |
| 30/68 host frames have `encode_out_ts_us < capture_ts_us`, worst ~9,553 µs | Confirmed: exactly 30/68, worst 9,553 µs |
| Global-best clock sample used on only 4 of 63 joins | Confirmed: seq usage {1: 4, 2: 23, 3: 17, 4: 19} |
| "Stage-1 frozen" survives in source comments | Confirmed at `control_v3.proto:3`, `protocol/src/lib.rs:15`, `WireRecords.swift:4` |
| `r4-summary.md` says 50 joined; sealed artifact says 63 | Confirmed stale |

## 2. Dispositions (all ACCEPTED; mechanisms as amended by the response review)

**F1 — R4 receive-identity.** The defect is my frozen schema (per-emission records weld the
trigger packet's receive stamp to whatever frame emerges). Corrected mechanism per the response
review — the v1 pre-queue ledger placement is withdrawn (it would turn the queue's *intentional*
non-keyframe drops into false unresolved-identity failures): `recv_ts_us` stays stamped at
assembly completion and immutable in `AssembledFrame`; the hard-capped `frame_id → recv_ts_us`
ledger will be inserted **on the decode thread, immediately before submitting each admitted
frame**; resolved/removed by each emission's `recovered_frame_id`; the current input recorded
separately as `decode_trigger_frame_id`; queue-dropped frames create no ledger entry (counted
`queue_drop` event); after a successful decoder EOF/tail drain any remaining entry invalidates
the trace, as do duplicate input IDs, duplicate recovered IDs, missing/out-of-range PTS, missing
entries, overflow, or any silent eviction. Negative tests will include the
`decode_trigger_frame_id != recovered_frame_id` fixture proving the joined receive stamp follows
the recovered identity. The upload correction stands: `new_video_frame` and
`last_uploaded_recovered_id` will advance only after `update_frame` succeeds.

**F2 — R5 semantics.** `DispatchFacts { run: NoRun | Candidate(nonzero) | Active(nonzero);
diagnostics: Normal | TraceOrDoctor; oldest_outstanding_ordinal }` will be consumed by **both**
`validate_inbound` and the shared outbound gate (inbound-only enforcement would leave normal-mode
clock sends and unknown-run fatal sends legal). Remote fatal routing stays **outside** the six
protocol phases — `Failed`/`Backoff` are lifecycle dispositions, not phases — with the exact
result shape `AcceptedTransition(next_phase) | RemoteFatal(failure_class) | ProtocolError(code)`;
inconsistent run/phase facts rejected. Regenerated vectors will explicitly cover: ACK
match/mismatch/no-outstanding; bootstrap FatalReport rejection; zero-run rejection and valid
nonzero announce learning; Candidate/Active run match and mismatch; fatal code 0 / unknown /
deterministic / transient / terminal; accepted/rejected ProfileResult code combinations; and
Normal-vs-TraceOrDoctor clock matrices for **both** directions. The python oracle will get a
documented line-by-line review against V11 §4 + WIRE §1 before grading either implementation.

**F3 — ERR-01 write-level proof.** A deterministic scheduler plus writer-attempt spy around the
shared outbound gate, exercising **named delayed/reordered cross-TCP schedules** (video-Ack
handler vs control handler in both orders and with delays), retaining the observed
writer-attempt traces: zero client control writes before activation, exactly the expected first
write after. The existing phase-prefix tests remain as the model half; a serial before/after
assertion alone does not close this.

**F4 — Clock path.** (4.1) V11's literal conversion will be implemented:
SCK PTS → `CMSyncConvertTime(…, stream.synchronizationClock, CMClockGetHostTimeClock())` →
bracketed calibration; nil clock → the labeled fallback. The causal-sanity gate check uses
**host-local uncertainty only** (capture conversion + encoder-stamp uncertainty — cross-machine
clock uncertainty does not belong in a host-local invariant); every violation explained and
bounded or the run is invalid. (4.2) Session-global minimum-delay sample for every frame, per
ERR-08 as written. (4.3) The gate predicate gains the six required conditions, **plus the
missing termination protocol the response review identified**: both endpoints get a trace-mode
shutdown path (stop intake → drain admitted queue → decoder EOF/tail through the same ledger →
require zero pending identities → append a `trace_complete` footer with run token, status, and
pending/failure/drop counts → synchronous flush+sync before exit; `trace_aborted` on failure);
the live runner will request graceful termination and fail on timeout/forced kill; the joiner
will require one matching clean footer per side and reject missing/duplicate/aborted/corrupt/
truncated footers. Corrupt input fails; `never_received` stays an allowed counted drop.

**F5 — Zero-output evidence.** v1's CUVID-first proposal is withdrawn — CUVID is supplementary;
ERR-03 closure matters for the **selected** backend. C4 will run a bounded real-forcing attempt
on the exact selected `sw1-lowdelay` configuration first. If unforced: record the bounded
negative outcome, then write dated **ERR-09** before accepting equivalence, scoped to the exact
backend id, FFmpeg build, decoder flags/options, and sample/configuration fingerprint, with
those inputs named as invalidation triggers requiring re-characterization.

**F6 — Repeated-run tooling.** Will be rebuilt fail-closed with the tightened provenance: an
isolated log dir or unique run token per run; the host's actual `doctor_host.json` copied and
parsed (not console stdout labeled `.json`); raw doctor JSONL retained with three fresh
`doctor_complete` records tied to the three run tokens (not a global +3 that rotation could
confuse); every local and SSH exit captured immediately including the remote receiver's;
copy/missing-file failures fatal; exactly the current three reports per side validated;
`sender_integrity_pass == true` and the receiver's full v2 predicate required — not exit codes
or the legacy `sustained_60hz` name (kept only as a labeled misnomer; the sustained-rate
decision stays in A0).

**F7 — Evidence seal.** v1's model risked self-reference (a manifest cannot contain its own
containing commit's hash). Adopted: the three-commit topology — **C** (code checkpoint) →
**E** (evidence commit: commit-qualified artifacts + manifest recording `code_commit = C`,
exact per-run executable command/cwd/start/end/exit/machine/gate, environments incl. SDL, raw
outputs where they are the proof, hashes; no self-hash field) → **R** (report/attestation
commit recording `code_commit = C` and `evidence_commit = E` and proving no source **or
contract** difference from C through R). The manifest generator will validate every referenced
path, byte count, hash, JSON/JSONL parse, required provenance field, and asserted-empty
clean-tree output, failing on any miss. `r4-summary.md` will be reconciled **after C7 against
the new sealed run**, not against the obsolete 63-frame artifact.

## 3. Corrective cycle C1–C8 (the response review's revised sequence, adopted verbatim)

| # | Work (all future tense; nothing below exists yet) |
|---|---|
| C1 | Inbound **and outbound** `DispatchFacts`, external fatal disposition result shape, documented oracle review, full negative vectors both languages |
| C2 | Delayed/reordered cross-TCP scheduler + writer-attempt spy proof, traces retained |
| C3 | Decode-side receipt ledger, upload-confirmed identity, `CMSyncConvertTime`, host-local causal check, global-best clock, clean/aborted trace termination protocol, hardened joiner, negative tests |
| C4 | Bounded real zero-output attempt on selected `sw1-lowdelay`; scoped dated ERR-09 before equivalence if unforced |
| C5 | Fail-closed repeated-run tooling with run-token provenance; `sender_integrity_pass`; candidate source comments |
| C6 | Clean code checkpoint **C** |
| C7 | Complete applicable-A0.0 matrix from C: Mac, Ubuntu, real cross pair, selected backend where relevant (no A0 optical/window work) |
| C8 | Reconcile summaries to C7's sealed run; evidence commit **E**; report/attestation commit **R**; independent re-review |

Time estimates are omitted per the review: they are informational only and never exit criteria.

## 4. Standing acknowledgment

v1 of this response — the document acknowledging premature status claims — itself claimed its
fixes in the past tense before any existed. That is now three escalating instances of the same
class (completion before evidence; consistency read as completeness; commitment written as
completion), each caught by the review layer above the author. The ladder wording rule is
therefore extended in practice: execution commitments are written in the future tense, and only
retained evidence from a sealed run may use the past. State 5 depends on the new checkpoint,
complete evidence, and independent re-review — not on acceptance of this response.
