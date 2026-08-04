# Implementation Plan V9 Review — Conditional Final Go

Reviewed document: `IMPLEMENTATION_PLAN_V9.md`  
Document SHA-256: `2f89eb3cbaa28df850fb22121998cc2de32a26c6b240344772d016f280a960cf`  
Review basis: the V8 review, current Swift/Rust/protobuf implementation, `protoc 3.20.3` syntax validation, and independent protocol, simplicity, and architecture audits  
Deployment assumption: one user, one fixed Mac–Ubuntu pair on a trusted wired LAN  
Verdict: **the architecture is final; apply one compact V9.1 contract patch before freezing protocol v3 or entering T1**

## Executive verdict

V9 successfully incorporates the architecture and most contract corrections from the V8 review. It is the first plan that is substantially standalone:

- one explicit trusted-LAN threat model
- one exact personal profile
- fixed listener and reconnect ownership
- a host-authoritative session bootstrap
- a literal generated-protobuf schema
- exact fixed-width video and UDP layouts
- complete host, flow-control, decoder, input, clock, and diagnostic sections
- phase entry gates and an extensive standalone validation matrix

The embedded protobuf declaration is syntactically valid. The listed binary sizes also check:

- `VideoHello`: 32 bytes
- `VideoHelloAck`: 16 bytes
- frame header: 32 bytes
- mouse-move datagram: 26 bytes
- cursor datagram: 43 bytes

The remaining issues are not architectural. They are small contradictions or missing dispatch rules that would still force a Swift or Rust implementer to choose behavior independently. Patch V9 in place; do **not** begin another architecture cycle.

| Scope | Decision |
|---|---|
| A0.0 logging, doctors, dependency pinning, locks, tracing, decoder experiments, and codegen scaffolding | **Go now** |
| Final protocol-v3/schema semantics freeze | **Hold for V9.1** |
| A0 measurement | **Go after its stated trace/clock prerequisite** |
| Final profile and profile-dependent fixture freeze | **After A0** |
| T1 | **Go after V9.1 and all §12 entry gates** |
| T2–T4 direction | **Approved** |

## V8 review requirements that V9 resolves

V9 correctly:

- Restates the retained pipeline, timing, diagnostic, and validation rules rather than normatively importing V7/V8.
- Declares the wired LAN trusted and removes the half-specified PSK mechanism.
- Validates the control, video, and UDP peer IPs as accidental-peer guards.
- Limits T1 to one real profile and removes undeveloped safe-mode profiles.
- Separates placeholder canonicalization tests from the final post-A0 profile hash in principle.
- Defines candidate run adoption and a two-sided profile response.
- Carries both canonical profiles so both endpoints can diagnose differing fields.
- Publishes an actual `Envelope`, message bodies, field numbers, and typed `FatalCode`.
- Uses generated protobuf's enforceable last-one-wins behavior without adding a raw scanner.
- Defines all non-protobuf fields as little-endian and requires exact UDP lengths.
- Restates the full cursor body layout.
- Defines a finite reconnect schedule and prune/check/append retry ordering.
- Adds input-value validation and removes the arbitrary button force-unwrap.
- Makes doctor-over-allowlist an explicit upgrade policy.
- Restates latest-wins capture, replay, queue ownership, RA verification, ACK/EAGAIN behavior, decoder identity, render handoff, input cleanup, clocks, and diagnostic evidence.

Keep those decisions.

## Mandatory V9.1 corrections before schema freeze and T1

### 1. Bind the video socket explicitly to the accepted control session

V9 defines the `VideoHello` and `VideoHelloAck` fields, but it does not normatively state the equality checks that permit `Ack(OK)`:

- `IMPLEMENTATION_PLAN_V9.md:42-54`
- `IMPLEMENTATION_PLAN_V9.md:144-151`

Before the client returns `OK`, require:

- peer IP equals the profile Mac IP
- magic, version, length, and reserved fields are exact
- `session_run_id == activeRun`
- `profile_hash == activeProfileHash`
- no other video socket is active for that run

If a structurally valid Hello has the wrong run ID or profile hash, return `MISMATCH`, close it, and classify the session deterministically. A malformed Hello is a protocol violation. `BUSY` is only for an already-active socket; `INTERNAL` is only for a local preparation failure.

Before the host accepts the Ack, require:

