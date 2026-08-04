# Implementation Plan V8 Review — Conditional Go

Reviewed document: `IMPLEMENTATION_PLAN_V8.md`  
Review basis: the V7 review, current Swift/Rust protocol implementation, and independent protocol, simplicity, and architecture audits  
Deployment assumption: one user, one fixed Mac–Ubuntu pair, updated together on a trusted wired LAN  
Verdict: **keep the V8 architecture; make one compact V8.1 contract edit before freezing protocol v3 or starting T1**

## Executive verdict

V8 resolves the substantive V7 problems. Its product architecture is now appropriate for the actual deployment:

- one hard-coded personal profile
- Mac-owned control listener and Ubuntu-owned video listener
- separate control and video TCP connections
- UDP only for disposable latest-state mouse movement and cursor state
- a fixed one- or two-frame transport window selected by measurement
- full-session reconnect instead of generations or partial recovery
- reliable keyboard, button, scroll, release, and heartbeat messages
- client-side decoder-output liveness in addition to host-side transport liveness
- persistent diagnostics, doctor commands, dependency pinning, and instance locks

Do **not** restore V6-style negotiation, adaptive ladders, video generations, byte-credit accounting, or UDP-video recovery.

V8 is not quite ready for its claim that the contract is complete. The remaining problems are specification contradictions that would make an implementation agent guess. They require a short V8.1 edit, not another architecture cycle.

| Scope | Decision |
|---|---|
| A0.0 logging, doctors, dependency pinning, instance locks, tracing, and decoder experiments | **Go now** |
| Code-generation scaffolding | **Go now** |
| Final protocol-v3 schema and wire-fixture freeze | **Hold for V8.1** |
| A0 measurements | **Go after the stated A0.0 trace/clock evidence** |
| T1 implementation | **Hold for V8.1, generated fixtures, and the A0 constants** |
| T2–T4 direction | **Approved** |

## V7 review requirements that V8 resolves

V8 correctly:

- Fixes TCP listener ownership and reconnect initiation.
- Defines a host-authoritative, control-first handshake.
- Uses the Envelope run ID rather than impossible duplicated payload run IDs.
- Defines canonical profile bytes and treats the eight-byte hash as opaque bytes.
- Makes commit mismatch and ordinary dirty-build behavior explicit.
- Separates deterministic failures from transient failures.
- Adds bounded handshake, encoder, random-access, ACK, and decoder-output timers.
- Keeps ACK after decoder acceptance and drain, avoiding an ACK-after-output startup deadlock.
- Correctly limits the host ACK watchdog to transport/acceptance stalls.
- Adds a separate client watchdog for accepted-but-never-emitted decoder frames.
- Requires in-order ACKs if measurement selects a two-frame window.
- Defines exact 32-byte video records and checked widened length arithmetic.
- Scopes the memory bound to encoded transport records and separately enumerates raw/decoder/render retention.
- Describes the approximately 60-second safety IDR honestly.
- Adds `ReleaseInput` and heartbeat-based reliable-path liveness without breaking legitimate long holds.
- Carries the full 64-bit run ID in UDP packets.
- Bounds logs, permissions, fatal flushing, doctor reports, and retained secrets.
- Replaces startup `pgrep`/`SIGKILL` sweeps with advisory instance locks.
- Requires the real Mac–Ubuntu pair and selected decoder for the final flow-window decision.

Those decisions should remain unchanged.

## Mandatory V8.1 corrections before schema freeze and T1

### 1. Make V8 genuinely standalone

V8 says that V2–V7 are history or rationale and that V8 is the only normative document:

- `IMPLEMENTATION_PLAN_V8.md:1-9`

It nevertheless imports important behavior through phrases such as:

- “unchanged from v7”
- “unchanged four-timestamp/i128/bracketed-calibration contract”
- “Everything from v7 §9”
- “unchanged baseline”
- “v7 set, plus”

Examples occur at:

- `IMPLEMENTATION_PLAN_V8.md:100-107`
- `IMPLEMENTATION_PLAN_V8.md:120-126`
- `IMPLEMENTATION_PLAN_V8.md:136-145`

This contradicts the single-document promise and makes a future implementation or upgrade agent search obsolete plans to reconstruct current behavior.

