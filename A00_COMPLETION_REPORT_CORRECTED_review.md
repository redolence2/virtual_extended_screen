# Review of the A0.0 Corrected Completion Report

Reviewed: `A00_COMPLETION_REPORT_CORRECTED.md`

Report SHA-256: `a46615c8032f505fe0aa08448ccbae78463cae2ffcb530359679a55ea6ec719b`

Claimed checkpoints:

- code checkpoint C: `8e7170f04144ecabed35eb42bba111c4c375b7c0`
- evidence commit E: `5755c7aab7ffe3f9186a16a559c0b0d9d99e6f35`
- report/attestation commit R: `b747c24aa97eeb76b873d76dea20fcf5aaf5a71b`

Review date: 2026-08-05

## Executive verdict

**GO — release the current code as a personal demo candidate.**

**NO-GO only for the separate formal claim that A0.0 is complete and State 5 has been earned.**

The corrective work is substantial and several previously blocking mechanisms are now implemented correctly. In particular, the steady-state receipt ledger, recovered-versus-trigger identity separation, upload-confirmed presentation identity, literal SCK clock conversion, global-best clock selection, selected-SW1 real zero-output exercise, dispatch result shape, and writer-spy schedules are credible.

The earlier NO-GO applied the full formal evidence-seal standard. That standard is unnecessarily strict for the requested one-user demo. The existing real-pair run already demonstrates successful streaming, decoding, identity recovery, and presentation on the selected `sw1-lowdelay` path. The remaining findings are primarily trace-proof, test-coverage, and evidence-governance gaps rather than evidence that the normal demo path is broken.

For a demo release, do only one short manual real-pair smoke test from the exact demo commit:

1. start the host and client with the hard-coded personal settings;
2. create visible screen motion and confirm the Ubuntu display updates;
3. confirm the basic mouse/keyboard interaction intended for the demo;
4. stop and restart both processes once, confirming recovery without manual cleanup beyond the documented commands;
5. retain the ordinary host/client logs and record the exact commit used.

If that smoke test passes, the demo is ready. Do not require another full matrix, evidence commit, response cycle, or independent review before showing or using it.

The following gaps remain relevant only before a future formal State-5/A0.0-complete claim:

1. client trace shutdown does not first stop the video producer, so a frame can be admitted after the supposed final queue drain and escape both the ledger and footer;
2. the R4 host and client footers have different run tokens, while the joiner neither correlates them nor validates that each footer is terminal and internally consistent;
3. the required outbound Normal-versus-TraceOrDoctor vector matrix is incomplete;
4. the repeated-run proof does not retain/token-bind the raw doctor JSONL and does not parse every exact doctor report;
5. the evidence generator accepts malformed artifacts, asserts most gate exits as constants, lacks required provenance, and was added after C even though C was required to contain all corrective tooling.

These are not reasons for another architecture redesign and they do not block the personal demo. They should be handled later as one bounded hardening pass if formal A0.0 completion is still useful.

Formal phase bookkeeping remains unchanged, but it does not block the demo:

- **A0.0 remains incomplete.**
- **Stage 1 remains candidate and is not frozen.**
- **A0 remains NO-GO.**
- **T1 remains NO-GO.**

## Decision table

| Area | Decision |
|---|---|
| C→E→R ancestry | **Accepted as a linear Git chain** |
| Current hashes and byte counts for the 129 listed artifacts | **Accepted** |
| Decode-side receipt mapping during normal operation | **Accepted** |
| Upload-confirmed presentation identity | **Accepted in source** |
| SCK conversion and global-best clock use | **Accepted** |
| Selected-SW1 real zero-output result | **Accepted** |
| `DispatchFacts` and remote-fatal result shape | **Accepted in source** |
| Writer-spy S1–S4 proof | **Accepted for the intended pure-model scope** |
| Client stop-intake → drain → EOF protocol | **Rejected** |
| R4 footer correlation and terminal validation | **Rejected** |
| Complete inbound/outbound diagnostics clock vectors | **Rejected; outbound matrix incomplete** |
| R3b token-bound repeated-run provenance | **Rejected** |
| Validating evidence seal and 13-gate attestation | **Rejected** |
| Personal one-user demo candidate | **GO after one short manual smoke test** |
| Formal State 5 / A0.0 completion | **NO-GO; deferred hardening only** |

## Deferred formal-hardening findings

The findings below are not demo-release blockers. They remain requirements only if the repository later resumes the formal State-5 evidence process.

