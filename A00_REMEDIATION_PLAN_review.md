# A0.0 Remediation Plan Review — Direction Accepted, Execution Plan Needs Tightening

Reviewed: A00_REMEDIATION_PLAN.md

Plan SHA-256: 350baa7ffcc66a2ae1c44fb6fc833702ded3a8e2d6d590956307dcfbd9c9f6b6

Review date: 2026-08-04

## Executive verdict

**CONDITIONAL ACCEPT of the remediation direction.**

**NO-GO to implement the plan exactly as written.**

**A0.0 completion, Stage-1 freeze, A0 entry, and T1 entry remain NO-GO.**

The plan correctly absorbs the response review's central lessons: it adopts an evidence-based state ladder, stops treating dirty-tree patches as closed gates, retains typed dispatch in A0.0, governs the ASCII-only profile rule through an erratum, preserves the formal optical check for A0, and requires both simulated and real-backend decoder evidence.

It is close, but not yet a safe set of implementation briefs. The remaining problems are specification and ownership errors, not architectural failures. The V3 dispatcher cannot prove the pre-allocation cap through its proposed API; R4 does not define the exact identity carrier needed to eliminate the one-frame ambiguity; the real-decoder forcing assumptions are not guaranteed; R3 mixes host and client responsibilities and creates a dependency cycle; and the exit sentence incorrectly suggests T1 can unlock after the A0.0 re-review.

Do not create another architecture plan. Amend this remediation plan with the precise corrections below, then begin implementation.

| Decision point | Result |
|---|---|
| Five-state evidence ladder | **Accepted** |
| Overall R1–R7 remediation scope | **Accepted in direction** |
| ERR-07 ASCII governance | **Accepted; implementation/tests missing from work breakdown** |
| Optional ERR-08 escape hatch | **Accepted if used before evidence substitution** |
| D1 typed-dispatch intent | **Accepted; API/ownership incomplete** |
| D2 test-double plus real-backend intent | **Accepted; forcing protocol overclaims guarantees** |
| R3 doctor/harness specification | **Needs correction** |
| R4 trace/identity specification | **Needs an explicit carrier and mapping contract** |
| Parallel execution/ownership | **Not safe as written** |
| R7 clean checkpoint and re-review | **Accepted after gate/evidence wording corrections** |
| Implementation start | **Conditional on the amendments below** |

## What is already correct

- The header preserves the NO-GO for every downstream phase.
- The five-state ladder is a useful, maintainable replacement for ambiguous “done” and “closed” language.
- The current-state table accurately downgrades F8 and F10 to partial work.
- ERR-06 remains the correct narrow solution for the temporary V3 schema path.
- D1 correctly keeps typed dispatch in A0.0 without cutting the live A0 baseline over to V3.
- D2 correctly refuses to let a decoder test double substitute silently for real-backend evidence.
- D3 correctly recognizes that replacing NFC with an ASCII-only rule changes the normative contract and therefore needs ERR-07.
- The formal optical spot-check is correctly restored to A0.
- The client doctor is correctly intended to validate only the candidate/selected backend's required texture path.
- R7 correctly requires one clean checkpoint, same-commit verification, retained evidence, an amended report, and independent re-review.
- No V12 or general configuration system is introduced.

## Required amendments

### 1. Split the framing-length gate from typed dispatch

D1 proposes:

connection state + direction + envelope bytes → accepted typed message/next state or FatalCode

That API receives an already-materialized envelope buffer. It therefore cannot prove V11's 64 KiB requirement: the outer length must be rejected before allocating or reading the body.

Use two explicit layers in both languages:

1. **Framing-length gate**
   - consumes only the four-byte length prefix;
   - rejects length greater than 64 KiB before any body allocation/read;
   - returns the permitted exact body length;
   - has a test proving an oversized prefix causes zero body reads and zero body-sized allocation.
2. **Typed validator/router**
   - consumes an already bounded and generated-decoded V3 Envelope;
   - takes endpoint role, message direction, current protocol phase, active/candidate run context, and any minimal external facts needed for semantic checks;
   - validates field caps and semantic ranges before producing a transition;
   - returns a typed message plus next phase, or the exact FatalCode;
   - performs no socket, logging, injection, or other transport side effects.

