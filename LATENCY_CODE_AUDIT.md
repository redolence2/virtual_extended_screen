# Full-Pipeline Latency Code Audit — fresh read, for review

**Date**: 2026-08-08 · **Trigger**: owner: "any other optimization opportunity —
multithreading, async, zero-copy? Re-read the code entirely, from scratch."
**Method**: line-by-line fresh read of the complete video-latency spine on both ends —
`DisplayCapturer.swift`, `CaptureSlot.swift`, encode loop (`main.swift:171-177`),
`VideoEncoder.swift`, `VideoSender.swift`, `video_receiver.rs`, `jitter-buffer/lib.rs`,
decode thread + `FrameMailbox` (`main.rs:529-800`), `video-decode/lib.rs`,
`renderer/lib.rs` — cross-checked against this week's measured segment data
(`evidence/zero_copy/`). Non-video paths (cursor, input, control) skipped: they do not
carry frame latency.

## 1. Architectural verdict first: the threading/async design is already correct

Every handoff on the video path is event-driven; there is **no sleeping poll, no timer
tick, and no standing queue anywhere on the frame path**:

| handoff | mechanism | file:line | verdict |
|---|---|---|---|
| SCK frame → encode thread | `GenerationalFrameSlot`: semaphore signaled per store, blocking `waitAndTake()`, newest-wins | CaptureSlot.swift:99-185, main.swift:171-177 | clean |
| encode thread → VT | direct call, at-most-2 in-flight gate (measured necessary: encode 20 ms > 16.7 ms period) | VideoEncoder.swift:269-289 | clean |
| VT callback → wire | `sendFrame` inline in callback; chunk loop + `sendto`; measured encode-out→send p50 0.6 ms | VideoSender.swift:58-130 | clean |
| UDP recv → assembler | blocking per-packet recv (100 ms timeout only for stale-frame expiry), chunk fed inline | video_receiver.rs:100-190 | clean |
| assembler → decode thread | frame returned the instant the last chunk arrives (`is_complete()` in `process_chunk`); receipt stamped before any queue; `sync_channel(2)` with keyframe-block/delta-drop policy | jitter-buffer lib.rs:129-215, video_receiver.rs:186-218 | clean |
| decode thread → render thread | `FrameMailbox`: mutex+condvar, `notify_one` per put, newest-wins; the 2 ms `take_timeout` bounds only cursor-idle redraws, not frame wake-ups | main.rs:529-561 | clean |

The answer to "multithreading / async": already done, correctly — the pipeline is 7
dedicated threads with event-driven boundaries and deliberate shedding points. There is
no restructuring win available. Zero-copy: measured dead twice (W0a: whole-path removal
= 8.4 ms pre-HEVC; v1-stages: upload = 3.4 ms — a device-copy path could reclaim at
most ~3 ms at high complexity).

## 2. Where the ~80 ms actually lives (closed arithmetic, from this week's runs)

> Primary evidence: `evidence/zero_copy/ladder-day-record.md` (all 2026-08-08 runs,
> verdicts, and the vsync-falsification addendum) plus the per-run
> `*-metrics.json` files beside it. This section derives from those records and adds
> the fresh-read attribution; the records themselves stay sealed as evidence.

- **capture→encode ~22.5 ms** = HEVC hardware encode (~20.3 avg with E3, ~30 without) +
  ~2-3 ms depth-2 queue wait. Lever: E3 (shipping decision open). Structural remainder.
- **network ~5-6.5 ms** = wire serialization of ~50 Mbps bursts + last-chunk assembly
  wait. Irreducible at this bitrate/link.
- **receive→decode ~27-30 ms** = **~17.5 ms cuvid one-AU parser wait** (AUD proxy
  refuted; only an explicit `CUVID_PKT_ENDOFPICTURE` feed — custom decode path — removes
  it) + ~8 ms NVDEC decode + **~2-4 ms plane-ownership copy (F1 below — removable)**.
- **decode→present ~21-23 ms** = ~5-6 ms mailbox wait behind the previous frame's
  present (scales with present cost) + 3.4 ms upload + **13.4 ms blit+present, of which
  app vsync is only ~2 ms — something below the app syncs presents (GNOME/mutter
  compositing is the prime suspect; F2)**. This closes the owner-falsified vsync
  question: fix the present cost and the mailbox wait shrinks with it.

## 3. Findings (only two carry real meat)

| # | opportunity | est. gain | cost/risk | verdict |
|---|---|---:|---|---|
| F1 | **AVFrame pass-through**: skip the post-transfer plane copy — wrap the ffmpeg sw_frame (refcounted) into `DecodedFrame` instead of `to_vec()` copies (video-decode lib.rs extract path; sw_frames are not pool-limited, unlike the hw surfaces that killed zero-copy) | 2–4 ms | moderate: frame lifetime crosses threads; Send wrapper; mailbox drop must free promptly | worth doing with the compositor fix |
| F2 | **compositor unredirection** (already queued from the vsync falsification): verify `_NET_WM_BYPASS_COMPOSITOR` on the SDL window / try true-exclusive fullscreen; if mutter is compositing our presents, remove it from the path | 8–11 ms (present 13.4→~3 AND the mailbox wait shrinks ~3-4) | low: one xprop + a window-flag experiment | **do first — biggest single lever left** |
| F3 | sender per-chunk `Data` allocation; receiver per-packet syscalls (`recvmmsg`/buffer reuse) | ≤0.5 ms | low | CPU hygiene only; skip for latency |
| F4 | **custom NVDEC feed with `CUVID_PKT_ENDOFPICTURE`** (pinned ffmpeg patch or direct Video Codec SDK use) | ~17.5 ms | project-scale; the reviewer's "only after everything else and a fresh owner decision" | the only path below ~65 ms; unscheduled |
| F5 | SCK `queueDepth` 3→2, sender `sendmmsg`, channel tuning | ~0 | — | no-ops for latency; recorded to close them |

## 4. Projected outcomes (scenarios, gates decide — per review discipline)

| scenario | E2E p50 |
|---|---:|
| today (shipped) | ~80–85 ms measured |
| + F2 compositor (if suspect confirmed) | **~70 ms** |
| + F1 + E3 shipped | **~65 ms** |
| + F4 custom decode feed | **~48–55 ms** (stretch; new project) |
| below that | capture cadence + encode + wire + panel physics |

## 5. Questions for the reviewer

1. Concur that the threading/async architecture has no restructuring win (§1 table)?
2. F1's AVFrame pass-through: acceptable under the same ownership rules the zero-copy
   review demanded (clone-owned frame, prompt drop on mailbox overwrite, narrow Send
   invariant) — noting sw_frames are heap buffers, not decoder-pool surfaces?
3. F2 execution: xprop inspection first, then which variant — SDL `FULLSCREEN`
   (exclusive) vs `SDL_VIDEO_X11_NET_WM_BYPASS_COMPOSITOR` enforcement vs mutter
   unredirect verification — preferred order?
4. Any objection to closing F3/F5 permanently as non-levers?
5. F4 remains unscheduled pending owner appetite after F1/F2 — agreed?
