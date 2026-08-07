# Review of the Keyframe-Storm Fix Report

Reviewed: `KEYFRAME_STORM_FIX_REPORT.md`

Report SHA-256: `d6e7fbf3b73679ed75509617fc5c03c25ae93550a416b6e61a2cea636702b222`

Fix commit: `cad416d3fa8a7c89365cc0e6ac99383cedbaa14b`

Review date: 2026-08-06

## Executive verdict

**CONDITIONAL ACCEPT — the code change is a valid fix for the raw-`0xFA` false-positive defect and is suitable for the personal demo after one post-fix relaunch smoke test.**

The key technical decision is correct. The Mac now decodes a complete protobuf `Envelope` and acts only when the oneof payload is actually `.requestIdr`. A byte value of `0xFA` inside a session-ID varint, a Stats float, or another field remains field data and cannot masquerade as the outer field-31 tag. The legacy byte scan is gone. No wire bytes changed, and the Rust client already sends the matching generated `RequestIDR` message.

I therefore accept `cad416d` as the narrow root-cause fix. I do **not** yet accept the stronger statements that:

- the historical 4,246-request incident is conclusively proved to have been caused by the session-ID lottery;
- every genuine recovery request is validated and reliably honored;
- the fix has been live-verified.

Those qualifications do not justify another redesign. Relaunch once with the new binary, retain five minutes of fresh logs, and close the demo issue if the 3.33 Hz storm is absent. A deterministic regression test is more valuable than the report's proposed three random sessions.

## Decision table

| Claim | Decision |
|---|---|
| Replacing the byte scan with typed oneof decoding | **Accepted** |
| Existing client/server wire compatibility | **Accepted** |
| Elimination of Stats/session-ID `0xFA` false triggers | **Accepted by source inspection** |
| Session-ID lottery as the historical cause | **High-confidence explanation, not conclusive proof** |
| Exact three-way reason attribution | **Rejected; the wire mapping is coarser** |
| Existing build and fixture health | **Accepted, but fixtures do not test this handler** |
| Live runtime verification | **Pending; current running process predates the patch** |
| Personal demo readiness | **GO after one five-minute post-fix smoke test** |

## Why the core fix is valid

The production path now behaves as required:

1. `ControlChannel.swift:108-153` removes the four-byte length prefix and passes one complete protobuf payload to `HostSession`.
2. `HostSession.swift:85-105` decodes that payload as `Resc_Control_Envelope`.
3. SwiftProtobuf selects `.requestIdr` only when the envelope contains field 31 as a oneof member.
4. `HostSession.swift:193-197` no longer scans arbitrary bytes during streaming.
5. `ubuntu-client/crates/net-transport/src/control_channel.rs:173-184` sends the same generated `RequestIDR` oneof.

This is categorically safer than searching serialized bytes. A useful deterministic old-bug vector is `session_id = 250`: its protobuf varint is `FA 01`. Under the old implementation, every active Stats envelope with that session ID could request a keyframe. Under the new implementation, the envelope decodes as `.stats`, so no keyframe callback occurs.

The report's approximate lottery calculation is reasonable. A random `u64` varint has up to nine continuation bytes, each of which can equal `0xFA`; a probability around 6-7% is a sound estimate. A 10 Hz Stats stream plus a 250 ms limiter also explains the observed approximately 300 ms cadence. The earlier exact relationship—4,246 request logs and 4,247 keyframes including the initial keyframe—is especially consistent with repeated forced-IDR behavior.

The model is not literal proof because the storm session ID was never retained or logged. Still, the deleted byte scan was objectively defective even if some historical requests were genuine. There is no reason to reject the patch while waiting to reconstruct the old incident.

## Findings and required report corrections

### 1. The fix has not yet run in the live pair

The current `/tmp/resc4k-host.log` process started at approximately 18:48, while `cad416d` was committed at approximately 19:06. That log is therefore pre-fix. It shows a normal old-code session with zero `IDR requested` lines and, at 10,200 encoded frames, 20 keyframes. This supports the report's session-dependent-lottery theory, but it does not verify the patch.

The old `/tmp/resc4k-host-claude.log` that contained the storm is no longer present. The previous independent review did count 4,246 request lines before it disappeared, but future reviewers cannot reconstruct the session ID or timing from a retained artifact.

I independently rebuilt the current Mac host successfully, so the next launcher run will use a binary containing the typed handler. The existing `resc-fixture-check` suite also passes, but no fixture invokes the v1 `HostSession` request path.

### 2. Add a deterministic regression test instead of relying on three random sessions

Three sessions have only about a 19% chance of exercising a 7% lottery if the old bug were still present. Their absence of a storm would therefore be weak evidence.

The durable regression should prove at least:

1. a Stats envelope with `session_id = 250` and/or float bytes containing `0xFA` produces zero force-keyframe callbacks;
2. a valid current-session `RequestIDR` produces exactly one callback;
3. a malformed or mismatched request produces no callback;
4. the limiter behaves correctly immediately below and above 250 ms.

This can remain small: extract the v1 runtime-message classification and validation into a pure helper that `HostSession` calls, then exercise that helper from `resc-fixture-check`. There is no need for a new test framework or subsystem.

For the demo, the test is recommended rather than blocking. One post-fix live run is enough to proceed.

### 3. Typed structure is validated, but request identity is not

The new case checks only `sm.state == .streaming`. It does not verify:

