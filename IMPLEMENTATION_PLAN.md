# RESC Implementation Plan (v2)

| | |
|---|---|
| **Date** | 2026-07-29 |
| **Supersedes** | §7 ("Recommended plan") of `LATENCY_GLITCH_ANALYSIS.md` v1.1. The analysis document is deliberately left unmodified; corrections to its text are recorded here as errata (§13). |
| **Lineage** | `LATENCY_GLITCH_ANALYSIS.md` (v1.0 → v1.1) → `LATENCY_GLITCH_ANALYSIS_review.md` (review 1, incorporated in v1.1) → `LATENCY_GLITCH_ANALYSIS_review_v2.md` (review 2, incorporated **here**) |
| **RESC commit** | `12b87d1` (branch `main`) |
| **Verification status** | Every review-2 claim adopted below was independently re-verified in code before adoption (see §12). |
| **Estimates** | All effort figures are **initial estimates to be revised** after Phase A0 baseline data and the Phase B spike (§10) — per review-2 R10. |

Goal: eliminate loss/drop-induced visual corruption and reduce video and pointer latency for the fixed, wired-LAN Mac→Ubuntu extended-display setup, by migrating RESC to a reliable dedicated video stream with pre-encode backpressure, low-delay decode, and a decoupled render/cursor path — while keeping RESC's feature set (4K HEVC 40 Mbps, Night Shift sync, arbitrary resolutions, full input plumbing).

---

## 1. Findings model used by this plan (corrections relative to the v1.1 analysis)

The plan builds on the analysis v1.1 findings **G1–G6 / L1–L9** with these review-2 amendments, each re-verified:

**G7 (new) — local post-assembly encoded-frame drop on decoder-queue saturation (VERIFIED).**
When the 4-slot channel between assembly and decode is full, the receiver silently discards a *fully assembled non-keyframe* (`ubuntu-client/crates/net-transport/src/video_receiver.rs:191-203`) and keeps feeding later dependent frames to ffmpeg (`ubuntu-client/src/main.rs:348-424`). No discontinuity is signaled, no IDR is requested at the drop site. This breaks the reference chain with **zero packet loss and no oversized frame** — an independent glitch source that fires exactly when the decoder is busiest (window drags). Historical note: this is the corruption mechanism of TECHNICAL_REVIEW.md "Problem 11" (fixed then by a 64-deep blocking channel), *reintroduced* by Milestone A Item 5's queue-shrink-plus-drop policy. The latency-vs-corruption dilemma it embodies is resolved properly only by Phase B (credit-based transport); until then, a drop must trigger the existing recovery machinery instead of silent continuation (Phase A1.5).

