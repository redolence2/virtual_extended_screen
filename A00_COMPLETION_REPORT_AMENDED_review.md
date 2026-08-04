# A0.0 Amended Completion Report Review — State 5 Not Granted

Reviewed: `A00_COMPLETION_REPORT_AMENDED.md`

Review date: 2026-08-04

| Artifact | SHA-256 / commit |
|---|---|
| Amended completion report | `a4abab8fd68e52300542a8dd13d641038fac47fb02ad119b25c8514010cd806a` |
| Amended remediation plan | `e21f6d9724f57a99ed1f0c2af9631dbdc4869fb4837ed0ed4f34158d532f9d55` |
| Governing V11 plan | `f4925709d2655d05c373cccdb5fc106efb57d90463e803041af63be6acdf1d67` |
| Contract errata | `3c0a807130ebf0d9fffaa59b70274268731c8ec384f8ae5bb39c9b5741eb3803` |
| WIRE | `40456f813d100598aba6a5a9829d42e0437b924f7f3806edc47c7fced7f5744a` |
| Evidence manifest | `252d85070e6669ca2e435d4aa0d079af379755159abe9f823750f92b934a3491` |
| Claimed tested code checkpoint | `fbfbdc99b4350606e1d8cd78665c8396d7e029f3` |
| Report/evidence commit reviewed | `5512df552aa42e5cc9266327e5decc1187ad1c1d` |

## Executive verdict

**REJECT the report's overall ladder-state-4 claim.**

**STATE 5 IS NOT GRANTED.**

Therefore:

- **A0.0 remains incomplete.**
- **Stage 1 remains candidate; it does not freeze.**
- **A0 entry remains NO-GO.**
- **T1 entry remains NO-GO.**

The remediation is substantial and several components are now strong. This is not a reason to redesign the system or write another large plan. It needs one bounded corrective cycle. The blocking findings are concrete implementation, contract-coverage, and fail-open-evidence defects.

The most serious defect is in the retained R4 evidence itself: decoded frame identity is recovered correctly, but it is paired with the receive timestamp of the packet that happened to trigger the decoder drain. The joined artifact can therefore report zero identity ambiguities while computing end-to-end timing from mismatched identities. Separately, the R5 oracle and both implementations agree with one another but omit frozen V11 invariants, so the 335-vector agreement is not a complete contract proof.

## Decision by work item

| Item | Review decision |
|---|---|
| R1 pins + ERR-07 | **Accepted** |
| R5 framing layer | **Accepted in isolation** |
| R5 typed validator/router | **Not closed — incomplete semantics/context** |
| R2a ERR-01 phase model | **Useful foundation; required write-level proof missing** |
| R2b generational capture slot + cursor clock | **Accepted, except the SCK timestamp conversion path** |
| R4 trace identity/clock/joiner | **Not closed — retained timing is not identity-exact and clock rules diverge** |
| R3a doctors and core predicates | **Implementation mostly credible; evidence automation still fail-open** |
| R3b repeated evidence | **Not closed — runner can report success after failed doctor runs** |
| R6 backend selection | **`sw1-lowdelay` choice and measured bounds accepted provisionally; ERR-03 closure missing** |
| R7 checkpoint/matrix | **Not closed — archive and manifest do not satisfy the accepted evidence specification** |

## Blocking findings

### 1. R4 joins a recovered frame ID to another frame's receive timestamp

The decoder correctly treats each emitted frame's recovered PTS as its own identity:

- `ubuntu-client/crates/video-decode/src/lib.rs:199–207`
- `ubuntu-client/crates/video-decode/src/lib.rs:294–306`

The caller then logs, for every emitted frame, the current decode input's `assembled.frame_id` and `assembled.recv_ts_us`:

- `ubuntu-client/src/main.rs:431–440`
- `ubuntu-client/src/main.rs:454–488`

Those values do not necessarily belong to the emitted frame when decode is delayed or one drain emits multiple frames. The retained trace proves the mismatch on its first decoded record:

