# Review of the Response to the A0.0 Amended Completion Report Review

Reviewed: `A00_COMPLETION_REPORT_AMENDED_response.md`

Response SHA-256: `1d34ae091f7e75829f564b96c0179ab1a92c2dbfbfb75d1b041b5e128c9eb42c`

Prior review SHA-256: `1756e549a278c92cc5368794697d4f124152d314bed82784940faeaf23be51c0`

Review date: 2026-08-04

## Executive verdict

**CONDITIONAL ACCEPT of the response as the corrective-cycle disposition.**

**NOT implementation-ready exactly as written.**

The response accepts every substantive finding, preserves the phase gates, avoids another architecture rewrite, and adopts the right bounded C1–C8 shape. Nothing needs to be relitigated. However, three proposed mechanisms are internally wrong as written, and several status/evidence details remain fail-open or ambiguous:

1. the F1 receipt ledger is placed before a queue that intentionally drops frames, so permitted drops would become false unresolved-identity failures;
2. F5 proposes forcing real zero-output on CUVID even though `sw1-lowdelay` is the selected backend whose ERR-03 closure matters;
3. F7's manifest/evidence model risks requiring a commit to contain its own hash, which is self-referential.

Amend the response with the precise corrections below, then implementation of C1–C8 is **GO**. No new plan or another review-response round is needed before implementation.

There is no phase change now:

- **A0.0 remains incomplete.**
- **Stage 1 remains candidate.**
- **A0 remains NO-GO.**
- **T1 remains NO-GO.**

## Decision table

| Response section | Decision |
|---|---|
| Acceptance of the prior verdict | **Accepted** |
| Attribution and acknowledgment | **Accepted** |
| F1 identity-receipt correction | **Accepted in direction; ledger placement must change** |
| F2 dispatch facts/routing | **Accepted in direction; enforcement/result shape must be explicit** |
| F3 writer-spy proof | **Accepted; exact cross-TCP schedules must be named** |
| F4 timestamp and clock correction | **Accepted in direction; clean trace termination is missing** |
| F5 zero-output closure | **Accepted in direction; must target selected SW1** |
| F6 fail-closed repeated runs | **Accepted in direction; provenance assertions need tightening** |
| F7 evidence archive | **Accepted in direction; commit topology must avoid self-reference** |
| C1–C8 sequence | **Accepted after the amendments below** |
| Estimates | **Informational only; do not use as exit criteria** |

## Required amendments before implementation

### 1. Restore prospective status language and correct governance terminology

The response is an execution commitment, not an implementation report. The repository has no corrective source change; before this review was added, only the response and prior review were untracked. Nevertheless, the response says fixes are already “implemented,” “added,” “Rebuilt,” that the seal “follows” the new rules, and that factual corrections are “all applied.” Those statements conflict with C1–C8 scheduling the same work in the future and repeat the premature-status class the ladder was introduced to prevent.

Change all such wording to future or conditional form:

- “will implement”;
- “will add”;
- “will rebuild”;
- “the corrective seal will contain”;
- “scheduled for C5/C7/C8.”

Also correct two governance statements:

- ERR-09 is not yet “authorized” or present. It is a **possible future dated erratum**, triggered only if the bounded SW1 real-forcing attempt fails, and it must be written before equivalent evidence is accepted.
- `DispatchFacts` and regenerated vectors implement existing V11/WIRE rules; they are not a second erratum or contract delta. ERR-09 is the only possible new contract entry in this cycle.

### 2. Put the receipt ledger on the decode side, not before the allowed-drop queue

`recv_ts_us` is already stamped at assembly completion before the drop-capable queue in `ubuntu-client/crates/net-transport/src/video_receiver.rs:191–197`. That is correct and must remain immutable in `AssembledFrame`.

The response then says the pending receipt ledger is written before that queue. This conflicts with the intentional queue-full non-keyframe drop at `video_receiver.rs:201–210`: a dropped frame would retain a ledger entry forever and falsely fail the unresolved-at-EOF predicate.

Use the smaller single-threaded design:

1. Stamp `recv_ts_us` at assembly completion before the queue, as today.
2. Insert `frame_id -> recv_ts_us` into a hard-capped ledger **on the decode thread, immediately before submitting that admitted frame to the decoder**.
3. Resolve/remove it by each emitted frame's `recovered_frame_id`.
4. Record the current input separately as `decode_trigger_frame_id`.
5. Queue-dropped frames create no decoder-pending ledger entry; optionally emit a counted `queue_drop` trace event.
6. After a successful decoder EOF/tail drain, any remaining decoder-pending entry invalidates the trace.