Keep the component small. It need not be a second full lifecycle actor. A pure typed validator/router with legal role-and-phase checks is sufficient for this fixed pair.

Clarify two protobuf rules:

- unknown fields remain ignored by the generated decoder;
- an unknown-only oneof decodes as an absent payload and is rejected as PROTOCOL_VIOLATION. Do not add a raw unknown-field scanner.

The activation barrier must reuse this same phase model rather than create a second, drifting state machine.

On Swift, the proposed RescCore/V3Dispatch.swift needs generated V3 types. Explicitly add RescProto and SwiftProtobuf dependencies to RescCore, or place the dispatcher in a small protocol-runtime target. Adding the dependencies to RescCore is the smallest current-tree change, even though HarnessSender will inherit them transitively.

### 2. Define an implementable, immutable trace identity chain

R4 says “exact per-frame identity threaded capture→encode→wire→decode→present,” but it does not define the carrier. The current capture slot holds only a pixel buffer, the host synthesizes encoder PTS from a frame counter, and the legacy decoder does not recover packet PTS for each emitted frame. Without a concrete mapping, an implementation can recreate the same latest-value ambiguity under another name.

Freeze this minimal A0.0 trace identity design:

1. The SCK callback creates one immutable CapturedFrame containing:
   - pixel buffer;
   - capture generation/run tag;
   - capture sequence;
   - actual SCK presentation time converted to host continuous-monotonic microseconds;
   - conversion/fallback label and clock uncertainty.
2. LatestFrameSlot stores and consumes that entire CapturedFrame atomically. It never stores the pixel buffer separately from its identity.
3. Intentional latest-wins replacement increments a drop counter. A dropped identity is never reused or relabeled.
4. Encoder submission carries the capture sequence through presentationTime or an equally exact per-submit context. The asynchronous output callback must recover the identity belonging to that exact submitted frame.
5. The encoded output is bound to the actual V1 wire frameID and the host trace records the frameID-to-capture identity mapping.
6. The client timestamps receipt when the complete encoded frame is assembled, before a drop-capable queue.
7. The decoder packet PTS is set to frameID. Every emitted decoded frame recovers its own PTS; delayed and multi-output emissions must never inherit the ID of the current decode call.
8. The render trace records the recovered frameID immediately adjacent to the successful presentation call, not when upload is merely scheduled.
9. The joiner accepts only exact identities and records clock offset, delay, uncertainty, fallback status, and any rejected sample reason.

The retained live artifact must show zero identity ambiguities. Intentional capture/render drops are allowed and counted; identity substitution is not.

Also state the nil-SCK-sync-clock behavior exactly: use the labeled callback-time fallback allowed by V11, carry its larger uncertainty, and never mix it silently with true SCK-PTS samples.

R4 is the highest integration-risk item and is likely larger than its estimate. Correctness is more important than keeping the 1–1.5-day estimate.

### 3. Make ERR-03 forcing bounded and evidence-driven

The plan's statements that software EAGAIN is deterministic by the second packet and that CUVID is directly bounded by the surface-pool count are not established API guarantees. The fixed sample also has B-frames disabled, so a zero-output first packet or a multi-output drain is not automatically guaranteed.

Replace those predictions with a bounded characterization protocol:

- choose explicit maximum attempted packets and wall-clock timeout;
- record every attempted ordinal, accepted ordinal, EAGAIN result, drain start/stop, number of outputs per drain, and recovered PTS;
- on EAGAIN, retain and resubmit the exact same packet after draining;
- prove the packet is accepted exactly once and never skipped or double-counted;
- prove every real-backend EOF/tail drain ends with emitted == submitted and exact ordinal coverage;
- run both hevc and hevc_cuvid under the recorded machine/driver/FFmpeg versions;
- keep the deterministic test double for all hypothetical state-machine branches.

If EAGAIN, zero-output, or multi-output cannot be forced on a real candidate within the bounded protocol, record that outcome. Then add ERR-08 before accepting equivalent evidence. Do not run an unbounded experiment and do not claim the test double proves real decoder timestamp behavior.

R6 must explicitly include real-backend EOF/tail-flush ordinal evidence; it is currently named only for the test double.

At the end of A0.0, one backend must be selected and fixed for A0 together with decoder_lag_bound and output_deadline_ms. The final canonical profile artifact is not frozen until Stage 2, but the backend used during A0 must not remain an undecided “candidate.”

