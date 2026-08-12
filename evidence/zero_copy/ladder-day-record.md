# Optimization-ladder execution record — 2026-08-08 (complete)

All runs: HEVC 50 Mbps 2160×3840@60, r4_live_gate 60 s, zc_metrics. Drag workload
unless marked static. Reviewed plan: POST_HEVC_LATENCY_PLAN{,_review}.md.

| run | change | workload | cap→enc p50 | recv→dec p50 | dec→present p50 | E2E p50/p90 | verdict |
|---|---|---|---:|---:|---:|---|---|
| a1/a2 (08-07) | shipped baseline | drag | 23.5/23.8 | 25.2/29.5 | 23.1/23.2 | 81.2/87.0 · 79.6/94.9 | baseline |
| b1-noasus | ASUS unplugged | drag | 25.1 | 28.2 | 23.3 | 85.7/100.1 | **ASUS immaterial** — lever closed |
| e1-aud | AUD append (guard bug) | static | — | — | — | — | INVALID (no append; fed-bytes check caught it) |
| e1b-aud | AUD append (verified) | drag | 25.2 | 27.4 | 23.0 | 81.5/98.3 | **NEGATIVE** — gap stays 1; line closed |
| r2-nv12 | resident texture + native NV12 + scratch warm | drag | 27.5 | 27.9 | 23.0 | 87.8/106.8 | **latency-neutral** (hygiene + CPU savings; copies were never the meat — vsync owns the segment). E2 CUDA/GL formally dead by review stop-rule |
| e3-lowlat | Apple EnableLowLatencyRateControl | static | 19.4 | 27.5 | 23.7 | 78.4/85.6 | promising; static-flattered |
| e3b-lowlat | same, confirmation | drag | 22.5 | 30.4 | 23.5 | 84.6/**91.7** | **marginal real gain**: encoder time 30→20 ms avg, tightest drag p90 of the day, keyframes 4× smaller; CAVEAT: capture-side drops elevated (841/4196 vs ≤14 in non-LOWLAT runs) — effective fps and delivery (94.9%) unaffected, cause unexplained |

## Bottom line

**~80–85 ms E2E p50 under real drag is this architecture's measured number.** Every
reviewed lever is now executed: two neutral, one negative, one marginal-positive with an
open caveat. The remaining segments are architectural: ~17–19 ms cuvid parser frame
(AUD proxy refuted), ~19–21 ms vsync/pacing in present, ~20 ms encode, ~6 ms network.
Below this = different architecture (custom NVDEC feed with ENDOFPICTURE, non-vsync
present) — out of scope per review.

## Addendum: vsync-model falsified by owner's challenge → present path re-attributed

The owner challenged "vsync owns decode→present" (23 ms > 16.67 ms period — correct).
Falsification run `v0-novsync` (present_vsync disabled, verified by warn line):
decode→present 23.0 → **20.8 ms** — app vsync owned ~2 ms, not ~18. The invariance
argument was flawed (a constant 12 MB upload is as content-invariant as a clock).
Stage timers then named the owner (`v1-stages`): **upload (SDL_UpdateNVTexture) 3.4 ms**
— exonerated, so the CUDA/GL-interop revival dies again on real numbers (max ~3 ms
available) — and **blit+present 13.4 ms**, still ~11 ms with app vsync off ⇒ something
BELOW the app syncs presents. Checked: no ForceCompositionPipeline in metamode/xorg.
Confirmed: the box runs a GNOME/X11 session ⇒ **mutter compositing is the prime
suspect** (re-syncs every present regardless of app swap interval; SDL borderless
fullscreen_desktop windows do not always qualify for unredirection).
**Next probe (recorded, not run)**: xprop the client window for
`_NET_WM_BYPASS_COMPOSITOR` / verify mutter unredirection; try true-exclusive
fullscreen or the explicit bypass hint; expected prize if it lands: ~8–11 ms off
decode→present → E2E ~70 ms. Diagnostic switches (`RESC_NO_VSYNC`, stage timers) remain
in tree.

## E3 shipping decision: OPEN (owner)

Keep `RESC_LOWLAT=1` env-gated OFF (current state) or ship it in the launcher after a
normal-use smoke + an answer for the capture-drop anomaly. Its real benefits: −10 ms
encoder time, tighter p90 (fewer felt spikes), 4× smaller keyframes (gentler pacing
ripple).

## Pending commits (git blocked by the 08-08 permission re-roll; heal = Mac reboot)

Working-tree changes awaiting commit (Mac): `video-decode/src/lib.rs` (FrameFormat +
NV12 pass-through + E1 probe), `renderer/src/lib.rs` (resident texture + NV12 + scratch
warm), `VideoEncoder.swift` (E3 flag), `CGVirtualDisplayBridge.m` (live-ID enforcement
fix), plus `evidence/zero_copy/*` new records/metrics/bitstream. Box tree carries the
same client changes via scp (dirty vs 66fe9e8). NEXT SESSION: commit all of this first.