```json
{"recovered_frame_id":0,"wire_frame_id":4,"ts_recv_us":2980003}
```

That is `evidence/a00/wip/r4-live-client-trace.jsonl:2`. Frame 0 is paired with frame 4's assembly-completion timestamp. `tools/join_trace.py` joins on `recovered_frame_id` but carries that mismatched `ts_recv_us`, so its “zero identity ambiguities” predicate cannot detect this error.

This invalidates the report's exact capture→receive→decode→present timing claim. It does not invalidate the useful proof that FFmpeg PTS recovery itself works.

**Required correction:**

1. Keep a trace-only, hard-capped `frameID -> recv_ts_us` ledger, or emit a separate immutable receipt record keyed by wire frame ID before the drop-capable queue.
2. At decoded emission, use `recovered_frame_id` to retrieve the original identity's receipt timestamp. Record the current input separately as `decode_trigger_frame_id`; never call it the emitted frame's wire identity.
3. Fail the trace run on duplicate input ID, duplicate recovered ID, missing/out-of-range PTS, missing ledger entry, capacity overflow, or unresolved entries at EOF. Never evict silently.
4. Add delayed-output, multi-output, zero-output-then-later-output, duplicate, unknown, overflow, and unresolved-tail negative tests. Include a fixture where `decode_trigger_frame_id != recovered_frame_id` and prove the joined receive timestamp follows the recovered identity.

There is a second identity bug at presentation. `ubuntu-client/src/main.rs:462–465` ignores `renderer.update_frame(decoded)` failure, yet still sets `new_video_frame` and `last_uploaded_recovered_id`. A later `present()` can therefore show the previous texture while tracing the new ID. Update the presented ID only after a successful texture update; otherwise record a render failure/drop and do not claim a new-frame presentation.

### 2. R5's matching oracle and dispatchers omit frozen protocol semantics

The 335 shared-vector matches demonstrate cross-language consistency, but the Python oracle, Rust dispatcher, and Swift dispatcher share the same omissions:

1. **Zero run IDs are accepted or learned.** V11 §4 requires run IDs to be nonzero. `validate_inbound`/`validateInbound` learns `0` when no expected run is present.
2. **A bootstrap `FatalReport` can create the candidate run.** Current client-bootstrap logic accepts it and learns its ID. WIRE permits `FatalReport` only once a candidate run is already known; `HostProfileAnnounce` is what establishes the client's candidate.
3. **`FatalReport.code` is not validated or routed.** V11 requires zero/unknown to become `PROTOCOL_VIOLATION`; known deterministic/terminal codes route to Failed, and transient codes route to Backoff. Existing fatal-code classifiers can be reused.
4. **Rejected `ProfileResult` accepts any nonzero numeric.** V11 requires a known, nonzero deterministic reject code. Unknown, transient, or terminal numerics must not pass this field.
5. **`FrameAck` is phase-only.** Neither dispatcher receives or checks the oldest outstanding ordinal required by V11 §4 and WIRE §1.
6. **Clock messages are accepted in normal mode.** V11 permits them only in trace/doctor mode after profile acceptance, but the API/oracle has no diagnostics-mode fact.

Relevant source locations include:

- `ubuntu-client/crates/protocol/src/v3dispatch.rs:93–147,185–207,256–295`
- `mac-host/Sources/RescCore/V3Dispatch.swift:125–173,207–234,261–284`
- `tools/gen_dispatch_fixtures.py:244–290,363–377`

**Required correction:** extend the pure dispatch context rather than creating a second state machine. At minimum it needs:

```text
DispatchFacts {
  run: NoRun | Candidate(nonzero) | Active(nonzero),
  diagnostics: Normal | TraceOrDoctor,
  oldest_outstanding_ordinal: optional ordinal
}
```

