# RESC Implementation Plan v6 — Consolidated Contract Freeze

| | |
|---|---|
| **Date** | 2026-07-29 |
| **Supersedes** | `IMPLEMENTATION_PLAN_V5.md`. v5's architecture, phase skeleton (A0.0→A0→A1→B0→B1→B2→C→D→E), findings register, invariants, and gates stand; this document completes the contracts review 5 found under-specified and is the **freeze artifact** for §§2–10. Prior documents remain unmodified. |
| **Incorporates** | `IMPLEMENTATION_PLAN_review_v5.md` (verdict: **conditional go** — A0.0 approved immediately, §§2–10 frozen only after the "V5.1" corrections). All seven mandatory correction areas and all 13 checklist items are resolved here (§14 maps each). |
| **RESC commit** | `12b87d1` |
| **Go/hold boundary (from review 5, adopted)** | **A0.0: GO now.** **A0: GO** once A0.0 demonstrates trace joining + clock uncertainty. **A1: GO** on publication of this document. **B0+: GO** after A0.0's decoder-token gate passes (§6.5) and the B0-entry checklist (§11) is met. |
| **Freeze rule** | §§2–10 of this document are frozen. Numeric parameters marked *(init)* are tunable from A0 data; structures, layouts, field numbers, and transition rules are not. Changes require a new plan revision. |

---

## 1. Corrections accepted from review 5 (summary)

1. Full protobuf/transport assignment — every new message body, enum, default, and validation rule is now written (§3), including the `VIDEO_TRANSPORT_UNSPECIFIED = 0` fix for the proto3 zero-default trap, the `config_id` reuse decision, Envelope-vs-payload `session_id` equality, the legacy-message matrix, exact on-wire magic bytes, and `VideoHelloAck.status` values (§4).
2. Generation state machine completed — idempotent reset predicate, stale/duplicate/concurrent event handling, client pre-ready obligations, session-scoped retry budget, and typed peer-address rules (§2).
3. Credit/ACK/oversize liveness corrected — oversize streak re-scoped to session+config (v5's per-generation counter reset itself via the ladder's own step 1), hard ceiling advertised separately, `creditWindowBytes ≥ maxWireFrameBytes` validated after every change, ACK published only **after** the receive drain classifies `Again`, resumable partial writes distinguished from write failure, and two admission timestamps defined (§5).
4. Decoder tokens constrained to ffmpeg's **signed** PTS domain with exact field, wrap, and unknown-token rules; `decoded_retired_through_count` defined as a contiguous exclusive prefix; unknown-token output is never presented (§6).
5. UDP recovery transition (`AwaitingRA`) and the modular comparators written mathematically; exact v2 input/cursor byte tables; **button transitions moved to the reliable control channel** (same-epoch UDP loss/reorder could still stick a button — epoch fencing alone was insufficient); scroll gets sequence dedup with documented loss tolerance (§7, §8).
6. Encoder is created and validated **before** `ModeConfirm`; capability schema uses combination-preserving mode tuples instead of independent maxima (§10).
7. Clock arithmetic in widened signed integers with nonnegative-delay validation, `CMClockGetHostTimeClock()` named as the conversion target, and a bracketed (not "atomic") absolute/continuous calibration (§9).

---

## 2. Generation & connection state machine (complete)

Roles, session rule, and the 7-step happy path are unchanged from v5 §2 (host-only generation writer; one generation per connection attempt; fresh socket per generation; failed attempt consumes its generation; control-connection-scoped sessions, resume non-goal). This section adds the deterministic edges.

**Host states:** `Idle → Negotiated(config) → OfferPending(K) → HandshakeInProgress(K) → AwaitingFirstRA(K) → Streaming(K) → GenerationClosed(K) → (OfferPending(K+1) | SessionFailed)`.

**Reset-acceptance predicate (idempotence).** Host accepts `VideoResetRequest` iff `observed_video_generation == currentGeneration` **and** state ∈ {`AwaitingFirstRA`, `Streaming`}. `observed < current` ⇒ stale: ignore + count (the client will learn the newer generation from the in-flight/next `StartVideoGeneration`). `observed > current` ⇒ protocol error (client claims the future) ⇒ control-connection reset. A delayed reset for K arriving after K+1 was allocated therefore **cannot** trigger K+2.