### 1. The client does not stop frame intake before its final drain

The accepted shutdown order was:

1. stop accepting new frames;
2. drain every admitted frame;
3. send decoder EOF and resolve all tail emissions;
4. prove zero pending identities and failures;
5. write and sync the terminal footer.

The current client does not complete step 1:

- `ubuntu-client/src/main.rs:338–345` starts `VideoReceiver` without a stop signal;
- `ubuntu-client/crates/net-transport/src/video_receiver.rs:100–217` continues receiving, assembling, and enqueueing frames until the channel disconnects;
- shutdown in `ubuntu-client/src/main.rs:653–662` only drains the queue with `try_recv()` and then immediately flushes the decoder;
- `tools/r4_live_gate.sh:134–141` deliberately keeps the host alive while this client drain runs.

Therefore, the receiver can enqueue an admitted frame after the last `try_recv()` and before EOF/footer. That frame never enters the receipt ledger and cannot appear in `pending_identities`. A clean footer is consequently not yet proof that every admitted frame was resolved.

There is a second fail-open edge: `decoder.flush()` failure is only logged at `ubuntu-client/src/main.rs:690–705`; it is not included in the clean/aborted decision at lines 707–726. If the ledger happens to be empty, an unsuccessful EOF/tail operation can still produce `status = "clean"`.

Required before formal State 5, not before the demo:

- give the video receiver a shared stop flag;
- on trace shutdown, set it before draining;
- let the receiver exit and drop its sender, then drain until channel disconnection rather than one nonblocking snapshot;
- require decoder EOF/tail success for a clean footer;
- add direct negative tests for duplicate submit/recovery, ledger overflow, missing recovery, unresolved EOF, producer/drain race, and flush failure.

This can remain simple: an `Arc<AtomicBool>` checked on the existing 100 ms socket timeout is sufficient; no new subsystem is needed.

### 2. The R4 footers do not prove a single complete run

The sealed run contains clean footers, but their identities differ:

- host footer, `r4-live-host-trace.jsonl:77`: `88f95fc7bb4d8409`;
- client footer, `r4-live-client-trace.jsonl:131`: `fda759bdc737c624`.

This happens because each endpoint independently generates a per-process token. `tools/join_trace.py:146–164` checks only that each input contains exactly one `trace_complete` record whose status is `clean`. It does not:

- require the host and client tokens to be equal;
- require the footer to be the last nonblank record;
- require client `pending_identities == 0` and `identity_failures == 0`;
- reconcile footer counts with the actual frame, present, pong, submit, and emission records;
- reject a clean footer followed by additional valid records.

The current selftest even constructs host and client footers with different default tokens, so this missing same-run predicate is codified as success.

The causal check has a related scope defect. `tools/join_trace.py:286–301` evaluates the host-local capture/encode invariant only after host records have joined to client records. A causally impossible host frame that is never received is excluded and can still yield PASS. Since this invariant is entirely host-local, it must be checked on every real host frame, including allowed network drops. The three dropped frames in the sealed run happen to satisfy the one-frame bound, but the gate does not enforce that fact.

The runner also proves process disappearance rather than successful exit: its wait helpers poll `kill -0` but do not collect the local host exit code or a remote client exit file. Its no-PID fallback and unconditional EXIT-trap `pkill -9` are unnecessary fail-open/broad cleanup paths for this one-user deployment.

Required before formal State 5, not before the demo:

- generate one R4 token in the runner and pass it to both endpoints;
- require strict token equality and footer-last ordering;
- validate zero failure/pending counts and all footer-to-record count equalities;
- apply host-local causal validation to every host frame with real capture identity;
- capture actual host and client exits, with SSH/poll failures treated as failures;
- make failure cleanup target only the recorded PIDs and disable it after a successful graceful exit;
- add selftests for token mismatch, footer-not-last, nonzero footer counters, count mismatch, unjoined causal violation, and nonzero endpoint exit.

### 3. The outbound diagnostics vector matrix is incomplete

The dispatch implementation itself uses `DispatchFacts` on both inbound and outbound paths, maintains the six protocol phases, and returns remote fatal as a lifecycle disposition rather than inventing protocol phases. The ACK, run-ID, fatal-class, and `ProfileResult` special cases are also present.

The shared vector promise is nevertheless incomplete. `tools/gen_dispatch_fixtures.py:711–724` generates the full inbound clock matrix across:

- both roles;
- `profile_accepted`, `video_ack_accepted`, and `active`;
- `clock_ping` and `clock_pong`;
- Normal and TraceOrDoctor modes.

For outbound, `tools/gen_dispatch_fixtures.py:762–810` generates the 156-row base only in Normal mode and adds TraceOrDoctor clock cases only at `active`. It omits the legal TraceOrDoctor sends at `profile_accepted` and `video_ack_accepted` for both roles and both clock kinds. `c1-oracle-review.md` already identifies this as residual risk.

Before formal State 5, add the eight missing TraceOrDoctor outbound rows, regenerate the fixtures, and run both language consumers. This is a fixture-completeness fix; the underlying dispatch design need not change. It does not affect the current v1 demo path.

### 4. The repeated-run evidence is not token-bound and complete

The final token-specific R3b reports are valid and their observed values are encouraging. The runner also correctly captures sender and remote receiver results and requires the sender integrity and receiver-v2 predicates.

It still does not implement the exact evidence rule accepted in the prior review:

- host doctor runs count `doctor_complete` in one global `host.jsonl`; the events are not isolated or tied to the run token;
- client doctor JSONL is counted in a temporary directory but never retained;
- no raw host doctor JSONL segment is retained;
- the final predicate loop parses sender and receiver reports but not all three client-doctor reports;
- the sealed directory consequently contains no raw doctor JSONL for the final three runs.

Required before formal State 5, not before the demo:

- use a unique log directory or token-tagged record for every host and client doctor run;
- retain each raw doctor JSONL;
- require exactly one token-matching `doctor_complete` in each retained log;
- parse and validate all exact current host-doctor, client-doctor, sender, and receiver reports;
- bind the validated filenames to the token recorded in the final runner summary.

The three historical `r3b-host-doctor-{1,2,3}.json` files should not be carried forward as JSON. They begin with console log text and are not valid JSON documents. Rename them as logs or omit them from the replacement sealed set; retain only the valid token-specific reports as `.json`.

### 5. The evidence manifest is not a validating 13-gate seal

Some parts of the seal are real:

- C→E→R is a linear ancestry chain;
- all 129 manifest-listed artifacts currently exist;
- independent recomputation found zero SHA-256 or byte-count mismatches;
- all three sealed R4 JSONL files parse;
- replaying the sealed R4 traces reproduces 69 joined, 57 presented-joined, zero ambiguities, and zero identity-failure records;
- the selected SW1 report records one accepted parameter-set-only zero-output packet and exact 480/480 recovery.

Those facts do not support the validator description at report line 7:

- `tools/gen_evidence_manifest.py:121–138` parses only selected R4 files and the SW1 report, not every typed artifact;
- 3 of the 106 sealed `.json` files are malformed whole documents, yet all three are listed and hashed by the manifest;
- lines 146–159 hard-code 13 gate rows with successful exit values; most are not derived from retained command records;
- `token_line` is read at line 141 but never used;
- artifact hashes and sizes are generated from current bytes, with no verification mode that reloads an existing manifest and compares it to the tree;
- a dynamic directory scan plus `len >= 100` is not an expected-artifact inventory, so deletion or substitution of most artifacts is not detected;
- the manifest lacks the remediation plan's per-run working directory and start/end time, and it omits material environment fields such as Swift compiler/SDK and SDL;
- only M3, X1, and X2 name raw outputs; M1/M2/M4/M5/M6/U0–U4 are mostly prose/static assertions rather than validator-bound proof.

There is also a topology defect. `tools/gen_evidence_manifest.py` was added in E, not C. The accepted topology required C to contain all corrective source, contract, fixtures, and tooling. The generator explicitly exempts itself from the C→HEAD diff, and report line 8 omits that post-C tooling addition. The contract files are unchanged from C, but the stated all-tooling-in-C attestation is false.

Required before formal State 5, not before the demo:

- place the complete generator/validator in the replacement code checkpoint C′;
- define an explicit required-artifact inventory per gate;
- retain a machine-readable execution record for every gate with commit, dirty state, machine, command, working directory, start/end time, and exit;
- include the complete environment required by `A00_REMEDIATION_PLAN.md` §7: OS/build/architecture, relevant toolchains, FFmpeg, SDL, NVIDIA driver, protoc, and SwiftProtobuf;
- parse every `.json` and every nonblank `.jsonl` record;
- derive each PASS from its retained output and exact predicate rather than a constant;
- explicitly require `strategy_used != "not_forced"` and a real zero-output event while ERR-09 is absent;
- add a read-only verification mode that recomputes hashes/bytes from an existing manifest and fails on missing, extra where disallowed, changed, malformed, or semantically failed artifacts;
- semantically replay the strict R4 join and validate the token-specific R3b set.

