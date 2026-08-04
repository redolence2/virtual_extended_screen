# RESC Implementation Plan v5 — Contract Freeze

| | |
|---|---|
| **Date** | 2026-07-29 |
| **Supersedes** | `IMPLEMENTATION_PLAN_V4.md` (plan v4). v4's architecture, findings register (G1–G8, D1–D4, N1, H1–H3, C1, T1–T2, P1–P3), phase skeleton, and rationale stand except where amended here; prior documents remain unmodified. |
| **Incorporates** | `IMPLEMENTATION_PLAN_review_v4.md` (review 4) — verdict was **no-go on freezing v4's §3/§4**; this document is the demanded paper amendment. All nine mandatory amendments (V4-1…V4-9) and the smaller corrections are adopted; dispositions in §15. |
| **RESC commit** | `12b87d1` |
| **New verifications this revision** | Input staleness applies to mouse moves only, no source/session validation (`InputReceiver.swift:58-99`); keyframe flag defaults to *true* with absent attachments and no NAL parsing exists (`NALUPackager.swift:83-90`); SDL is initialized in both `main.rs:296` and `Renderer::new`; `generate_proto.sh` accepts any preinstalled `protoc-gen-swift` without a version check; cursor-stream restart is rejected by the client's wrap guard (`main.rs:174-182` vs `CursorTracker` seq restarting at 0). |
| **Freeze status** | §2–§10 are the frozen contracts (state machine, wire, credit, decoder, UDP interim, side channels, clocks, codec/RA). Implementation may begin at Phase A0.0. Numeric parameters marked *(init)* are initial values finalized from A0 data — the *structures* are frozen, the *numbers* are tuned. |

---

## 1. Corrections to v4 accepted in this freeze (summary)

1. **Generation/reset authority was not a state machine** (V4-1) → §2: single-writer generation offer/handshake; a fresh socket is the ordering barrier; failed attempts consume generations; session rule decided.
2. **Same-generation TCP recovery could deadlock behind deliberately-unACKed bytes** (V4-2) → §4: on TCP, *every* decoder discontinuity is generation-fatal; in-generation IDR requests exist only on the interim UDP path. Full host-flow invariant set + prefix ledger.
3. **Schema gaps** (V4-3) → §3: exclusive u64 counters (frame 0 vs no-progress distinguishable), presentation as telemetry only (latest-wins render drops are legal and never touch credit), one byte domain (`wireBytes = headerLen + payloadLen`), `streamID` in every progress message, `admissionTs` added to the frame header (38 bytes), all field numbers and fixed layouts assigned.
4. **"4 MiB covers any legal 4K IDR" was false** (V4-4) → §5: `DataRateLimits` is a decode-window constraint (10–12.5 MB/0.1 s at current settings), not a per-AU bound; the cap is now a **provisional application cap** with property validation at session creation (moved out of Phase C) and a non-looping oversize recovery ladder.
5. **Decoder identity/EAGAIN scheduled after phases that need it** (V4-5) → §6 + §11: minimum structured decoder API, token identity, retry, reset, table lifecycle move into **A1**; B2 only attaches credit/progress. Output watchdog added (input-credit alone lets a silent decoder leak).
6. **UDP barrier over-triggered and couldn't purge** (V4-6) → §7: hold/release/discard rules (benign duplicates never reset), local `recoveryEpoch`, one actor owning a purgeable decode-input queue.
7. **Side channels unfenced across reconnect** (V4-7, both bugs verified) → §8: epoch-fenced input/cursor at the v2 bump, source validation, fresh sockets per epoch.
8. **A0.0 was not implementable as written** (V4-8) → §9 + §11: v1 wire untouched (frameID is the baseline identity; `captureSeq` only enters the v2 header), four-timestamp clock sync with a narrow typed handler enabled in A0.0, explicit absolute/continuous clock bridge with nil-clock fallback and sleep invalidation.
9. **Codec negotiation too coarse; encoder pre-dates negotiation; random-access unverified** (V4-9, NALU default verified) → §10: capability schema, encoder created only after ModeConfirm, compressed-side RA contract with exact NAL types.
10. Cursor u24 comparison off-by-one fixed; render actor owns exactly one SDL context; codegen toolchain verified, not assumed (§11, §13).