Only a valid nonzero `HostProfileAnnounce` may move the client from `NoRun` to `Candidate`. Add a routing disposition for fatal reports rather than pretending their phase is unchanged. Regenerate vectors with explicit zero/unknown fatal codes, deterministic/transient/terminal classifications, accepted/rejected `ProfileResult` combinations, ACK match/mismatch/no-outstanding cases, bootstrap fatal rejection, nonzero announce learning, and normal-versus-diagnostics clock matrices. The Python oracle must be reviewed against V11/WIRE before using two implementations to validate it.

### 3. The ERR-01 proof stops at the pure model, not the required write boundary

The phase tests are valuable and show that `note_outbound` rejects pre-activation input. They do not observe a control writer or delayed/reordered TCP-handler scheduling. The normative proof in `docs/WIRE.md:133` requires showing that no client control payload is written before activation and that the first post-barrier input is accepted.

Because live V3 cutover remains T1 work, no production-network integration is needed here. The smallest closure is a deterministic scheduler plus writer spy around the shared outbound gate: reordered Video-Ack/control events must produce zero writes before activation and exactly the expected first write afterward. Keep the existing phase-prefix tests as the model half of this proof.

### 4. The SCK and cross-machine clock path does not implement its governing contract

#### 4.1 SCK PTS conversion is bypassed

V11 §10 requires:

```text
SCK PTS -> CMSyncConvertTime(..., stream.synchronizationClock,
                             CMClockGetHostTimeClock())
        -> bracketed host-time/continuous-time calibration
```

`mac-host/Sources/RemoteDisplayHost/DisplayCapturer.swift:254–263` instead assumes the numeric PTS is already in the mach-absolute host domain and skips `CMSyncConvertTime`. No dated erratum authorizes that deviation. An evidence summary calling it an “accepted worker deviation” cannot override V11.

The local ScreenCaptureKit SDK exposes nullable `SCStream.synchronizationClock`, and the callback already receives the `SCStream`, so the contracted conversion is directly implementable. If the clock is nil, use the labeled callback fallback already specified by V11.

The retained host trace also contains **30 of 68 frames** for which `encode_out_ts_us < capture_ts_us`, by as much as **9,553 µs**, while `capture_uncertainty_us` is zero. That may reflect SCK PTS semantics rather than a simple arithmetic error, but it is unexplained and cannot support a trustworthy latency path as recorded. After the conversion fix, add a causal-sanity check such as `encode_out + combined_uncertainty >= capture`; either explain and bound every violation or invalidate the measurement run.

#### 4.2 ERR-08 says global best; the joiner uses time-local samples

ERR-08 says the minimum-delay sample remains the authoritative offset. `ClockSync` retains that sample, but the caller logs every accepted sample and the joiner selects a minimum only within a ±5-second window (`tools/join_trace.py:120–131`).

In the retained run, the global best is sequence 1 at 7,449 µs delay. Only 4 of 63 joined frames use it; the other 59 use sequences 2–4 with larger delays. This is an undocumented change from the erratum, not its implementation.

Use the session-global best sample for every frame, or add a dated erratum defining and justifying a time-local estimator. For this fixed short run, global-best selection is the simpler choice.

#### 4.3 The live gate can pass without clock or presentation evidence

`tools/join_trace.py:266–270` passes when identity ambiguities are zero and at least one frame joins. It can pass with zero accepted clock samples, zero presented joined frames, missing offsets, and skipped malformed JSON. `tools/r4_live_gate.sh` exits only with that joiner result.

The A0 entry prerequisite is trace joining **plus clock uncertainty evidence**. The gate must require, at minimum:

- zero parse errors and zero explicit `trace_identity_failure` records;
- at least one accepted clock sample;
- at least one presented, exactly joined frame;
- offset and uncertainty on every frame used for end-to-end timing;
- zero duplicate/unrecovered/missing-ledger identities;
- successful upload before a presentation identity is recorded.

`never_received` can remain an allowed, counted drop. Corrupt input must fail rather than be silently skipped.

The 100 ms ERR-08 sanity ceiling is not itself a product-latency acceptance threshold and is not rejected by this review. It is diagnostics-only; the final A0 optical check remains required.

### 5. R6 silently substitutes a test double for an unforced real case