- exact magic, version, and length
- a known status value
- Ack run ID equals the dialed run
- only `OK` opens capture/input

An unknown status or mismatched Ack run ID is a protocol violation. This closes the last gap between the control handshake and the socket that carries video.

### 2. Add a control payload direction/state table

The validation gate says handshake step-skipping is rejected, but V9 does not define the legal direction and state for every payload:

- `IMPLEMENTATION_PLAN_V9.md:72-116`
- `IMPLEMENTATION_PLAN_V9.md:238-246`

Add a compact normative table:

| Payload | Direction | Legal state |
|---|---|---|
| `HostProfileAnnounce` | host → client | first bootstrap message only |
| `ProfileResult` | client → host | bootstrap response only |
| `FrameAck` | client → host | `AwaitingRA` or `Streaming`, for an outstanding ordinal |
| `DisplaySettings` | host → client | post-video-Ack |
| `KeyEvent`, `ButtonEvent`, `ScrollEvent`, `ReleaseInput` | client → host | post-video-Ack |
| `Heartbeat` | both | post-video-Ack |
| `ClockPing`, `ClockPong` | request/response | trace/doctor mode after profile acceptance |
| `FatalReport` | both | once a candidate run ID is known |

Any known payload received in the wrong direction or state is a deterministic protocol violation. Input received before video Ack is never injected.

Also freeze these message invariants:

- accepted `ProfileResult` ⇒ `reject_code == FATAL_UNSPECIFIED` and `video_listener_ready == true`
- rejected `ProfileResult` ⇒ `reject_code` is a known nonzero deterministic code and `video_listener_ready == false`
- unknown enum numerics are rejected
- `FrameAck` must name the oldest outstanding ordinal for both window one and window two
- an inbound `FatalReport` uses the code's frozen classification to choose `Failed` versus `Backoff`

Proto3 scalar enums do not expose ordinary field presence, so “set iff rejected” alone is not enforceable. The value matrix above is.

### 3. Correct the protobuf reservations and complete the fatal-code mapping

V8's removed `psk_proof` was field `5` inside `HostProfileAnnounce`. V9 instead reserves Envelope field `67` and labels it as the PSK slot:

- `IMPLEMENTATION_PLAN_V9.md:90`
- `IMPLEMENTATION_PLAN_V9.md:95-99`

Use:

```proto
message HostProfileAnnounce {
  bytes  profile_canonical = 1;
  bytes  profile_hash = 2;
  string build_commit = 3;
  bool   build_dirty = 4;
  reserved 5; // removed psk_proof
}
```

Envelope tag `67` may remain reserved, but it was the removed aggregate/`StatsSummary` slot, not the PSK field.

The frozen `FatalCode` also does not cover several failures promised elsewhere:

- invalid local profile constants
- a known payload in an invalid direction/state
- unknown video status/flags or other protocol-value violations
- failed required VirtualDisplay/SCK/CoreBrightness/SDL/native probes
- permission failures that prevent the fixed profile from operating

Add a small set of codes such as `PROFILE_INVALID`, `PROTOCOL_VIOLATION`, and `REQUIRED_NATIVE_API`, or explicitly map every case to an existing value. Do not leave the two languages to invent different codes.

State which existing code covers the first-RA deadline and every other named timer. Keep the classification table machine-tested in both languages.

### 4. Make the control bounds implementable with generated protobuf

V9 correctly caps the outer control frame at 64 KiB before allocation. Its validation gate additionally requires oversized inner fields to be rejected before allocation:

- `IMPLEMENTATION_PLAN_V9.md:62-64`
- `IMPLEMENTATION_PLAN_V9.md:243`

That inner-field promise is generally incompatible with the mandated generated decoder and no-scanner rule: generated protobuf normally materializes strings/bytes before application-level validation.

Use the simple enforceable rule:

1. Read the four-byte length prefix.
2. Reject a length above 64 KiB before allocating the frame buffer.
3. Decode with generated protobuf.
4. Immediately validate field byte lengths and semantic constraints before dispatch.

The outer cap already bounds decoder allocation. Change the validation gate to say:

- frame cap rejected before frame allocation
- per-field caps rejected immediately after generated decode and before state mutation, logging, injection, or further copying

Clarify that the general 256-byte string limit excludes the explicitly larger, 2-KiB `FatalReport.summary`.