Restate the retained load-bearing rules in V8 or move them into explicitly named current companion specifications. At minimum, the current normative set must contain:

- latest-wins capture and replay behavior
- encoder queue ownership and force-keyframe rules
- exact send-EAGAIN retry/drain behavior
- decoder PTS/FIFO/output-retirement rules
- the four-timestamp clock formula and signed arithmetic
- required JSONL fields, paths, correlation identifiers, and native-call evidence
- doctor operations and required failure-injection checks
- the complete validation gates that must pass before T1

Generated `proto/control.proto`, `docs/WIRE.md`, and fixtures may become normative companions. An obsolete implementation plan should not.

The statement that the packed cursor snapshot and “u24 rule” are unchanged should also move to T2 and be defined there. The current client uses separate atomics, so it is not an existing invariant.

### 2. Publish an exact, enforceable protobuf schema

The displayed schema lists message bodies and comments containing intended payload tags, but it does not declare the actual `Envelope`:

- `IMPLEMENTATION_PLAN_V8.md:56-82`

The wording `protocol_version = 2 (=3)` is also easy to misread. Field number `2` carries a runtime value that must equal `3`.

Before code generation, publish the literal declaration, conceptually:

```proto
message Envelope {
  uint64 session_run_id = 1;
  uint32 protocol_version = 2; // runtime value must equal 3

  oneof payload {
    DisplaySettings display_settings = 32;
    KeyEvent key_event = 40;
    HostProfileAnnounce host_profile_announce = 60;
    ProfileResult profile_result = 61;
    FrameAck frame_ack = 62;
    ButtonEvent button_event = 63;
    ScrollEvent scroll_event = 64;
    ClockPing clock_ping = 65;
    ClockPong clock_pong = 66;
    FatalReport fatal_report = 68;
    ReleaseInput release_input = 69;
    Heartbeat heartbeat = 70;
  }
}
```

Use the final chosen response message name rather than copying `ProfileResult` mechanically.

V8 also requires an “ambiguous” recognized payload to be rejected while forbidding a raw protobuf scanner. Generated protobuf oneof decoding normally keeps the last encountered member, so multiple recognized wire tags are no longer observable after decoding.

For this lockstep personal deployment, use the simpler enforceable rule:

- validate exact protocol version, build, and profile
- require the decoded oneof to contain one recognized payload
- treat an absent payload as a protocol error
- accept protobuf's normal last-one-wins handling if duplicate oneof fields appear
- do not add a raw tag scanner

Finally, define the promised stable fatal/event enum in `control.proto` and use `FatalCode code`, not an untyped `uint32`. The enum values, meanings, and retry classifications must be frozen and tested.

### 3. Complete bootstrap and profile rejection

The successful handshake is now clear, but the client’s transition from “no active run” to the host-proposed run ID should be explicit:

1. Before binding a session, the client accepts only a valid `HostProfileAnnounce` with a nonzero Envelope run ID.
2. It validates framing and version, parses the bounded canonical profile bytes, recomputes their SHA-256 prefix, and requires that result to equal the transmitted hash.
3. It compares the profile and build against its local values.
4. On success, it binds `activeRun` and echoes that run ID in the response.
5. On failure, it sends a bounded deterministic rejection under the proposed run ID and closes without entering retry.

V8 promises that each side can log both profiles and the differing keys:

- `IMPLEMENTATION_PLAN_V8.md:17-21`

However, only `HostProfileAnnounce` contains canonical profile bytes. `ProfileAckVideoReady` contains only the client hash, commit, and dirty flag:

- `IMPLEMENTATION_PLAN_V8.md:36-39`
- `IMPLEMENTATION_PLAN_V8.md:68-70`

Therefore the client can diagnose a mismatch, but the host cannot inspect the client profile. If the client merely closes, the host may also observe an announce timeout and classify a deterministic mismatch as transient.

Use one bounded response that carries:

- accepted/rejected status
- client canonical profile bytes
- client profile hash
- client full build commit and dirty state
- stable rejection code when rejected
- video-listener-ready state only when accepted

This makes the two-sided diagnostic promise true and keeps mismatch out of the reconnect budget.