The real characterization is useful: both backends show real EAGAIN, multi-output drains, exact 480/480/480 coverage, and EOF/tail behavior. The `sw1-lowdelay` selection, observed lag bound 1, and provisional 50 ms output deadline are reasonable for this fixed pair.

However, `evidence/a00/wip/r6-summary.md:18–24` records **zero real zero-output packets** on both backends and substitutes the scripted double for that branch. `A00_REMEDIATION_PLAN.md:42–50` explicitly says that when a stated real case cannot be forced, the outcome must be recorded and a dated equivalence erratum must be added before substitution. Current ERR-08 is about clocks and does not authorize decoder evidence substitution.

The simplest closure for this personal, fixed-backend system is likely a narrow **ERR-09**: state why real zero-output could not be forced under the bounded fixed sample/configuration; define the accepted combination of real EAGAIN/multi-output/exact-ordinal/tail evidence plus deterministic zero-output state-machine tests; and state what future backend/FFmpeg change invalidates that equivalence. Alternatively, force and retain a real zero-output case. Do not claim ERR-03 closed before one of those occurs.

### 6. The repeated doctor runner is fail-open

The six retained doctor reports are distinct and contain `exit_code: 0`; this finding does not allege that those six runs failed. It shows that `tools/r3b_runs.sh` cannot prove they passed:

- host status is inspected only after `echo`, so `$?`/`PIPESTATUS` refers to `echo`;
- the remote client command ends with `echo`, causing SSH to return success even if the doctor failed;
- neither `doctor_complete` delta is asserted to equal exactly `+3`;
- failed or partial `scp` is not fatal;
- unrestricted globs can accept stale prior reports;
- host files named `.json` contain console lines before the JSON body and fail normal JSON parsing.

**Required correction:** uniquely scope or remove prior artifacts; capture each exit immediately; propagate the remote exit through SSH; require exactly three fresh parseable reports per side with `exit_code == 0`; assert both completion deltas are exactly `+3`; make copy/missing-artifact failures fatal; and evaluate only the current run's exact paths. Copy the actual `doctor_host.json`, or store stdout as `.log` and retain the real JSON separately.

The sender field `sustained_60hz` is also misleading: the accepted R3a definition makes it an integrity predicate, so a 35 fps run is labeled true. This is not independently blocking because the formal rate/window decision is A0 work. Add `sender_integrity_pass` and label the old field a legacy misnomer; do not pull the formal 60 Hz threshold into A0.0.

### 7. R7's archive does not satisfy the accepted evidence specification

All 40 manifest-listed hashes match the current files. That proves current artifact integrity, but not the complete provenance required by `A00_REMEDIATION_PLAN.md:131–139`.

The checkpoint directory contains only `manifest.json`; the artifacts remain under `evidence/a00/wip/`. The manifest omits per-run working directories and start/end timestamps, omits explicit exit codes for some gates, omits SDL from the environment, and uses shorthand rather than executable commands for several gates. It retains no final raw output for several build/test/injection gates. Its `git status --porcelain` commands return zero even when a tree is dirty unless their output is explicitly asserted empty; no retained stdout proves that assertion.

The tested code commit (`fbfbdc9`) and later evidence/report commit (`5512df5`) can form a valid two-commit evidence seal, but that model must be explicit. The current manifest calls itself the evidence root for `fbfbdc9` while pointing outside its directory to mutable-looking `wip` paths.

On the corrective rerun:

1. Put every compact artifact beneath `evidence/a00/<full-code-commit>/`.
2. Record both `code_commit` and `evidence_commit`, and assert no source/contract difference between them.
3. Record exact executable command, working directory, start/end time, exit, machine, and gate for each run.
4. Retain raw output where it is the proof, plus parseable compact reports and hashes.
5. Assert clean-tree output, rather than relying on the exit status of `git status --porcelain`.
6. Generate the manifest from the artifacts where practical; validate every referenced path, byte count, hash, JSON/JSONL parse, and required field before sealing it.

## Important factual corrections

