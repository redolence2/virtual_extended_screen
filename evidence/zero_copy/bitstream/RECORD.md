# Bitstream evidence record (W0 cycle, preserved per W0 review §1)

Source: live-stream dump `/tmp/stream4k.h264` on the box (30,847,574 bytes, captured
2026-08-07 12:22 box time via `remote-display-client --headless --dump-h264`, host args
`2160 3840 60 --client 192.168.50.47`, H.264 50 Mbps).

- Full-dump SHA-256: `5f90028dc90e1400e0a5ec91bdb51cd193d6c017c813288c9e655fb495aff20e`
  (file itself remains on the box only; too large / unnecessary to commit).
- `sps-pps-sample.bin` (committed, 23 bytes = first configuration NALs, Annex-B):
  `00000001 27 64 00 34 ac 56 80 21 c0 78 64` (SPS, 11 bytes) +
  `00000001 28 ee 3c b0` (PPS, 4 bytes).
  SHA-256 `6018c1050550b5db005f41517f33cd8fa453948a124180399f87bba32d8b3690`
  — computed independently on the box and after transfer; identical.
- `trace_headers-full.txt` (committed): complete ffmpeg 7.1.5 `trace_headers` output for
  the first 1 MB of the dump (both SPS repetitions). Key fields: profile_idc=100,
  level_idc=52, seq_scaling_matrix_present_flag=0, pic_order_cnt_type=0,
  max_num_ref_frames=1, gaps_in_frame_num_allowed_flag=0, frame_mbs_only_flag=1,
  vui_parameters_present_flag=0.
- `decoder-selection-lines.txt` (committed): client log lines from the W0a-era run
  proving the decode path was `h264_cuvid` / "H.264 CUVID decoder initialized (NVDEC
  hardware, RTX GPU)" — no software or fallback decoder.
