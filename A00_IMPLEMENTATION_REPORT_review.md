# A0.0 Implementation Report Review — Strong Candidate, Formal Gates Still Open

Reviewed: A00_IMPLEMENTATION_REPORT.md

Report SHA-256: 820390c2ee914b5ca5bd1d7b0e412e9dce2dc0e64e4f8064510aa4d0ac9678d7

Review date: 2026-08-04

## Executive verdict

**GO to continue the current implementation. NO-GO to declare A0.0 complete, freeze Stage 1, or enter A0.**

The implementation is a strong A0.0 candidate. The protocol definitions, fixtures, generated sources, canonical profile artifact, diagnostics foundation, decoder experiments, and native probes represent substantial and useful progress. The local structural tests I reran are clean.

However, the report's completion claim is ahead of the evidence. Several requirements explicitly made blocking by Implementation Plan V11 and CONTRACT_ERRATA.md are either absent, only documented rather than implemented and tested, or measured through tooling that can report success despite a failed invariant. The most important gap is the trace path: it does not currently measure the required capture-to-presentation interval with reliable per-frame identity or the specified clock conversion.

This does not call for an architecture rewrite or an Implementation Plan V12. Keep the work, close the concrete gates below, commit a clean evidence state, and issue a corrected A0.0 report.

| Decision point | Result |
|---|---|
| Code generation, canonical artifact, and static fixtures | **PASS** |
| Diagnostics and protocol unit-test foundation | **PASS** |
| A0.0 implementation candidate | **Strong** |
| A0.0 complete | **NO-GO** |
| Stage-1 structural freeze | **NO-GO** |
| Entry into A0 measurement | **BLOCKED** |
| Entry into T1 | **Not yet eligible** |
| Continue focused remediation without a new plan | **GO** |

## Blocking findings

### 1. The trace path does not yet prove capture-to-present latency

This is the highest-priority issue because V11 makes the trace/clock gate an entry condition for A0.

- DisplayCapturer records RescClockBridge.continuousNowUs() in the capture callback and labels it as contentCaptureTsUs. That is callback-observation time, not the ScreenCaptureKit presentation timestamp converted from the SCK clock.
- V11 requires SCK PTS conversion through CMSyncConvertTime plus bracketed host-clock calibration. RescClockBridge contains calibration helpers, but the capture path does not call them.
- RescTrace retains only the latest capture metadata and attaches that value to the next encoder output. Its own comment admits a possible one-frame mismatch. At a 60 Hz target, one frame is already approximately 16.7 ms and is therefore material to the latency claim.
- On the client, ts_recv_us is taken when the frame leaves an internal channel, not at network arrival.
- The trace record contains ts_decode_done_us and a presented boolean, but no timestamp taken at the actual SDL presentation point. Presentation occurs later in the loop.
- There is no retained joiner output that proves exact host-frame/client-frame identity, clock offset, delay, uncertainty, and the final joined capture-to-present sample.

As implemented, the traces can still help debug relative behavior, but they cannot support the formal A0 latency baseline or its optical spot-check.

Required closure:

1. Convert each SCK frame's actual PTS into host continuous-monotonic time using the specified bracketed calibration.
2. Carry one exact frame identity through capture, encode, wire, decode, and presentation. Do not use a latest-value approximation.
3. Timestamp network receipt at the real receive boundary and timestamp presentation at the actual present call.
4. Emit joined records with the clock sample and uncertainty that justify each latency result.
5. Retain a small validation artifact and compare it with the optical spot-check before starting A0.

### 2. Backend selection and clean decoder bounds are explicitly unfinished

The report itself states that backend selection remains open and that decoder_lag_bound and output_deadline_ms still require stall-free measurements. Those are A0.0 outputs, not later A0 work.

The two real backend runs are encouraging, but they do not fully close ERR-03:

- Both reported runs observed eagain_retries = 0, so they did not exercise retain–drain–resubmit behavior under send-side EAGAIN.
- The current sample does not establish the required identity behavior for packets that emit zero or multiple frames.
- Induced stalls are useful for ordinal testing, but their timing distributions cannot supply the clean decoder-lag and output-deadline values.

Required closure:

1. Add a deterministic test or controlled decoder test double that forces send-side EAGAIN and covers zero-output and multiple-output packet behavior.
2. Prove ordinal recovery remains exact in every case, including flush/tail handling.
3. Run both candidates without injected stalls using the fixed sample and environment.
4. Select one backend and freeze its measured conservative decoder_lag_bound and output_deadline_ms.

Until those steps are complete, A0.0 and the Stage-1 backend contract remain open.

### 3. Three mandatory behavioral proofs are missing

CONTRACT_ERRATA.md and the V11 review made these proofs part of the freeze, not optional follow-up work.

#### ERR-01 activation barrier

WIRE.md contains the corrected cross-TCP activation-barrier wording, but no implementation/state-machine test proves delayed and reordered control/video handler scheduling. The required test must demonstrate that the client writes no control input before activation and that the first post-activation event is accepted.

#### Late capture callback isolation