---

## 2. Generation & connection state machine (frozen)

**Writers.** `videoGeneration` has exactly one writer: the host. The client never proposes generation numbers. One generation is allocated **per connection attempt**; a failed dial/handshake consumes it; there is no "reset in place" (rejected: it would need an in-band video-socket reset record + ack for cross-connection ordering — a fresh socket *is* the ordering barrier).

**Session rule (decided).** `sessionID` is **control-connection-scoped**: every control reconnect runs a fresh `ModeRequest`/`ModeConfirm` and allocates a new `{sessionID, streamID, configGeneration}` tuple. Resume/proof-of-prior-session is an explicit non-goal (single-user LAN; renegotiation is sub-second). What persists across disconnects is the *virtual display* (windows stay put, per the existing grace design) — not protocol state.

**Handshake** (initial connection starts at step 2; client-triggered recovery starts at step 1):

1. client → control: `VideoResetRequest{sessionID, streamID, configGeneration, observedVideoGeneration, reason}` — carries **no** new generation.
2. host: freezes the pump, closes the old video socket (if any), discards old-generation encoder callbacks (I5), allocates exactly **one** new `videoGeneration` for the next attempt.
3. host → control: `StartVideoGeneration{sessionID, streamID, configGeneration, videoGeneration, nonce[16], maxWireFrameBytes, creditWindowBytes}` — the nonce is host-generated and **precommitted here**, so the video socket can be bound to control-plane state.
4. client: binds a fresh ephemeral listener → control: `StreamingReady{sessionID, streamID, configGeneration, videoGeneration, nonce, listenPort}` (echoes the committed tuple).
5. host: dials `(control-connection remote address, listenPort)` → video socket: fixed-layout `VideoHello` echoing the tuple + nonce.
6. client: validates against the precommitted values → `VideoHelloAck`; any mismatch ⇒ close, counted.
7. host sends **nothing** on the socket until it holds a verified random-access access unit (§10) for this generation (`awaitingFirstIDR`).

Timeouts at steps 4–6 (init: 2 s each) consume the generation and return to step 2. Cross-connection ordering: because every generation has its own socket and frames carry `videoGeneration`, a delayed control message can never cause a new-generation frame to enter old decoder state (gate §12).

## 3. Wire & progress schema (frozen)

### 3.1 Control protobuf (proto3, `protocol_version = 2`)

New `Envelope` oneof entries (numbers frozen; all messages carry the full tuple meaningful in their state — the v4 "validate IDs where defined in the current state" rule stands):

| Field # | Message | Dir | Payload |
|---|---|---|---|
| 33 | `ClockPing` | either | `t1_us:u64` (sender monotonic) |
| 34 | `ClockPong` | either | `t1_us, t2_us (recv), t3_us (send)` — four-timestamp with requester's local `t4` (§9) |
| 35 | `DecoderProgress` | client→host | `session_id, stream_id, config_generation, video_generation, accepted_frame_count:u64, accepted_wire_bytes:u64, decoded_retired_through_count:u64, last_presented_frame_id:u32, render_intentional_drops:u64` |
| 36 | `VideoResetRequest` | client→host | `session_id, stream_id, config_generation, observed_video_generation, reason` |
| 37 | `StartVideoGeneration` | host→client | `session_id, stream_id, config_generation, video_generation, nonce:bytes[16], max_wire_frame_bytes:u32, credit_window_bytes:u32` |