Duplicate input IDs, duplicate recovered IDs, missing/out-of-range PTS, missing ledger entries, capacity overflow, or silent eviction must invalidate the trace. This preserves allowed drops while proving every submitted identity exactly.

An immutable multi-event trace (`receipt`, `queue_drop`, `decode_submit`, `decode_emit`) is also valid, but it is a larger joiner change. Do not create a cross-thread pre-queue ledger unless its full received→enqueued/dropped→submitted→emitted lifecycle is explicitly modeled.

The accepted upload correction remains unchanged: advance `new_video_frame` and `last_uploaded_recovered_id` only after `update_frame` succeeds.

### 3. Add a clean/aborted terminal protocol to the R4 trace gate

The response hardens the joiner's content predicate but does not define how a complete trace is produced. Current mechanics cannot prove unresolved-at-EOF or parse completeness:

- the client decode loop stops on channel disconnection without a decoder tail drain;
- the client trace has only periodic buffered flushes and no public finish/sync operation;
- the host trace has asynchronous writes/timer flushing but no completion footer;
- `tools/r4_live_gate.sh` terminates both processes with `pkill`/`kill` and then accepts whatever JSONL prefix survived.

Add a trace-mode shutdown path on both endpoints:

1. stop accepting new frames;
2. drain the admitted queue;
3. send decoder EOF and process all tail emissions through the same identity ledger;
4. require zero pending identities and zero trace-identity failures;
5. append one `trace_complete` footer containing a run token, status, pending/failure counts, and relevant drop counts;
6. synchronously flush and sync the trace file before exit.

On failure or interrupted shutdown, emit `trace_aborted` if possible. The live runner must request graceful termination, wait for clean exits, and fail on timeout/forced kill. The joiner must require one matching clean footer from each side and reject a missing, duplicate, aborted, corrupt, or truncated footer.

For the causal check, use host-local uncertainty only: capture timestamp/conversion uncertainty plus any encoder-stamp uncertainty. Cross-machine clock uncertainty does not belong in the host-local `capture <= encode_out + tolerance` invariant.

### 4. Make C1/C2's enforcement boundary and result types explicit

`DispatchFacts` must be consumed by **both** inbound validation and the shared outbound gate. Otherwise normal-mode clock sends or unknown-run fatal sends can remain legal even if inbound validation is fixed.

Keep remote fatal routing separate from the six protocol phases. `Failed` and `Backoff` are lifecycle dispositions, not seventh/eighth protocol phases. Use an exact result shape such as:

```text
AcceptedTransition(next_protocol_phase)
RemoteFatal(failure_class)       // session actor maps to Failed or Backoff
ProtocolError(fatal_code)
```

Reject inconsistent run/phase facts. The shared vectors must explicitly cover:

- ACK match, mismatch, and no-outstanding cases;
- bootstrap `FatalReport` rejection;
- zero-run rejection and valid nonzero announce learning;
- matching/mismatching Candidate and Active runs;
- fatal code 0, unknown numeric, deterministic, transient, and terminal classes;
- accepted/rejected `ProfileResult` code combinations;
- normal-versus-TraceOrDoctor clock matrices for inbound and outbound paths.

C2 must exercise deliberately delayed/reordered Video-Ack-handler and control-handler schedules and retain observed writer-attempt traces. A serial “before/after activation” assertion alone is not the required cross-TCP proof.

### 5. Force zero-output on `sw1-lowdelay`, not CUVID

CUVID is no longer the selected A0 backend. A real zero-output result from CUVID would be supplementary evidence; it would not close ERR-03 for `sw1-lowdelay`.

C4 must first run a bounded real-forcing attempt on the exact selected SW1 configuration. If it remains unforced:

1. record the bounded negative outcome;
2. enter dated ERR-09 **before** accepting the test-double equivalence;
3. scope the equivalence to the exact backend, FFmpeg build, decoder flags/options, driver/runtime where applicable, and sample/configuration fingerprint;
4. name changes to any of those inputs as invalidation triggers requiring re-characterization.

CUVID may also be tested, but it cannot substitute for selected-backend evidence.

### 6. Tighten F6 run identity and artifact assertions

The proposed runner repair is directionally correct. Add these exact requirements:

- use an isolated log directory or unique run token for every doctor/harness run;
- copy and parse the host's actual `doctor_host.json`, not console stdout labeled `.json`;
- retain the raw doctor JSONL and prove three fresh `doctor_complete` records tied to the three run tokens, rather than only a global `+3` count that rotation/concurrency could confuse;
- capture every local and SSH exit immediately, including the remote harness receiver's exit;
- make copy/missing-file failures fatal;
- validate exactly the current three host-doctor, client-doctor, sender, and receiver reports;
- require `sender_integrity_pass == true` and the receiver's full v2 predicate, not only exit zero or the legacy `sustained_60hz` name.

`sender_integrity_pass` remains an A0.0 integrity label, not the later A0 sustained-rate decision.

### 7. Use a non-self-referential evidence seal

A manifest committed in evidence commit `E` cannot contain `E`'s final Git hash: changing the manifest to insert that hash changes the commit hash.

Use one of these valid models. The simplest is:

1. **C — code checkpoint:** all corrective source, contract, fixtures, and tooling.
2. **E — evidence commit:** all commit-qualified artifacts plus a manifest that records `code_commit = C`, exact provenance, hashes, and matrix results. The manifest does not claim its own containing commit hash.
3. **R — report/attestation commit:** the corrected completion report records both `code_commit = C` and `evidence_commit = E`, and proves there is no source **or contract** difference from C through R.

Alternatively use only C→E and let the independent review identify E externally. Do not invent a self-hash field.

The manifest validator must fail on missing paths, byte-count/hash mismatches, malformed JSON/JSONL, absent required provenance/environment fields, nonempty clean-tree output, or incomplete predicate records. “Validating manifest generator” is not specific enough without those exit-affecting checks.

Finally, reconcile `r4-summary.md` **after C7 against the new sealed run**, not during C5 against the obsolete 63-frame artifact.

## What is accepted without change

- The response honestly accepts the prior verdict and keeps every downstream phase closed.
- No new architecture plan is proposed.
- The proposed nonzero run context, diagnostics-mode fact, oldest-outstanding fact, fatal classification reuse, and oracle review are the right R5 direction.
- The writer-spy approach is appropriately narrower than a premature live V3 cutover.
- V11's literal SCK→host-clock conversion, nil-clock fallback, session-global minimum-delay sample, and stronger live-gate predicates are the correct clock direction.
- The real EAGAIN/multi-output/exact-ordinal/tail evidence and provisional SW1 selection can be preserved.
- A dated equivalence erratum is an acceptable simple solution if selected-backend zero-output cannot be forced.
- The full Mac/Ubuntu/real-pair matrix rerun and independent re-review remain mandatory.
- A0 optical and formal window work correctly remain outside this corrective cycle.

## Revised execution sequence

The response's C1–C8 remains recognizable; only sharpen it as follows:

1. **C1:** complete inbound/outbound `DispatchFacts`, external fatal disposition, oracle review, and full negative vectors.
2. **C2:** add delayed/reordered cross-TCP scheduler + writer-attempt spy proof.
3. **C3:** repair decode-side receipt mapping, upload-confirmed identity, SCK conversion, host-local causal check, global-best clock use, clean/aborted trace termination, hardened joiner, and negative tests.
4. **C4:** attempt bounded real zero-output on selected SW1; add scoped ERR-09 before equivalence if unforced.
5. **C5:** make repeated-run tooling fail closed; add candidate comments and integrity naming.
6. **C6:** cut clean code checkpoint C.
7. **C7:** run the complete applicable-A0.0 matrix from C on Mac, Ubuntu, and the real pair.
8. **C8:** reconcile summaries to C7, seal evidence commit E, create report/attestation R, and request independent re-review.

After the amendments in this review are incorporated, implementation may begin immediately. State 5 still depends on the new checkpoint, complete evidence, and independent re-review—not on acceptance of this response.

## Verification performed for this response review

- Read the response against every finding and closure step in `A00_COMPLETION_REPORT_AMENDED_review.md`.
- Confirmed no corrective source/contract change exists yet; before adding this review, the response and prior review were the only new untracked files.
- Re-checked the pre-queue timestamp and intentional non-keyframe drop boundary in `video_receiver.rs`.
- Re-checked that the current client lacks decoder-tail/trace-finalization evidence and that the live R4 script uses forced termination.
- Re-checked the current trace writers' buffered-flush behavior and absence of completion/abort footers.
- Reconciled independent contract, runtime, and architecture assessments; all reached the same conditional-accept conclusion.

No tests or hardware runs were repeated because this response contains prospective commitments, not implementation changes.