**Coalescing.** A host-detected socket failure moves to `GenerationClosed(K)` directly. While in `OfferPending`/`HandshakeInProgress`, an accepted trigger (predicate above never matches these states, so only internal failures apply) sets a single `retryPending` flag — at most one recovery is ever in flight; multiple triggers coalesce into one allocation.

**Stale/duplicate handshake messages.**
- Client rejects `StartVideoGeneration` with `video_generation < lastOffered` (stale duplicate: ignore+count). `== lastOffered` with an **identical** nonce ⇒ idempotent resend, re-ack; same generation with a **different** nonce ⇒ protocol error ⇒ control reset.
- Host ignores a `StreamingReady` that does not echo the exact outstanding offer tuple+nonce (stale; counted).
- `VideoHello`/`VideoHelloAck` validation is tuple+nonce as in v5; the client's listener for generation K is closed upon accepting the offer for K+1, so a stale dial cannot land.
- Client obligations **before** sending `StreamingReady(K+1)`, in order: stop the old socket reader; purge the decode-input queue (in-band barrier); dispose the decoder and identity table; fence/clear the render slot of old-generation output; close the old listener; bind the new listener. Only then advertise readiness.

**Retry budget (liveness).** Per session: backoff 250 ms, ×2, cap 2 s; **max 8 consecutive failed generations** (handshake timeout, dial failure, RA-verification failure after one re-force, oversize step-3) ⇒ `SessionFailed`, surfaced to the user. Any generation reaching `Streaming` with one ACK-accepted frame resets the consecutive-failure counter.

**Peer addressing.** Video dial target = the control connection's remote IP as a **typed** sockaddr: family preserved (IPv4→IPv4, IPv6→IPv6, IPv6 scope-id preserved for link-local); `listen_port` must be nonzero and in 1024–65535 (validated); the client binds the same family as the control connection. Source validation on accept: remote IP must equal the control peer.

## 3. Control protocol v2 — complete protobuf assignment

`protocol_version = 2`. **`configGeneration` (decision):** reuses the existing `config_id` wire fields everywhere — no new field; under v2 the host **increments `config_id` per `ModeConfirm` within a session** (it is a generation counter, not the constant `1` the current code sends). **Session equality rule:** wherever a payload carries `session_id`, it must equal `Envelope.session_id`; mismatch ⇒ message dropped + counted; 3 mismatches per connection ⇒ control reset.

**Transport enum (fixes proto3 zero-default):**
```proto
enum VideoTransport { VIDEO_TRANSPORT_UNSPECIFIED = 0; VIDEO_TRANSPORT_UDP_V1 = 1; VIDEO_TRANSPORT_TCP_V2 = 2; }
```
**Transport matrix (decision):** `TCP_V2` is the **only** legal transport under pv2. A pv2 peer not offering it ⇒ `ModeReject(UNSUPPORTED_MODE)`. The interim UDP path exists only under pv1 (Phases A0–A1); pv2 never carries UDP video.

**Legacy-message matrix under pv2:** `StartStreaming(23)` — never sent; if received ⇒ protocol error. `RequestIDR(31)` — pv1-only; under pv2 ⇒ rejected + counted (recovery is `VideoResetRequest`). `Stats(30)` — retained, telemetry-only (never credit). All other pv1 messages keep their meaning.