- `envelope.protocolVersion` against the current protocol;
- `envelope.sessionID` against `sessionID`;
- `idr.streamID` against `streamID`;
- `idr.configID` against `configID`;
- a non-`.unspecified`, recognized reason.

Consequently, an empty but structurally valid `RequestIDR`, a stale request from another stream, or a request with an unknown reason can still force a keyframe. This does not recreate the Stats/`0xFA` storm with the trusted current client, so it does not invalidate `cad416d`. It is nevertheless a small protocol-hardening omission.

Recommended guard before calling the handler formally complete:

```swift
guard envelope.protocolVersion == UInt32(ProtocolConstants.protocolVersion),
      envelope.sessionID == sessionID,
      idr.streamID == streamID,
      idr.configID == configID,
      idr.reason != .unspecified
else {
    // Log once/rate-limited and ignore or close the bad control session.
    return
}
```

Also reject `.UNRECOGNIZED`. For this fixed, single-client demo, this is hardening rather than a launch blocker.

### 4. Reason attribution is useful but not as exact as the report states

The client has three internal reasons, but the current mapping is:

- `DecodeError` → wire `DECODE_ERROR` (`2`);
- `CorruptFrame` → wire `DECODE_ERROR` (`2`);
- `ReferenceLoss` → wire `PARAMETER_SET_LOSS` (`3`).

The host will therefore log `decodeError` or `parameterSetLoss`, not the original `DecodeError / CorruptFrame / ReferenceLoss` triplet claimed by the report. The fix does distinguish a genuine typed request from stray payload bytes, which is the important improvement, but it does not preserve the precise decoder cause.

For now, correct the report and interpret reason `2` as `decode-or-corrupt` and reason `3` as the client's current `ReferenceLoss` mapping. A later cleanup can align the internal enum with the schema without delaying the demo.

### 5. The malformed-message retry justification is incorrect

Ignoring an unparseable streaming record is safer than guessing that it requests an IDR. That fail-safe direction is acceptable for the current trusted pair.

However, the report says a client that truly needs recovery re-requests within 250 ms. The current decoder does not guarantee that. Once it enters `WaitingForIDR`, non-keyframes are discarded at `video-decode/src/lib.rs:237-240` without generating another request. The original pending reason is taken once by `main.rs`, so a lost or malformed request can leave the client waiting indefinitely.

Do not restore the byte scan. Instead, log a decode failure and close/restart the control session, or add an explicit retry timer while the decoder remains in `WaitingForIDR`. For the current generated-schema pair, valid requests parse correctly, so this is a robustness follow-up rather than a reason to reject the fix.

### 6. Genuine force-keyframe delivery still has a small data race

`HostSession` invokes `encoder.forceKeyframe()` from the control queue, while `VideoEncoder.encode()` reads and clears `pendingForceKeyframe` on the encoder thread. That Boolean is not protected by the existing encoder lock. A request arriving between the read and clear can be lost, and Swift does not define unsynchronized cross-thread access as safe.

This is pre-existing and unrelated to the false-positive storm, but it limits the report's claim that real requests are reliably honored. Protect the flag with the existing lock, or replace it with a lock-protected consume operation. The proposed forced-recovery smoke test should remain until this is corrected.

### 7. The `StreamingReady` defect remains separate

The report correctly flags that `handleStreamingReady` still accepts any non-intercepted message while awaiting readiness. A Stats, unknown, or malformed record can therefore start streaming and force the startup keyframe. The new typed RequestIDR interception removes one way to reach that bug but does not fix the handshake generally.

Leaving Stats silently consumed during the streaming state is acceptable for this keyframe fix. Leaving the readiness guard as-is is not a sound final protocol invariant, but it is already part of the previously identified receiver-ready hardening and need not block this demo patch.

## Minimal acceptance gate

Do not require another long review cycle. Use this bounded check:

1. Relaunch the pair once so the host process definitely starts from the rebuilt post-`cad416d` binary.
2. Retain fresh Mac and Ubuntu logs.
3. Stream for at least five minutes. The old storm would add roughly 1,000 forced keyframes in that period, so it is easy to distinguish.
4. Require no periodic approximately 3.33 Hz `IDR requested` pattern. Do not require an exact 600:1 frame ratio: actual encode throughput is below 60 fps, the encoder also creates startup keyframes, and the ten-second duration limit is the meaningful baseline.
5. Exercise one genuine recovery request. Confirm a host `reason=` line, a subsequent keyframe, and client recovery.

If those five checks pass, **close the keyframe-storm item for the personal demo**. Add the deterministic `session_id = 250` test when making the small protocol-hardening corrections; do not wait for three lucky or unlucky random sessions.

## Answers to the report's questions

1. **Does the lottery model explain the old storm?** Yes, with high confidence. The static defect and observed cadence are sufficient to accept the code fix. They are not enough to label the exact old session causally proved, because its ID and raw log were not retained.
2. **May Stats remain silently consumed?** Yes, for this narrow fix and personal demo. **May `StreamingReady` remain accept-any-message forever?** No, but it is a separate bounded handshake fix and not a blocker here.
3. **Is ignoring unparseable records acceptable?** It is safer than guessing. The current re-request rationale is wrong, though; log/fail/reconnect or implement an explicit recovery retry rather than silently relying on a retry that does not exist.

## Final opinion

**The fix is valid. Accept it conditionally, perform one clean post-fix relaunch, and move forward.**

Do not reopen the transport architecture or require another multi-session evidence campaign. The only near-term additions worth making are a deterministic no-false-trigger regression, basic request identity validation, and synchronization of the encoder's force-keyframe flag.
