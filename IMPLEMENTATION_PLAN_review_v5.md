# Implementation Plan V5 Review

Reviewed document: `IMPLEMENTATION_PLAN_V5.md`  
Review type: static architecture and contract review  
Verdict: **conditional go**

## Executive verdict

V5 is substantially stronger than V4. Its overall architecture is now coherent, and no major redesign is required. In particular, the fresh-socket generation model, host-only generation authority, precommitted nonce, generation-fatal TCP recovery, byte-prefix credit ledger, bounded decoder bookkeeping, and random-access verification are all sound directions.

However, V5 is **not yet ready to declare §§2–10 frozen**. Several wire formats, state transitions, and liveness rules remain ambiguous enough that independent Swift and Rust implementations could make incompatible choices or reproduce a reset/credit loop.

The recommended boundary is:

| Scope | Decision |
|---|---|
| A0.0 tooling, traces, clocks, token experiments, and test harnesses | **Go** |
| A0 measurement | **Go after A0.0 demonstrates trace joining and clock uncertainty** |
| A1 implementation | **Hold until the V5.1 contract corrections below are written** |
| B0 and later | **Hold until §§2–10 are genuinely frozen** |

## Confirmed improvements

The following V4 blockers are adequately addressed in direction:

- `VideoHello` is 44 bytes, `VideoHelloAck` is 28 bytes, and the frame header is 38 bytes. The listed offsets and arithmetic in §3.2 are correct.
- Host-only generation allocation and the control-channel-precommitted nonce remove the earlier split-authority reset problem.
- A fresh video socket for every generation gives recovery a clean ownership and credit boundary.
- Making TCP decoder discontinuity generation-fatal avoids deliberately withholding an ACK while retaining unusable in-generation credit.
- `wireBytes`, a prefix ledger, cumulative exclusive progress, and fixed memory bounds provide a workable credit model.
- The 8 MiB frame limit is correctly described as provisional rather than falsely derived from `DataRateLimits`.
- Decoder token identity, structured send/receive outcomes, bounded side tables, and an output watchdog are moved early enough to support A1.
- The UDP receive path is moving toward an actor-owned, purgeable queue rather than the current unpurgeable bounded channel.
- Clock synchronization now uses a four-timestamp exchange and acknowledges the absolute-versus-continuous clock problem.
- Codec negotiation, exact random-access NAL verification, parameter-set requirements, and single-thread SDL ownership are explicitly recognized.

These are meaningful corrections. The remaining work is primarily contract completion, not architecture replacement.

## Mandatory corrections before freeze

### 1. Fully specify the v2 protobuf and transport contract

Section 3.1 assigns new `Envelope.oneof` tags, but it does not assign the fields, tags, scalar types, enum values, defaults, and validation rules inside:

- `ClockPing`
- `ClockPong`
- `DecoderProgress`
- `VideoResetRequest`
- `StartVideoGeneration`
- `CodecCapability`

The plan also uses `configGeneration` in the lifecycle without defining whether it reuses the existing `config_id` fields or introduces a new wire field. Payload-level and `Envelope`-level `session_id` values need an explicit equality requirement.

Additional required decisions:

- Add `VIDEO_TRANSPORT_UNSPECIFIED = 0`. With `UDP_V1 = 0`, an omitted proto3 enum silently becomes UDP.
- Either make `TCP_V2` the only legal v2 transport or publish the complete v2 lifecycle for `UDP_V1`.
- Define whether legacy `StartStreaming` and `RequestIDR` are rejected, ignored, or state-scoped under v2/TCP.
- Define `VideoHelloAck.status` values, reserved-zero validation, and unknown-extension handling.
- Express record magic as exact on-wire bytes or exact little-endian numeric constants, not language-dependent multi-character integers.

Until these details are assigned, the statement that the v2 schema is frozen is premature.

### 2. Complete the generation state machine

The happy-path ordering is good, but stale, duplicated, and concurrent events are not deterministic.

The contract must define:

- A duplicate or stale `VideoResetRequest` acceptance predicate.
- How a host-detected socket failure is coalesced with a simultaneous client reset request.
- Handling of stale `StartVideoGeneration`, `StreamingReady`, `VideoHello`, and `VideoHelloAck`.
- That the client stops the old reader, purges queued work, disposes old decoder state, and fences old output **before** acknowledging readiness for the new generation.
- A session-scoped retry/backoff limit so repeated timeouts, failed random-access output, or handshake failures cannot create an infinite generation loop.
- Typed peer-address ownership, address-family behavior, IPv6 scope preservation, and validation of the advertised nonzero listen port.

Without an idempotent transition rule, a host that already allocated generation `K+1` could process a delayed reset for `K` and unnecessarily allocate `K+2`.

### 3. Correct credit, ACK, and oversize liveness

The oversize ladder currently counts occurrences “in a generation,” while its first step creates a new generation. That resets the stated counter and can make every occurrence look like the first one forever.

Required correction:

- Track an oversize streak at session/configuration scope across generation replacements.
- Reset the streak only after a successfully delivered under-cap random-access frame or another explicitly defined success condition.
- If an encoded access unit exceeds the hard 16 MiB ceiling, skip cap growth and move directly to bitrate reduction or session failure.
- Advertise a session hard ceiling separately from each generation’s selected cap.
- Require and validate `creditWindowBytes >= maxWireFrameBytes` after every cap change.
- Define that an oversize encoder callback is rejected before being retained in bounded pending storage.