**New message bodies (field numbers frozen):**
```proto
message ClockPing  { uint64 t1_mono_us = 1; uint32 seq = 2; }            // Envelope field 33
message ClockPong  { uint64 t1_mono_us = 1; uint64 t2_mono_us = 2;       // Envelope field 34
                     uint64 t3_mono_us = 3; uint32 seq = 4; }
// Timestamps are each sender's own monotonic domain; requester records t4 locally.

message DecoderProgress {                                                 // Envelope field 35
  uint64 session_id = 1; uint32 stream_id = 2; uint32 config_id = 3; uint32 video_generation = 4;
  uint64 accepted_frame_count = 5;          // exclusive; credit cursor
  uint64 accepted_wire_bytes  = 6;          // wireBytes domain; prefix-paired with field 5
  uint64 decoded_retired_through_count = 7; // contiguous exclusive prefix (§6)
  uint64 last_presented_frame_id_plus_one = 8;  // 0 = nothing presented (frame 0 unambiguous)
  uint64 render_intentional_drops = 9;      // telemetry
}

message VideoResetRequest {                                               // Envelope field 36
  uint64 session_id = 1; uint32 stream_id = 2; uint32 config_id = 3;
  uint32 observed_video_generation = 4;
  enum Reason { REASON_UNSPECIFIED = 0; DECODE_FATAL = 1; WATCHDOG = 2; IDENTITY_FAILURE = 3;
                PRIMING_TIMEOUT = 4; PROTOCOL_ERROR = 5; USER = 6; }
  Reason reason = 5;
}

message StartVideoGeneration {                                            // Envelope field 37
  uint64 session_id = 1; uint32 stream_id = 2; uint32 config_id = 3;
  uint32 video_generation = 4;
  bytes  nonce = 5;                    // exactly 16 bytes; any other length ⇒ protocol error
  uint32 max_wire_frame_bytes = 6;     // this generation's cap
  uint32 credit_window_bytes  = 7;     // MUST be ≥ field 6; both ends validate (§5)
}

message SupportedMode { uint32 max_width = 1; uint32 max_height = 2; uint32 max_fps = 3;   // jointly valid
                        uint32 profile_idc = 4; uint32 level_idc = 5; uint32 bit_depth = 6; }
message CodecCapability { Codec codec = 1; repeated SupportedMode modes = 2; bool hardware = 3; }

message ButtonEvent {                                                     // Envelope field 41 (§8)
  uint32 seq = 1; uint32 button = 2; bool is_down = 3;
  sint32 x_px = 4; sint32 y_px = 5; uint32 modifiers = 6;
}
```
Amended existing messages (numbers frozen): `ModeRequest` + `video_transport_supported = 5 (repeated VideoTransport)`, `codec_capabilities = 6 (repeated CodecCapability)`. `ModeConfirm` + `video_transport_selected = 23`, `max_wire_frame_bytes = 24`, `credit_window_bytes = 25`, `side_channel_epoch = 26 (u32)`, `max_wire_frame_bytes_ceiling = 27` (session hard ceiling, advertised separately from the per-generation cap — §5). `StreamingReady` + `video_generation = 3`, `nonce = 4`, `listen_port = 5`.

## 4. Video-socket records (amendments to v5 §3.2; layouts otherwise unchanged)

- **Magic as exact on-wire bytes** (no multi-character integer ambiguity): `VideoHello` begins `52 53 43 56` ("RSCV"), `VideoHelloAck` begins `52 53 43 41` ("RSCA"), each per-frame record begins `56 46` ("VF"). All multi-byte integers little-endian, as before.
- **`VideoHelloAck.status` (u8):** `0 OK · 1 TUPLE_MISMATCH · 2 NONCE_MISMATCH · 3 BUSY · 4 INTERNAL`. Nonzero ⇒ host closes the socket, the generation is consumed, retry per §2 backoff.
- **Reserved/extension rules:** Hello/Ack reserved bytes must be zero on send and are validated zero on receive; their layouts extend only via the version byte. Frame-header bytes between the known layout and `headerLen` are opaque-ignored (forward extension); **unknown `flags` bits set ⇒ protocol error** (unchanged from v5).
- Frame header remains the v5 38-byte layout, including both timestamp fields.

## 5. Credit, ACK, and oversize (corrected)

**Two admission times (new distinction):** `preEncodeAdmissionTs` — when the pump submits a capture to the encoder (encode-stage age accounting); `admissionTs` — when the encoded record is admitted to byte credit and enqueued to the bounded writer (this is the header field and the flow-control age basis). The pump ledger tracks both.

**Writer semantics (corrects v5):** ordinary partial TCP writes are **resumed** by the single bounded writer until the record completes; only EOF or a real write error closes the generation. `sentWireBytes` increments at enqueue-to-writer (unchanged); a generation that closes mid-record discards writer state with the socket — counters never roll forward.