### 4. Correct R3's host/client responsibilities and dependency cycle

R3 currently assigns NV12 texture success and submitted == emitted to the host doctor. Those are client-side decoder/SDL conditions.

Use this split:

**Host doctor**

- check every VideoToolbox property-setting return status;
- read back every load-bearing property;
- check PrepareToEncodeFrames;
- use the profile-true keyframe interval and encoder settings;
- encode the bundled frame and verify the required RA NAL set;
- make every required condition affect the exit code;
- synchronously persist doctor_complete before exit.

**Client doctor**

- open the explicit candidate backend during A0.0;
- decode the complete bundled sample and perform EOF/tail drain;
- require submitted == emitted, exact ordinal recovery, and zero unknown/duplicate/reordered outputs;
- validate the candidate's actual decoded-frame-to-SDL texture update path, not only texture creation;
- for sw1-lowdelay, require its specified software/IYUV path;
- for cuvid-lowdelay, require the specified transferred NV12 path if that remains the WIRE contract;
- treat the other candidate's format as informational only;
- synchronously persist doctor_complete before exit.

The selected backend does not exist until R6, so split R3:

1. **R3a:** parameterized fail-closed infrastructure and failure-injection tests for either candidate;
2. **R6:** real characterization, bounds, and backend selection;
3. **R3b:** repeated selected-profile doctor and harness evidence.

Harden the receiver predicate further. In addition to accepted == acked, emitted == submitted, unknown_pts == 0, and nonzero frames, require:

- zero duplicates;
- zero reorders/skips;
- zero ACK-order violations;
- zero protocol/fatal decoder errors;
- clean EOF/tail drain;
- no outstanding frames at successful exit.

The cursor-clock test should not mention calibration refresh. CursorTracker uses the host continuous clock directly; test injected-clock monotonicity and sequence-number ordering as separate properties.

### 5. Complete R1's canonicalization and reproducibility work

D3 correctly specifies ERR-07, WIRE updates, and two validator changes, but R1 currently schedules only writing ERR-07. No later step implements or tests the new ASCII rule.

R1 must include:

- dated ERR-07 in CONTRACT_ERRATA.md;
- WIRE replacement of the NFC rule with the exact ASCII-only rule;
- recursive or schema-specific ASCII validation in Swift;
- the identical validation in Rust;
- a shared valid-ASCII fixture;
- shared non-ASCII rejection fixtures, including a canonically encoded non-ASCII string that would otherwise pass sort/minify checks;
- tests proving both languages return the same result.

Define “ASCII vocabulary” precisely. The simplest rule is: every profile key and string value must consist only of ASCII bytes, followed by the existing schema/value validation. Do not add Unicode normalization machinery.

R1's Cargo evidence sentence is also wrong. Cargo metadata output cannot remain byte-identical after manifest requirements change.

Use:

1. SHA-256 of Cargo.lock before the manifest edit;
2. exact manifest edits for the root package and video-decode;
3. cargo metadata --locked --offline showing exact =7.1.0 and =7.1.3 requirements/resolution;
4. SHA-256 or cmp proving Cargo.lock bytes did not change;
5. the lockfile committed at R7.

Add a dated governance status note to CONTRACT_ERRATA.md before changing WIRE's status from frozen to candidate, or explicitly document why the status line is metadata rather than a structural contract fact. The first option is clearer for a future agent.

### 6. Replace unsafe parallelism with explicit ownership and dependencies

The claimed R2/R3 and R4/R5 parallelism overlaps Package.swift, fixture checks, capture slots, DisplayCapturer, VideoEncoder, doctors, decoder experiments, and state-machine logic. Section 4 assigns worker categories but no file ownership.

Use this dependency order:

1. **R1 first:** documentation, errata, pins, canonicalization validators/tests.
2. **R5 framing/dispatcher contract next:** both pure cores, package wiring, shared state vectors.
3. **R2a activation barrier:** reuse the R5 phase model.
4. **R2b capture generation and cursor clock:** may proceed independently after file ownership is assigned.
5. **R4 trace propagation:** only after the atomic CapturedFrame/slot design from R2b is merged.
6. **R3a fail-closed parameterized infrastructure.**
7. **R6 decoder characterization and backend selection.**
8. **R3b selected-profile doctor/harness evidence.**
9. **R7 clean integration checkpoint strictly last.**

