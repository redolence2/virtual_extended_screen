# Review of the Keyframe-Storm Fix Review Response

Reviewed: `KEYFRAME_STORM_FIX_REPORT_response.md`

Response commit: `e9b8f78731ef710004fbb859b9ce25efe244d7d5`

Review date: 2026-08-06

## Executive verdict

**ACCEPT WITH CORRECTIONS — close the raw-`0xFA` keyframe-storm defect for the personal demo, but do not describe genuine recovery or reconnect behavior as verified.**

The implementation is good enough to move forward:

- `cad416d` replaced the unsafe raw-byte search with typed protobuf oneof decoding.
- `e9b8f78` correctly added request tuple/reason validation and synchronized the encoder's force-keyframe flag.
- The retained client streamed for 9 minutes 45 seconds and decoded 30,450 frames without an `IDR requested` storm.
- I independently rebuilt the current Mac host successfully, and `resc-fixture-check` passed.

The response nevertheless cannot be accepted verbatim. Its opening says the genuine-recovery exercise was completed, while its own table and later text correctly say it was **not exercised**. It also misidentifies a UDP receive interruption as a control-channel error and overstates what happened during the attempted reconnect.

These are report corrections, not reasons for another keyframe-fix cycle. For this personal demo, **GO** on the storm fix. Track recovery and reconnect as separate robustness work, and restart both ends together if the client dies until reconnect handling is fixed.

## Decision table

| Claim | Decision |
|---|---|
| Typed parsing eliminates raw-`0xFA` false triggers | **Accepted** |
| Request version/session/stream/config/reason guard | **Accepted by source inspection and build** |
| `pendingForceKeyframe` data race removed | **Accepted** |
| Post-fix stream ran for at least five minutes without a storm | **Accepted, with provenance qualification** |
| A genuine `RequestIDR` was exercised live | **Rejected — not exercised** |
| Zero invalid-request logs prove the guard accepts valid requests | **Rejected — the guard was not invoked by the retained run** |
| SIGSTOP caused a control-read failure | **Rejected — the retained error is UDP `EINTR`** |
| Host logged no disconnect and consumed a replacement `ModeRequest` while streaming | **Rejected — contradicted or unsupported by the retained logs** |
| Raw-byte keyframe-storm item is closed for the personal demo | **Accepted** |
| General recovery/reconnect reliability is closed | **Not accepted; separate open scope** |

## What is technically valid

### 1. Request validation is correct

`HostSession.swift:97-118` now calls the force-keyframe path only when all of the following match:

- session state is `streaming`;
- envelope protocol version and session ID are current;
- request stream ID and config ID are current;
- reason is recognized and is not `unspecified`.

The Rust client sends the assigned session/stream/config tuple in the generated `RequestIDR` oneof (`control_channel.rs:172-184`), so the validation is compatible with the existing client. The host control callbacks execute on one serial queue, making the two host-side request-log limiters safe on the current path.

The response should replace “no silent drops” with “invalid requests are observable at most once per second and never force a keyframe.” Repeated invalid records inside the one-second logging window are deliberately unlogged.

### 2. The encoder race fix is correct

`VideoEncoder.swift:269-289` reads and clears `pendingForceKeyframe` under the same lock used by `forceKeyframe()` at lines 349-353. A frame rejected by the in-flight gate returns before consuming the flag. This removes the unsynchronized read/write and the specific lost-request race identified in the prior review.

The wording “the next encoded frame honors it” is slightly too absolute. The flag is consumed before `VTCompressionSessionEncodeFrame`; a synchronous submission failure does not restore it. The accurate guarantee is that the next subsequently admitted, successful submission normally carries the force-keyframe property. This edge case does not invalidate the storm fix.

### 3. Current source health is acceptable

Independent verification on current `HEAD`:

- `swift build` — **PASS**;
- `swift run resc-fixture-check` — **PASS, all checks**.

The fixture suite still does not invoke the v1 `HostSession` `RequestIDR` handler. That test remains useful but non-blocking under the owner's personal-demo/simple-design requirement.

## What the retained evidence actually establishes

The retained logs support a clean smoke run:

- host PID 6584 started at 2026-08-06 21:08:07 JST;
- client connected and began streaming at 12:09:03 UTC;
- client stopped at 12:18:48 UTC after decoding 30,450 frames;
- elapsed live time was 9 minutes 45 seconds;
- host log contains zero `IDR requested` lines and zero `Ignoring invalid RequestIDR` lines;
- the cumulative encoder snapshot at 31,800 frames reports 55 keyframes, consistent with ordinary GOP behavior rather than a 3.33 Hz storm.

