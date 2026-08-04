# RESC Implementation Plan v4

| | |
|---|---|
| **Date** | 2026-07-29 |
| **Supersedes** | `IMPLEMENTATION_PLAN.md` (plan v2) in full. That file and `LATENCY_GLITCH_ANALYSIS.md` (v1.1) are left unmodified as history; errata against both are carried in §12. |
| **Lineage** | `LATENCY_GLITCH_ANALYSIS.md` v1.0→v1.1 → review 1 (`…_review.md`) → plan v2 (`IMPLEMENTATION_PLAN.md`) → review 2 (`…_review_v2.md`, incorporated in plan v2) → **review 3** (`IMPLEMENTATION_PLAN_review_v3.md`) → **this document**. (Numbering note: "v3" is the review; the plan series jumps v2→v4 so document numbers stay unambiguous.) |
| **RESC commit** | `12b87d1` (branch `main`) |
| **Verification status** | Every review-3 finding adopted below was re-verified against code before adoption, or is explicitly marked as accepted-at-claim-level. Traceability in §11. |
| **Estimates** | Initial; re-estimated **after Phase B0 (contract spike)** by commitment — see §9. |
| **Scope statement** | TLS/pairing remain out of scope (LAN trust model unchanged): generation nonces prevent *accidental* stale joins, they do not authenticate peers. This is a recorded residual risk, unchanged from the current plaintext design. |

Goal unchanged: eliminate corruption and cut video/pointer latency for the fixed wired-LAN Mac→Ubuntu extended display by moving to a dedicated reliable video stream with pre-encode backpressure, low-delay decode, and a decoupled render/cursor path — keeping RESC's features (4K HEVC 40 Mbps, Night Shift, arbitrary resolutions, full input plumbing). Review 3 approves this architecture; what v4 adds is the **contracts at its boundaries**: ordering, generations, credit liveness, decoder identity, and lifecycle.

---

## 1. Findings register (delta on analysis v1.1 + plan v2 §1)

New entries, all re-verified at `12b87d1` unless noted:

| ID | Finding | Evidence | Status |
|---|---|---|---|
| **G8** | Post-assembly encoded-frame **reordering** before decode: the assembler returns whichever slot completes (`jitter-buffer/src/lib.rs:174-204`), the receiver forwards immediately (`video_receiver.rs:185-210`), `frame_id` is used for slot lookup/eviction only, and the decode loop processes arrival order (`main.rs:348-424`). Frame N+1 can be decoded before frame N with **zero loss** — an out-of-order reference stream. | code above | Mechanism **VERIFIED**; wire-reorder frequency on this LAN = **MEASURE** (A0 order telemetry) |
| D1 | Decoder outputs are **mislabeled**: `Packet::copy` sets no PTS/identity (`video-decode/src/lib.rs:198`), and every emitted frame is stamped with the *current input's* `timestamp_us` (`:279-284, 324-330`). With CUVID display delay or frame threading, outputs belong to earlier inputs → A0 submit→output measurements, `decodedOutputFrameID`, and present telemetry would all be wrong without §4's identity contract. | code above | VERIFIED |
| D2 | **EAGAIN conflation**: `send_packet` treats every error (incl. EAGAIN) as decode failure (`:198-202`); the `receive_frame(...).is_ok()` drain conflates EAGAIN/EOF/real errors (`:204-207`). ffmpeg's contract requires drain-then-retry on send-side EAGAIN. | code above | VERIFIED |
| D3 | **`WaitingForIDR` liveness bug**: non-keyframes are rejected pre-`send_packet` (`:193-196`) but the state advances only when an IDR produces *output* (`:263-275`). An accepted IDR that yields EAGAIN (decoder wants more input) strands recovery — dependent frames withheld, IDR never emitted. Masked today by 0.5 s periodic IDRs; **persistent** under the planned ~60 s maximum. | code above | VERIFIED |
| D4 | **Recovery request can be silently lost**: `idr_tx.try_send(...)` result ignored on a 4-slot channel (`main.rs:379-387`). | code | VERIFIED |
| N1 | **Codec negotiation is not real**: client advertises H.264 only (`control_channel.rs:69-78`); host never decodes `ModeRequest` and picks HEVC from `--hevc` (`HostSession.swift:86-132, 251-257`); on HEVC decoder init failure the client builds an H.264 decoder *for the ongoing HEVC stream* (`main.rs:278-292`) — not a fallback, a guaranteed-garbage path. | code above | VERIFIED |
| H1 | **No generation fencing through the async encoder**: submissions/completions carry no session/config/video generation (`VideoEncoder.swift:166-199`); completions send via whichever global sender is current (`main.swift:129-133`, `StreamingState.swift`). A generation-K encode can complete after K+1 installs its sender and be transmitted — possibly mistaken for K+1's first keyframe. | code | VERIFIED (StreamingState wiring at claim level, consistent with `main.swift`) |
| H2 | **`LatestFrameSlot` is a coalescing store, not a gate/replay controller**: every `store` signals the semaphore while overwriting the single buffer, so permits accumulate and later `waitAndTake()` calls return nil (benign today — encoder loop `continue`s); `take` clears the only buffer (nothing left to replay); nothing wakes a drain on encode-completion/ACK/reconnect. | `LatestFrameSlot.swift:16-47`, `main.swift:135-146` | VERIFIED |
| H3 | Sender does **not enforce** the advertised `maxFrameBytes` (guards only `totalChunks ≤ u16::max`, `VideoSender.swift:59`). | code | VERIFIED |
| C1 | **SDL texture destruction-order UAF risk**: `Renderer.canvas` is declared before `persistent_tex` (`renderer/src/lib.rs:13-23`); Rust drops fields in declaration order; `SDL_DestroyRenderer` frees its textures → `PersistentTexture::drop`'s manual `SDL_DestroyTexture` then operates on freed state. Latent today (Renderer lives to process exit); must be fixed in the A1 renderer refactor, not deferred to D. | code + Rust/SDL docs | VERIFIED (structure) |
| T1 | **Clock contract violations**: `CFAbsoluteTimeGetCurrent()` (non-monotonic, used by `CursorTracker.swift:107-123`); capture discards SCK sample timing (`DisplayCapturer.swift:105-127`); video PTS synthesized from a frame counter (`main.swift:129-145`). | code + Apple docs | VERIFIED |
| T2 | **Double-counted drop stats**: an assembled-then-queue-dropped frame increments both `frames_completed` (assembly) and `frames_dropped` (queue-drop branch), corrupting any rate that uses their sum as denominator. Frame-ID **gaps** after a bounded reorder interval are a valid whole-frame loss proxy (host increments `frame_id` per encoded frame) — correcting plan v2's "chunk-0 metadata is the only loss proxy". `SO_RXQ_OVFL` requires `recvmsg` ancillary data (current `UdpSocket::recv` can't read it); `/proc/net/snmp` is system-wide only. | `jitter-buffer:193-204`, `video_receiver.rs:191-203`, `main.rs:231-256` | VERIFIED |
| P1 | **A0's ping/pong doesn't exist yet**: `control.proto` has no clock-sync, progress, or reset payloads; typed Swift protobuf isn't wired (plan v2 scheduled it in A1, *after* A0 needed it). Hence Phase A0.0. | `proto/control.proto`, `Package.swift` | VERIFIED |
| P2 | **Peer addressing + disconnect lifecycle**: streaming still depends on `--client` for the video/input/cursor destinations (`main.swift:168-199`; `HostSession` gets only a debug endpoint string); host clears a failed control connection without notifying the session; the client's control task exits on error while decode/render continues (`main.rs:197-264`). Minimal correct lifecycle belongs in **B1**, not E. | code (ControlChannel internals at claim level) | VERIFIED / HIGH |
| P3 | Codegen/runtime **version skew**: `generate_proto.sh` pins SwiftProtobuf 1.28.1 (verified) vs resolved runtime 1.36.1 (`Package.resolved`, claim level). No SwiftPM test target exists (verified). | script + Package files | VERIFIED / claim |

Confidence discipline carried forward from plan v2 §1: real-world IDR sizes, G6/G8 event frequencies, the ~51 fps beat, CUVID's exact lag on the deployed ffmpeg, and all latency totals/targets are **measured quantities, not assumptions**; every gate asserts measured bounds.

---

## 2. Invariants (v4 — the contract every phase preserves)