Amended existing messages: `ModeRequest` + `video_transport_supported = 5 (repeated enum {UDP_V1=0, TCP_V2=1})`, `codec_capabilities = 6 (repeated CodecCapability`, §10`)`; `ModeConfirm` + `video_transport_selected = 23`, `max_wire_frame_bytes = 24`, `credit_window_bytes = 25`, `side_channel_epoch = 26 (u32)`; `StreamingReady` + `video_generation = 3`, `nonce = 4 (bytes)`, `listen_port = 5`.

**Progress semantics.** All progress counters are **exclusive counts** (0 = "none yet"), eliminating the proto3 zero-default ambiguity with `frameID` starting at 0. `accepted_frame_count`/`accepted_wire_bytes` are the **credit cursors** and must jointly name one prefix boundary of the host ledger (§5). `decoded_retired_through_count` feeds the output watchdog (§6). Presentation fields are **telemetry only, never credit**: Phase D's latest-wins handoff makes intentional never-presented decoded frames legal, so no contiguous presentation watermark exists by design.

### 3.2 Video socket records (little-endian, fixed layout)

`VideoHello` (44 bytes): `magic:u32 'RSCV' | version:u8=2 | reserved:u8 | headerLen:u16=44 | sessionID:u64 | streamID:u32 | configGeneration:u32 | videoGeneration:u32 | nonce[16]`.
`VideoHelloAck` (28 bytes): `magic:u32 'RSCA' | version:u8=2 | status:u8 | reserved:u16 | videoGeneration:u32 | nonce[16]`.

