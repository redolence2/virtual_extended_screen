# Demo Release Note — RESC personal virtual extended screen

| | |
|---|---|
| **Date** | 2026-08-05 |
| **Demo commit** | the commit carrying this note (see `git log` — it contains the portrait virtual display, the cuvid LOW_DELAY fix, and this evidence set) |
| **Release basis** | `A00_COMPLETION_REPORT_CORRECTED_review.md` executive verdict: **GO** for the personal demo after one short manual real-pair smoke test — performed below |
| **Machines** | Mac host 192.168.50.125 (virtual display + HEVC encode) → Ubuntu box 192.168.50.47 (`wan`), LS32D70xE 32" physically-vertical monitor, GbE LAN |
| **Configuration** | Portrait-native virtual display **1080×1920@60**, HEVC, legacy v1 wire, client decode `hevc_cuvid` with `AV_CODEC_FLAG_LOW_DELAY` + `extra_hw_frames=8` (mirrors the R6-characterized `cuvid-lowdelay` config), unrotated 1:1 render + identity input mapping |

## Smoke test — all five review steps PASS

1. **Start** — host and client started with hard-coded personal settings; virtual display
   created (1080×1920@60, sizeInMM 299×531); client connected, decoded at ~58.5 fps.
2. **Motion** — user dragged an iMessage window onto the vertical screen: display updates
   live, upright, correct orientation. Confirmed by the user ("Feels good. this demo covers
   normal usage").
3. **Input** — user drove the box's mouse/keyboard: pointer and keystrokes act correctly on
   the extended screen (typing latency initially "perceivable, feels 100ms" — see annex;
   after the low-delay fix, "Feels good, low latency").
4. **Restart** — both processes stopped and restarted multiple times through the session
   (including two graceful traced stops with clean `trace_complete` footers both ends);
   recovery required no manual cleanup beyond the documented start/stop commands.
5. **Retain** — ordinary host/client logs from every phase retained in this directory;
   commit recorded by this note's containing commit.

## Latency annex (traced runs, real user motion, clock-corrected ±3.4ms)

Instrumentation: `RESC_TRACE=1` both ends + `tools/join_trace.py` exact-identity join.
Both runs PASSed the strict gate (baseline: 3,636 joined / low-delay: 3,592 joined —
zero identity ambiguities, zero identity failures, clean footers).

| segment (p50) | baseline | low-delay settled |
|---|---|---|
| capture → encode done (Mac) | 0.6ms | 0.6ms |
| encode → send | 0.2ms | 0.2ms |
| send → receive (net + assembly) | 4.8ms | 4.4ms |
| receive → decode done (box) | 77.0ms | **26.6ms** |
| decode done → present (vsync) | 15.3ms | 15.0ms |
| **E2E capture → present** | **94.1ms** | **49.3ms** (p90 55.1, p99 65.3) |

Root cause of the baseline 77ms, proven by the trace's `decode_trigger_frame_id`: FFmpeg
cuvid's default 4-frame display queue (`ulMaxDisplayDelay=4`) — 99.8% of frames sat exactly
4 deep. `AV_CODEC_FLAG_LOW_DELAY` zeroes it; residual is cuvid's inherent one-AU parse
delay (~17ms). Known further step if ever wanted: `sw1-lowdelay` (the A0-selected backend)
emits same-submit — projected ~32ms E2E.

## File inventory (this directory)

- `demo-host-landscape-prelim.log`, `demo-client-landscape-prelim.log` — first run, before
  the portrait switch (historical).
- `demo-host-portrait-run1.log`, `demo-client-portrait-run1.log` — portrait phase 1.
- `demo-latency-*` — baseline traced run (traces, joined, summary, logs).
- `demo-lowdelay-*` — settled-configuration traced run (traces, joined, summary, logs).

## Phase bookkeeping (unchanged by this release)

Formal **A0.0 remains incomplete; State 5 not granted; Stage 1 candidate; A0/T1 NO-GO** —
per the review, closing it requires the deferred D1–D5 hardening pass and a fresh
C′→E′→R′ evidence chain with one independent re-review
(`A00_COMPLETION_REPORT_CORRECTED_response.md` §5). The demo release does not depend on it.