**ACK publication point (corrects v5 §6):** the credit-releasing ACK for a frame is published only after **both** (a) the packet was accepted exactly once, and (b) the post-accept receive drain completed with classified `Again`. A drain `Error` closes the generation **without advancing the ACK**. `accepted_wire_bytes` increments by exactly the frame's `wireBytes` (= `headerLen + payloadLen`), explicitly.

**Oversize policy (re-scoped — v5's per-generation streak reset itself via the ladder's own new-generation step):**
- `oversizeStreak` lives at **session + config** scope and survives generation replacement.
- Streak resets **only** after a generation delivers an under-cap random-access frame that is ACK-accepted.
- Ladder: streak 1 ⇒ new generation with a raised cap (never above the session hard ceiling, `max_wire_frame_bytes_ceiling = 16 MiB`, advertised in ModeConfirm); streak 2 ⇒ bitrate −25% + encoder recreation + new generation; streak 3 ⇒ session failure.
- An access unit exceeding the **hard ceiling** skips cap growth and goes directly to the bitrate-reduction step (or session failure if already reduced).
- An oversize encoder callback is rejected **at the callback boundary**, before the frame can enter the bounded encoded-pending slot.
- `creditWindowBytes ≥ maxWireFrameBytes` is validated by **both** ends after every cap change (`StartVideoGeneration` carries both; a violating offer is a protocol error).

All other v5 §5 invariants (window bound, one encoded-pending frame, memory bound, age bounds, ACK coalescing delay, prefix ledger) stand.

## 6. Decoder identity (tightened)

1. **Token domain:** tokens are `1 ..= i64::MAX − 2³²` *(margin)*, strictly monotonically increasing for the decoder instance's lifetime, assigned as the packet PTS. `0` is never used; `AV_NOPTS_VALUE` (`i64::MIN`) is excluded by construction; tokens continue across generations (table disposal is the fencing mechanism); reaching the margin ⇒ generation-fatal reset *(unreachable in practice; paranoia rule)*.
2. **Output timestamp field:** identity is read from `AVFrame.pts`; if unset, `best_effort_timestamp`; if both are `AV_NOPTS_VALUE` ⇒ unknown-token path.
3. **Unknown/duplicate/reordered tokens:** such an output is **discarded — never presented** — and counted; more than 3 per generation ⇒ `IDENTITY_FAILURE` reset. A duplicate token or a token below the retired prefix is the same class.
4. **`decoded_retired_through_count` (contiguous exclusive prefix):** the largest `T` such that every accepted token `≤ T` has either produced an emitted output or been *proven skipped* (a higher token was emitted after it, meaning the decoder consumed it without output — counted as `decoderSilentSkips` telemetry). Not "highest observed."
5. **A0.0 token gate (hard):** passthrough is validated on the deployed CUVID **and** software builds before A1 relies on identity. Fallback ladder if a path fails: (a) that path may use FIFO correlation **only** under provably reorder-free config (`threads = 1` + low-delay); (b) a path failing both is excluded from use until revisited in B2, decision recorded.