- `evidence/a00/wip/r4-summary.md` says the final artifact has 50 joined frames; the current artifact and manifest say 63. Reconcile the summary with the sealed run.
- The report's statement that every status line says “candidate” is false. `proto/control_v3.proto:3`, its generated Swift comment, `ubuntu-client/crates/protocol/src/lib.rs:15`, and `mac-host/Sources/RescCore/WireRecords.swift:4` say “Stage-1 frozen.” Keep them candidate until a future review actually grants State 5.
- The active legacy client still auto-selects CUVID/fallback behavior. Before the A0 latency baseline, ensure its measurement path explicitly uses the provisionally selected `sw1-lowdelay` configuration. This is an A0 preparation item, not a reason to perform A0 now.

## What is accepted and worth preserving

- Exact Cargo pins, unchanged lockfile evidence, and ERR-07 ASCII validation are coherent.
- The 64 KiB framing gate and pre-allocation proof are sound.
- The shared six-phase model is a useful base for both dispatch and ERR-01.
- The immutable generational capture slot and stale-callback rejection design are sound at the pure-model level.
- Cursor timestamps now use the intended continuous clock, with sequence number correctly retaining ordering authority.
- Encoder/decoder doctors have materially stronger fail-closed checks, and the six retained reports themselves contain zero failed checks.
- Real decoder characterization demonstrates EAGAIN retain/drain/resubmit, multi-output, exact ordinal recovery, and tail completion on both candidates.
- `sw1-lowdelay` is a sensible provisional A0 backend for this machine; the recorded `decoder_lag_bound=1` and `output_deadline_ms=50` may be retained unless the corrective run disproves them.
- The report correctly keeps T1 behind A0, Stage 2, and the separate T1 entry gate.
- Every manifest-listed artifact hash was independently recomputed successfully, and no source/contract change exists between `fbfbdc9` and `5512df5`.

## Minimum closure sequence

Do not create another broad architecture plan. Apply this finite sequence:

1. Complete R5 semantics/context and regenerate independently reviewed Swift/Rust vectors.
2. Add the ERR-01 writer-spy scheduling proof.
3. Repair the R4 receive-identity ledger, upload-success identity, SCK clock conversion, authoritative clock selection, and live-gate predicate; add the negative tests above.
4. Close real zero-output evidence through a narrow ERR-09 or a forced real case.
5. Make `r3b_runs.sh` fail closed; add the `sender_integrity_pass` naming correction and restore candidate comments.
6. Cut a new clean code checkpoint.
7. Rerun the full applicable-A0.0 matrix on Mac, Ubuntu, and the real cross-machine pair. Use the selected backend where the gate depends on it. This rerun does **not** need to perform A0's optical check or formal 60 Hz window decision.
8. Seal a self-contained commit-qualified evidence directory and issue a short corrected completion report for independent re-review.

Only a successful re-review of that new evidence may grant State 5 and open A0.

## Independent verification performed for this review

- Confirmed the repository was clean at HEAD `5512df5` before adding this review.
- Confirmed there is no source/contract diff from `fbfbdc9` to HEAD; later changes are report/evidence only.
- Recomputed all 40 manifest-listed SHA-256 hashes: all matched.
- Re-ran protobuf regeneration check: pass.
- Re-ran locked portable Rust tests for `protocol`, `diagnostics`, and `jitter-buffer`: pass (including the ERR-01 model tests).
- Re-ran `tools/join_trace.py --selftest`: pass against its current, insufficient predicate.
- Ran the already-built `resc-fixture-check`: all 554 checks passed.
- Inspected all retained doctor, harness, decoder-characterization, and R4 trace artifacts.
- Did not rerun display capture or the Ubuntu/cross-machine hardware gates; this review audits their retained evidence.

The local Swift source rebuild could not be repeated because the currently selected Command Line Tools compiler and SDK builds do not match, and the sandbox blocks the default Clang module cache. The retained checkpoint build and the existing checkpoint-built fixture binary remain available; this environment issue is recorded but is **not** treated as a product-source failure.
