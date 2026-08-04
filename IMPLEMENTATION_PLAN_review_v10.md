# Implementation Plan V10 Review — Conditional Final Go

Reviewed document: `IMPLEMENTATION_PLAN_V10.md`  
Document SHA-256: `53f7c40fa61a4d266e5d60e722fa5b3cd8a6157b458a0cf7affcc7a2552793a2`  
Review basis: the V9 review, current Swift/Rust/protobuf implementation, `protoc 3.20.3` validation, strict JSON parsing, and independent protocol, simplicity, and architecture audits  
Deployment assumption: one user, one fixed Mac–Ubuntu pair on a trusted wired LAN  
Verdict: **the architecture and implementation direction are approved; repair five terminal specification defects before Stage-1 freeze or T1**

## Executive verdict

V10 substantively closes the V9.1 contract work. It now provides:

- one honest trusted-LAN threat model
- one fixed personal profile with backend pinning
- explicit candidate/active run adoption
- deterministic versus transient failure rules
- a valid generated-protobuf schema with correct field reservations
- control payload direction/state restrictions
- video Hello/Ack session binding
- exact fixed-width layouts and cursor/input semantics
- bounded ACK/EAGAIN and decoder-output behavior
- complete diagnostic and upgrade-probe requirements
- a non-circular A0 measurement harness
- a two-stage artifact/profile freeze

The embedded protobuf declaration compiles successfully with `protoc 3.20.3`. Its tags and enum numerics are unique, `HostProfileAnnounce` correctly reserves field 5, and `Envelope` correctly reserves field 67.

The binary arithmetic is also correct:

| Record | Declared size | Verified |
|---|---:|---:|
| `VideoHello` | 32 B | 32 B |
| `VideoHelloAck` | 16 B | 16 B |
| Frame header | 32 B | 32 B |
| Mouse-move UDP | 26 B | 26 B |
| Cursor UDP | 43 B | 43 B |

No architectural redesign is warranted. The remaining findings are small but real: without them, the claimed terminal document cannot independently generate its profile, wire fixtures, or complete validation suite.

| Scope | Decision |
|---|---|
| A0.0 logging, doctors, locks, tracing, decoder experiments, and harness work | **Go now** |
| Stage-1 schema/WIRE freeze | **Hold for the terminal corrections below** |
| A0 measurement | **Go after its stated trace/clock evidence** |
| Stage-2 profile/fixture freeze | **After A0 and the corrected Stage-1 contract** |
| T1 | **Hold until corrections and §12 entry gates pass** |
| T2–T4 direction | **Approved** |

Patch V10 in place. Do not start another architecture-plan series.

## V9 review requirements that V10 resolves

V10 correctly:

- Binds `VideoHello` to the active run, profile hash, and expected peer.
- Adds the control message direction/state matrix.
- Defines the accepted/rejected `ProfileResult` value matrix.
- Reserves the removed PSK field at `HostProfileAnnounce.5`.
- Correctly identifies Envelope tag 67 as the removed aggregate slot.
- Adds stable codes for invalid profiles, protocol violations, native APIs, permissions, and first-RA timeout.
- Maps every named timer to a fatal code.
- Makes only the 64-KiB outer frame limit pre-allocation and performs field checks immediately post-decode.
- Uses explicit candidate versus active run terminology.
- Uses defensive `>=` restart-limit checks.
- Adds a literal profile field list, selected decoder backend, and full build-object ID.
- Splits structural and final-profile fixture freezes.
- Names an A0 framed-TCP/ACK measurement harness.
- Restates grab, coordinate, rotation, scroll, cursor-shape, and hidden-cursor rules.
- Corrects decoder-memory and stale-encoder-callback overclaims.
- Limits T2 cursor coherence to the tuple actually carried by the packed atomic.

Keep those corrections.

## Mandatory terminal corrections

### 1. Publish valid canonical profile bytes

V10 calls the profile block a literal canonical JSON schema and says canonical bytes are:

- valid UTF-8 JSON
- lexicographically sorted
- comment-free through normal JSON syntax
- whitespace-free

The displayed block does not satisfy that contract:

- it contains `/* ... */` comments, which JSON forbids
- `client_ip` is placed after `video_port`, although it sorts between `bitrate_bps` and `codec`
- it is formatted with whitespace while being described as the literal canonical form

References:

- `IMPLEMENTATION_PLAN_V10.md:21-47`

The displayed block fails strict `jq` parsing.

Publish either:

1. a valid readable template and separately state that the canonicalizer sorts/minifies it, or
2. the exact placeholder bytes that are hashed

For maximum simplicity and reproducibility, show exact placeholder bytes:

```json
{"bitrate_bps":20000000,"client_ip":"192.168.50.47","codec":"hevc-main-8bit","control_port":9870,"cursor_udp_port":9873,"decoder_backend":"TBD-A00","decoder_lag_bound_frames":4,"display_index":0,"flow_window_frames":1,"frame_reordering":false,"height_px":1920,"host_ip":"192.168.50.125","max_record_bytes":2097184,"move_udp_port":9872,"output_deadline_ms":200,"profile_id":"moyunfei-desk-1","protocol_version":3,"refresh_hz":60,"rotation":"portrait-neg90-render","video_port":9871,"width_px":1080}
```

Keep `TBD-A0`/`TBD-A00` explanations outside the hashed bytes.

The backend configuration also needs an exact schema. The current comment says the backend field contains an allowed name “plus load-bearing config,” but only one free-form string exists:

- `IMPLEMENTATION_PLAN_V10.md:29`
- `IMPLEMENTATION_PLAN_V10.md:49`

Choose one:

- define each backend string as a closed configuration ID whose complete options are frozen in the current source/WIRE documentation, or
- add explicit canonical fields for the load-bearing decoder options and surface-pool size

Stage 2 may select values; it must not invent new hashed keys after Stage 1.

### 2. Restore the `VideoHelloAck.status` byte mapping

V10 defines `status:u8` and names four states, but does not assign numeric wire values:

- `IMPLEMENTATION_PLAN_V10.md:188-192`

`docs/WIRE.md` and the four required golden Ack fixtures cannot be generated independently without that mapping.

Freeze:

| Byte | Status | Meaning |
|---:|---|---|
| `0` | `OK` | Hello accepted; video/input may open |
| `1` | `MISMATCH` | Valid Hello, wrong active run or profile |
| `2` | `BUSY` | An active video socket already exists |
| `3` | `INTERNAL` | Local video preparation failed |

Every other byte is `PROTOCOL_VIOLATION`.

### 3. Make the validation gates standalone

V10 declares itself the only normative document and supersedes all earlier plans:

- `IMPLEMENTATION_PLAN_V10.md:1-9`

Section 13 then says it is only a delta and that V9's validation table “carries forward”:

- `IMPLEMENTATION_PLAN_V10.md:271`

This directly violates the terminal standalone claim. A future agent must currently consult V9 for core gates covering:

- trace/clock and decoder FIFO evidence
- canonicalization and instance locks
- baseline constants and final fixtures
- handshake and two-sided mismatch
- legacy rejection and control bounds
- ACK/EAGAIN ordering
- host and client watchdogs
- cap and RA validation
- retry accounting
- stuck-input and long-hold behavior
- UDP hygiene
- diagnostics operations
- decoder identity
- T2 render/cursor ownership
- T3 final-static and latency acceptance

Merge the complete V9 table with V10's changed rows and rename §13:

```text
Validation gates (complete and standalone)
```

No obsolete implementation plan should carry a current entry or exit gate.

### 4. Correct the lost-rejection lifecycle

V10 says:

- Ubuntu alone initiates control connections.
- A deterministically rejecting Ubuntu client enters `Failed`.
- That client initiates no further connections.
- The Mac's idle listener waits indefinitely without consuming budget.

It later claims that after a lost rejection, the host's “next attempts” time out and exhaust its budget:

- `IMPLEMENTATION_PLAN_V10.md:55-69`
- `IMPLEMENTATION_PLAN_V10.md:279`

Those next attempts cannot exist because the Mac does not initiate control.

Use the actual behavior:

1. The client's rejection transmission is lost.
2. The client remains `Failed` and does not reconnect.
3. The host observes the current EOF/announce timeout and may charge exactly one transient restart.
4. After its backoff, the host returns to the process-owned idle listener.
5. It waits indefinitely without further budget use until the user fixes/restarts the failed client.

