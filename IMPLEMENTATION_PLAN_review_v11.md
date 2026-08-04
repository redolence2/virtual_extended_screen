# Implementation Plan V11 Review — Implementation GO, Freeze Conditional

Reviewed: `IMPLEMENTATION_PLAN_V11.md`

Plan SHA-256: `f4925709d2655d05c373cccdb5fc106efb57d90463e803041af63be6acdf1d67`

Verdict: **Start A0.0 now. Do not create V12. Before the Stage-1 freeze, record and implement the contract errata in this review.**

## Executive decision

V11 is the first version that is sufficiently complete to stop rewriting the architecture plan and begin implementation. It incorporates the substantive V10 corrections, provides a compilable protobuf schema, supplies valid placeholder profile bytes, restores exact wire values, and makes its validation gates standalone.

It is not yet safe to call the contract frozen exactly as written. Two behaviors remain ambiguous enough to produce incompatible implementations:

1. control input can race ahead of the host's processing of `VideoHelloAck(OK)` because control and video use separate TCP connections;
2. the decoder gate permits FIFO behavior even though the runtime identity contract requires output ordinals from decoder timestamps.

There are also three smaller pre-freeze corrections: fully specify both decoder configuration IDs and the A0.0 placeholder/doctor rule, resolve scroll rounding, and remove an impossible UDP-reserved-field test.

These findings do **not** justify another plan. V11 already defines the intended governance: put the exact corrections in `CONTRACT_ERRATA.md`, reflect them in `docs/WIRE.md`, fixtures, and code, then freeze Stage 1.

| Milestone | Review decision |
|---|---|
| A0.0 implementation | **GO now** |
| Stage-1 freeze | **Conditional** on ERR-01 through ERR-05 below |
| A0 measurement | **GO** after V11's trace/clock gate |
| Stage-2 freeze | **Conditional** on measured final profile bytes and cross-language fixtures |
| T1 | **GO only through V11's entry gate** |
| T2–T4 direction | **Approved; must not delay the first working T1 system** |
| V12 plan | **Do not create** |

## Literal verification performed

### Canonical profile

The line-23 placeholder is valid UTF-8 JSON, minified, and lexicographically key-sorted. Hashing the exact JSON bytes with **no trailing LF** gives:

```text
SHA-256  0cc22496628805973f8d52292e7f838b95ec023faf658d71dd862f3fbf4ed6ff
prefix   0cc2249662880597
```

The prefix is the hexadecimal display of the first eight opaque hash bytes. The Stage-1 canonicalization fixture must ensure that a file-writing helper does not accidentally include its terminating newline in the hashed bytes.

### Protobuf and binary layouts

- The embedded schema compiles with local `protoc 3.20.3`.
- Its tags, reservations, `oneof`, and enum values are syntactically valid.
- `VideoHello` totals 32 bytes and `VideoHelloAck` totals 16 bytes.
- The frame header totals 32 bytes.
- Move and cursor UDP datagrams total 26 and 43 bytes respectively.
- Ack status bytes `0..3`, wrong-run handling, and stale video-dial protection are explicit.

### V10 errata closure

V11 materially resolves the nine items from the V10 review:

| V10 item | V11 result |
|---|---|
| Valid canonical profile bytes | Resolved |
| Closed backend IDs | Structurally resolved; exact configurations remain a Stage-1 requirement |
| Ack status byte mapping | Resolved |
| Standalone validation table | Resolved |
| Lost-rejection lifecycle | Resolved |
| Zero/unknown `FatalReport.code` | Resolved |
| Stale video-dial callbacks | Resolved |
| Exact scroll unit | Mostly resolved; rounding/scale sentence still needs ERR-04 |
| Cursor hotspot/scale constants | Resolved |

## Required contract errata before Stage-1 freeze

### ERR-01 — Add an explicit cross-TCP activation barrier

**Severity: blocking for Stage-1 freeze and T1.**

The client writes `VideoHelloAck(OK)` on the video TCP connection. Input and heartbeats travel on the separate control TCP connection. If the client enables input immediately after writing the Ack, its first control event can reach the host before the host receives and processes that Ack. V11 makes such a pre-Ack input a `PROTOCOL_VIOLATION`, so a correct low-latency implementation can nondeterministically kill a valid session.

Use the smallest barrier that requires no new protobuf message:

1. after writing `VideoHelloAck(OK)`, the client does not yet emit input or client heartbeats;
2. after the host receives and accepts that Ack, it sends an immediate `Heartbeat` on the control connection;
3. receipt of that host heartbeat is the client's activation signal; only then does it arm input and start normal heartbeats;
4. capture may start when the host accepts the Ack, as V11 already states.

Freeze this in `docs/WIRE.md` and test delayed/reordered scheduling of the two TCP handlers. The test must prove that no client control payload is written before activation and that the first post-barrier input is accepted.

### ERR-02 — Freeze both decoder IDs and define A0.0 placeholder behavior

**Severity: blocking for Stage-1 freeze, not for starting A0.0.**

`cuvid-lowdelay` and `sw1-lowdelay` are good closed IDs, but an ID is only closed if `docs/WIRE.md` gives every load-bearing option. At minimum, each row must freeze:

- FFmpeg decoder name and hardware-device setup;
- pixel-format selection;
- codec flags/options, threading mode, and exact thread count;
- frame-reordering/low-delay settings;
- exact surface-pool or `extra_hw_frames` value;
- permitted output format and the fact that fallback is forbidden.