**Corrected cursor-latency model (replaces the analysis's L7 attribution).**
Cursor state is latest-wins (four atomics written by the cursor thread, read fresh each render iteration — `ubuntu-client/src/main.rs:139-182, 505-515`), so the pointer does **not** inherit the *age* of queued video frames (L3) or of frames held inside the decoder (L1). Its real costs are: whatever work the shared decode/render loop is currently doing (L2), the full-frame texture upload every cursor redraw performs today (L9), and presentation/vsync. Implication: L1/L3 fixes help *video content* age; the pointer is fixed by the thread split + texture-resident cursor path (Phases A1.4, D). Pointer-latency targets and tests are set against this model, measured separately from video (§9).

**R5a (new, minor) — torn cursor snapshots (VERIFIED).**
Position, shape, and sequence live in four independent `Relaxed` atomics with no coherence protocol (`src/main.rs:50-56, 174-182, 505-515`); a read overlapping a write can render new-X/old-Y or wrong-shape combinations. Correctness/jitter issue, not latency. Fixed in Phase A1.6.

**R7 (new, telemetry) — `packet_loss_rate` does not measure network loss (VERIFIED).**
`packets_dropped` counts only *received-but-invalid* datagrams (`video_receiver.rs:121-153`); the wire format has no global sequence, so datagrams that never arrived are invisible. The stat the client reports is therefore not loss, and G2's burst-loss magnitude cannot be validated with it. Phase A0 defines real counters.

**Confidence discipline (R5/R9).** Deterministic mechanisms above are VERIFIED; the following remain **runtime hypotheses to be measured, not assumed**: real-world 4K IDR size distribution (G6 band occupancy), G6 being the primary abrupt-change glitch cause, the ~51 fps capture beat at 1/60, CUVID's exact 4-frame output lag on the deployed ffmpeg (mechanism confirmed in ffmpeg 7.1 `cuviddec.c`: `ulMaxDisplayDelay = (flags & AV_CODEC_FLAG_LOW_DELAY) ? 0 : 4`), the 130–200 ms additive total, and the 40–80 ms / 10–20 ms targets. Tests assert *measured bounds and improvements*, never assumed exact values (e.g., "submit→output gap ≤ 1 frame with low-delay on," not "= 0").

---

## 2. Load-bearing invariants (the contract every phase must preserve)

- **I1 — No silent encoded-reference-frame loss.** After assembly/deframing, an encoded frame may only be (a) decoded, or (b) discarded *with* an explicit discontinuity transition: decoder marked out-of-sync, dependent frames withheld, rate-limited IDR request, resume on matching-generation IDR. (G7 violates this today.)
- **I2 — Cursor independence.** Cursor repaint must not depend on new video frames arriving (texture-resident redraw), must not perform video uploads, and must publish/read coherent snapshots. Its latency budget is loop-work + present only — and after Phase D, input-cadence work + present only.
- **I3 — Host gating with coalescing.** At most **one encode in flight**, plus **exactly zero-or-one newest pending capture retained while any gate is closed**, plus bounded unacknowledged encoded bytes. Gate the *consumption* of `LatestFrameSlot`, never the *storage* into it — RESC's slot is already the correct coalescing primitive; do not copy opendisplay's skip-and-return pattern verbatim (review-2 R3: after FramePacer removal, a skipped final capture on a static screen would never be re-delivered). When credit reopens, the pending capture is submitted immediately without waiting for a new SCK callback.
- **I4 — Credit advances on decoder-accepted progress only.** Sender flow-control credit is released by cumulative `decoderAcceptedFrameID/Bytes`: an encoded frame counts as accepted only after `send_packet()` succeeded **and** available output was drained to EAGAIN (the wrapper must distinguish EAGAIN from real errors — today `receive_frame(...).is_ok()` conflates them, `crates/video-decode/src/lib.rs:193-207`). Socket receipt is not progress (an undecoded queue could grow unbounded behind TCP); presentation is telemetry, never credit (it would recouple the host to vsync). Frames discarded while `WaitingForIDR` are *not* progress — that path goes through reset/new-generation. All ACKs cumulative and tagged with session + config generation. Deadlock guard: the byte/frame window must exceed the decoder's measured startup/display depth (relevant until low-delay mode is verified on the deployed ffmpeg).

---

## 3. Phase A0 — record the untouched baseline (no behavior changes)

Land instrumentation and **record before fixing anything** (including L9), or before/after cannot be compared.

1. Host: capture-frame struct `{pixelBuffer, sckPTS, callbackMonotonicTs, captureSeq}` through `LatestFrameSlot` (record *both* SCK PTS and callback time so SCK delivery delay is visible); encoder-out timestamp; encoded bytes + chunk count per frame; keyframe flag.
2. Client: per-frame timestamps at socket-complete, decode-submit, decode-output, upload start/end, present call/return; cursor sample→receive→present sequence/timestamps (separate from video).
3. Reason-coded drop counters replacing the misleading `packet_loss_rate` (R7): received-invalid/misrouted; assembly timeouts; **missing-chunk counts for frames whose chunk-0 metadata arrived** (expected−received — the only real loss proxy the wire format allows); oversize (G6) with the offending byte size; slot evictions; **`decode_queue_full_nonkey` (G7)**; Linux socket-overflow counters (`SO_RXQ_OVFL` / `/proc/net/snmp` UDP InErrors) where available.
4. Clock sync for cross-machine timestamps: monotonic clocks both ends + NTP-style offset from timestamped ping/pong on the control channel, lowest-RTT sample wins.
5. Run the baseline protocol: ≥10 min soak with (a) idle desktop, (b) continuous window drag, (c) full-screen content flips, (d) `iperf3` background load. Record p50/p95/max video e2e and cursor latency, capture fps, all drop counters.

*Exit gate:* baseline report exists; G6 band occupancy (how many frames land in 695,296 B–1.67 MB) and G7 drop frequency are now measured facts.

## 4. Phase A1 — surgical correctness fixes (still UDP)

1. **Wire generated Swift protobuf into the build** (R6, verified): `tools/generate_proto.sh` emits to `mac-host/Sources/Protocol/`, which no target compiles (`Package.swift` builds only `Sources/RemoteDisplayHost`). Decision: add a `Protocol` library target at that path, make `RemoteDisplayHost` depend on it, **commit the generated sources** (codegen is network/tool-dependent; pinned protoc 27.3 / swift-protobuf 1.28.1 already in the script).
2. **Replace the 0xFA scan with typed `Envelope` decoding** (kills G3); validate session/stream/config IDs on every message.
3. **Make force-keyframe state race-free** (`VideoEncoder.pendingForceKeyframe`): encoder-queue-confined or atomic. Must be airtight before periodic keyframes are removed (a lost forced-IDR flag then means an indefinitely stale client).
4. **Texture-resident cursor path** (kills L9, prerequisite for I2 and FramePacer removal): renderer gains two operations — upload-on-new-video, and composite-existing-texture + cursor + present. Count uploads per path.
5. **G7 interim mitigation under I1**: a queue-full non-keyframe drop sets a discontinuity flag → decoder enters `WaitingForIDR` → rate-limited `RequestIDR` (all machinery already exists client-side) instead of silently feeding dependent frames. Telemetry from A0.3 counts occurrences.
6. **Coherent cursor snapshots** (R5a): publish one packed `AtomicU64` {x:i16, y:i16, shape:u8, seq:u8…} (coords fit i16 at ≤4K incl. the −1 sentinel) or a seqlock with acquire/release double-read; stress test that no mixed-sequence tuple is ever observed.
7. **G6 interim decision**: recommendation is to **fast-track Phase B** rather than patch the ceiling (raising `max_total_chunks` to `ceil(2 MB/1358)=1473` needs dynamic bitsets and a both-ends protocol change that Phase B deletes anyway). Fallback if B slips and A0 shows the band is hot: implement the 1473-chunk dynamic tracking as a stopgap.

*Exit gates:* crafted Stats envelope with `session_id = 250` (varint contains 0xFA) never triggers an IDR; cursor-only presents perform **zero** `SDL_UpdateYUVTexture` calls; every G7 drop produces a counted, controlled reset/IDR — never silent continuation; 512-vs-513-chunk unit test documents the ceiling.

## 5. Phase B — dedicated reliable video transport with defined credit semantics

1. **Separate TCP video socket** (never multiplexed on control — head-of-line blocking + `ControlChannel` assumes every payload is an Envelope). `TCP_NODELAY` both ends. Negotiated via the existing `ModeConfirm` port fields.
2. **Ordering fix**: client binds/listens **before** sending `StreamingReady` (today the receiver thread starts after — `src/main.rs:112-137`); host connects, then sends the initial IDR. Connection generation number so a stale socket cannot join a new stream.
3. **Versioned binary framing** (no JSON/heuristics): `{len, frameID:u32, captureSeq:u32, captureTs_us:u64, keyframe:u8, configGeneration:u32}` + session ID in the connect handshake; bounded max length.
4. **Progress/credit protocol per I4**: client sends cumulative `decoderAcceptedFrameID` + `decoderAcceptedBytes` (after send_packet-OK + drain-to-EAGAIN, with EAGAIN distinguished from errors); `decodedOutputFrameID` and `lastPresentedFrameID` reported separately as telemetry. Host caps outstanding-unaccepted bytes (initial window: ≥ 2× the largest IDR observed in A0, and above the decoder's measured retention depth — deadlock guard) and oldest-unaccepted capture age (initial: 250 ms). Bound violated → close/reset video connection, fresh IDR, counted.
5. **Keyframe contract**: IDR on video-connection ready, on decoder reset/request (rate-limited), and static-screen replay from the retained newest capture (I3) when an IDR is needed with no fresh capture. `MaxKeyFrameInterval=3600` + `MaxKeyFrameIntervalDuration=60` (≈60 s *maximum when honored* — log actual cadence; check every `VTSessionSetProperty` result, per codec).
6. **Client keeps a no-drop path to the decoder** (I1): deframe directly into the decode stage or through a bounded queue that backpressures the socket read instead of dropping — G7's condition becomes structurally impossible; delete the FrameAssembler/jitter-buffer crate, assembly deadlines, and the gray-frame heuristic; retain the minimal recovery path (decoder error / config-generation change / reconnect → flush-or-recreate → one rate-limited IDR request → resume on matching-generation IDR).

*Exit gates:* zero corruption under `iperf3` saturation + drag stress; **credit stall test** — stall decoder consumption while socket reads continue: sender credit stops advancing; **age test** — stalled receiver forces a clean reset+IDR within the bound, never unbounded catch-up; control/cursor/input traffic stays responsive while a large IDR is in flight.

## 6. Phase C — coalesced host gating, tuning, FramePacer removal

1. Implement I3 exactly: one encode in flight; `LatestFrameSlot` retains the newest pending capture while encode/credit gates are closed; on gate-open, submit it immediately (no new SCK callback required).
2. **Final-static-update gate** (the review-2 R3 scenario, must pass before FramePacer removal): close encoder/network credit → generate exactly one final capture → stop all captures → reopen credit → that exact capture sequence must display, with FramePacer off.
3. Remove FramePacer (prerequisites: A1.4 texture-resident cursor; B.5 static replay; this gate).
4. A/B (measured, not copied): `EnableLowLatencyRateControl` per codec (upstream evidence is H.264-only — verify HEVC honors it on this hardware), `PrioritizeEncodingSpeedOverQuality=true`, `MaxFrameDelayCount=0`; `minimumFrameInterval` 1/60 vs 1/120 (delivered fps + capture-age; the ~51 fps beat is a hypothesis to confirm); `queueDepth` 3/5/8 — smallest depth without SCK starvation drops.

## 7. Phase D — client decode/render split

1. Thread model: **decode thread** (socket deframe + decoder consumption coupled, per I1) → owned-frame handoff → **render thread** owning *all* SDL state (init, event pump, texture create/update/destroy, cursor composition, present). The existing `unsafe impl Send for Renderer` (`crates/renderer/src/lib.rs:36-50, 105-112`) is an assertion, not evidence — SDL objects do not migrate threads in the new design.
2. Handoff carries an **owned/refcounted decoded frame** (RAII `AVFrame` ref via the wrapper, or an explicit pooled-planes copy with a return path) into a latest-wins slot — never borrowed ffmpeg plane pointers (the decoder reuses that storage). Render thread drops older *decoded* frames freely (I1 concerns encoded frames only).
3. Decoder low-delay: `AV_CODEC_FLAG_LOW_DELAY` before open (CUVID display queue); SW: A/B threads 1/2/4 and slice-vs-frame threading. Gate: numbered-frame submit→output gap **measurably bounded** (expect ≈4 → ≤1 on CUVID; assert the measured bound, not zero).
4. Upload once per new frame from frame planes+strides (`SDL_UpdateYUVTexture` accepts pitch — the two explicit intermediate CPU copies in `extract_yuv`/`update_frame` are deleted); capability-test `SDL_PIXELFORMAT_NV12` on the deployed SDL/backend to skip the CPU deinterleave, I420 fallback.
5. Night Shift filter off the per-frame CPU UV pass if A0 data justifies it.

*Exit gates:* sustained 4K soak with no borrowed-frame or SDL cross-thread violations (ASAN/TSAN-clean where applicable); cursor latency measured at input-cadence + present cost with video decode saturated; decoded-but-never-presented counter behaves (only latest-wins drops).

## 8. Phase E — polish (after latency/glitch gates pass)

Virtual-display resilience from opendisplay's `Mac/VirtualDisplay.swift` (continuous HiDPI re-assertion, mirror-set detachment, arrangement persistence); keyboard forwarding (client send at `src/main.rs:454` is the only missing piece — host injector complete); sleep/wake + reconnect lifecycle; long soak + fault-injection suite. Optional: re-wire bitrate adaptation (currently dead code — see analysis G4 retraction) against the *new* telemetry, if 40 Mbps ever proves contention-limited on the wired link.

---

## 9. Validation gate summary

| Gate | Test | Phase |
|---|---|---|
| Baseline integrity | p50/p95/max video + cursor timing recorded **before** behavioral fixes | A0 |
| Typed parsing | Stats envelope with 0xFA-bearing session varint never triggers IDR | A1 |
| G6 ceiling | 512 vs 513 chunk unit test; real IDR-size histogram vs oversize drops | A0/A1 |
| G7 | Saturate the 4-slot queue: every encoded-P drop → counted, controlled reset/IDR; never silent continuation | A1 |
| Cursor residency | Cursor-only redraw: zero texture uploads | A1 |
| Cursor coherence | Publication/render race: every observed X/Y/shape tuple belongs to one sequence | A1 |
| ACK semantics | Decoder stalled, socket flowing → sender credit halts | B |
| Bounded age | Receiver stall + saturation → bytes/age within limits or clean reset+IDR | B |
| Final static update | Credit closed → one last capture → credit reopened → that capture displays, FramePacer off | C |
| Low delay | Numbered-frame submit→output gap: measured bound, off vs on | D |
| Thread ownership | 4K soak, no lifetime/cross-thread violations | D |

## 10. Effort (initial — revise after A0 and a B spike)

A0: 0.5–1 day · A1: 1–1.5 days · **B spike first** (0.5–1 day: minimal TCP video path end-to-end, no credit, to de-risk the estimate) · B complete with credit + fault-injection tests: 2–4 days · C: 0.5–1 day · D: 2–3 days · E: open-ended. Review 2 (R10) is right that v1.1's "Phase B ≈ one day" was not a planning-grade estimate; these figures are inputs to revision, not commitments.

## 11. Decision log

| Decision | Choice | Why |
|---|---|---|
| Video transport | Dedicated TCP socket, binary framing | HOL-blocking + framing ambiguity rule out multiplexing on control |
| Credit boundary | Cumulative decoder-accepted (send_packet-OK + drain-to-EAGAIN) | Socket receipt bounds nothing; presentation recouples host to vsync |
| Host coalescing | Gate consumption of `LatestFrameSlot`, store always | Preserves final-static-update delivery; RESC's primitive already correct |
| Protobuf wiring | New `Protocol` SwiftPM target at `Sources/Protocol`, generated sources committed | Matches script's existing output dir; codegen is tool/network-dependent |
| G6 stopgap | Skipped in favor of fast-tracking B (fallback: 1473-chunk dynamic tracking) | The patch is protocol surgery Phase B deletes |
| Cursor snapshot | Single packed `AtomicU64` (seqlock fallback) | Cheapest coherent publish; coords fit i16 at ≤4K |
| SDL threading | All SDL state on the render thread | `unsafe impl Send` is unproven; SDL's model is single-threaded |
| Decoder handoff | Owned/refcounted frames or pooled copies only | ffmpeg reuses plane storage; borrowed pointers are UAF across threads |

## 12. Traceability (review 2 → this plan)

R1/G7 → §1, A1.5, B.6, gate G7 · R2 cursor model → §1, I2, §9 targets · R3 coalescing → I3, C.1–2, final-static gate · R4 ACK semantics → I4, B.4, ACK gates · R5 G6 confidence → §1 hypotheses, A0.5 · R5a → A1.6, coherence gate · R6 → A1.1 (verified: script outputs to `Sources/Protocol`; `Package.swift` compiles only `Sources/RemoteDisplayHost`) · R7 → A0.3 · R8 → §13 errata · R9 → §1 confidence discipline, measured-bound gates · R10 → §10 · §4 A0/A1 split → §3/§4 · §4 Phase B/C/D amendments → §5/§6/§7.

## 13. Errata against `LATENCY_GLITCH_ANALYSIS.md` v1.1 (recorded here; the analysis file is intentionally unmodified)

1. §2.1 pipeline diagram and G1 say "≤1362 B" payloads (a stale plan.md figure); actual is **1358** (`ProtocolConstants.swift:18`). G6's arithmetic already uses 1358 correctly.
2. "Three full-frame CPU copies" should read "**two explicit CPU copies plus one full texture upload**" (whether `SDL_UpdateYUVTexture` copies again internally is backend-dependent).
3. "opendisplay has *no* periodic keyframes" should read "≈60 s maximum interval when honored" (`MaxKeyFrameInterval=3600` + `Duration=60`); actual cadence must be logged, especially under low-latency rate control.
4. §3's "TCP retransmit stalls are single-digit ms and absorbed by the pendingSends gate" is an unproven magnitude, and `pendingSends` is driven by local `.contentProcessed` callbacks (not peer consumption) — superseded by I4's credit design.
5. L7's "the pointer exhibits the full video-pipeline latency" is mechanically too strong — superseded by the corrected cursor model in §1.
6. G6's "repeat-drop loop" is a runtime hypothesis (the assembler does not itself request an IDR; the request arises later via decoder errors): rejection VERIFIED, discontinuity HIGH, loop-dynamics MEASURE.
7. L1's "expect 0 gap with low-delay on" hardened to a measured-bound assertion (§7.3).