Remove the claim that the host terminates boundedly in this path. Persistent logs and the client-side deterministic failure provide the diagnosis.

### 5. Define `FatalReport{FATAL_UNSPECIFIED}`

`FATAL_UNSPECIFIED = 0` is necessary as the normal `ProfileResult.reject_code` value when `accepted == true`.

For `FatalReport`, however, V10 says every inbound code selects a frozen failure class, while code zero has no class:

- `IMPLEMENTATION_PLAN_V10.md:136-178`

Add:

```text
FatalReport.code must be a known nonzero FatalCode.
FATAL_UNSPECIFIED or an unknown numeric in FatalReport ⇒ PROTOCOL_VIOLATION.
```

Then the inbound transition is complete:

- deterministic or terminal code ⇒ `Failed`
- transient code ⇒ `Backoff`
- zero or unknown ⇒ `PROTOCOL_VIOLATION` ⇒ `Failed`

## Required implementation clarifications

These do not change the architecture, but should be placed in V10/`WIRE.md` before their corresponding implementation.

### Stale video-dial callbacks

Every asynchronous video connect/Hello/Ack operation must capture its run ID. Before mutating state:

- host requires `ack.run == dialedRun == activeRun`
- client requires the received Hello run equals its current `activeRun`
- a late socket/response for an old run is closed and logged without failing a newer active run

A wrong-run Hello may receive `MISMATCH`, but the client should continue awaiting the correct current-run Hello until the current handshake deadline. This prevents a late old connection from killing a valid replacement session.

### One exact scroll unit

“Line/pixel deltas” permits incompatible Swift and Rust scaling:

- `IMPLEMENTATION_PLAN_V10.md:207`

Freeze one unit—for example:

```text
One wire unit equals one raw SDL wheel step, interpreted by the Mac as one fixed pixel-scroll quantum of N pixels.
```

Specify `N`, rotation, sign, rounding, saturation, and natural-scrolling handling in the reference fixtures.

### Cursor hotspot/scale constants

T2 now treats hotspot and scale as immutable per-shape constants, while the T1 UDP record still transmits them. Before T2:

- define the expected constants for each shape
- require received values to equal those constants, or remove the redundant wire fields
- keep the packed snapshot coherence claim limited to `(x, y, shape, seq)`

## Verified sound portions

The following no longer need paper redesign:

- Trusted-LAN scope and source-IP guards
- Listener ownership and full-session reconnect
- Candidate/active run bootstrap
- Profile/build comparison and two-sided diagnostics
- Generated protobuf and last-one-wins handling
- Control-frame allocation bound
- Video/frame/UDP byte arithmetic
- Oldest-first ACK behavior for window one or two
- ACK after decoder acceptance/drain
- EAGAIN resubmission contract
- Client output watchdog
- Encoder and RA verification
- Reliable discrete input and release triggers
- Heartbeat-based stuck-input prevention
- Diagnostic logging, doctor commands, dependency locks, and upgrade probes
- Two-stage artifact/profile freeze
- T2–T4 architectural direction

## Recommended terminal-errata checklist

- [ ] Replace the profile block with valid, sorted, comment-free JSON bytes.
- [ ] Define the complete hashed decoder backend/configuration schema.
- [ ] Freeze Ack status bytes `0…3`.
- [ ] Merge the complete validation table into V10.
- [ ] Correct lost-rejection behavior to one host transient event followed by idle listening.
- [ ] Reject zero/unknown `FatalReport.code`.
- [ ] Run-tag stale video dial/Hello/Ack callbacks.
- [ ] Freeze one scroll unit and conversion.
- [ ] Define or remove redundant cursor hotspot/scale wire values before T2.

## Final recommendation

**V10 is conditionally and finally approved.**

Proceed now with A0.0 work that does not depend on the final wire/profile freeze. Apply the terminal errata above before completing Stage 1 or starting T1.

After those edits and the existing A0.0/A0 empirical gates, proceed directly to generated artifacts and implementation. The next substantive review should inspect `proto/control.proto`, `docs/WIRE.md`, fixtures, and code—not another architecture plan.