Some non-overlapping Rust decoder work can run while Mac capture work proceeds, but workers must receive exact owned paths. No two workers should modify Package.swift, FixtureCheck, DisplayCapturer/LatestFrameSlot, VideoEncoder/Doctor, or decoder-experiment concurrently.

Replace the transient “Fable/Sonnet” execution labels with role-based ownership such as root reviewer, protocol implementer, capture/trace implementer, and decoder/harness implementer. Actual model routing can change without changing the software plan.

### 7. Name the evidence artifacts and pass predicates

R7 says evidence will be retained but gives no stable location. Future-agent maintainability needs one predictable entry point.

Use a small fixed structure such as:

evidence/a00/<full-commit>/

with a manifest containing:

- exact commit and dirty=false;
- Mac and Ubuntu OS/build/architecture identifiers;
- toolchain, FFmpeg, SDL, NVIDIA, protoc, and SwiftProtobuf versions;
- command, working directory, start/end time, exit code, and applicable gate for every run;
- path and SHA-256 for each doctor, decoder, harness, fixture, and joined-trace artifact;
- selected backend and measured bounds;
- explicit PASS/FAIL predicate results.

Retain compact JSON reports and a representative joined-trace sample. Large raw media does not need to be committed; retain its hash, generation command, size, and stable machine path.

R7's “every gate” must mean every applicable A0.0 gate. It must not pretend that unfinished A0 or T1 gates passed.

Use a platform matrix rather than implying every command runs on both machines:

- Mac: protobuf regeneration check, Swift build/fixture checker, host doctor, harness sender, capture/trace evidence;
- Ubuntu: locked workspace build/tests, client doctor, decoder experiments, harness receiver;
- cross-machine: harness pair, clock/join trace, same-commit and environment checks.

If any source or contract correction occurs after the clean checkpoint, create a new checkpoint and rerun the complete A0.0 evidence matrix before re-review.

### 8. Correct the phase exit sentence

The final sentence currently says Stage-1 freeze, A0 entry, and T1 entry unlock afterward. That is not V11's sequence.

Use this exact phase result:

1. R1–R6 locally verified with retained evidence;
2. R7 clean commit and complete A0.0 matrix pass;
3. independent re-review returns GO;
4. A0.0 becomes complete, Stage 1 freezes, and A0 may begin;
5. A0 performs the real-pair histogram, software latency plus optical spot-check, window trial, bitrate confirmation, and Stage-2 final profile/fixture freeze;
6. only then, after the separate V11 T1 entry checks and both final-profile doctors pass, may T1 begin.

T1 does not unlock from the A0.0 review.

## Corrected readiness matrix

| Item | Required before implementation | Required before A0.0 closure |
|---|---|---|
| R1 paper/pins/ASCII scope | Precise edits and tests in plan | Implemented, locally verified, committed |
| D1 framing + dispatcher | Two-layer API and ownership defined | Both languages pass shared vectors |
| ERR-01 barrier | Reuses dispatcher phase model | Delayed/reordered proof passes |
| Capture generation | Atomic CapturedFrame contract defined | Late-callback proof passes |
| Cursor clock | Correct clock test defined | Both semantics tests pass |
| R3 doctors/harness | Host/client split and ordering fixed | Failure injection and selected-profile runs pass |
| R4 trace | Exact carrier/mapping specified | Joined live artifact has zero ambiguity |
| R6 decoder | Bounded protocol and ERR-08 rule defined | Real + double + tail evidence; backend/bounds selected |
| Evidence | Stable manifest/path defined | Same clean commit, hashes and matrix retained |
| Independent review | Not applicable | Must return GO |

## Final recommendation

**Accept the plan's remediation direction, but amend the execution specification before assigning implementation workers.**

The smallest path forward is:

1. update this plan with the eight corrections above;
2. keep the architecture and V11 governance unchanged;
3. implement against explicit file ownership and pass predicates;
4. rerun the complete A0.0 matrix from one clean commit;
5. request independent re-review.

Once the amendments are incorporated, the remediation implementation is **GO**.

Until the implementation and clean-checkpoint evidence pass that re-review:

**A0.0 completion, Stage-1 freeze, A0 entry, and T1 entry remain NO-GO.**