Per-frame header (**38 bytes**; `admissionTs` added per V4-3, resolving v4's header/side-table contradiction):

| Off | Field | Notes |
|---|---|---|
| 0 | `magic:u16` `0x5646` | |
| 2 | `headerLen:u8` | min 38, max 64; payload begins at `headerLen` (forward-compatible extension) |
| 3 | `flags:u8` | bit0 keyframe-claim (validated vs payload, §10) · bit1 replay · **unknown bits set ⇒ protocol error ⇒ close generation** |
| 4 | `codec:u8` | 0=H.264, 1=HEVC — must equal the negotiated codec or close |
| 5 | `reserved:u8` = 0 | |
| 6 | `videoGeneration:u32` | must equal the socket's generation or close |
| 10 | `frameID:u32` | starts 0 per generation, +1 per AU; wrap-safe compare |
| 14 | `captureSeq:u32` | |
| 18 | `contentCaptureTs_us:u64` | original content time (telemetry; replays keep it) |
| 26 | `admissionTs_us:u64` | flow-control admission time (age bounds; replays get fresh) |
| 34 | `payloadLen:u32` | `headerLen + payloadLen = wireBytes(frame) ≤ maxWireFrameBytes` or close |

**Payload contract:** exactly one Annex-B access unit per record, of the negotiated codec. A generation's first AU must be random-access with in-band parameter sets (§10). Malformed/oversize records **fail closed** (generation reset) — no resync scanning.

## 4. Recovery model (frozen; replaces v4's same-generation TCP recovery)

**TCP: every decoder discontinuity is generation-fatal.** Decoder error, priming timeout, output-watchdog trip, identity failure, config change, oversize, malformed record, or control loss ⇒ stop accepting old-generation input, dispose the identity table, flush-or-recreate the decoder, close the socket, and run §2 recovery (steps 1–7). Rationale (V4-2, verified flaw in v4): frames deliberately discarded in `WaitingForIDR` are never ACKed, so their bytes permanently occupy the credit window — a same-generation recovery IDR can be unadmittable. New generation ⇒ new socket ⇒ fresh window; the deadlock is structurally impossible. No emergency-credit class exists (rejected: second credit class, more failure states, no benefit).

Consequences: on the TCP path there is **no in-generation IDR request message at all**; `wantIDR` (the idempotent latch, v4 §4) survives only on the interim UDP path scoped by `recoveryEpoch` (§7).

## 5. Credit, ledger, and frame-size policy (frozen)

**Byte domain:** `wireBytes(frame) = actual headerLen + payloadLen`. `sentWireBytes` increments when a credit-admitted record is enqueued to the **single bounded writer**; a partial or failed socket write closes the generation; counters never roll into the next generation.

**Host-flow invariants (all enforced, all telemetered):**
- `sentWireBytes − acceptedWireBytes ≤ creditWindowBytes`;
- `encodedPendingWireBytes ≤ maxWireFrameBytes` and at most **one** encoded-pending frame (admission policy 2, unchanged);
- no second encode starts while an encoded-pending frame exists (with `encodeInFlight ≤ 1` this bounds host-owned encoded memory by `creditWindowBytes + maxWireFrameBytes`);
- age bounds cover encode-in-flight, encoded-pending, writer-queued, and sent-unaccepted frames (all via `admissionTs`; init 250 ms) ⇒ violation is generation-fatal;
- client progress ACKs have a maximum coalescing delay (init: every 16 ms or every accepted frame, whichever first).

**Prefix ledger.** The host retains `(frameID, wireBytes, admissionTs)` for every sent-unaccepted frame. An ACK is valid iff `(accepted_frame_count, accepted_wire_bytes)` names an exact prefix boundary of that ledger (monotonic `old ≤ ack ≤ sent` alone is insufficient — V4-3); mismatched-but-plausible cursors are rejected without releasing credit, counted.

**Frame-size policy (V4-4).** `maxWireFrameBytes` is a **provisional application cap** — *not* derived from `DataRateLimits`, which the SDK defines as compressed bytes over a contiguous decode-time window (10,000,000–12,500,000 B/0.1 s at current 40/50 Mbps settings) and which proves no per-AU bound. Init: **8 MiB**, `creditWindowBytes` init `maxWireFrameBytes + 1 MiB`; finalized from A0 IDR histograms + safety margin. Every encoder property set is **validated at session creation** (moved out of Phase C; failures logged and, for load-bearing properties, session-fatal). Sender enforces the cap at admission. **Oversize recovery ladder** (each step changes something; gate: no reset loop): 1st occurrence in a generation ⇒ new generation with a renegotiated larger cap (hard ceiling 16 MiB); 2nd ⇒ bitrate −25% + encoder recreation + new generation; 3rd ⇒ session failure with diagnostics.

## 6. Decoder contract v2 (frozen; implemented in A1, credit attached in B2)

**Identity by token (replaces v4's pts bit-packing).** A decoder-local monotonically increasing **u64 token** is set as packet PTS; a side table maps token → `{videoGeneration | recoveryEpoch, frameID, captureSeq, contentCaptureTs, admissionTs}`. Outputs recover identity via emitted `pts`; the mechanism is validated on **both** deployed CUVID and software paths in A0.0 (ffmpeg-next is pinned only to major 7 — passthrough is a hypothesis until tested). `AV_NOPTS_VALUE`/unknown token on output ⇒ counted; more than *(init)* 3 per generation ⇒ identity broken ⇒ generation-fatal. Table entries are removed on output, reset, error, and generation disposal; bound: `maxOutstandingIdentityEntries = maxDecoderLagFrames + 4`.

**Structured outcome** (unchanged from v4 in shape): `DecodeOutcome{inputAcceptedExactlyOnce, acceptedFrameID, acceptedBytes, outputs[], drainState: Again|EndOfStream|Error, recoveryTransition}` — EAGAIN, EOF, and errors are distinct; only `Again` is normal drain completion.

**Retry-once-effective (exact):** (1) retain the exact packet; (2) on send-side EAGAIN, drain receive side; (3) require drain progress or fail the generation; (4) retry the same packet until accepted exactly once; (5) drain to classified `Again`. The frame is ACKed exactly once, after step 4.

**`PrimingAfterIDR` bounds:** entered on verified-RA input acceptance; feeds subsequent in-order AUs; suppresses presentation until the output mapped (by token) to the RA unit or later appears; `maxPrimingFrames` *(init 30)* / `maxPrimingDuration` *(init 500 ms)*; timeout ⇒ generation-fatal (TCP) / epoch reset (UDP). The IDR latch clears only when a **verified** RA unit is accepted into priming — never on a header flag alone (the flag defaults to true on absent attachments today; §10 fixes the source, the client still validates).

**Output watchdog (new, V4-5):** input acceptance remains the credit boundary, but a decoder that accepts input while emitting nothing would silently release credit and grow age; enforce `accepted_frame_count − decoded_retired_through_count ≤ maxDecoderLagFrames` *(init 12)* **and** oldest-outstanding-accepted age ≤ *(init)* 250 ms ⇒ violation is generation-fatal. This does not move the credit boundary to output.

## 7. Interim UDP path (until B2 deletes it): reorder gate + recovery epoch

Corrected trigger rules (V4-6 — benign duplicates must not cause IDR storms):
- expected frame completes ⇒ release it, then drain consecutive held frames;
- ahead-of-expected completion within the window (init 8 frames / 50 ms) ⇒ hold;
- behind-expected duplicate/late completion ⇒ **discard + count, no discontinuity**;
- missing expected frame at deadline, conflicting duplicate, slot eviction, or decode-queue admission failure ⇒ exactly **one** `Discontinuity` per `recoveryEpoch`.

UDP v1 has no `videoGeneration`; a client-local `recoveryEpoch:u32` fences held frames, the identity table, and the `wantIDR` latch. **Queue ownership:** one decode-input actor owns the reorder buffer *and* the linearized `Frame | Discontinuity` queue (replacing `sync_channel(4)`), so it can purge held+queued dependents and place the barrier even when the queue is full; the assembler surfaces reason-coded events (timeout/eviction) to that actor instead of bare counters.

## 8. Side-channel fencing (input + cursor; verified gaps closed at the v2 bump)

Verified today: input staleness applies only to mouse moves (buttons/scroll bypass; a pre-disconnect mouse-down can re-press after `releaseAll()`); no source or session validation on an INADDR_ANY socket; cursor tracker restarts `seq` at 0 while the client's wrap guard rejects the restarted stream (`seq 1` vs old high seq fails both branches).

Frozen fixes: the v2 packet prefix for input and cursor adds `sideChannelEpoch:u32` (allocated in `ModeConfirm` per negotiation); receivers validate (a) epoch, (b) source address == the control connection's remote address. On lifecycle change: close old sockets (drains kernel queues), bind fresh ports, reset all sequence state, then enable the new epoch. Buttons/scroll are protected by epoch (never by sequence ordering — they must not be reorder-dropped); cursor sequences restart naturally per epoch. Cursor comparison fix (review "smaller"): `delta = (new − old) & 0xFF_FFFF; newer ⇔ 0 < delta < 0x80_0000`; documented half-range assumption: a consumer must not miss more than 2²³−1 publications without an epoch reset.

## 9. Clock contract v2 (frozen)

Four-timestamp sync (V4-8): requester `t1` → responder receive `t2` → responder send `t3` → requester receive `t4`. `offset = ((t2−t1)+(t3−t4))/2` (sign: responder−requester), `delay = (t4−t1)−(t3−t2)`; accept iff `delay <` *(init)* 5 ms; retain the min-delay sample; `uncertainty = delay/2`; resample every *(init)* 10 s and immediately after reconnect and wake; sleep/wake **invalidates** the current offset and the host bridge until recalibrated. Host bridge: latency timestamps use a continuous monotonic epoch; CoreMedia host time (mach_absolute, halts in sleep) is bridged via an atomically captured (absolute, continuous) calibration pair; SCK PTS is mapped with `CMSyncConvertTime` against `SCStream.synchronizationClock` **which is nullable** — nil ⇒ fall back to callback timestamps, explicitly labeled in traces. No unrelated-timebase subtraction anywhere (T1).

## 10. Codec negotiation, encoder lifecycle, and random-access verification (frozen)

**Capability schema:** `CodecCapability{codec, max_profile_idc, max_level_idc, bit_depth, max_width, max_height, max_fps, hardware:bool}` (repeated, client→host in `ModeRequest`; probed at client startup by actually opening decoders). Host intersects with its encoder capabilities and the requested mode; `ModeConfirm` fixes `{codec, profile, level}` under the new `configGeneration`, or `ModeReject(UNSUPPORTED_MODE)`.

**Encoder lifecycle:** the encoder is created **only after ModeConfirm**, scoped to the config generation (today it is built and started in `main.swift` before any negotiation — that startup order is restructured in B0/B1; the A1 interim keeps the current order but the negotiated codec must match the CLI-selected one or reject). Client decoder-init failure ⇒ renegotiation under a new config generation or session failure — never a codec switch inside a generation (the current mid-stream "H.264 fallback" is deleted in A1).

**Random-access contract** ("verified on output" = **encoder-output access unit**, never a decoded frame): the host parses NAL unit types of each output AU — H.264: RA iff an IDR slice (type 5) is present; generation-first AU must carry SPS(7)+PPS(8). HEVC: RA iff IDR_W_RADL(19)/IDR_N_LP(20); **CRA(21) is not accepted** as generation-first (leading-picture complexity; recorded decision); generation-first must carry VPS(32)+SPS(33)+PPS(34). The header keyframe flag must agree with the parse — today's flag defaults to *true* when attachments are absent and parses nothing (verified); that derivation is replaced. A forced-KF output that is not actually RA ⇒ one re-force, then generation failure. The client independently validates flag vs payload; disagreement ⇒ protocol error ⇒ reset.

---

## 11. Phase amendments (skeleton unchanged: A0.0 → A0 → A1 → B0 → B1 → B2 → C → D → E)

**A0.0** — (a) Swift protobuf target wired; toolchain **verified**: rebuild `protoc-gen-swift` from the pinned version matching the resolved runtime and make CI regeneration run from a clean pinned toolchain (the script currently accepts any preinstalled plugin unchecked); (b) enable a **narrow typed handler for ClockPing/Pong and trace export only** (the 0xFA legacy path remains untouched until A1 — "additive" now means additive *messages with a scoped typed dispatcher*, resolving v4's contradiction); (c) clock bridges + sleep invalidation per §9; (d) **v1 wire untouched**: baseline identity = existing `frameID` + a host trace mapping `frameID → {captureSeq, contentCaptureTs}`; `captureSeq` first appears on the wire in the v2 header (appending to the v1 header would shift the payload offset and corrupt Annex-B for unchanged parsers); (e) decoder **token-passthrough experiments** on deployed CUVID and software builds; (f) fake-encoder (delayed completions), scripted-decoder, and protocol-state test harnesses.

**A0** — unchanged from v4, plus: run only after trace joining and clock uncertainty are demonstrated; software vs optical measurements labeled separately; soak wall-time budgeted separately from engineering effort.

**A1** — v4's list, plus the **minimum decoder contract** moved in from B2 (structured outcome, token identity, retry-once-effective, decoder reset + table lifecycle — priming's A1 gate is unimplementable without them), the corrected UDP gate rules + `recoveryEpoch` + purgeable-queue actor (§7), reason-coded assembler events, and codec **capability intersection controlling accept/reject** (full encoder-lifecycle restructure lands with B0/B1).

**B0** — implements §2 handshake, §3 schemas, §8 fencing, the bounded writer/reader, the prefix ledger, provisional cap enforcement + property validation at session creation, and one accepted frame + ACK end-to-end under fixed memory bounds. Still a non-deployable, credit-bounded lab branch. **Re-estimation checkpoint (commitment).**

**B1/B2** — as v4 (pump, replay, lifecycle, cumulative progress, fault suite, UDP deletion) with the §4 change: any TCP decoder discontinuity allocates a new generation; plus the output watchdog and oversize ladder gates.

**C/D** — as v4, with: encoder property-validation already done (moved to A0.0/B0); Phase D names the mechanism — one render actor owns exactly one SDL context/video subsystem/event pump/window/canvas and every texture (today SDL is initialized in both `main.rs` and `Renderer::new` — collapsed to one owner), NV12 via `SDL_UpdateNVTexture` where available with a tested lock/update fallback, else I420.

## 12. Additional validation gates (v5 = v4's table + these)

| Gate | Required result | Phase |
|---|---|---|
| Generation allocation | Simultaneous reset + reconnect + failed dial ⇒ exactly one authoritative generation per attempt, never a double increment | B0 |
| Nonce binding | A `VideoHello` whose nonce was not precommitted via `StartVideoGeneration` is rejected | B0 |
| Cross-connection ordering | Delayed control/reset messages can never place a new-generation frame into old decoder state | B0 |
| Frame-zero progress | "No progress" vs "frame 0 accepted" distinguishable (exclusive counters) | B0 |
| ACK prefix | Valid-looking but mismatched (count, bytes) cursors are rejected without credit release | B2 |
| Credit recovery | Decoder fault with the window fully consumed by unACKed frames ⇒ new generation established and its first RA frame displays | B2 |
| Output watchdog | Decoder accepts input, emits nothing ⇒ bounded identity memory/age, generation resets | B2 |
| Token identity | Induced CUVID and SW output delay preserves identity; `AV_NOPTS`/unknown token fails through the defined path | A0.0/A1 |
| Priming timeout | Recovery RA unit never emerges ⇒ frame/time bounds terminate priming cleanly (no hang, no partial presentation) | A1/B2 |
| Random access | Header flag, parsed NAL type, and parameter-set presence must agree, both codecs; non-RA forced output ⇒ one re-force then fail | A1/B0 |
| Repeated oversize | Ladder renegotiates/reconfigures or terminates — never a reset loop | B2 |
| UDP duplicate | Late/duplicate old frames: counted + discarded, **no** second discontinuity | A1 |
| UDP full queue | A full decode-input queue can still purge stale dependents and linearize exactly one barrier | A1 |
| Stale input | Old-epoch mouse/button/scroll datagrams cannot act after disconnect cleanup (incl. the verified stale-mouse-down case) | B1 |
| Stale cursor | Old-epoch cursor packets and restarted sequences cannot corrupt the new epoch (incl. the verified restart-rejection bug) | B1 |
| Clock processing delay | Injected responder delay removed by t1–t4 math, reflected in uncertainty | A0.0 |
| Suspend clock | Sleep/wake invalidates and recalibrates the bridge before any cross-machine comparison | A0.0 |
| Codec/config matrix | Capability matrix incl. profile/level/bit-depth; encoder creation matches the confirmed config; decoder-init failure renegotiates, never switches codec in-generation | A1/B0 |
| Presentation gaps | Intentional latest-wins render drops neither stall credit nor claim contiguous presentation | D |
| Cursor wrap | Equal, forward, wrap-forward, stale, and half-range-ambiguous u24 sequences behave exactly as §8 specifies | A1 |

## 13. Effort (initial; re-estimate committed at B0 exit)

A0.0: **2–3 d** (codegen+toolchain 0.5–1 · clock bridges + sync 0.5–1 · identity/trace + three harnesses 1–1.5) · A0: 0.5–1 d engineering + soak/optical wall time separate · A1: 2.5–3.5 d (decoder minimum moved in) · B0: 1.5–2 d · B1: 1.5–2.5 d · B2: 2–3 d (fault-suite development included explicitly) · C: 0.5–1 d · D: 2–3 d · E: open. These remain best-case implementation figures; the B0 re-estimation checkpoint is where they become commitments.

## 14. Decision log (delta on v4 §10)

| Decision | Choice | Why |
|---|---|---|
| Session persistence | Control-connection-scoped; new tuple per reconnect; resume = non-goal | Removes unimplemented grace-persistence prose; display persistence ≠ protocol persistence |
| Reset shape | Fresh socket per generation; no in-place reset; failed attempt consumes a generation | Socket = ordering barrier; halves the state machine |
| TCP recovery | Generation-fatal discontinuities; no in-generation IDR message on TCP | Kills the WaitingForIDR credit deadlock (V4-2) without an emergency-credit class |
| Progress encoding | Exclusive u64 counters + prefix ledger | Frame-0 ambiguity; ACK forgery/corruption safety |
| Header | 38 bytes incl. `admissionTs_us` | Resolves v4's header/side-table contradiction; enables output/present age telemetry |
| Frame cap | Provisional 8 MiB (ceiling 16 MiB), A0-finalized; oversize ladder | 4 MiB "derived" claim was false (V4-4); ladder guarantees progress |
| Decoder identity | Local u64 token + side table (not pts bit-packing) | No generation truncation; works for UDP epochs; passthrough testable |
| Output lag | Watchdog `accepted − retired ≤ 12` frames / 250 ms, generation-fatal | Input-credit alone lets a silent decoder leak credit and age |
| CRA | Not accepted as generation-first RA | Leading-picture complexity; IDR-only keeps priming simple |
| Side channels | Epoch + source validation + fresh ports per negotiation | Two verified stale-input/cursor bugs (§8) |
| Baseline identity | v1 `frameID` + host trace map; no v1 header change | Appending to the fixed v1 header corrupts payload offsets (V4-8) |
| Clock sync | Four-timestamp NTP form | Two-timestamp form cannot remove responder processing delay |

## 15. Traceability (review 4 → this document)

V4-1 → §2, §3.1 (36/37, StreamingReady fields), session decision, gates 1–3 · V4-2 → §4, §5 invariants, credit-recovery gate · V4-3 → §3 (counters, byte domain, ledger, streamID, admissionTs, field/layout assignments), presentation-gaps gate · V4-4 → §5 frame-size policy, property validation move, oversize gates · V4-5 → §6, §11 A1 move, token/watchdog/priming gates · V4-6 → §7, UDP gates · V4-7 → §8, stale-input/cursor gates (both underlying bugs verified this revision) · V4-8 → §9, §11 A0.0, clock gates, baseline-identity decision · V4-9 → §10, codec/RA gates (NALU default-keyframe verified) · Smaller: cursor compare fix (§8), single SDL owner + `SDL_UpdateNVTexture` (§11 D), codegen toolchain verification (§11 A0.0), effort split (§13).

## 16. Errata against plan v4 (v4 left unmodified)

(a) §3.2 `VideoReset` conflated request and authority — split into 36/37 with host-only generation writing; (b) "bumped on establishment **or** reset" permitted double increments — one generation per attempt; (c) `VideoHello` self-introduced its nonce — now precommitted on control; (d) listen-port left as an either/or — now `StreamingReady.listen_port`; (e) sessionID "survives grace" had no mechanism — replaced by the scoped-session decision; (f) I4's "always admissible recovery IDR" was false under same-generation unACKed discards — recovery is generation-fatal; (g) `(gen&0x7FFF)<<32|frameID` pts packing — replaced by tokens; (h) "contiguous output/presentation progress" — presentation is non-contiguous by design, telemetry only; (i) 4 MiB "≥ any legal 4K IDR" — false, now provisional-cap policy; (j) A1's priming gate depended on B2's identity work — dependency inverted; (k) discontinuity on "late frame, duplicate" — over-triggering, corrected rules; (l) A0.0's end-to-end `captureSeq` and two-timestamp clock sync were unimplementable — corrected; (m) cursor u24 compare treated `delta = 0` as newer — fixed; (n) `MaxKeyFrameInterval` property checks deferred to C while B0 relied on encoder behavior — validation moved to session creation.
