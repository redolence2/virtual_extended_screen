# RESC Latency & Glitch Root-Cause Analysis, and opendisplay Comparison

| | |
|---|---|
| **Date** | 2026-07-29 |
| **Author** | Claude (Fable 5) analysis session; code read directly, docs digested by a subagent |
| **RESC repo** | `/Users/moyunfei/Downloads/personal/AGI/remote_extended_screen` @ commit `12b87d1` (branch `main`, clean) |
| **opendisplay repo** | `/Users/moyunfei/Downloads/personal/AGI/opendisplay` @ commit `b3311e1` (v1.13.0, upstream [github.com/peetzweg/opendisplay](https://github.com/peetzweg/opendisplay)) |
| **Purpose** | Root-cause the two user-reported problems (~200ms pointer latency on the extended screen; visual glitches during window drags / abrupt content changes), compare against opendisplay, and recommend a path. Written to be reviewed from scratch — no conversation context required. |
| **Status of claims** | Each finding is tagged **VERIFIED** (read directly in code at the cited location), **HIGH** (strong inference corroborated by code + docs), or **MEDIUM** (mechanism identified in code, magnitude needs a runtime measurement). A reviewer checklist is in §9. |
| **Revision** | **v1.1** — updated after the independent review in `LATENCY_GLITCH_ANALYSIS_review.md`. Two review-discovered root causes added (G6 chunk ceiling, L9 cursor-path texture upload), one finding retracted (G4), one number corrected (G3 cadence math), plan restructured per §7. Dispositions in §11. |

All RESC paths below are relative to the RESC repo root; all opendisplay paths are relative to the opendisplay repo root.

---

## 0. TL;DR

RESC (this repo) is a Mac→Ubuntu virtual extended display: Swift host (`mac-host/`) creates a `CGVirtualDisplay`, captures with ScreenCaptureKit, encodes H.264/HEVC with VideoToolbox, and streams over **custom chunked UDP**; a Rust client (`ubuntu-client/`) reassembles, decodes with ffmpeg, and renders fullscreen with SDL2. opendisplay is a mature Mac→**iOS** sidecar app with the same host-side concept but a fundamentally different transport/pipeline philosophy: **TCP + backpressure + render-immediately**, no jitter buffer, no periodic keyframes.

**Findings, in one paragraph.** The glitches have three interacting structural causes: unprotected UDP plus a forced keyframe every 0.5 s (each 4K IDR is a hundreds-of-packets unpaced burst; one lost packet discards the frame and breaks the codec reference chain until the next IDR — §3 G1/G2); a live host bug in which the byte-scan used to detect client IDR requests false-positives on the client's own 100 ms stats messages (§3 G3); and — found by the independent review — a **deterministic size ceiling**: the negotiated 512-chunk limit caps assemblable frames at 695,296 bytes while the separately negotiated `maxFrameBytes` permits 1.67 MB at 4K/40 Mbps, so large IDRs (exactly the ones produced by abrupt full-screen changes) are dropped by construction even on a perfect network (§3 G6). The ~200 ms latency is an accumulation of buffering stages the project's own 20–45 ms budget (plan.md) never counted: ffmpeg decoder-internal delay (CUVID display queue ≈ 4 frames, or software frame-threading ≈ 3 frames), a 4-deep frame channel, a single client thread that serializes decode + SDL events + vsync-blocked present, three full-frame CPU copies per 4K frame, and encoder settings that prioritize quality/pipelining over latency (§4). The pointer lags the same amount because the cursor overlay only redraws once per iteration of that congested loop, and — also review-found — every cursor-only redraw re-uploads the complete cached 4K YUV frame (~12 MB) to the GPU before presenting (§4, L7/L9). RESC's FramePacer hack keeps the loop permanently congested even on an idle desktop.

**Recommendation: fix RESC by porting opendisplay's architecture (transport + pipeline discipline), not by building an Ubuntu client for opendisplay.** opendisplay has zero Linux/receiver code to reuse (its receiver is pure iOS), so an Ubuntu client is a from-scratch build either way; and opendisplay's Mac side would need a fork for this use case anyway (H.264-only ≤ 18 Mbps vs RESC's 4K HEVC 40 Mbps, no keyboard/right-click in its protocol, @2x phone-panel sizing, no Night Shift sync). Every property that makes opendisplay feel good is a portable technique, demonstrated concretely in its source with explanatory comments. Porting plan with parameters in §7.

---

## 1. Symptoms under investigation

Reported by the user (single-user personal setup, Mac host + Ubuntu machine as the extended display, LAN):

1. **Pointer latency ~200 ms**: moving the mouse (physically attached to the Mac; pointer moved onto the virtual display) takes ~200 ms to be reflected on the Ubuntu monitor.
2. **Visual glitches**: corruption/artifacts during window dragging and on abrupt full-screen content changes.

Context worth knowing: RESC's own `plan.md` publishes a target end-to-end budget of **20–45 ms** (capture 4–12, encode 2–8, LAN <1, jitter 0–8, decode 2–10, render+vsync 0–16) and a cursor budget of **2–5 ms** (host-driven mode). Nothing in the repo's docs accounts for 200 ms. This analysis reconciles that gap mechanically.

---

## 2. The two systems, as actually built

### 2.1 RESC (this repo)

```
Mac host (Swift, mac-host/Sources/RemoteDisplayHost/)
  CGVirtualDisplay (private API, ObjC bridge in Sources/VirtualDisplay/)
  → ScreenCaptureKit  (NV12, minimumFrameInterval=1/60, queueDepth=3, showsCursor=false)   DisplayCapturer.swift
  → LatestFrameSlot   (latest-wins slot, lock + semaphore — good, no queue here)           LatestFrameSlot.swift
  → encoder thread    (waitAndTake → VTCompressionSessionEncodeFrame)                      main.swift:136
  → VideoToolbox      (RealTime=true, no B-frames, CABAC; keyframe FORCED every 0.5s;
                       PrioritizeEncodingSpeedOverQuality=FALSE; no MaxFrameDelayCount;
                       no LowLatencyRateControl)                                           VideoEncoder.swift:95-129, main.swift:116
  → NALUPackager      (AVCC→Annex B, SPS/PPS on keyframes)
  → VideoSender       (chunked UDP, ≤1362B payload/packet, tight sendto loop, NO pacing,
                       NO FEC, NO retransmit)                                              VideoSender.swift:51-113

Ubuntu client (Rust, ubuntu-client/)
  UDP receiver        (SO_RCVBUF 2MB, 100ms read timeout)                                  crates/net-transport/src/video_receiver.rs
  → FrameAssembler    ("jitter-buffer" crate — actually only reassembly: 4 slots,
                       100ms assembly deadline, whole frame dropped on any missing chunk)  crates/jitter-buffer/src/lib.rs
  → sync_channel(4)   (keyframes block, non-keyframes dropped when full)                   src/main.rs:124, video_receiver.rs:193-209
  → decode-render thread (ONE thread: drain channel → decode ALL queued frames →
                       render last → SDL event pump → vsync present)                       src/main.rs:339-532
  → ffmpeg decode     (h264_cuvid/hevc_cuvid NVDEC if available, else SW with
                       4 frame-threads; GPU→CPU transfer; per-pixel NV12→I420 in Rust)     crates/video-decode/src/lib.rs
  → Renderer          (extract_yuv copies → update_frame re-copies + CPU Night Shift
                       UV pass → SDL_UpdateYUVTexture → present with .present_vsync())     crates/renderer/src/lib.rs:144-237
```

Side channels: TCP control (protobuf `Envelope`, u32_le length framing, port 9870); UDP input events client→host (9872); UDP cursor position host→client at 120 Hz (9873), drawn client-side as an overlay sprite (capture excludes the cursor). Keyboard forwarding is **not implemented** (client-side send is a literal `// TODO`, `src/main.rs:454`; host has a complete HID→CGKeyCode map in `EventInjector.swift`). A `FramePacer` (1×1 window toggling alpha at 60 Hz on the virtual display, `FramePacer.swift`) forces the compositor to emit frames even when idle.

### 2.2 opendisplay (v1.13.0)

```
Mac (Mac/MacSender.swift, ~1290 lines)
  CGVirtualDisplay (Mac/VirtualDisplay.swift — with continuous HiDPI-mode re-enforcement,
                    mirror-set detachment, arrangement persistence)
  → ScreenCaptureKit  (NV12, minimumFrameInterval=1/120 ← deliberate, see §5; queueDepth=8;
                       showsCursor=false — cursor streamed as position + PNG sprite on the
                       control channel, drawn by the receiver on its ~2ms input path)
  → backpressure gate (maxPendingEncodes=1: skip capture while an encode is in flight —
                       latest-wins BEFORE the encoder; maxPendingSends=3: skip capture
                       while >3 TCP sends unacked. Drops are pre-encode → reference chain
                       stays valid, next frame is a normal P-frame)                        MacSender.swift:127-160, 1104-1128
  → VideoToolbox      (H.264 only; EnableLowLatencyRateControl=true;
                       PrioritizeEncodingSpeedOverQuality=true; MaxFrameDelayCount=0;
                       MaxKeyFrameInterval=3600 — i.e. NO periodic keyframes; forced IDR
                       only on (re)connect or receiver "kf" request)                       MacSender.swift:1039-1076
  → framed TCP        ([4B big-endian length][Annex B or JSON], TCP_NODELAY,
                       over Wi-Fi (Bonjour _opensidecar._tcp) or USB (usbmuxd dial))

iOS receiver (iOS/PhoneReceiver.swift, ~930 lines — the part with NO Linux equivalent)
  NWListener :9000 → deframe → JSON-vs-video heuristic (<32KB ∧ starts '{' ∧ no NUL)
  → Annex B parse → CMSampleBuffer → AVSampleBufferDisplayLayer with
    kCMSampleAttachmentKey_DisplayImmediately = true  (render the moment it decodes;
    no PTS scheduling, no jitter buffer, no frame queue)                                   PhoneReceiver.swift:727-741
  + full latency instrumentation: per-frame {"cap":…,"snd":…} JSON prefix stamped by the
    Mac, NTP-style clock offset from timestamped ping/pong (lowest-RTT sample wins),
    p50/p95 e2e + capture→socket + decode + photon-time overlay                            PhoneReceiver.swift:395-421, 744-835
```

Static-screen handling: instead of forcing idle frames (RESC's FramePacer), the Mac keeps the last pixel buffer and **re-encodes it as an IDR** when a reconnect happens on an unchanged screen (`MacSender.swift:818-824`). Recovery model: TCP never loses data, so the only resync need is "decoder joined mid-GOP" → receiver sends `{"type":"kf"}`.

Maturity note: 167+ merged PRs of lifecycle fixes (sleep/wake, half-open connection recovery, zombie displays, arrangement memory, transport migration USB↔Wi-Fi). License: **GPL-3.0 since v0.5.0; releases ≤ v0.4.x remain MIT**.

---

## 3. Root cause — the glitches

**G1. Unprotected UDP × codec reference chains (VERIFIED — design-level).**
Every encoded frame is split into ≤1362-byte UDP chunks (`VideoSender.swift:51-113`). There is no FEC, no retransmission, no pacing. The assembler discards the *entire frame* if any chunk is missing after 100 ms (`crates/jitter-buffer/src/lib.rs:106,211-228`). A discarded P-frame breaks the H.264/HEVC reference chain; subsequent frames decode against wrong references until the next IDR (severity varies with referencing structure and decoder concealment, but with B-frames and reordering disabled essentially every frame is a reference, so visible corruption is the common case). The client's mitigations (decoder recovery state machine, gray-frame variance detector, `error_concealment=0`, IDR requests — `crates/video-decode/src/lib.rs:171-275`) shorten the corruption window but cannot remove it. RESC's own `TECHNICAL_REVIEW.md` §5 concedes the residual: "brief gray or corrupted frames can still appear during rapid content changes before the next keyframe arrives."

**G2. Forced IDR every 0.5 s + unpaced keyframe bursts (VERIFIED).**
`main.swift:116` sets `keyframeIntervalSeconds: 0.5` (overriding the 1.0 default in `VideoEncoder.swift:25`). A 4K HEVC IDR at 40 Mbps is roughly 0.5–2 MB → **350–1400 UDP packets emitted back-to-back in a tight `sendto` loop** with no inter-packet spacing (`VideoSender.swift:61-113`). That burst is a loss lottery twice per second: switch/AP queues and the receiver's 2 MB `SO_RCVBUF` can both overflow, and `sendto` itself can drop locally with ENOBUFS (only the *first* send error is ever printed, `VideoSender.swift:105-108`). Window drags make every frame large, raising per-frame loss probability exactly when the user is watching most closely — hence "glitches particularly when dragging." Note the vicious cycle: the periodic IDR exists to *bound* loss-corruption (G1), but each IDR is itself the most loss-prone packet burst in the stream.

**G3. Live bug: IDR-request detection false-positives on stats traffic (VERIFIED mechanism; frequency MEDIUM — needs a log check).**
The host never parses inbound protobuf during streaming. Instead `HostSession.handleStreamingMessage` (`HostSession.swift:135-160`) scans the raw bytes for `0xFA` — the Envelope tag of `request_idr` (field 31, wire type 2 → (31<<3)|2 = 250) — and, on any hit, forces a keyframe (rate-limited to 250 ms). Two false-positive sources, both verified in the client encoder path (`crates/net-transport/src/control_channel.rs:136-154`):
  - Every client message embeds the **host-assigned random u64 `session_id` as a varint** (`HostSession.swift:91` generates it; client echoes it in every envelope). A random u64's varint is ~9–10 bytes, all but the last in 0x80–0xFF; the probability that some byte equals 0xFA is ≈ 7% **per session**. In such a session, *every* stats message (sent every 100 ms, `src/main.rs:203-260`) triggers the forced-keyframe path, whose 250 ms rate limit yields up to ~4 forced IDR/s on top of the 2/s scheduled cadence — roughly **2–3× the intended IDR rate** for the session's entire lifetime (v1.1 correction: originally overstated as "8×"; forced and scheduled keyframes also partially overlap). Symptom signature: some sessions glitch far more than others, from connection until restart.
  - `Stats.packet_loss_rate` / `frame_drop_rate` are IEEE-754 float32 fields whose payload bytes are arbitrary; any byte equal to 0xFA triggers the same path sporadically — most likely under load, when the rates are non-zero and changing.
  Verification is trivial: correlate host log line `"[RESC] IDR requested by client (rate-limited)"` with the client not having logged any `Requesting IDR`.

**G4. ~~Bitrate-adaptation coupling~~ — RETRACTED in v1.1 (review finding, verified).**
The original claim — that keyframe-burst loss drags the adaptive bitrate down after glitch episodes — is **false at commit `12b87d1`**: `BitrateAdapter` has no call site anywhere in `mac-host/Sources/` (verified by grep; the only references are inside `BitrateAdapter.swift` itself), and inbound `Stats` are consumed unparsed by the 0xFA scanner (`HostSession.swift:159`). The class implements the behavior the docs describe (plan.md's ×0.8 / ×1.05 algorithm) but is dead code. Consequence for the plan: bitrate adaptation is a *future wiring decision*, not a live interaction — and the docs-based claim slipping through is exactly why §9's "verify call sites, not just class existence" discipline matters.

**G5. Gray-frame heuristic can discard legitimate frames (VERIFIED mechanism, minor).**
The software-decode path samples 16 luma pixels and discards frames with variance < 4 and mean 100–160 as "concealment output" (`crates/video-decode/src/lib.rs:239-261`) — a legitimately uniform gray screen (dark-mode apps, full-screen dialogs) can be misclassified, forcing spurious IDR requests and frame skips during "abrupt display changes," which is one of the two reported glitch triggers.

**G6. Deterministic 695 KB frame ceiling — large IDRs are dropped even on a perfect network (VERIFIED; found by the independent review).**
Three independently-negotiated limits are mutually inconsistent:

- Max UDP payload: `maxVideoPayloadBytes = 1400 − 42 = 1358` (`ProtocolConstants.swift:14-18`).
- Host advertises `max_total_chunks_per_frame = 512` (`HostSession.swift:262`); the client's assembler hard-rejects any frame whose metadata claims more (`crates/jitter-buffer/src/lib.rs:137-150`, `oversize_drops`), and its bitset is sized to exactly 512 (`[u64; 8]`).
- Host advertises `maxFrameBytes = min(20 × avg, 2 MB)` (`ProtocolConstants.swift:64-67`) — at 4K HEVC 40 Mbps/60 fps: avg ≈ 83,333 B → **≈ 1.67 MB allowed**.

But the largest *assemblable* frame is `512 × 1358 = 695,296 bytes`. The sender happily chunks anything up to `UInt16.max` chunks (`VideoSender.swift:59`), so any encoded frame in **(695 KB, 1.67 MB]** passes every host-side policy, is transmitted in full, and is then discarded by construction at the client — zero packet loss required. High-entropy IDRs (full-screen content changes, the exact reported trigger) are the frames most likely to land in that band; each drop breaks the reference chain (G1), triggers a `RequestIDR`, and the replacement IDR of the same scene is likely oversized again — a repeat-drop loop. Interaction with G3: spurious forced keyframes multiply the number of huge IDRs generated. This may well be the *primary* mechanism behind "glitches on abrupt display changes"; Phase A logging (encoded-frame sizes + reason-coded drops) confirms or refutes it cheaply, and the TCP migration (Phase B) removes the ceiling entirely.

**Why opendisplay has none of this class:** TCP (`noDelay`) never surfaces loss to the codec, so there is no reference-chain breakage, no reassembly deadline, no gray-frame forensics — and *because* loss is impossible, it disables periodic keyframes entirely (`MaxKeyFrameInterval=3600`, comment at `MacSender.swift:1066-1069`: "No periodic IDRs: each one is a bitrate spike → transmit-time hiccup. TCP never loses data, and we force a keyframe on reconnect/drop."). On a wired LAN, TCP's cost (occasional retransmit stall) is single-digit milliseconds and is absorbed by the pendingSends backpressure gate rather than accumulating as delay.

---

## 4. Root cause — the ~200 ms latency

The docs' 20–45 ms budget measures per-stage *processing* time. The observed latency is dominated by *queues* between stages. Itemized stack, worst-case during activity (e.g., a window drag), 60 fps source:

| # | Stage | Est. contribution | Evidence & confidence |
|---|---|---|---|
| L1 | **ffmpeg decoder-internal delay.** NVDEC path: ffmpeg's cuvid wrapper sets parser `ulMaxDisplayDelay = 4` unless `AV_CODEC_FLAG_LOW_DELAY` is set — RESC never sets it (`crates/video-decode/src/lib.rs:75-130` sets only `hw_device_ctx`). SW fallback: frame-based threading with `count: 4` (`lib.rs:143-147`) delays output by N−1 frames. | **50–67 ms** (3–4 frames) | HIGH (mechanism VERIFIED in RESC code; the `=4` default is from ffmpeg `cuviddec.c` — reviewer: confirm against installed ffmpeg, §9.3) |
| L2 | **Single-threaded client loop.** One thread does: drain channel → decode *all* queued frames → render last → SDL event pump → `canvas.present()` **with vsync** (`renderer/src/lib.rs:146` `.present_vsync()`; loop at `src/main.rs:339-532`). Present blocks up to 16.7 ms; during that block, new frames queue. 4K SW decode of a several-frame batch adds tens of ms per iteration. | **20–50 ms** (present block + serialization) | VERIFIED (structure) / MEDIUM (magnitude) |
| L3 | **Frame channel depth 4** between receiver and decoder (`src/main.rs:124`), keyframes block-send (`video_receiver.rs:195-203`). Under L2's slow iterations the channel runs full. | **0–66 ms** (0–4 frames) | VERIFIED (structure) |
| L4 | **Three full-frame CPU copies per frame**: (1) `extract_yuv` copies planes to fresh `Vec`s, incl. a scalar per-pixel NV12→I420 deinterleave on the NVDEC path (`video-decode/src/lib.rs:289-331`) after an explicit GPU→CPU `av_hwframe_transfer_data`; (2) `update_frame` re-copies row-by-row + full-plane CPU pass over U and V when Night Shift is active (`renderer/src/lib.rs:180-221`); (3) `SDL_UpdateYUVTexture` uploads to GPU again. At 4K ≈ 12 MB/frame × 3. | **5–15 ms** | VERIFIED (code) / MEDIUM (magnitude) |
| L5 | **Encoder tuned against latency**: `PrioritizeEncodingSpeedOverQuality=false` (`VideoEncoder.swift:126-129`), no `MaxFrameDelayCount=0`, no `EnableLowLatencyRateControl` — VideoToolbox may pipeline frames internally. | **0–20 ms** (0–1+ frame) | VERIFIED (settings) / MEDIUM (magnitude) |
| L6 | **Capture rate-limiter beat**: `minimumFrameInterval = 1/60` (`DisplayCapturer.swift:60`) makes SCK skip frames that arrive marginally early; RESC's own runtime warning fires below 50 fps (`DisplayCapturer.swift:134`). opendisplay hit the same bug, measured ~51 fps, and requests 1/120 as the fix (comment, `MacSender.swift:444-448`). Lower delivered fps ⇒ higher average frame age. | **~3 ms avg** (+ jitter) | HIGH |
| L7 | **FramePacer keeps the pipeline saturated** (`FramePacer.swift`): 60 fps of encode/decode even on an idle desktop, so L2's loop is never quiet. The **cursor overlay only redraws once per loop iteration** (`src/main.rs:505-530`) — so the 120 Hz cursor UDP feed (CursorTracker) is repainted at the pace of the congested video loop. This is why the *pointer* exhibits the full video-pipeline latency even though its data path is ~2 ms. | (couples cursor to L1–L4) | VERIFIED (structure) |
| L8 | Capture/encode/network baseline | ~15–25 ms | consistent with docs' own budget |
| L9 | **Cursor-only redraws re-upload the full video frame** (v1.1, found by review). Every `present_with_cursor()` call runs `update_and_copy()` → `SDL_UpdateYUVTexture` of all three planes (`renderer/src/lib.rs:256-263, 53-61`) — ≈ 12 MB per cursor-driven present at 4K I420, followed by the vsync-blocked present, even when no new video frame exists. The renderer has no "re-composite existing texture + overlay" path. | adds upload+present cost to *every* cursor move | VERIFIED (structure) / MEDIUM (magnitude) |

**Sum during activity: ≈ 130–200+ ms**, matching the report. The two structural killers are L1 (decoder-internal queue — invisible to RESC's docs, which budget "decode 2–10 ms" as throughput) and L2/L3/L7 (the client's single-loop design). None of these are load-bearing features; all are removable.

**Cursor-specific note:** in grabbed mode (Ubuntu-side mouse) the client renders its *own* position immediately, so the complaint almost certainly concerns host-driven (LocalControl) mode — confirmed consistent with L7+L9: the cursor pays one congested loop iteration *plus* a full-frame GPU upload per repaint. `plan.md`'s 2–5 ms cursor budget implicitly assumed an uncongested render loop and a texture-resident redraw path; neither exists at HEAD. Fixing L9 (upload only on new video; cursor-only presents reuse the resident texture) is also a hard prerequisite for deleting FramePacer — on an idle screen with no video frames arriving, the cursor path must not depend on fresh uploads.

**How opendisplay bounds the same path:** at most 1 frame in the encoder + 3 in TCP flight (pre-encode drops keep it there), decoder is VideoToolbox with no display queue, `DisplayImmediately` bypasses PTS scheduling, and the receiver's cursor is drawn by the UI layer on the touch-input path, decoupled from video (comment at `MacSender.swift:202-206`: cursor baked into video would carry "~30ms perceived"; the control-channel path is "~2ms"). Their A/B testing even rejected their own Metal renderer because the system layer reached glass faster (CHANGELOG v0.3.0).

---

## 5. Technique-by-technique comparison

| Concern | RESC today | opendisplay | Portable? |
|---|---|---|---|
| Video transport | chunked UDP, no FEC/retransmit/pacing | TCP + `TCP_NODELAY` (+ `serviceClass: .interactiveVideo` on iOS listener) | ✅ trivially — RESC already has a TCP control channel with framing |
| Loss recovery | assembly deadline, IDR requests, decoder state machine, gray detector | *none needed* (TCP) + "kf" resync on mid-GOP join | ✅ (as deletion) |
| Keyframes | forced every 0.5 s | none periodic (interval 3600); IDR on reconnect/request only; replay-last-frame-as-IDR for static-screen reconnects | ✅ |
| Latency control | queues (channel 4, decoder-internal, vsync) absorb bursts | backpressure: `maxPendingEncodes=1`, `maxPendingSends=3`, drop **before** encode, latest-wins; drops split into enc↓/net↓ counters for the HUD | ✅ |
| Encoder flags | RealTime, no B-frames; quality-over-speed; no delay cap | + `EnableLowLatencyRateControl`, `PrioritizeEncodingSpeedOverQuality=true`, `MaxFrameDelayCount=0` | ✅ |
| Capture | 1/60 min interval (beat bug), queueDepth 3 | 1/120 min interval, queueDepth 8 | ✅ |
| Idle screen | FramePacer forces 60 fps always | accept variable rate; keep `lastPixelBuffer`, re-encode as IDR on reconnect | ✅ |
| Receiver render | queue → batch decode → vsync present, single thread | decode-and-display-immediately, no queue | ✅ concept (Linux impl: low-delay ffmpeg + decoupled present) |
| Cursor | 120 Hz UDP position + client overlay (good design) **but** repainted by the congested video loop | 120 Hz position + PNG sprite on control channel, drawn on the input path, independent of video | ✅ (thread split) |
| Instrumentation | packet/frame counters only | per-frame cap/snd stamps, NTP-style clock sync, e2e/encode/decode/photon p50-p95 overlay, both ends logged in one file | ✅ — **port first** |
| Codec | H.264 + HEVC, 4K @ 40 Mbps HEVC | H.264 only, ≤ 18 Mbps (phone panels; HEVC is an open roadmap item) | RESC is *ahead* here |
| Input | mouse move/click/scroll (works), keyboard (host ready, client send is TODO), grab mode | touch-as-left-click + scroll only; right-click/keyboard are open roadmap items | RESC is ahead |
| Display sizing | arbitrary WxH from client args (1x) | @2x HiDPI derived from phone panel; continuous HiDPI/mirror-set/arrangement enforcement (`Mac/VirtualDisplay.swift`) | enforcement loop worth porting |
| Extras | Night Shift → client warm filter | sleep/wake lifecycle, transport migration, version handshake (COMPATIBILITY.md) | selective |

---

## 6. Options analysis

### Option A (recommended): keep RESC, port opendisplay's architecture into it

- The Ubuntu client must be written/kept under **every** option — opendisplay has no Linux receiver, and "additional client platforms" is an open exploratory roadmap item (#15). The client-side latency work (low-delay decode, single-copy upload, thread split) is therefore common to all paths.
- RESC's Mac host already has the features this use case needs that opendisplay lacks: HEVC 4K @ 40 Mbps (4K text at opendisplay's 18 Mbps H.264 ceiling will be visibly softer), arbitrary resolution negotiation, right-click + grab-mode input with keyboard 95% plumbed, Night Shift sync, cursor shape enum.
- Everything RESC lacks is a small, well-understood edit demonstrated in opendisplay's source *with explanatory comments* (§5 table, all "✅ portable").
- What gets **deleted** from RESC (net simplification): VideoSender chunking, FrameAssembler/jitter-buffer crate, assembly deadlines, gray-frame detector, most of the decoder recovery machinery, the 0xFA scan, FramePacer.

### Option B: build an Ubuntu receiver speaking opendisplay's protocol

For completeness — the protocol is genuinely simple and an Ubuntu receiver is feasible: listen on TCP :9000, advertise `_opensidecar._tcp` via Avahi with `id`/`pv` TXT records, send `hello` JSON `{pixelsWide, pixelsHigh, scale, device, id, pv}`, then deframe `[4B len][payload]`, route JSON-vs-video by the documented heuristic (<32 KB ∧ starts `{` ∧ no NUL), parse Annex B H.264 → ffmpeg → render; send `touch`/`scroll`/`ping`/`stats` back; draw the PNG cursor sprite from `cursorImg` messages. Estimated 1–1.5k lines of new Rust (some RESC crates reusable after the same low-latency fixes Option A needs).

Why it still loses for this use case: the stock Mac app caps quality at 18 Mbps H.264 (`StreamQuality`, `MacSender.swift:30-64`), derives the virtual display from an @2x phone-panel model, and its input protocol has no right-click/keyboard — so a fork of the Mac app is required anyway for 4K sharpness and input parity, at which point you maintain a GPLv3 Swift fork *plus* the new receiver *plus* re-implementing Night Shift, for a Mac binary whose distinctive strengths (USB/usbmuxd transport, iOS lifecycle handling, App Store update gates) don't apply to a fixed Ethernet-connected Linux box.

When B *would* be right: if the goal shifted from "best experience on my Ubuntu 4K monitor" to "minimal maintenance, riding upstream releases, phone-grade quality is fine, no Ubuntu-side input needed."

### Licensing

opendisplay is GPL-3.0 (since v0.5.0; ≤ v0.4.x remain MIT). Techniques and parameter choices are not copyrightable; private (non-distributed) use imposes zero obligations; if RESC is ever published with verbatim-copied opendisplay code, it must be GPL-3.0 — or copy only from the MIT-licensed ≤ v0.4.x tags.

---

## 7. Recommended plan (v1.1 — restructured per the independent review: surgical fixes and a measured baseline land *before* the transport rewrite)

**Phase A — observable baseline + surgical correctness fixes (no architecture change).**
1. **Real capture metadata**: `DisplayCapturer` currently discards sample timing and `main.swift:140-145` synthesizes PTS from a frame counter. Introduce a capture-frame struct `{pixelBuffer, captureTimestamp (callback time or mapped SCK PTS), captureSeq}` through `LatestFrameSlot`; carry `{frameID, captureTs, encoderOutTs, byteLen, keyframeFlag, configGeneration}` in the video framing. Client records socket-complete / decode-submit / decode-out / upload-done / present-return, plus cursor seq → cursor-present separately (video timestamps cannot validate the cursor path). Clock offset via timestamped ping/pong on the control channel, lowest-RTT sample wins (`PhoneReceiver.swift:395-407` pattern). Note: software capture→present is *not* photon time — keep the optical check (§8).
2. **Log encoded frame sizes + reason-coded drops** (host: bytes/chunks per frame; client: `oversize/timeout/eviction` counters surfaced per-interval) — this confirms or refutes G6 within minutes of a window-drag session.
3. **Replace the 0xFA scan with typed protobuf decoding** (kills G3). The codegen pipeline already exists — `tools/generate_proto.sh` builds `protoc-gen-swift` — but no generated `.pb.swift` is wired into the target; run it, add the output to `mac-host`, decode `Envelope` properly. (The hand-rolled encoding in `HostSession.swift` was explicitly "Phase 3" scaffolding.)
4. **Fix the `pendingForceKeyframe` data race** (`VideoEncoder.swift:48,160-163,206` — written from the control-channel callback thread via `main.swift:200-202`, read/cleared on the encoder thread, unsynchronized): make it atomic or encoder-queue-confined. After Phase B removes periodic keyframes, a lost forced-IDR flag would mean an indefinitely stale/black client, so this must be airtight first.
5. **Split cursor-only presentation from video upload** (kills L9): upload/replace the GPU texture only when a new decoded frame arrives; cursor-only redraws re-composite the resident texture + overlay + present. Cheap, high-value, and a prerequisite for FramePacer removal.

*Acceptance:* a Stats envelope crafted with `session_id = 250` (0xFA varint) never triggers an IDR; every large-frame rejection is explained by size/reason logs; cursor-only presents perform zero `SDL_UpdateYUVTexture` calls (count them); baseline p50/p95/max video and cursor latency visible on both ends.

**Phase B — dedicated reliable video transport (kills G1/G2/G6).**
6. **Separate TCP video socket** — do *not* multiplex raw video onto the control connection (`ControlChannel` assumes every length-prefixed payload is a protobuf Envelope, and large video frames would head-of-line-block keyframe requests, display settings, stats, and future keyboard traffic). Negotiate it in `ModeConfirm` (the port fields already exist), with `TCP_NODELAY` both ends, a small **binary** frame header `{len, frameID, captureTs, keyframeFlag, configGeneration}` (no JSON/Annex-B heuristics), bounded max length, and a connection generation so a stale socket can't join a new stream.
7. **Fix readiness ordering**: today the client sends `StreamingReady` *before* starting its receiver (`src/main.rs:112-137` — benign under UDP, wrong for TCP). New order: bind/listen → advertise ready → host connects → host sends initial IDR.
8. **Backpressure that bounds age and bytes, not just frame count**: pre-encode gating (`pendingEncodes ≤ 1`, latest-wins — `MacSender.swift:127-160` pattern) plus an outstanding-**byte** budget and a max oldest-frame **age** on the send side (an IDR can be 20× a P-frame, so `pendingSends ≤ 3` alone is not an end-to-end bound; note `NWConnection`'s `.contentProcessed` means "handed to the network stack," not "peer-consumed" — RESC's raw-socket/tokio implementation should track receiver progress acks by frameID). Age bound exceeded → reconnect + fresh IDR.
9. **Keyframe contract, stated explicitly**: IDR on video-connection ready; IDR on decoder reset/request (rate-limited); static-screen replay from a retained `lastPixelBuffer` when an IDR is needed but no capture arrives (`MacSender.swift:818-824` pattern); `MaxKeyFrameInterval=3600` + `MaxKeyFrameIntervalDuration=60` (≈ 60 s *maximum* interval when honored — not literally "none"; log actual keyframe cadence to verify, and check every `VTSessionSetProperty` return value instead of ignoring them).
10. **Keep a minimal decoder recovery path** (delete the gray-frame heuristic and UDP-loss machinery, not recovery itself): decoder error / config-generation change / reconnect → stop feeding dependent frames → flush or recreate decoder → request IDR once (rate-limited) → resume on a matching-generation IDR. The receive side must decode every arriving reference frame; it may drop *decoded* frames at the render handoff, never encoded P-frames without a reset+IDR.

*Acceptance:* zero transport-loss corruption and bounded p95/max frame age under `iperf3` saturation and induced receiver stalls (no unbounded catch-up); control/cursor/input traffic stays responsive while a large IDR is in flight.

**Phase C — host latency tuning (A/B-measured, not copied blind).**
11. Encoder per codec: A/B `EnableLowLatencyRateControl` (proven upstream for H.264 only — verify it's honored for HEVC on this macOS/hardware), `PrioritizeEncodingSpeedOverQuality=true`, `MaxFrameDelayCount=0`; log resulting quality/bitrate behavior.
12. Capture: A/B `minimumFrameInterval` 1/60 vs 1/120 (delivered-fps + capture-age; the ~51 fps beat should vanish) and `queueDepth` 3/5/8 — pick the smallest depth that avoids SCK starvation drops rather than copying 8 unconditionally.
13. Remove FramePacer **only after**: cursor-only redraw is texture-resident (A.5), static-reconnect IDR replay works (B.9), and idle/reconnect/decoder-reset tests pass.

**Phase D — client pipeline redesign (kills L1–L4).**
14. Decoder low-delay before open: `AV_CODEC_FLAG_LOW_DELAY` (drops CUVID's 4-frame display queue — A/B with numbered frames: submit-seq vs output-seq gap must go to 0); SW path: A/B threads 1/2/4 and slice-vs-frame threading for the latency/throughput trade at 4K.
15. Decode/render thread split **with explicit frame ownership**: raw ffmpeg plane pointers must not cross the handoff (the decoder reuses that storage) — hand off an owned/refcounted frame (`av_frame_ref` semantics or a pooled copy) into a latest-wins slot; render thread drops older *decoded* frames freely; vsync never blocks decode; cursor repaints at input cadence.
16. Upload once per new frame, directly from frame planes+strides (`SDL_UpdateYUVTexture` accepts pitch — delete both intermediate copy layers in `extract_yuv`/`update_frame`); capability-test `SDL_PIXELFORMAT_NV12` upload on the deployed SDL/renderer backend to skip the CPU deinterleave, with I420 fallback.
17. Night Shift warm filter off the per-frame CPU UV pass if measurements justify it (render-stage color transform or LUT).

**Phase E — polish (after latency/glitch acceptance gates pass).**
Port from `Mac/VirtualDisplay.swift`: continuous HiDPI re-assertion, mirror-set detachment, arrangement persistence. Wire the client's keyboard send (`src/main.rs:454` TODO — host injector is complete). Consider opendisplay's sleep/wake lifecycle. Longer soak + fault-injection runs.

**Effort estimate:** Phase A ≈ half a day–one day; B ≈ one day; C+D ≈ one–two days; each phase independently testable against the Phase A baseline. **Target end state** (acceptance targets once Phase A establishes the baseline on the actual Mac/Ubuntu hardware — plausible goals, not predictions): wired LAN 4K60 HEVC glass-to-glass ≈ 40–80 ms p50, pointer ≈ 10–20 ms, zero loss-induced corruption.

---

## 8. How to measure (for before/after)

- End-to-end: Phase A numbers (capture-stamp → present-stamp with clock offset). Caveat: software capture→present is not photon/glass time — sanity-check optically with 240 fps phone slo-mo of a timer window spanning both screens.
- Pointer: measured **separately** from video (cursor seq → cursor-present timestamps, plus the same slo-mo counting frames between physical mouse motion and overlay motion) — video timestamps cannot validate the cursor path.
- Capture health: host's existing FPS log (expect ~60 after Phase C's interval change, ~51 before).
- Loss/corruption: soak with `iperf3` background load + continuous window drag; count corrupt-frame events (should be zero after Phase B by construction) and — pre-Phase-B — correlate glitches against the G6 size/oversize-drop logs.

---

## 9. Reviewer verification checklist

Claims a from-scratch reviewer should independently confirm, cheapest first:

1. **G2**: `keyframeIntervalSeconds: 0.5` at `mac-host/Sources/RemoteDisplayHost/main.swift:116`; burst loop with no pacing at `VideoSender.swift:61-113`.
2. **G3**: the 0xFA scan at `HostSession.swift:135-160`; client stamps random session_id + float stats at `crates/net-transport/src/control_channel.rs:136-154`; `request_idr = 31` ⇒ tag 0xFA at `proto/control.proto:38`. Runtime confirmation: host logs `IDR requested by client` with no matching client `Requesting IDR` log.
3. **L1 (the one external-knowledge claim)**: ffmpeg `cuviddec.c` sets `ulMaxDisplayDelay = (flags & AV_CODEC_FLAG_LOW_DELAY) ? 0 : 4` — check the installed ffmpeg source/version on the Ubuntu box; and frame-threading delay ≈ threads−1 for `Type::Frame` at `crates/video-decode/src/lib.rs:143-147`.
4. **L2**: `.present_vsync()` at `crates/renderer/src/lib.rs:146` — note `TECHNICAL_REVIEW.md` §7 claims vsync was *removed* in a latency pass; the code at HEAD has it back (see §10). Single-loop structure at `src/main.rs:339-532`.
5. **L6**: run host, observe `Capture FPS` log at 1/60 vs 1/120 `minimumFrameInterval`.
6. **L7**: with the stream idle, confirm client decode log still shows ~60 fps (FramePacer effect), and cursor repaint cadence tracks loop iterations.
7. **opendisplay claims**: encoder flags at `Mac/MacSender.swift:1039-1076`; backpressure at `:127-160, 1104-1128`; 1/120 rationale comment at `:444-448`; no-periodic-IDR comment at `:1066-1069`; DisplayImmediately at `iOS/PhoneReceiver.swift:727-741`; cursor-path comment at `Mac/MacSender.swift:202-206`.
8. **License**: `opendisplay/LICENSE` header (GPLv3); README FAQ notes ≤ v0.4.x MIT.
9. **G6 (v1.1)**: constants at `ProtocolConstants.swift:14-18` (1358) and `:64-67` (20×/2 MB cap); 512 advertised at `HostSession.swift:262`; client rejection at `crates/jitter-buffer/src/lib.rs:137-150`. Discriminating tests: unit-test assembly at 512 vs 513 chunks; log real 4K IDR byte sizes during window drags / full-screen changes and match them against `oversize_drops`.
10. **L9 (v1.1)**: `present_with_cursor` → `update_and_copy` → unconditional `SDL_UpdateYUVTexture` at `crates/renderer/src/lib.rs:256-263` and `:53-61`. Test: count texture-upload calls for cursor-only vs new-video presents.
11. **G4 retraction (v1.1)**: `grep -rn BitrateAdapter mac-host/Sources/` — only hits inside `BitrateAdapter.swift` itself (no call site at `12b87d1`).
12. **G3 crafted-input test**: send a valid Stats envelope with `session_id = 250` (varint contains 0xFA) — current host must false-trigger an IDR; typed parsing must not.
13. **L1 A/B**: numbered-frame test — decoder submit-sequence vs output-sequence gap with `AV_CODEC_FLAG_LOW_DELAY` off (expect ≈4 for CUVID) vs on (expect 0); SW threading 1/2/4 + slice-vs-frame.

## 10. Doc-vs-code discrepancies noticed in passing (for whoever updates TECHNICAL_REVIEW.md)

- `TECHNICAL_REVIEW.md` "Latency Optimization" claims `present_vsync()` was removed; HEAD has it present (`renderer/src/lib.rs:146`) — presumably re-reverted when tearing (its Problem 14) returned. The tearing/latency tension dissolves once present moves off the decode thread (Phase D.15).
- Three different frame-assembly deadlines appear across docs (30 ms plan.md, 100 ms review §7, 500 ms review §2); code says 100 ms (`jitter-buffer/src/lib.rs:106`). Moot after Phase B.
- `plan.md`'s wire-format section still shows the pre-fix 32-byte header (actual: 36) and the 5× `max_frame_bytes` multiplier (actual: 20×, capped 2 MB).
- `ModeConfirm` carries independent `video_port`/`input_udp_port`/`cursor_udp_port`, but `main.swift` hardcodes controlPort+1/2/3 (review flagged this "Unchanged"). Moot after Phase B (TCP).
- Keyboard forwarding: protocol + host injector complete; the only missing piece is the client's TCP send (`src/main.rs:454`, `// TODO`).
- Docs claim control transport is "TCP+TLS" (`proto/control.proto:6`); TLS was never implemented (plaintext; also flagged in review §5).

## 11. Independent-review response (v1.1)

`LATENCY_GLITCH_ANALYSIS_review.md` (2026-07-29, static review at the same commit) **concurs with the Option A verdict and the broad architecture** and challenged the implementation plan. Dispositions — every accepted item was independently re-verified in code before adoption:

| Review finding | Disposition in this document |
|---|---|
| §2.1 — 695,296-byte deterministic frame ceiling (512 chunks × 1358 B vs 1.67 MB `maxFrameBytes`) | **Accepted; independently verified** (constants re-read at `ProtocolConstants.swift:18,64-67`, `HostSession.swift:262`, `jitter-buffer/src/lib.rs:137-150`; sender's own limit is `UInt16.max` chunks, `VideoSender.swift:59`). Added as **G6**, treated as co-primary cause of the abrupt-change glitches; interaction with G3 noted (spurious IDRs multiply oversized frames). |
| §2.2 — cursor-only redraw re-uploads the full 4K YUV texture | **Accepted; verified** (`renderer/src/lib.rs:256-263 → 53-61`). Added as **L9**; the texture-resident cursor path is now Phase A.5 and an explicit precondition for FramePacer removal. |
| §2.3 — BitrateAdapter has no call site at HEAD | **Accepted — original G4 retracted** (grep: only self-references). The claim had been carried over from docs without a call-site check. |
| §2.4 — `pendingForceKeyframe` data race | **Accepted; verified structurally**. Plan A.4 (must be airtight before periodic keyframes are removed). |
| §1.3 — "8×" IDR-cadence math wrong | **Accepted; corrected** to ~2–3× (4/s forced atop 2/s scheduled, overlapping). |
| §1.1 — "all subsequent frames corrupt" too absolute | **Accepted**; wording softened (with no B-frames/reordering, visible corruption remains the common case). |
| §1.3 — swift-protobuf dependency ≠ implementation | **Accepted with a nuance the review missed**: `tools/generate_proto.sh` already builds `protoc-gen-swift`, so the fix is running existing codegen + wiring the output, not integration from scratch. |
| §3.2 — dedicated video TCP socket; corrected bind/ready/connect order | **Adopted** (Phase B.6–7); the multiplexing option from v1.0 is dropped for the head-of-line-blocking and framing reasons the review gives. |
| §3.3 — backpressure must bound bytes and age; `.contentProcessed` ≠ peer-acked | **Adopted** (Phase B.8, with receiver progress acks by frameID). |
| §3.4 — explicit keyframe contract; verify `VTSessionSetProperty` results; HEVC unproven upstream | **Adopted** (Phases B.9, C.11). |
| §3.5 — A/B capture settings instead of copying 1/120 + depth 8; gate FramePacer removal | **Adopted** (Phases C.12–13). |
| §3.6 — retain a minimal decoder recovery path | **Adopted** (Phase B.10) — consistent with v1.0's intent ("keep a kf request"), now specified. |
| §3.7 — decoded-frame ownership across the thread split; NV12 capability test | **Adopted** (Phases D.15–16); v1.0's "pass ffmpeg pointers directly" is only safe intra-thread. |
| §6 — 40–80 ms / 10–20 ms are goals, not predictions | **Adopted**; reworded as acceptance targets pending the Phase A baseline. |

No review finding contradicted the core recommendation; the review's Phase A–E ordering (surgical fixes + baseline before the transport rewrite) replaced v1.0's Phase 0–4 ordering in §7.
