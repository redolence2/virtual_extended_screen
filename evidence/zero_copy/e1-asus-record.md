# E1 (AUD append) + ASUS A/B — results record (2026-08-08)

All runs: HEVC 50 Mbps, 2160×3840@60, r4_live_gate 60 s, owner drag workload unless
noted. Client binary: 66fe9e8 + E1 patch deployed via scp (git blocked by the morning
permission re-roll; proper commit queued). Same-day control: `b1-noasus`.

## ASUS A/B (review §4.1)

- a1/a2 (2026-08-07, ASUS attached): capture→encode p50 23.47 / 23.78 ms.
- b1-noasus (2026-08-08, ASUS unplugged): capture→encode p50 **25.12 ms**.
- **Verdict: the second display is immaterial to the encode segment** (unplugged run
  is not faster; differences are day-to-day variance). Owner replugs freely; lever
  closed. Note: displays must not be plugged/unplugged DURING a run — topology changes
  renumber the virtual display mid-measurement (see below).

## E1 — same-buffer trailing AUD (review §2)

- Pre-checks (evidence/zero_copy/bitstream/ + /tmp/stream4k.h265 on box): stream has
  **zero native AUDs** (NAL types 1,20,32,33,34,39 only), all NALs layer 0 /
  temporal_id_plus1 1, one VCL NAL per AU; HEVC SPS declares
  `sps_max_num_reorder_pics=0`, `sps_max_dec_pic_buffering_minus1=4` (documentary
  confirmation of why HEVC removed the H.264 DPB wait).
- Run `e1-aud` (first attempt): **INVALID** — env active but the codec guard compared
  lowercase "hevc" against display name "HEVC CUVID"; no AUD was ever appended (the
  required "first fed packet" evidence line was absent — the review's fed-bytes
  requirement caught the bug immediately). Also static content (owner not dragging).
- Run `e1b-aud` (guard fixed, verified): `E1 first fed packet: len=416257
  tail16=…00 00 00 01 46 01 50`; 0 decode errors; drag workload (223 MB sent).
  Results: gap mode **1** (3,011/3,015 presented), recv→decode p50 **27.37 ms**
  (control b1: 28.17), E2E p50 81.46 (control 85.70 — within variance).
- **Verdict: NEGATIVE.** A trailing AUD does not make cuvid emit the current picture
  early; the one-frame parser wait persists. Per POST_HEVC_LATENCY_PLAN_review.md §2,
  one clean negative paired run closes this line. Confidence in the
  `CUVID_PKT_ENDOFPICTURE` ffmpeg-patch variant is correspondingly reduced (the
  boundary-detection hypothesis took the hit); it stays unscheduled.
- The E1 code remains in-tree, env-gated OFF (value-parsed); removal or retention is a
  seal-time decision.

## Same-day incident (separate record): topology-change blur

Unplugging the ASUS renumbered the virtual display (14→15) and reverted it to 1×;
the enforcement loop polled the stale ID where macOS served a cached healthy mode —
fixed in CGVirtualDisplayBridge.m (loop re-reads live displayID each tick; built,
ships with next launch; commit queued on git heal).

## Ladder state after today

Remaining (reviewed): renderer cleanup (resident texture on cursor-only redraws —
currently re-uploads full 4K every redraw — native NV12 via SDL_UpdateNVTexture,
buffer reuse; est. single-digit ms) → Apple EnableLowLatencyRateControl HEVC probe
(encode segment) → owner gate → CUDA/GL (5–10 ms est.). Realistic post-ladder
outlook: ~70–75 ms from today's ~80–85; the two big-swing hypotheses (copies, DPB)
are both resolved.