Section 6 also says the packet is ACKed “after step 4,” although step 5 drains decoder output to classified `Again`. The credit-releasing ACK should be published only after:

1. The packet is accepted exactly once.
2. The normal receive drain completes at `EAGAIN`/`Again`.

If the drain fails, the generation should close without advancing the ACK. `acceptedBytes` must explicitly use `wireBytes`.

The plan should also distinguish pre-encode admission time from byte-credit admission time and clarify that ordinary partial TCP writes are resumed by the bounded writer; only EOF or an actual write failure terminates the generation.

### 4. Tighten the decoder identity contract

FFmpeg PTS is a signed 64-bit domain, but V5 specifies a monotonically increasing `u64` token. The contract must:

- Constrain tokens to a valid signed `int64` range.
- Exclude `AV_NOPTS_VALUE`.
- Define pre-wrap/reset behavior.
- Name the exact output timestamp field used for identity.
- Define behavior for missing, unknown, duplicated, reordered, or non-one-to-one output tokens.
- Define `decodedRetiredThroughCount` as a contiguous exclusive prefix rather than a count of arbitrary completed outputs.
- State whether unknown-token output may ever be presented. It should normally be discarded and treated as an identity-contract failure.

The A0.0 CUVID and software token-passthrough experiments are therefore a real gate. A1 should not depend on token identity until both deployed paths pass or a concrete fallback is selected.

### 5. Finish UDP recovery and side-channel reliability

The interim UDP rules do not fully define recovery after a missing expected frame. The plan needs an exact transition such as:

1. Enter `AwaitingRA` and increment `recoveryEpoch` once.
2. Purge old assembler, reorder, decode, and presentation work.
3. Reject dependent frames while awaiting recovery.
4. Validate the first acceptable random-access frame.
5. Seed/release that frame and set the next expected frame ID.
6. Fence all output belonging to the old epoch.

The wrap-safe `u32` comparator must be defined mathematically. “Wrap-safe compare” alone is not sufficient for two independent implementations.

For input and cursor packets, publish exact v2 byte tables: offsets, endianness, total sizes, sequence width, epoch position, source-address rule, ports, and IPv4/IPv6 behavior. V5 currently refers to a `u24` cursor comparison while the existing wire carries a `u32` sequence.

Epoch fencing fixes cross-session staleness but does not fix same-epoch UDP loss, duplication, or reordering. A delayed mouse-down can still arrive after mouse-up and leave a button stuck. Button transitions should use either:

- The reliable ordered control channel, or
- Idempotent button-state snapshots with sequence/acknowledgement semantics.

Scroll deltas also need explicit duplicate and loss behavior. Mouse movement may remain UDP latest-wins.

### 6. Make negotiation truthful before confirmation

V5 says `ModeConfirm` fixes the applied configuration, but creates and validates the VideoToolbox encoder afterward. Encoder creation or a load-bearing property can therefore fail after the host has confirmed a mode it cannot actually produce.

The preferred order is:

1. Intersect capabilities and choose a candidate.
2. Create the VideoToolbox session.
3. Set and read back load-bearing properties.
4. Check `VTCompressionSessionPrepareToEncodeFrames`.
5. Only then send `ModeConfirm`.

Alternatively, the protocol would need an explicit post-confirm initialization-failure and renegotiation state.

`CodecCapability` should also avoid representing support solely as independent maxima. Maxima can falsely imply unsupported combinations—for example, support for 4K30 and 1080p60 does not imply 4K60. Prefer repeated supported mode/profile/level/tier tuples or another representation that preserves combination constraints.

### 7. Make clock arithmetic and bridging exact

The four-timestamp formula is correct, but timestamp differences from different machines can be negative. Calculating `t2 - t1` directly as unsigned arithmetic can underflow.

Specify:

- Widened signed arithmetic, preferably `i128` for intermediate calculations.
- A signed offset representation and range checks.
- Nonnegative delay validation before accepting a sample.
- `CMClockGetHostTimeClock()` as the target for `CMSyncConvertTime`.
- A bracketed absolute/continuous calibration sample, such as continuous-before, absolute, continuous-after with midpoint and uncertainty, rather than claiming an unavailable atomic dual-clock read.
- Recalibration and sample invalidation after sleep/wake.

## Minimum V5.1 amendment checklist

Before marking §§2–10 frozen:

- [ ] Assign every new protobuf field and enum value.
- [ ] Resolve `configGeneration` versus existing `config_id`.
- [ ] Add an unspecified transport value and define the v2 transport/state matrix.
- [ ] Publish stale, duplicate, concurrent reset, and retry transitions.
- [ ] Publish exact input/cursor v2 binary layouts.
- [ ] Make button state reliable or idempotent.
- [ ] Define the UDP random-access reseed transition and modular comparator.
- [ ] Scope the oversize counter across generations.
- [ ] Enforce `creditWindowBytes >= maxWireFrameBytes`.
- [ ] Move ACK publication after the successful receive drain.
- [ ] Constrain decoder tokens to FFmpeg’s signed PTS domain.
- [ ] Validate encoder creation before `ModeConfirm`.
- [ ] Prescribe signed clock arithmetic and an exact CoreMedia conversion target.

## Final recommendation

**Proceed immediately with A0.0.** It is useful and deliberately preserves the v1 wire while validating the highest-risk assumptions.

In parallel, issue a focused V5.1 errata containing the contract corrections above. Once that errata is incorporated, V5’s architecture is suitable to freeze and use as the basis for A1/B0 implementation.

This review does not recommend another architectural rewrite.