The v5 outcome struct, retry-once-effective (with §5's corrected ACK point), priming bounds, and output watchdog stand.

## 7. Interim UDP path — recovery transition and comparators (complete)

**`AwaitingRA` transition** (exactly once per trigger burst; idempotent while already in `AwaitingRA`):
1. `recoveryEpoch += 1`.
2. Purge: assembler slots, reorder buffer, decode-input queue (in-band barrier), identity table entries of the old epoch; fence the presentation slot.
3. While `AwaitingRA`, dependent (non-RA) frames are rejected + counted.
4. The first frame whose header claims keyframe **and** passes client-side NAL validation (§10: H.264 type 5 + SPS/PPS; HEVC 19/20 + VPS/SPS/PPS) is accepted.
5. That frame seeds `expectedFrameID = frameID + 1` and enters priming.
6. All outputs tagged with the old epoch are fenced (discarded at the render handoff).

**Modular comparators (mathematical, for independent implementations):**
- u32 "a newer than b": `d = (a − b) mod 2³²` (Rust: `a.wrapping_sub(b)`); newer ⇔ `d ≠ 0 ∧ d < 2³¹`.
- u24 (internal cursor snapshot only, §8): `d = (a − b) & 0xFF_FFFF`; newer ⇔ `d ≠ 0 ∧ d < 0x80_0000`.

## 8. Side channels v2 — exact layouts and reliability decisions

**Wire vs internal sequence domains (clarifying v5):** the cursor **wire** sequence remains `u32` (compared with the u32 rule above); the u24 domain exists only inside the client's packed `AtomicU64` snapshot (low 24 bits of the wire seq; at 120 Hz, 2²³ publications ≈ 19 h — the half-range assumption plus per-epoch reset make ambiguity unreachable).

**v2 packet prefix (input & cursor), 10 bytes:** `magic[4] = 52 45 53 43 ("RESC") | version:u8 = 2 | packet_type:u8 | side_channel_epoch:u32 LE`. Receivers validate: magic, version, type, epoch == current, and source IP == control peer (family-matched, IPv6 scope respected). Any failure ⇒ drop + count.

**Input v2 (client→host, UDP, 32 bytes total):** prefix(10) + `seq:u32 | event_type:u8 | x:i32 | y:i32 | reserved:u8 | scroll_dx:i16 | scroll_dy:i16 | modifiers:u32` (22). Under v2, `event_type ∈ {0 move, 3 scroll}` only — **button events are invalid on UDP** (rejected + counted).
- *Moves*: latest-wins by u32 comparator (per epoch).
- *Scroll*: sequence-**deduplicated** (reject `seq` not newer than the last seen scroll seq) — duplicates cannot double-scroll; losses drop deltas (documented, acceptable).
- ***Buttons* (decision):** transition to the **reliable ordered control channel** as `ButtonEvent` (Envelope 41, §3) — epoch fencing alone cannot fix same-epoch UDP loss/reordering, and a delayed mouse-down after mouse-up would stick a button. Keyboard (`KeyEvent`, 40) is already control-channel; buttons join it. Latency note: one small TCP_NODELAY message per click edge is well within budget.

**Cursor v2 (host→client, UDP, 39 bytes):** prefix(10) + the existing 29-byte `CursorUpdate` body unchanged; per-epoch `lastSeq` reset on epoch change (fixes the verified restart-rejection bug).

Ports remain per-negotiation (ModeConfirm), sockets closed/rebound per epoch, as frozen in v5.

## 9. Clock contract (exact arithmetic)

- All cross-machine differences are computed in **widened signed arithmetic (i128 intermediates)**; the stored offset is signed i64 µs with u32 µs uncertainty; range-checked before use.
- Sample acceptance: `delay = (t4 − t1) − (t3 − t2)` computed signed; **reject `delay < 0`** (clock misbehavior) and `delay ≥ 5 ms` *(init)*; retain the minimum-delay sample; `uncertainty = delay / 2`.
- `CMSyncConvertTime` target is **`CMClockGetHostTimeClock()`**, explicitly.
- Absolute↔continuous calibration is **bracketed**, not "atomic": read `continuous t_c1 → absolute t_a → continuous t_c2`; require `t_c2 − t_c1 < 50 µs` *(init)* else retry; mapping anchors `t_a ↔ (t_c1 + t_c2)/2` with uncertainty `(t_c2 − t_c1)/2`.
- Recalibrate and invalidate: on wake (mandatory), on reconnect, and every 60 s *(init)* as drift guard. SCK nil-clock fallback unchanged from v5.

## 10. Codec negotiation — truthful confirmation

**Order (corrects v5):** (1) intersect `CodecCapability` mode tuples with host encoder capability and the requested mode; (2) **create** the VideoToolbox session for the candidate; (3) set **and read back** every load-bearing property; (4) `VTCompressionSessionPrepareToEncodeFrames`; (5) only then send `ModeConfirm`. A failure at 2–4 tries the next candidate; none left ⇒ `ModeReject(UNSUPPORTED_MODE)`. `ModeConfirm` therefore never confirms a mode the host has not actually constructed. (Runtime encode failures after confirm remain generation/session recovery — that path is unchanged.)

**Capability representation (corrects v5):** `repeated SupportedMode` tuples — each `{max_width, max_height, max_fps, profile, level, bit_depth}` is jointly valid — replacing independent maxima (which falsely imply, e.g., 4K60 from 4K30 + 1080p60). The client generates tuples by probing its decoders at the specific mode points it cares about (its panel mode + common fallbacks).

Random-access verification (NAL contract) is unchanged from v5 §10, including CRA rejection and flag-vs-payload agreement.

---

## 11. Phase impacts (skeleton unchanged; review-5 go/hold boundary adopted)

- **A0.0 (GO now):** unchanged from v5 — plus the §9 bracketed-calibration procedure and the §6.5 token-gate framing (its pass/fail now formally gates B0 entry, with the FIFO fallback ladder as the recorded alternative).
- **A0 (GO after A0.0 demo):** unchanged.
- **A1 (GO on this document):** unchanged from v5, with three deltas: the UDP `AwaitingRA` transition implements §7 exactly; scroll dedup + button-path-to-control land here (client emits `ButtonEvent`; host injects from the control channel; UDP button types rejected); the pv1 wire remains untouched otherwise.
- **B0 (HOLD until token gate + this checklist):** implements §§2–5, 8–10 as frozen here. **B0-entry checklist:** A0.0 token gate concluded (either path validated or fallback recorded); A0 baseline recorded; this document's §§2–10 unchanged for ≥ one review cycle.
- **B1/B2, C, D, E:** unchanged from v5/v4, inheriting the corrected ACK point, oversize scoping, and state-machine edges.

## 12. New validation gates (v6 = v5 gates + these)

| Gate | Required result | Phase |
|---|---|---|
| Idempotent reset | Delayed `VideoResetRequest(K)` after K+1 allocated ⇒ ignored; simultaneous socket-failure + reset ⇒ exactly one allocation | B0 |
| Retry budget | 8 consecutive failed generations ⇒ `SessionFailed`, no infinite loop; one streamed+ACKed frame resets the counter | B0/B2 |
| Stale handshake | Stale/duplicate `StartVideoGeneration`, `StreamingReady`, `VideoHello`, `VideoHelloAck` each handled per §2 (idempotent resend re-acked; nonce-mismatch fatal) | B0 |
| Transport default | An Envelope omitting `video_transport_*` never silently selects UDP (`UNSPECIFIED` handled); pv2 rejects UDP_V1-only peers | B0 |
| Legacy matrix | `StartStreaming`/`RequestIDR` under pv2 ⇒ rejected + counted, never acted on | B0 |
| Session equality | Payload/Envelope `session_id` mismatch ⇒ dropped + counted; 3 ⇒ control reset | B0 |
| ACK-after-drain | Drain `Error` after exact-once accept ⇒ generation closes with **no** ACK advance; drain `Again` ⇒ ACK includes exactly `wireBytes` | B2 |
| Partial write | Interrupted short writes resume to completion; only EOF/error closes the generation | B0/B2 |
| Oversize streak | Oversize → new generation → oversize again is streak 2 (not a fresh streak); under-cap ACKed RA frame resets it; > ceiling skips cap growth | B2 |
| Window inequality | An offer with `credit_window_bytes < max_wire_frame_bytes` is rejected as protocol error | B0 |
| Token domain | Tokens stay within the signed range; `AV_NOPTS`/duplicate/reordered outputs are discarded (never presented) and counted; 4th ⇒ identity reset | A1/B2 |
| Retired prefix | `decoded_retired_through_count` advances only over emitted-or-proven-skipped prefixes; silent skips counted | B2 |
| UDP reseed | Missing expected frame ⇒ one epoch increment, full purge, dependent rejection, validated RA reseed, old-epoch output fenced | A1 |
| Comparator edges | u32 and u24 rules: equal ⇒ not newer; forward, wrap-forward, stale, half-range boundary all per §7 | A1 |
| Stuck button | Drop/delay/duplicate injection on input paths cannot leave a host button stuck (buttons ride control; UDP button types rejected) | A1/B1 |
| Scroll dedup | Duplicated scroll datagrams never double-scroll; losses only shorten the scroll | A1 |
| Encoder-before-confirm | Induced session-creation/property failure ⇒ next candidate or `ModeReject`; `ModeConfirm` is never sent for an unconstructed mode | B0 |
| Clock signs | Negative cross-machine differences and injected negative delay are handled (rejected) without underflow; i128 intermediates verified | A0.0 |
| Bracketed calibration | Calibration rejects wide brackets and records midpoint+uncertainty; wake invalidates | A0.0 |

## 13. Decision log (delta on v5 §14)

| Decision | Choice | Why |
|---|---|---|
| `configGeneration` wire form | Reuse `config_id`, incremented per ModeConfirm | No parallel field to drift; existing UDP/proto plumbing already threads it |
| v2 transport | TCP_V2 only; UDP exists only under pv1 | Halves the v2 state space; interim UDP is scaffolding, not a product mode |
| Buttons | Reliable control channel (`ButtonEvent`) | Same-epoch UDP loss/reorder can stick a button; epochs can't fix that |
| Scroll | UDP + sequence dedup | Duplicates are harmful (double-scroll), losses benign (shorter scroll) |
| Presented telemetry | `last_presented_frame_id_plus_one` (0 = none) | Frame-0 ambiguity, same fix as the credit counters |
| Oversize scope | Session+config streak; ceiling advertised separately | v5's per-generation counter was self-resetting via the ladder itself |
| ACK point | After exact-once accept **and** drain-to-`Again` | An ACK before a failed drain would credit a frame the decoder never finished |
| Token domain | `1..=i64::MAX−2³²`, `AVFrame.pts` → `best_effort_timestamp` fallback | ffmpeg PTS is signed; `AV_NOPTS` excluded by construction |
| Unknown-token output | Discard, never present | Presenting unidentifiable frames breaks priming and telemetry truthfulness |
| Capability shape | Joint `SupportedMode` tuples | Independent maxima imply unsupported combinations (4K30+1080p60 ⇏ 4K60) |
| Encoder timing | Construct + validate before `ModeConfirm` | Confirm must be truthful; no post-confirm init-failure state needed |
| Clock math | i128 intermediates, signed offset, `delay ≥ 0` required | Unsigned cross-machine subtraction underflows |
| Calibration | Bracketed continuous/absolute/continuous | An atomic dual-clock read does not exist; midpoint+uncertainty is honest |

## 14. Review-5 checklist → resolution map

| Checklist item | Resolved in |
|---|---|
| Assign every new protobuf field/enum | §3 |
| `configGeneration` vs `config_id` | §3 (reuse decision) |
| Unspecified transport + v2 matrix | §3 |
| Stale/duplicate/concurrent reset + retry transitions | §2 |
| Exact input/cursor v2 layouts | §8 |
| Buttons reliable/idempotent | §8 (control channel) |
| UDP RA reseed + modular comparator | §7 |
| Oversize counter across generations | §5 |
| `creditWindowBytes ≥ maxWireFrameBytes` | §3 + §5 |
| ACK after successful drain | §5 |
| Tokens in signed PTS domain | §6 |
| Encoder creation before `ModeConfirm` | §10 |
| Signed clock arithmetic + exact CM target | §9 |

## 15. Errata against v5 (v5 left unmodified)

(a) §3.1 assigned oneof tags but not message bodies — bodies frozen here; (b) `UDP_V1 = 0` proto3 default trap — enum renumbered with `UNSPECIFIED = 0`; (c) "ACKed after step 4" contradicted step 5 — ACK now explicitly after drain-`Again`; (d) "partial or failed write closes the generation" — partial writes are resumable, only EOF/error closes; (e) per-generation oversize streak self-reset — re-scoped to session+config; (f) u64 tokens vs ffmpeg signed PTS — constrained to the signed domain; (g) `decodedRetiredThroughCount` under-defined — contiguous exclusive prefix with proven-skip rule; (h) cursor comparison text conflated the u32 wire sequence with the internal u24 snapshot — both domains and their reduction now explicit; (i) `ModeConfirm` preceded encoder construction — order inverted; (j) unsigned four-timestamp arithmetic could underflow — i128 + signedness rules; (k) "atomically captured calibration pair" — replaced by the bracketed procedure; (l) independent capability maxima — replaced by joint mode tuples; (m) same-epoch UDP button loss unaddressed — buttons moved to control; (n) `last_presented_frame_id` had the frame-0 ambiguity — plus-one encoding.