Because this software has one real deployment, remove the undeveloped H.264/software and alternate-host profiles from the T1 contract unless they are actually needed now. If retained, each requires its own checked-in canonical artifact, codec/RA rules, tests, and launcher pairing. Merely naming profiles does not define them.

### 4. State the trust model and remove the half-specified PSK branch

The optional `psk_proof` has no defined:

- algorithm
- nonce or challenge
- transcript
- direction/role binding
- client proof
- proof length
- secret storage
- constant-time comparison rule

It is off by default and therefore does not protect the actual profile:

- `IMPLEMENTATION_PLAN_V8.md:23`
- `IMPLEMENTATION_PLAN_V8.md:33-38`

For this one-user wired-LAN deployment, the simplest honest T1 rule is:

- declare the physical wired LAN trusted
- reserve or remove `psk_proof`
- remove PSK failure behavior from T1
- log `auth_mode = trusted_lan_none`
- continue validating expected source IPs as an accidental-peer guard, not as authentication

If authentication becomes necessary later, specify mutual authentication separately rather than retaining a unilateral, potentially replayable proof.

The host validates the control peer IP, but V8 does not require the Ubuntu video listener to validate the accepted socket’s source IP. Add:

- video peer IP must equal the profile's Mac address before processing `VideoHello`

UDP already has an equivalent source-IP rule.

### 5. Finish the fixed binary-wire definition

The record-size arithmetic in V8 is correct:

- `VideoHello`: 32 bytes
- `VideoHelloAck`: 16 bytes
- frame header: 32 bytes
- mouse-move datagram: 26 bytes
- cursor datagram: 43 bytes

The contract still does not state the byte order for all numeric fields. It also refers to an “existing 29-B body” whose offsets and endianness will be placed in a future `docs/WIRE.md`:

- `IMPLEMENTATION_PLAN_V8.md:86-92`
- `IMPLEMENTATION_PLAN_V8.md:113-118`

V8 cannot simultaneously be the source of those generated rules and leave the choice to the generator.

Use one simple rule:

- every fixed-width integer and IEEE-754 float in non-protobuf RESC records is little-endian
- magic values are the literal listed bytes
- every reserved field must be zero
- every UDP datagram must have exactly its declared length; truncated or trailing data is rejected

Freeze the cursor body explicitly:

| Body offset | Type | Field |
|---:|---|---|
| 0 | `u32_le` | `seq` |
| 4 | `u64_le` | `timestamp_us` |
| 12 | `i32_le` | `x_px` |
| 16 | `i32_le` | `y_px` |
| 20 | `u8` | `shape_id` |
| 21 | `u16_le` | `hotspot_x_px` |
| 23 | `u16_le` | `hotspot_y_px` |
| 25 | `f32_le` | `cursor_scale` |

The V8 UDP prefix is 14 bytes, so these body fields occupy absolute packet offsets 14 through 42.

Require golden fixtures for:

- successful and rejected profile responses
- `VideoHello` and every `VideoHelloAck` status
- frame headers at minimum and maximum permitted values
- mouse-move and cursor datagrams
- malformed length, reserved-field, flag, and overflow cases

### 6. Close the remaining lifecycle timing gap

Connection ownership, deadlines, teardown coalescing, and transient/deterministic classification are now sound:

- `IMPLEMENTATION_PLAN_V8.md:25-54`

`Backoff` itself has no duration even though V8 says no state may wait indefinitely.

Define a fixed schedule, for example:

- 250 ms
- 500 ms
- 1 s
- 2 s for subsequent attempts

Reset the schedule after 30 seconds of uninterrupted `Streaming`; do not reset the process-total restart counter.

Also clarify:

- the three-second control timeout applies to each Ubuntu connect attempt
- the Mac's process-owned listener may wait indefinitely in idle/no-session state without consuming retry budget
- prune restart timestamps older than 60 seconds before each transient restart decision
- reject a restart if five prior transient restarts remain in the deque or if the process total is already eight
- append the accepted restart timestamp and increment the process total exactly once
- map `VideoHelloAck` statuses to deterministic or transient outcomes

This removes off-by-one and idle-listener ambiguity without adding lifecycle machinery.