### 5. Make candidate-run and failure wording exact

The bootstrap correctly says the client has no active run while validating the announce, yet the later rule says Envelope equality with `activeRun` applies after step 3:

- `IMPLEMENTATION_PLAN_V9.md:42-52`

Use state-specific wording:

- the host binds `activeRun` when it sends the announce
- the client holds `candidateRun` while validating that announce
- a rejection echoes `candidateRun` but never promotes it
- an accepted result promotes `candidateRun → activeRun`
- after promotion, every control Envelope must equal `activeRun`

V9 also overstates delivery semantics by saying an explicit rejection consumes no retry budget “on either side.” The client can classify its own mismatch deterministically, but if the response is lost, the host sees EOF/timeout and can only classify that observation as transient.

State instead:

- a valid received rejection is deterministic on both sides
- a lost rejection may cost the host one transient attempt
- the client remains `Failed` and does not initiate another connection

Finally:

- change “control connect/accept 3 s” to “Ubuntu control-connect attempt 3 s”
- retain the explicit rule that the idle Mac listener may wait indefinitely
- use `deque.count >= 5` and `processTotal >= 8` in the retry guard

The last change is defensive and preserves the intended boundary even if state is restored or corrupted.

### 6. Define the literal hashed profile, including the decoder backend

The profile table is descriptive rather than a literal JSON field schema. Placeholder expressions such as `20 Mbps` and `2 MiB+32` are also not the promised base-10 canonical JSON integers:

- `IMPLEMENTATION_PLAN_V9.md:17-32`

Publish a placeholder JSON template containing every hashed key with exact primitive types and decimal values. It should include at least:

- profile ID and protocol version
- both IP addresses and all four ports
- width, height, refresh, display index, and rotation
- codec, profile, bit depth, and frame-reordering rule
- bitrate, record cap, flow window, decoder lag, and output deadline
- selected decoder backend and its load-bearing configuration

Section 8 says the profile records the chosen backend, but the profile table omits it:

- `IMPLEMENTATION_PLAN_V9.md:191`

The backend is a categorical A0.0 result, separate from the five numeric constants. Record it in the final canonical profile and require the doctor to open exactly that backend. The current client must not retain its CUVID-then-software silent fallback after the profile is frozen.

Also define `build_commit` as one exact representation—for example, the full lowercase object ID returned by `git rev-parse HEAD`—so two builds do not disagree merely because one embeds a short hash.

### 7. Resolve the two-stage fixture freeze

V9 correctly says the final profile and hash freeze after A0, but elsewhere says all schemas and fixtures freeze at the end of A0.0:

- `IMPLEMENTATION_PLAN_V9.md:8`
- `IMPLEMENTATION_PLAN_V9.md:30-32`
- `IMPLEMENTATION_PLAN_V9.md:218-222`
- `IMPLEMENTATION_PLAN_V9.md:282-286`

Use two explicit stages:

**End of A0.0**

- freeze `control.proto`
- freeze structural `WIRE.md` layouts
- verify generated Swift/Rust code
- test canonicalization with the marked placeholder profile
- test profile-independent malformed fixtures

**End of A0**

- commit the three A0 constants and two A0.0 decoder bounds
- commit the selected decoder backend/configuration
- generate the final canonical profile and hash
- regenerate profile-bearing `ProfileResult` fixtures
- generate maximum-record fixtures using the measured cap
- run all final fixtures in Swift and Rust

Only the second stage satisfies the T1 entry gate.

The A0 window experiment also depends on behavior that production T1 has not implemented yet. State that A0 uses a narrow measurement harness, built during A0.0, that exercises the proposed framed-TCP/ACK path on the real encoder and selected decoder. Otherwise `flow_window_frames` is circularly required before the code needed to measure it exists.

### 8. Finish the remaining standalone input/cursor semantics

V9 still says “grab/release semantics unchanged,” which contradicts its standalone claim:

- `IMPLEMENTATION_PLAN_V9.md:198-203`

Restate the client-local state machine:

- initial state is local/released
- Ctrl+Alt+G is consumed locally and enters remote-grabbed state
- only the remote-grabbed state emits keyboard, button, scroll, and move traffic
- Ctrl+Alt+Esc is consumed locally, sends reliable `ReleaseInput`, and returns to released state
- quit sends `ReleaseInput` before teardown when possible