The placeholder profile contains `"decoder_backend":"TBD-A00"`, which is intentionally not a runtime backend ID. Make its scope explicit:

- `TBD-A00` is accepted only by the placeholder canonicalization fixture and A0.0 measurement tooling;
- a normal handshake or final-profile doctor must reject it;
- during A0.0, the decoder doctor takes one explicit candidate from the two closed IDs and logs that candidate;
- after Stage 2, normal doctor mode opens exactly the backend in the final profile and accepts no override.

This preserves a single production profile without inventing a general configuration system.

### ERR-03 — Require ordinal-faithful decoder timestamps

**Severity: blocking for backend selection and Stage-1 freeze.**

Section 8 identifies output only through `AVFrame.pts`, falling back to `best_effort_timestamp`. The A0.0 validation gate nevertheless accepts a decoder that is merely FIFO with `threads=1`. FIFO alone does not define how an emitted frame receives its `frameOrdinal`, especially across EAGAIN and packets that emit zero or multiple frames.

For simplicity, remove the FIFO alternative. A candidate backend passes only if induced-delay tests prove that every emitted frame preserves the submitted `frameOrdinal` in `pts` or `best_effort_timestamp`. If neither candidate can do that, an explicit accepted-ordinal FIFO mapping would need its own exact contract and tests; do not silently infer one.

### ERR-04 — Make scroll injection mathematically exact

**Severity: required before the scroll fixture/WIRE freeze.**

V11 freezes one SDL step as a 10-pixel quantum, rotation, sign, integer arithmetic, and saturation, but then says the pixel event is converted to points using display scale. It does not define rounding for a fractional conversion.

The simplest fixed-pair rule is to avoid conversion: rotate the signed SDL steps, multiply each by 10 using widened checked arithmetic, saturate to `i32`, and pass the result directly to a CoreGraphics **pixel-unit** scroll event. If point conversion is intentionally retained, `docs/WIRE.md` must instead freeze the scale source and signed rounding rule. Fixtures must cover positive, negative, rotated, overflow, and any fractional-scale cases.

### ERR-05 — Remove `reserved` from the UDP hygiene gate

**Severity: clerical but required for a truthful standalone gate table.**

The move and cursor UDP layouts have no reserved field, yet the UDP hygiene gate requires rejection of a nonzero UDP `reserved` value. Remove only that word from the UDP gate. Keep reserved-zero validation for the video handshake and frame formats that actually contain reserved bytes. Do not change either UDP layout.

## Required implementation proofs, not reasons to revise the plan

### Late capture callbacks

V11 tags asynchronous video-dial operations and encoder callbacks by run, but it does not explicitly tag ScreenCaptureKit callbacks. A callback from a torn-down capture session must not populate the pending slot of a newer run. Bind each capture callback to its creation run/generation, check it before storing, and add a teardown/new-run/late-callback test.

### Cursor timestamp semantics

`Cursor.timestamp_us` has a frozen field but no stated clock or consumer. In Stage-1 WIRE, define it as sender-local diagnostic time in the host continuous-monotonic domain and explicitly state that sequence number—not timestamp—governs ordering, liveness, and presentation. This prevents a future agent from adding cross-machine timing logic to a disposable cursor packet.

### Exact profile artifact bytes

The Markdown example is sufficient as a specification, but the generated canonical profile artifact and both language fixtures must pin the full SHA-256 and the eight-byte prefix. Hash the JSON payload, not a text file's optional line terminator.

## Simplicity and maintainability assessment

V11 now reflects the personal fixed-pair requirement well:

- fixed IPs, ports, resolution, rotation, and one final profile;
- trusted-LAN operation without authentication machinery;
- no discovery, compatibility matrix, silent decoder fallback, or process-killing sweep;
- probe-based doctors and persistent structured diagnostics for future OS/API breakage;
- pinned dependencies and reproducible build/profile identity.

The measurement work in A0.0/A0 is justified because this project exists to fix latency and glitch behavior; it prevents replacing observed bugs with guessed constants. Keep those tools narrow and disposable where possible.

T2–T4 contain useful optimization and resilience work, but none should delay a stable T1 stream on the actual Mac/Ubuntu pair. Do not generalize the profile, introduce plugin abstractions, or support unowned devices. When a fixed value works, commit it, log it, and test it.

## Recommended implementation handoff

1. Create `CONTRACT_ERRATA.md` with ERR-01 through ERR-05 as dated normative corrections.
2. Begin A0.0: codegen, logging, doctors, locks, candidate-backend experiments, and the measurement harness.
3. At Stage 1, commit `control.proto`, complete `docs/WIRE.md`, generated Swift/Rust, placeholder-hash tests, and profile-independent malformed fixtures.
4. Run A0 on the real pair; commit the six measured/selected profile values.
5. Regenerate the final canonical artifact, golden hash, and all profile-bearing fixtures for Stage 2.
6. Enter T1 only when both doctors and both-language fixtures pass at the same clean commit.

## Final recommendation

**V11 is good enough to implement now. It is not an unconditional Stage-1 freeze until the five precise errata above are recorded and tested. Do not write V12. The next substantive review should inspect `CONTRACT_ERRATA.md`, `proto/control.proto`, `docs/WIRE.md`, fixtures, doctors, and code.**