DisplayCapturer has no run/generation binding on SCK callbacks. A callback stores into the shared frame slot unconditionally, and a stale callback/error path can affect a later capture run. Add a capture generation, reject callbacks from older generations, and test teardown → new run → late old callback.

#### Cursor timestamp clock

CursorTracker still derives its timestamp from CFAbsoluteTimeGetCurrent, a wall-clock source. The frozen contract requires sender-local host continuous-monotonic microseconds, with sequence—not timestamp—governing ordering. Move it to the same continuous clock and add a focused test.

### 4. The Stage-1 normative schema is not frozen at its contracted path

V3 currently lives in proto/control_v3.proto while proto/control.proto remains the legacy resc.control schema. The report argues that the freeze is satisfied by equivalent content under the V3 filename, but neither V11 nor CONTRACT_ERRATA.md authorizes moving the normative Stage-1 artifact.

Choose one explicit resolution before claiming the freeze:

- place the V3 Stage-1 contract at proto/control.proto and retain the A0 legacy schema under an explicitly legacy filename/target; or
- add a narrowly scoped contract erratum that names proto/control_v3.proto as the normative Stage-1 artifact and updates every freeze/check reference accordingly.

There is a second, practical issue: the report correctly says all work is uncommitted. A structural freeze cannot be reproduced from base commit 12b87d1 while its schemas, generated sources, lockfiles, fixtures, and evidence exist only in a dirty working tree. Freeze at a clean commit after all blocking corrections pass.

The typed-dispatch claim should also be narrowed. Generated V3 types and parsers exist, but the live host remains on manual legacy-envelope construction and raw 0xFA scanning. That is acceptable for an intentionally untouched A0 baseline, but it is not yet proof of the final stateful V3 typed dispatcher.

### 5. The doctors can produce false-positive success

The native probes are useful and the retained results are promising, but their pass logic does not yet prove every load-bearing condition claimed by the report.

Client doctor:

- decode_sample passes when submitted > 0 and emitted > 0; it does not require emitted == submitted, so a trailing frame drop can pass;
- the SDL check attempts both IYUV and NV12, but the final success condition depends only on IYUV. A required NV12 texture failure can therefore coexist with exit 0.

Host doctor:

- VideoEncoder.start does not check the return status of each VTSessionSetProperty call or VTCompressionSessionPrepareToEncodeFrames;
- read-back covers only a subset of load-bearing properties;
- the doctor uses the current one-second keyframe interval rather than the intended final 60-second/3600-frame profile setting;
- doctor_complete is queued immediately before process exit. The retained host log from the reported run lacks that event, confirming that this evidence can be lost.

Required closure:

1. Make every required decode, pixel-format, native-call, and read-back invariant participate in the exit result.
2. Require exact submitted/emitted ordinal equality, including tail drain.
3. Check every load-bearing VideoToolbox return status and log the failing native domain/code.
4. Exercise the selected profile's actual settings, including the final keyframe interval once selected.
5. Flush/synchronize the final evidence record before exit.

### 6. The A0 harness can report success after a failed run

The reported 594-sent/594-acked run with no observed ordering violation is useful exploratory evidence. The harness itself is not yet a trustworthy automated gate:

- the sender increments frames_sent before confirming a successful socket write;
- a write failure does not reliably fail and stop the run;
- sustained_60hz is derived from frames_sent/time only and does not require frames_acked == frames_sent, zero outstanding frames, or zero ACK-order violations;
- ack_order_violation is printed but omitted from the JSON result;
- the sender exits 0 unconditionally;
- the receiver's pass expression does not require accepted == acked, emitted == submitted, or unknown_pts == 0; a clean EOF with zero frames can pass;
- the harness uses VideoEncoder's one-second keyframe default rather than the final load-bearing encoder setting.

The report already correctly labels the synthetic smoke run as a preview rather than formal A0. Harden the pass predicates and exits before relying on it for a gate.

## Important non-blocking corrections

### 7. Fatal/lock evidence can be lost at immediate process exits

Both applications log an instance-lock denial through buffered or asynchronous logging and then immediately exit(20). That record is not guaranteed to reach disk. Add a synchronous fatal/flush path and a two-process contention test. The same rule should cover doctor completion and every other deliberately immediate exit.

For the Rust logger, OpenOptions.mode(0o600) sets permissions only when creating a file. Reassert or validate mode on an existing log/report/trace file so later environmental changes are visible rather than silently accepted.

### 8. The FFmpeg dependency claim is not exact as written

The report says several manifests use exact =7.1.0 and =7.1.3 pins. They currently contain 7.1.0 and 7.1.3 without the leading equals sign, which Cargo interprets as compatible/caret requirements. The legacy video-decode crate is looser still at version 7.

Use =7.1.0 and =7.1.3 where exact resolution is intended, eliminate or quarantine the loose legacy dependency at its planned boundary, and commit Cargo.lock and Package.resolved with the frozen artifacts.

### 9. Generic canonicalizers do not define non-ASCII normalization

The current 497-byte canonical profile is ASCII, has no trailing LF, and hashes correctly, so this does not block the fixed personal deployment. The generic Rust and Swift canonicalizers sort and minify but do not normalize or reject non-NFC strings.