The existing non-self-referential principle remains correct: E′'s manifest records C′, while R′ records both C′ and E′. The manifest still must not try to contain E′'s own hash.

## Report corrections that do not require design changes

1. Report line 50 says `ERR-09 written-but-never-needed`. No ERR-09 exists in `CONTRACT_ERRATA.md`, and `c4-summary.md` correctly says no equivalence erratum entered the contract. Change this to: **“the ERR-09 escape path was implemented but not used; ERR-09 was not written.”**
2. Qualify `graceful-termination-only runner` until the unconditional SIGKILL trap and exit-status gaps are removed.
3. Keep the 16,667 µs causal bound. The bound is supported: the final sealed trace's maximum lead is 9,003 µs, and the C checkpoint's earlier retained WIP trace contains the cited 9,190 µs maximum. State which run each number comes from instead of presenting 9,190 µs as the final sealed run's measurement.
4. Do not call all 13 gates validator-proven until their raw execution records and predicates are actually bound into the seal.

## What is accepted without change

- The report correctly leaves State 5 to independent review and keeps all downstream phases closed.
- The receipt timestamp remains before the drop-capable queue, while the pending ledger is correctly inserted on the decode side.
- `decode_trigger_frame_id` is informational and recovered identity remains the join authority.
- Presentation identity advances only after a successful texture upload.
- The literal `stream.synchronizationClock` → `CMSyncConvertTime` path and labeled fallback are present.
- The session-global minimum-delay clock sample is applied consistently.
- The selected backend is still `sw1-lowdelay`; its real zero-output result is credible, so ERR-09 is not required.
- The dispatch result/lifecycle separation and named writer-spy schedules are suitable for the intended pre-T1 pure-model scope.
- No A0 optical, real-motion histogram, window, bitrate, or Stage-2 work should be pulled into this correction.

## Later formal completion sequence

This section is explicitly **not a demo gate**. If formal A0.0 completion is pursued later, do not rewrite the existing history; append a replacement evidence chain:

1. Implement the bounded fixes above, including the final manifest validator, and add the missing negative/vector tests.
2. Cut a clean replacement code checkpoint **C′** containing all source, contract, fixtures, and tooling.
3. Run the complete applicable-A0.0 matrix from C′ on the Mac, Ubuntu box, and real pair, retaining every required raw and machine-readable execution record.
4. Verify the evidence directory with the C′ validator, then commit it as **E′** without any source/contract/tooling change.
5. Write a corrected attestation **R′** that identifies C′ and E′ and proves no source, contract, fixture, or tooling change from C′ through R′.
6. Request one independent State-5 re-review.

No new implementation plan or response-review round is needed before that later bounded work begins. Do not delay the demo for it.

## Verification performed

- Read the corrected report against every amendment in `A00_COMPLETION_REPORT_AMENDED_response_review.md` and the evidence requirements in `A00_REMEDIATION_PLAN.md`.
- Confirmed C→E→R ancestry and a clean pre-review worktree.
- Inspected the correctness-critical dispatch, receipt-ledger, decoder-tail, trace-writer, joiner, live-runner, repeated-runner, zero-output, and manifest-generator paths.
- Recomputed all 129 listed artifact SHA-256 values and byte counts: zero mismatches.
- Parsed all sealed JSON/JSONL: 103 of 106 `.json` valid, 3 malformed; all 3 `.jsonl` files valid.
- Replayed the sealed R4 inputs and independently reproduced the recorded passing metrics.
- Recomputed causal leads: final sealed run 30 positive of 72 host frames, maximum 9,003 µs; C's earlier retained WIP run maximum 9,190 µs.
- Ran `python3 tools/join_trace.py --selftest`: PASS. The missing token/counter/footer-last/unjoined-causal cases are not in that selftest.
- Ran `cargo test -p protocol --locked`: PASS (66 library tests + 4 ERR-01 barrier tests + 4 writer-spy tests).
- Attempted `swift run resc-fixture-check`; the current local CommandLineTools Swift compiler/SDK pair is incompatible, so this rerun did not start. This is not counted as a product-code failure, but it reinforces the need to record the exact Swift toolchain/SDK in evidence.
- Did not repeat the Ubuntu or real-pair hardware runs; their sealed artifacts were inspected instead.
