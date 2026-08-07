# zero_copy evidence — run record (B0 + W0a, 2026-08-07)

Per ZERO_COPY_PLAN_review.md amendment A ("retain the exact host/client SHAs, dirty
state, codec, resolution, display mode, driver, and run arguments").

## Common conditions (both runs)

- Mac host: HEAD `8e8c7ee` (dirty: only untracked `KEYFRAME_STORM_FIX_REPORT_review.md`,
  committed alongside this record). Client-relevant sources byte-identical to `f32ba62`
  (commits between are tools/docs/mac-host only). Host binary: `.build/debug`, built at
  `f32ba62`-equivalent source + keyframe-fix hardening (e9b8f78 content).
- Box client: `~/resc/remote_extended_screen` at `f32ba62`, clean. Binary
  `target/release/remote-display-client` rebuilt at `f32ba62` (W0a probe string verified
  present via `strings`).
- Box: NVIDIA driver 570.169, RTX 4090, X11, monitor DP-4 2160×3840 (rotate left),
  WiFi powersave off. Both machines freshly rebooted this morning.
- Stream: H.264 (no `--hevc`), 50 Mbps, 2160×3840@60, `HOST_ARGS="2160 3840 60
  --client 192.168.50.47"`, runner `tools/r4_live_gate.sh`, `STREAM_SECS=60`,
  joiner `--causal-slack-us 16667`.
- Workload: owner performing continuous circular window drag on the remote screen
  (blind during W0a — probe renders nothing by design; cursor overlay visible).

## B0 (`b0-legacy-*`)

- Joiner: pass=true, joined 3,663, presented 3,158, 0 identity failures/ambiguities,
  rejected.never_received=99, clock offset median_delay 10,506 µs (7 samples).
- Host: 4,077 frames encoded, 8 KF, avg encode 26.7 ms; sender 296,053 packets /
  402,200 KB.
- Metrics (`b0-legacy-metrics.json`): e2e p50 161.990 / p90 177.910 ms;
  recv→decode p50 113.187 / p90 123.907 ms; gap mode 6 (presented: 6→3,146, 7→12).

## W0a (`w0a-probe-*`)

- `CLIENT_ENV="RESC_W0A_NO_DOWNLOAD=1"`; probe active (client log warn line).
- Joiner: joined 3,717, 0 identity failures; **pass=false solely due to presents=0**
  (probe never renders; predicate not applicable — recorded per the "don't claim gates
  the tool didn't evaluate" rule). rejected.never_received=107.
- Host: 4,157 frames, 8 KF, avg 26.7 ms; sender 314,595 packets / **427,534 KB**
  (+6.3% vs B0 → content parity accepted).
- Metrics (`w0a-probe-metrics.json`): recv→decode p50 **104.790** / p90 111.387 ms;
  gap (all emissions) 6→3,703 of 3,717 (99.6%); no present/e2e segments (by design).

## Gate outcome

W0a required recv→decode p50 ≤ 50 ms and gap → ≤ 2. Observed 104.790 ms and 6.
**FAIL → STOP** (zero-copy/GL abandoned). Root-cause analysis and candidate fixes:
`W0_REPORT_AND_REORDER_FIX_PLAN.md`.

## Bitstream evidence (dump run, post-W0a)

- 30.8 MB Annex-B dump via `--headless --dump-h264` (box `/tmp/stream4k.h264`).
- First SPS NAL (11 bytes): `27 64 00 34 ac 56 80 21 c0 78 64`.
- ffmpeg 7.1.5 `trace_headers`: profile_idc=100, level_idc=52,
  seq_scaling_matrix_present_flag=0, pic_order_cnt_type=0, max_num_ref_frames=1,
  gaps_in_frame_num_allowed_flag=0, frame_mbs_only_flag=1,
  **vui_parameters_present_flag=0** (both SPS repetitions inspected identical).