### 7. Correct profile-fixture phase ordering

The canonical profile contains values that A0 has not measured yet:

- `bitrate_bps`
- `max_record_bytes`
- `flow_window_frames`
- decoder lag and output-deadline bounds

V8 nevertheless schedules the golden profile fixture and hash tests in A0.0, before A0 commits those constants:

- `IMPLEMENTATION_PLAN_V8.md:13-17`
- `IMPLEMENTATION_PLAN_V8.md:136-140`

Generate and test the canonicalization mechanism during A0.0, but freeze the final canonical profile bytes and golden hash only after A0 fills every profile constant. Then run both Swift and Rust fixtures against the final artifact.

The final T1 gate should explicitly require:

- all five measured constants committed
- final profile artifact and hash regenerated
- Swift and Rust golden fixtures passing
- generated control schema and `WIRE.md` matching the same commit
- both doctors passing on the real endpoints

### 8. Complete value validation and upgrade diagnostics

Button validation is now correct. Add similarly explicit handling for:

- unknown `KeyEvent.hid_usage`: never inject; emit a bounded/aggregated `unsupported_hid` diagnostic
- `DisplaySettings.warm_strength`: require a finite value in `[0,1]`
- cursor `shape_id`: require a defined value
- cursor scale: require finite and positive
- all profile hashes: exactly eight bytes
- all run IDs: nonzero

These are small fail-closed checks at untrusted byte boundaries, not generalized compatibility machinery.

V8 also says “doctor-over-allowlist stands” without restating what that means:

- `IMPLEMENTATION_PLAN_V8.md:7`

Make the upgrade policy normative:

- an unlisted macOS or Ubuntu release is not rejected solely because its version string is new
- startup/doctor probes the required native APIs, symbols, selectors, properties, and return values
- successful probes permit operation while logging the unlisted version
- failed required probes stop the affected feature or process with a stable code and persistent evidence
- no native failure silently falls back to behavior that changes the fixed profile

Before T1 is considered complete, validate:

- both doctor commands and their JSON schemas
- log creation, rotation, `0600` permissions, and fatal flush
- forced native-API failures producing actionable records
- all connection and watchdog timeouts producing the expected stable codes
- instance-lock denial without killing any process
- dependency lockfiles tracked by Git

## Nonblocking implementation notes

These do not require another plan revision:

- The exact Swift code-generation target can be selected during A0.0, provided generated code is compiled as a normal package target and verified in CI/local tests.
- Initial timer values remain placeholders until their designated measurement gates.
- Window size one versus two is correctly an empirical result, not a paper decision.
- Actor/thread ownership, socket-loop mechanics, and logging-library choice are implementation details as long as the frozen lifecycle and diagnostics contracts are met.
- `Heartbeat.t_mono_us` may remain for diagnostics even though liveness only needs message receipt time.

## Recommended V8.1 checklist

- [ ] Restate every retained normative V7 rule or move it into a named current specification.
- [ ] Publish the literal Envelope oneof and a typed stable `FatalCode`.
- [ ] Replace unenforceable ambiguous-oneof rejection with normal generated-protobuf semantics.
- [ ] Define provisional run-ID adoption and a deterministic, two-sided profile result/rejection.
- [ ] Remove undeveloped safe profiles from T1 or define them completely.
- [ ] Remove/reserve the undefined PSK branch and state the trusted-LAN threat model.
- [ ] Validate the accepted video peer IP.
- [ ] Freeze little-endian binary layouts, exact UDP lengths, and the complete cursor body.
- [ ] Define the reconnect backoff and exact retry-counter operation.
- [ ] Move the final profile/hash fixture after A0 constants are committed.
- [ ] Add HID, display-strength, cursor-shape/scale, hash-length, and nonzero-run validation.
- [ ] Restate the doctor-over-allowlist policy and final diagnostic gates.

## Final recommendation

**V8 is conditionally approved.**

Proceed with the non-wire-dependent parts of A0.0 and the A0 measurement work now. Publish a compact V8.1 incorporating the corrections above before generating and freezing the final protocol-v3 schema or starting T1.

After that edit and the already-defined empirical gates, the plan is good to implement. No further architectural review is needed.