- **I1 — Ordered, exactly-once decode per generation** *(review-3 wording adopted)*: every encoded access unit is either decoded exactly once **in source order within its video generation**, or a **linearized discontinuity barrier** invalidates that generation before any later dependent access unit reaches the decoder. Non-drop alone (plan v2's I1) is insufficient — see G8. A shared boolean is not a barrier (later frames may already be queued); the barrier travels **in-band** in the decode input stream (§4).
- **I2 — Cursor independence (corrected bound)**: cursor updates inherit **no queued-video temporal age** and trigger **no upload** on cursor-only redraws; under simultaneous video work they are bounded by *at most the current render/upload operation plus presentation*. The stronger "input cadence + present only under saturated video" claim is an explicit **non-goal** unless cursor-priority scheduling is added and tested (§6 Phase D). Snapshots are coherent per §7.
- **I3 — Host pump owns gating, coalescing, and replay**: at most one encode in flight; **`latestPendingCapture`** (overwritten by capture, consumed only when all gates allow, drained immediately on gate-open without waiting for a new SCK callback) is **distinct from `lastReplayableCapture`** (retained after consumption for reconnect/reset IDR). Two timestamps per frame: **`contentCaptureTs`** (original screen-content time; telemetry) and **`admissionTs`** (when admitted to the current flow-control generation; drives age bounds). A minutes-old static replay starts its flow age at admission — otherwise every static reconnect instantly violates the age bound and reset-loops.
- **I4 — Credit = cumulative decoder-accepted progress, with whole-frame liveness**: an encoded frame is *accepted* only after `send_packet` succeeded and output was drained to EAGAIN (EAGAIN ≠ error, §4); socket receipt is never credit; presentation is telemetry. **Hard rule, not heuristic**: a negotiated `maxEncodedFrameBytes` is enforced by the sender, and total credit ≥ one complete maximum frame + framing, with an explicit admission policy (§3.6) — so one full recovery IDR is always admissible and the stream cannot deadlock. Cumulative counters are u64, connection-local, reset only with a new `videoGeneration`; ACKs validated `old ≤ ack ≤ sent`, duplicates idempotent, wrong-generation discarded+counted. Decoder *display depth* is an output-age telemetry concern, **not** a credit parameter (corrects plan v2's misplaced deadlock guard).
- **I5 — Generation fencing end to end** *(new)*: `videoGeneration` covers sockets, frame headers, ACKs, resets, **encoder submissions/completions**, and decoder state together. Every encode submission captures `{sessionID, configGeneration, videoGeneration, captureSeq, contentCaptureTs, admissionTs, forceIDR, isReplay}`; completions with mismatched generation are discarded and counted (H1). A new generation stays `awaitingFirstIDR` until an access unit **submitted under that generation and verified as random-access on output** (NALU type, not merely the requested flag) is ready.

---

## 3. Wire & protocol contract (freeze before Phase B0)

### 3.1 Versioning & capability
- `protocol_version` bumps **1 → 2**. `ModeRequest`/`ModeConfirm` gain a `video_transport` capability (`UDP_V1` | `TCP_V2`). Mixed versions fail closed with `ModeReject(INCOMPATIBLE_PROTOCOL_VERSION)` — a v1/UDP peer can never half-join the v2/TCP path.
- Identifier validation rule: "validate session/stream/config IDs on every message **where those identifiers are defined and valid in the current state**" — pre-`ModeConfirm` messages legitimately carry `session_id = 0`; `Stats`/`DisplaySettings` carry no stream/config IDs by design.

### 3.2 New control messages (additive protobuf; typed codegen required first — A0.0)
- `ClockPing{t1_us}` / `ClockPong{t1_us, t2_us}` — monotonic-domain clock sync (§3.7), repeated periodically and after reconnect; lowest-RTT offset + uncertainty recorded.
- `DecoderProgress{session_id, config_generation, video_generation, accepted_frame_id:u32, accepted_bytes:u64, output_frame_id:u32, presented_frame_id:u32}` — cumulative; accepted\* is credit, output/presented are telemetry watermarks (contiguous progress, not max-observed — requires §4 identity).
- `VideoReset{reason, new_video_generation}` — reasons: age-bound, decode-fatal, handshake-fail, control-loss, config-change. **Host** increments `videoGeneration`; client requests via reason-coded message, host executes.
- Structured trace/counter schema for A0 telemetry export.

### 3.3 Generation lifetimes
| Identifier | Created/bumped by | Lifetime |
|---|---|---|
| `sessionID` | Host, on ModeConfirm | control connection acceptance → disconnect grace expiry |
| `configGeneration` | Host, on ModeConfirm and any codec/resolution/bitrate renegotiation | until next renegotiation |
| `videoGeneration` | Host, on every video-socket establishment **or** reset | one video connection epoch; fences frames, ACKs, encoder callbacks, decoder state (I5) |
| `frameID` | Host, +1 per encoded frame | per videoGeneration, starts at 0; wrap-safe compare = `(a −₃₂ b) as i32` |

### 3.4 Video connection & framing (TCP, little-endian, fixed-width)
- Ordering: client binds/listens → sends `StreamingReady` → host connects → **`VideoHello{magic, version, sessionID, streamID, configGeneration, videoGeneration, nonce}`** → client validates against control-channel state → `VideoHelloAck` → host sends first IDR. Stale/unknown hello ⇒ close, counted.
- Per-frame header (after hello):
  `magic:u16 | headerLen:u8 | flags:u8 (bit0 keyframe, bit1 replay) | codec:u8 | reserved:u8 | videoGeneration:u32 | frameID:u32 | captureSeq:u32 | contentCaptureTs_us:u64 | payloadLen:u32` — 30 bytes, `headerLen` allows forward-compatible extension; `payloadLen ≤ maxEncodedFrameBytes` or the connection fails closed (malformed/oversize ⇒ reset, never resync-scan).
- `TCP_NODELAY` both ends. Peer address for the video dial = **the accepted control connection's remote address** (P2); `--client` demoted to a debug override. Client owns the listening port (advertised in `StreamingReady` or fixed from ModeConfirm's `video_port`).

### 3.5 Disconnect lifecycle (implemented in B1, polished in E)
Control loss ⇒ atomically: freeze credit, close video generation (both ends), host releases pressed input (`PressedKeyState` already exists), then renegotiate within the grace period or terminate. Client control-task exit must tear down or renegotiate the video path — never continue decoding an unmanaged stream (P2).

### 3.6 Credit admission policy (decision)
**Policy 2 — one encoded-pending frame**: pre-encode gating cannot know encoded size, so the pump allows exactly one encoded-but-not-yet-admitted frame; its actual bytes and age are counted; no further encode until it is admitted into credit or the generation resets. (Rejected alternative: reserve `maxEncodedFrameBytes` per encode and refund — more churn for the same bound at our 1-in-flight encode depth.) Initial `maxEncodedFrameBytes = 4 MiB` (≥ any legal 4K IDR under `DataRateLimits`; sender-enforced: an encoder output exceeding it is an encode failure → reset path, closing H3). Initial credit window = `maxEncodedFrameBytes + headers`, tuned upward from A0 IDR histograms.

### 3.7 Clock contract
Host: `mach_continuous_time`-derived monotonic epoch for all latency stamps; SCK PTS kept as diagnostic metadata and mapped via `CMSyncConvertTime` from the `SCStream` synchronization clock — never subtracted across unrelated timebases. `CFAbsoluteTimeGetCurrent()` banned from latency paths (T1). Client: `Instant`/`CLOCK_MONOTONIC` epoch. Cross-machine mapping only via ClockPing/Pong offset ± uncertainty. Software `present()` return is not photon time — the final performance gate includes an optical (high-speed camera) measurement.

---

## 4. Client decode contract

- **Linearized input stream**: `DecodeInput::{Frame(EncodedFrame), Discontinuity(reason, generation)}` — one ordered channel from the transport/reorder stage to the decoder; the barrier is an in-band item, so no later dependent frame can overtake it (I1). On any of gap-deadline, slot eviction, late frame, duplicate, or queue-drop (UDP interim): emit reason-coded telemetry, enqueue `Discontinuity`, clear held+queued dependents, enter recovery once, latch one IDR request.
- **UDP interim reorder gate (Phase A1, deleted with UDP in B2)**: bounded completed-frame reorder window (initial: 8 frames / 50 ms gap deadline — tune from A0), wrap-safe `frame_id` comparison; in-order release; gap after deadline ⇒ Discontinuity. Also yields the frame-gap loss counter (T2).
- **Identity**: input `AVPacket.pts = ((videoGeneration & 0x7FFF) << 32) | frameID`; a side table keyed by pts carries `{captureSeq, contentCaptureTs, admissionTs}`. Each output `AVFrame`'s decoder-emitted pts recovers the originating input; identity is preserved through the owned-frame handoff to presentation (fixes D1; makes `output/presented` watermarks truthful).
- **Structured outcome** (fixes D2): `DecodeOutcome{ inputAcceptedExactlyOnce, acceptedFrameID, acceptedBytes, outputs[], drainState: Again|EndOfStream|Error, recoveryTransition }`. Only `Again` is normal drain completion. Send-side EAGAIN: retain the exact packet, drain outputs, retry once-effective — the frame is neither dropped nor double-ACKed (exact-once ACK accounting).
- **Recovery state machine** (fixes D3): `Healthy → WaitingForIDR → PrimingAfterIDR → Recovering → Healthy`. `PrimingAfterIDR` entered immediately after successful **IDR input acceptance**; a bounded run of subsequent access units is fed (they are post-IDR in-order dependents, safe to decode); presentation of outputs is suppressed until the output mapped to the IDR (or later) appears; then Recovering (N clean frames) → Healthy. Frames discarded in `WaitingForIDR` are **not** accepted-progress (I4) — they are non-admitted and the generation's reset path accounts for them.
- **Idempotent IDR latch** (fixes D4): a generation-scoped `wantIDR(reason)` latch with rate-limited retry replaces fire-and-forget `try_send`; cleared on matching-generation IDR arrival.

## 5. Host pump specification

One serial actor/queue owns: `latestPendingCapture`, `lastReplayableCapture`, `encodeInFlight (0/1)`, the one encoded-pending frame (§3.6), credit accounting (`sentBytes/ackedBytes/oldestUnackedAdmissionTs`), `{sessionID, configGeneration, videoGeneration}`, `awaitingFirstIDR`. Wake sources: capture arrival, encode completion, ACK arrival, connection transition, reset request. On every wake: re-evaluate gates; if open, consume `latestPendingCapture` (copying it to `lastReplayableCapture`), submit with the full I5 context; on completion, discard generation mismatches, frame the output, admit per credit. Replays are flagged (`flags.replay`, fresh `admissionTs`, original `contentCaptureTs`). `LatestFrameSlot` may be reused as the storage cell only — its semaphore signaling is replaced by pump wakeups (H2). The encoder loop in `main.swift:135-146` (blocking `waitAndTake` + immediate resubmit) is retired.

---

## 6. Phases

### Phase A0.0 — instrumentation & protocol scaffold (additive only)
1. Wire generated Swift protobuf: `Protocol` SwiftPM target at `Sources/Protocol` (matches `generate_proto.sh` output), generated sources committed; **align codegen and runtime versions** (regenerate with a plugin matching the resolved 1.36.x runtime, or pin both to one version; add a CI check that regeneration is clean) (P3). Behavioral parsing changes NOT yet enabled.
2. Add `ClockPing/Pong` + trace/counter schemas (§3.2) additively to proto v1 (v2 bump happens in B0).
3. Establish monotonic epochs per §3.7; thread frame identity (captureSeq) capture→encode→wire→decode→present.
4. Create test targets: SwiftPM test target (none exists today) + Rust test scaffolding; fixtures include a **fake encoder with delayed completions** (for I5 tests) and a scripted decoder (for §4 tests).
5. Buffered low-overhead structured logging on both ends.

### Phase A0 — behaviorally unchanged baseline
1. Disjoint counters (no aggregate `frame_drop_rate`; T2): received-invalid/misrouted; assembly timeout; missing-chunks-with-metadata; **frame-ID gap after reorder interval** (whole-frame loss); out-of-order/late/duplicate completions (G8 frequency); oversize (G6) with byte size; slot evictions; `decode_queue_full_nonkey` (G7); decoder submit/output/present sequences; upload timings; cursor sample→receive→present. `SO_RXQ_OVFL` via `recvmsg` ancillary data if adopted; `/proc/net/snmp` only as a labeled system-wide fallback.
2. Baseline soak protocol (idle / drag / full-screen flips / `iperf3`), ≥10 min each; record p50/p95/max video e2e + cursor latency + all counters + capture fps; include software-timing uncertainty and one optical spot-check.
*Exit gate:* baseline report; G6 band occupancy, G7 and G8 event rates are measured facts.

### Phase A1 — UDP-path correctness (surgical; UDP remains explicitly non-release-quality until B2 deletes it)
1. Enable typed protobuf parsing (kills G3) with a state×message validation matrix (§3.1 rule); replace **outbound** hand-rolled encoders too (`HostSession` buildModeConfirm/StartStreaming/DisplaySettings).
2. Force-keyframe state race fix (encoder-queue-confined / atomic).
3. Texture-resident cursor redraw **with safe texture lifetime**: fix C1 now — drop the `mem::forget`/raw-pointer trick or make destruction order explicit (texture destroyed before canvas); upload-on-new-video vs composite-only paths instrumented separately.
4. Coherent cursor snapshot per §7.
5. **Ordered decode-input stream**: reorder gate + `DecodeInput::{Frame,Discontinuity}` channel (G7+G8 under I1).
6. **`PrimingAfterIDR` + idempotent IDR latch** (D3, D4).
7. **Codec capability intersection** (N1): client advertises its real decoder set (probe at startup); host decodes `ModeRequest` (typed now) and selects from the intersection or `ModeReject(UNSUPPORTED_MODE)`; the client's mid-stream "H.264 fallback" for an HEVC stream is deleted (decoder init failure ⇒ renegotiate or fail loudly).
8. G6 interim stance unchanged from plan v2: fast-track B; the 1473-chunk stopgap only if B slips and A0 shows the band is hot.
*Exit gates:* plan v2's A1 gates **plus**: G8 gate (complete N+1 before N ⇒ decoder sees source order, no duplicates; gap ⇒ exactly one ordered Discontinuity); IDR-latch liveness gate (full request channel then drain ⇒ one generation-scoped request delivered); priming gate (IDR yielding no immediate output ⇒ dependents admitted, nothing presented pre-boundary, recovery completes); codec-matrix gate (H.264-only / HEVC-capable / HEVC-init-failure peers ⇒ both ends always converge on one codec).

### Phase B0 — contract-first spike (**non-deployable lab branch**)
1. Freeze §3 (proto v2 bump, framing bytes, generation lifetimes, validation matrix).
2. Implement VideoHello/Ack + peer-address derivation from the control connection.
3. Generation fencing proven with the fake delayed-completion encoder (I5).
4. Hard `maxEncodedFrameBytes` + whole-frame credit rule (§3.6) with a **fixed one-frame write/read bound** — the spike carries minimal hard-bounded credit from day one (review-3 M5: a no-credit TCP branch must never exist even as an intermediate).
5. Prove one frame + one ACK end to end under fixed memory bounds.
*Exit:* re-estimate B1/B2/C/D (§9 commitment).

### Phase B1 — host pump, replay, lifecycle
1. Implement §5 (pump, dual capture state, fenced submissions).
2. Video reset + minimal control-disconnect behavior (§3.5), including client-side teardown when the control task dies.
3. Static replay proven with separate content/admission timestamps (minutes-old buffer must not trip the age bound).
*Gates:* stale-ACK (generation-K ACKs after K+1 starts ⇒ no K+1 credit movement); stale encode callback (K output after K+1 sender installed ⇒ discarded, counted, never K+1's "first IDR"); gate-open wake (credit closed → one final capture → callbacks stopped → credit reopened ⇒ that exact captureSeq encodes and displays with no new SCK event); static-replay age (no reset-loop); control-disconnect (both ends close/reset video generation and release input deterministically).

### Phase B2 — complete reliable transport
1. Cumulative decoder-accepted progress (frames+bytes u64), exact-once ACK accounting on the §4 outcome struct.
2. Output/presented watermarks from recovered identity (D1 fixed).
3. `awaitingFirstIDR` verified on output NALU type per I5.
4. Fault suite: stall (decoder stalled, socket flowing ⇒ credit halts), age-bound reset, delayed callbacks, reconnect storms, oversize/malformed framing fail-closed, whole-frame-credit recovery (a maximum legal IDR admits and recovers at minimum legal credit).
5. Delete the UDP path (VideoSender chunking, FrameAssembler, reorder gate, assembly deadlines, gray-frame heuristic) **only after** the full gate set passes.

### Phase C — final-static verification, FramePacer removal, host tuning
Unchanged from plan v2 §6 except the coalescing/replay machinery already landed in B1: run the final-static gate, remove FramePacer, then A/B encoder flags (**note: `EnableLowLatencyRateControl` is an encoder-specification key — each A/B variant requires a fresh `VTCompressionSessionCreate`**), capture interval 1/60 vs 1/120, queueDepth 3/5/8; check every property-set result, per codec.

### Phase D — client decode/render split
Plan v2 §7 plus review-3 additions: frame identity preserved through present; SDL destruction order explicit; separate NV12 texture path (`SDL_PIXELFORMAT_NV12` capability-tested, I420 fallback explicit); Night Shift on GPU or an explicit documented I420-CPU fallback; cursor bound validated **under simultaneous 4K upload** per corrected I2 (assert "≤ current upload + present", not "input cadence"); all SDL calls on the render thread (the existing `unsafe impl Send` is deleted, not trusted); owned/refcounted decoded-frame handoff; sanitizer-clean 4K soak.

### Phase E — polish
As plan v2 §8 (virtual-display resilience, keyboard send, sleep/wake polish, long soak; optional bitrate-adaptation re-wiring against new telemetry), now explicitly *excluding* first-correct-lifecycle work (moved to B1).

---

## 7. Cursor snapshot representation (decision, documented layout)

Single `AtomicU64`, little-endian bit layout: `bits 0–15 x:i16 | 16–31 y:i16 | 32–39 shape:u8 | 40–63 seq:u24` (wrapping). Sequence comparison: `((new − old) & 0xFF_FFFF) < 0x80_0000`. Coordinate domain: i16 covers ±32,767 — sufficient for any real panel including 8K (7,680); the −1 "off-display" sentinel fits. **Documented constraint**: if virtual-display coordinates ever exceed i16, this representation is replaced by a seqlock (double-sequence acquire/release read) — recorded here so "arbitrary resolutions" and the packed layout can't silently conflict. Stress test: concurrent publish/render race must never observe fields from different sequences.

## 8. Validation gate summary (v4 = plan v2 gates + review-3 gates)

| Gate | Phase |
|---|---|
| Baseline integrity (record before any behavior change) | A0 |
| Typed parsing: 0xFA-bearing session varint never triggers IDR | A1 |
| G6: 512 vs 513 unit test + IDR size histogram | A0/A1 |
| G7: queue saturation ⇒ counted, controlled reset/IDR; no silent continuation; no dependent P crosses the barrier | A1 |
| **G8: N+1-before-N ⇒ source-order decode, no duplicates; gap ⇒ one ordered Discontinuity** | A1 |
| **IDR-request liveness: full channel, then drain ⇒ one generation-scoped request delivered** | A1 |
| **Priming: IDR with no immediate output ⇒ dependents admitted, nothing presented pre-boundary** | A1 |
| **Codec negotiation matrix: both ends always converge on one codec** | A1 |
| Cursor residency (zero uploads on cursor-only) + coherence (no mixed-sequence tuple) | A1 |
| **Renderer lifetime: create/destroy under sanitizers; texture dies before renderer; all SDL on render thread** | A1/D |
| **Protocol version: v1/UDP peer cannot enter v2/TCP; malformed/oversize fail closed** | B0 |
| **Stale ACK: generation-K ACKs after K+1 ⇒ no credit movement** | B1 |
| **Stale encode callback: K output after K+1 sender ⇒ discarded, never K+1's first IDR** | B1 |
| **Gate-open wake: final capture encodes with no new SCK event** | B1 |
| **Static replay age: minutes-old replay ⇒ no reset-loop** | B1 |
| **Control disconnect: deterministic video-generation close + input release both ends** | B1 |
| ACK semantics: decoder stalled, socket flowing ⇒ credit halts | B2 |
| Bounded age: stall+saturation ⇒ bounds hold or clean reset+IDR | B2 |
| **Whole-frame credit: max legal IDR admits at minimum legal credit** | B2 |
| **Decoder identity: induced output delay ⇒ every output carries correct input identity/metadata** | B2/D |
| **Decoder EAGAIN: identical packet drained/retried once, ACKed exactly once** | B2 |
| Final static update (FramePacer off) | C |
| Low delay: measured submit→output bound, off vs on | D |
| Thread ownership: sanitizer-clean 4K soak | D |
| **Optical latency: high-speed-camera comparison before/after final changes** | A0 + final |

## 9. Effort (initial; **re-estimated at B0 exit by commitment**)

A0.0: 1–1.5 d · A0: 0.5–1 d · A1: 1.5–2.5 d (grew: reorder gate, priming, codec negotiation, texture lifetime) · B0: 1–1.5 d · B1: 1.5–2.5 d · B2: 2–3 d · C: 0.5–1 d · D: 2–3 d · E: open. Review 3 (M5) is right that plan v2's lower bounds were aggressive once the wire schema, lifecycle tests, host pump, and decoder state machine are counted — these figures assume the §3/§4 contracts are frozen on paper first (this document is that freeze; B0 validates it).

## 10. Decision log (delta on plan v2 §11)

| Decision | Choice | Why |
|---|---|---|
| Ordering barrier | In-band `DecodeInput::{Frame,Discontinuity}` channel | A flag can be outrun by already-queued frames (I1) |
| UDP interim reorder gate | 8 frames / 50 ms initial window, wrap-safe compare | Cheap; deleted with UDP in B2; parameters tuned from A0 |
| Credit admission | Policy 2: one encoded-pending frame, actual bytes counted | Pre-encode size unknown; reservation churn buys nothing at depth 1 |
| `maxEncodedFrameBytes` | 4 MiB initial, sender-enforced, negotiated | ≥ any legal 4K IDR; closes H3; whole-frame liveness (I4) |
| Decoder identity | pts = (gen&0x7FFF)<<32 \| frameID + side table | Survives ffmpeg's pts passthrough; recovers identity on delayed output |
| Priming | Distinct `PrimingAfterIDR` state | Fixes D3 without presenting pre-boundary output |
| Peer addressing | Video peer := control connection remote addr; `--client` = debug override | Removes correctness dependency on CLI args (P2) |
| Generation authority | Host bumps `videoGeneration`; client requests resets by reason | One writer per counter; client is stateless across resets |
| Display depth | Output-age telemetry only, not credit | Credit is input-acceptance-based; corrects plan v2's guard |
| B spike | Credit-bounded from day one, non-deployable branch | I4 holds in every artifact that can touch a real session (M5) |
| Cursor snapshot | Packed AtomicU64, layout in §7, seqlock fallback documented | Coherence with one load; explicit range constraint |
| Codegen versions | Regenerate against resolved runtime (1.36.x) + CI regen-clean check | 1.28.1 generator vs 1.36.1 runtime skew (P3) |
| TLS/pairing | Out of scope; nonce ≠ auth, recorded residual risk | Fixed LAN, single user; unchanged trust model |

## 11. Traceability (review 3 → this plan)

V3-1/G8 → §1 G8, I1, §4 reorder gate, A1.5, G8 gate; also T2's frame-gap loss proxy · V3-2 → §3 entire (versioning, messages, generations, framing, hello, peer address, disconnect-in-B1, validation-matrix rule) · V3-3 → I3, §5 pump, B1, gate-open-wake + static-replay gates · V3-4 → I5, §5 fenced submissions, stale-callback gate, content-vs-admission timestamps · V3-5 → §4 (identity, DecodeOutcome, EAGAIN, PrimingAfterIDR, IDR latch), D1–D4, related gates · V3-6 → I4 whole-frame rule, §3.6, H3, whole-frame-credit gate · V3-7 → Phase A0.0, §3.7 clocks, T1/T2 counters, optical gate · M1 → C1, A1.3, renderer-lifetime gate · M2 → I2 corrected bound, §7 layout, D cursor test · M3 → N1, A1.7, codec gate · M4 → P3, A0.0.1/.4, C's session-recreation note, test-target budget · M5 → B0 constraints, §9, TLS scope statement.

## 12. Errata carried forward (prior documents intentionally unmodified)

Against `LATENCY_GLITCH_ANALYSIS.md` v1.1: items 1–7 of plan v2 §13 still stand. Against `IMPLEMENTATION_PLAN.md` (plan v2): (a) its I1 lacked ordering (G8); (b) its framing header listed no version/magic/endianness and conflated config/connection generations; (c) its I2 overpromised "input cadence + present only" post-D; (d) its B.4 "window ≥ 2× largest observed IDR ≥ decoder retention depth" mixed a tuning heuristic with a mis-layered liveness condition — replaced by the hard whole-frame rule with display depth as telemetry; (e) its A0 required clock-sync messages that did not exist (fixed by A0.0); (f) its B spike allowed a no-credit branch; (g) its A0.3 called chunk-0 metadata "the only loss proxy the wire format allows" — frame-ID gaps are a second, better proxy; (h) "validate IDs on every message" needed the defined-in-current-state qualifier; (i) `LatestFrameSlot` was described as "already the correct coalescing primitive" — it is the correct *storage cell*; the pump, replay copy, and wakeups it lacks are mandatory (H2).