State whether the host tracks grab state. The simplest current contract is that grab is client-local: `ReleaseInput` releases reliable pressed state, while a delayed pre-release UDP move may move the pointer once and is accepted as harmless disposable state. If strict post-release pointer fencing is required, add an explicit reliable grab transition; do not leave the behavior implicit.

Freeze the remaining wire semantics in §5/`WIRE.md`:

- mouse/button coordinates are portrait StreamSpace pixels
- the client applies the inverse −90° render transform before sending
- the host clamps to profile bounds and maps using live `CGDisplayBounds`
- scroll units and axis/sign transformation
- cursor shape IDs `0…15` and their names
- cursor coordinates, hotspot meaning, scale meaning, and the off-display/hidden convention
- whether `contentCaptureTs_us` is the host continuous-monotonic microsecond epoch

Without those definitions, a future agent still needs the current implementation to reproduce input and cursor behavior.

## Correct important overclaims

### Decoder memory

`decoder_lag_bound` bounds unresolved accepted-versus-emitted ordinals. It does not necessarily equal the FFmpeg/CUVID reference-surface allocation:

- `IMPLEMENTATION_PLAN_V9.md:186`

Replace the claim that decoder-internal frames are bounded by the lag number with:

- unresolved ordinal count is bounded by `decoder_lag_bound`
- decoder surfaces/memory are bounded by the selected backend's configured/tested pool
- the chosen pool size and observed memory are logged by doctor and soak tests

### Stale encoder callbacks

V9 correctly forbids video generations but later calls pre-teardown output “a generation of encoder output”:

- `IMPLEMENTATION_PLAN_V9.md:174`

Call it a callback tagged with the submitted `session_run_id`. Discard it when that tag no longer equals the active run. No generation counter is needed.

### Cursor snapshot coherence

The proposed T2 `AtomicU64` contains only `x`, `y`, `shape`, and sequence, while the UDP cursor tuple also contains hotspot and scale:

- `IMPLEMENTATION_PLAN_V9.md:155-166`
- `IMPLEMENTATION_PLAN_V9.md:196`

For this fixed profile, either:

- declare hotspot/scale immutable per shape and keep them outside the atomic snapshot, or
- include them in a coherent seqlock/owned snapshot

Do not claim whole-cursor tuple coherence while variable fields remain outside the packed value. This is a T2 correction, not a T1 blocker.

## Nonblocking simplifications

- Keep the clock/optical system confined to benchmark and diagnostic modes as V9 already states; normal streaming should not depend on it.
- The exact actor classes, socket-loop implementation, log library, and Swift target layout remain implementation choices.
- If Night Shift mirroring is not actually required, it can be deferred to reduce private-API and doctor surface. If retained, V9's explicit probe-and-fail behavior is appropriate.
- Initial numerical timer values remain provisional until their named measurements, but their owners and start events should be recorded beside the final constants.

## Recommended V9.1 checklist

- [ ] Bind `VideoHello`/Ack to the active run and profile hash.
- [ ] Add the control payload direction/state table.
- [ ] Freeze `ProfileResult`, `FatalReport`, and unknown-enum semantics.
- [ ] Reserve `HostProfileAnnounce.psk_proof` field 5; relabel Envelope tag 67.
- [ ] Complete the stable fatal-code mapping.
- [ ] Make only the 64-KiB outer cap pre-allocation; validate fields immediately post-decode.
- [ ] Clarify candidate versus active run, lost rejection, connect timeout, and `>=` retry guards.
- [ ] Publish the literal canonical profile schema and selected decoder backend/configuration.
- [ ] Split the A0.0 structural freeze from the post-A0 final-profile freeze.
- [ ] Name the A0 stop-and-wait measurement harness.
- [ ] Restate grab/release, coordinate, scroll, cursor-shape, and hidden-cursor semantics.
- [ ] Correct decoder-memory, stale-callback, and T2 cursor-coherence wording.

## Final recommendation

**V9 is conditionally and finally approved.**

Proceed now with the non-wire-dependent A0.0 work. Apply the compact V9.1 corrections above before freezing `control.proto`, finalizing the wire fixtures, or starting T1.

After that patch and the already-defined A0.0/A0 empirical gates, proceed directly to implementation. No further architecture review is warranted; the next review should be of generated artifacts and code, not another redesign document.