This is sufficient as a smoke check because source inspection already proves that Stats/session-ID bytes can no longer enter the typed `.requestIdr` arm. It is not necessary to run three random sessions.

Two qualifications should be recorded:

1. `55 KF / 31,800 frames` is process-lifetime encoder telemetry, not a pure client-session measurement. The host had already encoded 2,100 frames and four keyframes before the client connected. Keep the snapshot as supporting evidence, but use the 9-minute-45-second client interval plus zero request lines as the primary runtime statement.
2. The retained logs do not embed a commit SHA or build transcript. Their timing is consistent with a post-`cad416d` run, and the locally observed pre-launch source/build timing was consistent with the hardening edits, but the archive alone does not prove that the smoke binary contained every uncommitted `e9b8f78` change. The current committed source independently builds successfully.

The response's statement of zero `sendto` errors should be narrowed to zero **logged** initial `sendto` errors. `VideoSender.swift:113-116` logs a send failure only while `totalPacketsSent == 0`; later failures would not appear.

## Required response corrections

### 1. The acceptance sentence contradicts the evidence table

`KEYFRAME_STORM_FIX_REPORT_response.md:4-5` says the post-fix relaunch, soak, and genuine recovery exercise “has been executed.” Lines 62-68 and 104-111 correctly say the genuine request was **not exercised** and remains statically validated only.

Replace the opening with the equivalent of:

> The post-fix relaunch and 9-minute-45-second soak passed. A genuine recovery request was not exercised; the reviewer accepts closure of the raw-byte storm independently and leaves recovery reliability as separate scope.

### 2. The interrupted operation was UDP receive, not control receive

The retained client log at lines 535-538 shows:

- `UDP recv error: Interrupted system call (os error 4)`;
- video receiver stop;
- frame-channel disconnect;
- decoder shutdown.

There is no `Control recv error` in the retained log. Source confirms that `video_receiver.rs:111-118` breaks on `Interrupted`, after which the disconnected frame channel drives shutdown in `main.rs:633-637`.

Correct the new finding to say that the client exits when the video receive loop treats UDP `EINTR` as fatal. A future minimal fix can retry `ErrorKind::Interrupted`; it is not part of the keyframe-storm fix.

### 3. The reconnect diagnosis is not supported

The response says the host logged no disconnect, stayed in `streaming`, and silently consumed the replacement client's `ModeRequest`. The retained host log instead shows:

- line 290: `Control connection closed by peer`;
- lines 381-383: a replacement connection followed by `Session: streaming → negotiating`;
- no second `ModeConfirm`, `StreamingReady`, or `Streaming started` event.

No replacement-client log was retained, so the evidence does not prove that a second `ModeRequest` was sent or consumed. The source would route a message received in `negotiating` to `handleModeRequest`, not the streaming sink.

A plausible separate defect exists in `ControlChannel.swift:57-75`: cancelling the old connection can asynchronously clear the shared `connection` after the new connection has already been assigned. That could prevent the new connection from being read, but this is an inference and should be tested before being named as the cause. Record the observed result simply as “replacement connection did not complete negotiation.”

### 4. Remove or rename unsupported evidence statements

- Change `evidence/keyframe_fix/client-tail.log` to the existing `evidence/keyframe_fix/client.log`.
- Remove “post-incident fresh session (12 KF / 6,600 frames).” The retained host log contains only one successful `Streaming started` event and does not substantiate that metric as a fresh session.
- Do not say zero invalid-IDR lines demonstrate that the identity guard did not false-reject a valid request. No valid request reached the guard during this run.
- Change “recovery stalls for at most ~10 s” to “the host normally attempts a scheduled keyframe around the 10-second GOP bound.” A lost scheduled keyframe can extend recovery beyond ten seconds.

## Scope of the final acceptance

The original defect was false keyframes caused by scanning arbitrary serialized bytes for `0xFA`. Typed envelope parsing removes that defect categorically, the additional hardening is sound, and the long smoke run shows no runtime regression. Therefore:

**Close the keyframe-storm item and continue toward the demo. Do not require another soak, random-session campaign, architecture redesign, or test framework.**

Keep these as separate, non-blocking follow-ups:

1. retry UDP receive on `Interrupted`;
2. make disconnect/replacement connection ownership explicit, or continue restarting both ends together;
3. later add one deterministic valid-`RequestIDR` exercise if reliable live recovery becomes a demo requirement.

This verdict accepts the narrow fix without pretending that the unexecuted recovery gate passed.