Keep the simplest rule: explicitly restrict profile string values to the required ASCII vocabulary. Only add Unicode normalization machinery if a future contract genuinely needs non-ASCII values.

### 10. Report precision should be corrected

These are documentation defects, not implementation blockers:

- resc-fixture-check currently executes 126 assertions, not approximately 140;
- fatal_code_classes.json contains 23 enum entries when code 0 is included, not 22;
- the repository currently contains five proto inputs, not four;
- Doctor.swift is currently 451 lines, not 480;
- “diffs against committed Swift sources” is premature while those sources are untracked.

Exact counts are not intrinsically important, but a cold-start evidence report should be mechanically reproducible.

## Verification performed for this review

| Check | Review result |
|---|---|
| tools/generate_proto.sh --check | **PASS** with pinned protoc 27.3 and SwiftProtobuf plugin 1.36.1 |
| Fresh swift build | **PASS** |
| .build/debug/resc-fixture-check | **PASS**, 126 assertions observed |
| cargo test -p protocol -p diagnostics | **PASS**: 49 protocol tests and 12 diagnostics tests; one ignored filesystem test |
| cargo test -p diagnostics -- --include-ignored | **PASS**: all 13 diagnostics tests |
| Canonical profile byte check | **PASS**: 497 bytes, no trailing LF |
| Canonical SHA-256 | **PASS**: 0cc22496628805973f8d52292e7f838b95ec023faf658d71dd862f3fbf4ed6ff |
| Protobuf syntax/code generation | **PASS** locally |
| Retained host doctor artifact | Exit 0 and mode 0600; final doctor_complete log event is absent |
| Working-tree freeze/reproducibility | **FAIL**: material implementation and lock artifacts remain uncommitted |

I did not rerun the remote Ubuntu GPU/SDL/CUVID tests in this review. Their results are assessed as retained report evidence, not independently reproduced live evidence. I also did not rerun the Mac doctor because it briefly creates a display; I inspected the retained report and log instead.

## What is already good and should be preserved

- The canonical placeholder bytes, hash, and prefix are correct.
- The WIRE document is substantially clearer and incorporates ERR-01 through ERR-05 in useful, implementable language.
- V3 protobuf tags, reservations, framing structures, caps, and malformed-fixture coverage are strong.
- Swift and Rust fixture checks share the same truth artifacts.
- Pinned protobuf generation and regeneration checking work.
- Both decoder candidates open and decode the real sample under the reported Ubuntu environment, including the CUVID transfer path.
- The experiment correctly retains a packet across EAGAIN in its implementation and includes a valuable tail-drop check; it needs deterministic coverage, not replacement.
- The structured diagnostics, environment capture, fatal taxonomy, file rotation, native doctors, and instance locks are the right maintainability mechanisms for this single-user fixed pair.
- Additive V1 ClockPing/ClockPong fields, the RescProto module naming, an executable Swift fixture checker, the private harness ACK format, and the small disposable harness duplication are reasonable scoped deviations.
- The report openly identifies the dirty working tree and major A0 work not yet run. That honesty makes the remaining correction straightforward.

## Minimal closure plan

Keep the implementation simple and complete the gates in this order:

1. **Correct the status language.** Treat the current document as an A0.0 progress/candidate report, not a completion/freeze report.
2. **Resolve the normative schema path.** Update the artifact or add the narrow erratum; keep legacy A0 compilation explicit.
3. **Implement the three missing proofs.** Activation barrier scheduling, capture-generation isolation, and continuous-monotonic cursor timestamps.
4. **Repair the trace contract.** SCK PTS conversion, exact per-frame identity, true receive/present timestamps, clock uncertainty, joined retained output, and optical validation.
5. **Finish ERR-03 and backend selection.** Force EAGAIN/zero/multiple-output cases, run clean trials, select one backend, and freeze the two bounds.
6. **Make doctors fail closed.** Exact frame equality, both texture formats, every VideoToolbox status/read-back, actual profile settings, and synchronous evidence flush.
7. **Make the harness fail closed.** Successful writes, full count/ordinal invariants, zero outstanding work, JSON violation fields, realistic encoder settings, and nonzero exit on any failure.
8. **Harden immediate-exit logging and test lock contention.**
9. **Use true exact dependency requirements and commit lockfiles.**
10. **Rerun every gate from one clean commit** and record commands, hashes, host/box environment identity, exit codes, and retained evidence paths.

After these steps pass, a short amended report can legitimately declare A0.0 complete and Stage 1 frozen. A0 may then begin. Formal A0 still needs the real-capture record-size histogram, window trials, joined latency baseline, optical spot-check, and final bitrate confirmation already acknowledged in the report.

## Final recommendation

**Do not discard or redesign this work. It is a strong implementation candidate with several real successes. But do not freeze Stage 1 or start A0 yet. Close the trace identity/clock path, the explicit A0.0 measurements, the mandatory behavioral proofs, and the false-positive gate logic; then freeze everything at a clean commit and re-review.**
