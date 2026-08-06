# RESC Native-4K — Changes, Bug Fixes, and Usage

**Date**: 2026-08-05 → 06 · **Commits**: `cfbad1d` (v1) + `767e845` (v2) · both machines synced.

**What it is**: the one-screen answer to "sharp AND fast AND input" — a true Retina
2160×3840 virtual display (macOS renders @2x, zero resamples anywhere), streamed over
RESC's own UDP wire, H.264 hardware encode, CUVID hardware decode, ~50fps, with working
mouse/keyboard and the instant cursor overlay channel. Sharpness is cable-class
(owner-verified). Content latency measures **157ms p50** with the last ~100ms precisely
attributed (see "Remaining work").

## The bug fixes (root cause → fix → evidence)

1. **Slow VideoToolbox encode path (41.5 → 19.2ms/frame).** The session's
   `encoderSpecification`/`imageBufferAttributes` dicts forced a slow buffer path.
   Created with `nil/nil` (hardware verified via `UsingHardwareAcceleratedVideoEncoder`,
   now logged). This also cured HEVC's old "42ms wall" — it was never the codec.
2. **Permanent black screen at 4K: 512-bit chunk bitset.** The client assembler tracked
   chunk receipt in a fixed `[u64; 8]` — every ~753-chunk 4K keyframe silently died at
   exactly 512/753 and timed out. Bitset now sized from the wire's `max_chunks` (2048).
3. **Kernel UDP drops on keyframe bursts.** A ~1MB keyframe at GbE line rate overflowed
   the box's 416KB socket buffer (906 measured drops → keyframes never assembled). The
   sender now paces 1.5ms per 128 chunks; drops fell to zero.
4. **Decoder state-machine deadlock + mid-GOP wedge.** Leaving `WaitingForIDR` only on a
   keyframe *emission* deadlocked against cuvid's one-AU parse gap (the keyframe's
   emission needs the next feed; the next feed was gated). The transition now happens at
   **feed** time. Also: the decoder starts gated (initial `WaitingForIDR`) because feeding
   mid-GOP reference-less P-frames intermittently wedged `h264_cuvid` (silent zero-output).
5. **Encode pipeline queueing.** Unconditional submits kept ~2.5 frames inside VT ("encode
   time" was mostly waiting). An at-most-N in-flight gate was added; **depth 2** (depth 1
   halved throughput to 28fps because 19ms encode > 16.7ms frame period).
6. **Client 30fps ceiling: fused decode+render loop.** Decode serialized with the
   vsync-blocked present. Split into a decode thread (decoder + receipt ledger + identity
   + IDR + the C3 trace footer — semantics preserved, proven by a PASSing live
   joined-trace gate: 2,669 joined / 0 ambiguities) and a render-input thread (SDL whole:
   renderer, event pump, input, cursor), joined by a newest-wins mailbox. ~50fps now.
7. **`send_packet` EAGAIN treated as an error.** Now drains-and-retries (bounded), the
   same discipline `flush()` always had — correct backpressure, no IDR spam.

Supporting changes: wire chunk cap 512→2048 (host advertise), 4K keyframe interval 10s
(recovery via the existing client IDR-request path; keyframe forced at connect), client
frame channel 4→2 (standing-queue latency), host stdout line-buffered (fully-buffered
`print()` hid all periodic stats and died unflushed on SIGTERM), `tools/r4_live_gate.sh`
gained `HOST_ARGS`/`RUN_TAG`/`EVID` env overrides for 4K measurement runs.

## Measured state (traced runs, real motion, ±3.5ms clock uncertainty)

| segment | p50 |
|---|---|
| capture → encode out | 19.3ms |
| encode out → send | 0.6ms |
| network (UDP, paced) | 5.0ms |
| receive → decode done | **109.9ms** ← the remaining bottleneck |
| decode done → present | 23.3ms |
| **E2E capture → present** | **157ms** (p90 220) |

The 110ms is NOT decode (5–9ms) or the app queue (2-deep): `decode_trigger` gap analysis
shows **cuvid's decode pipeline running exactly 6 frames deep (84% of frames)** under GPU
contention — NVDEC decode, the 12MB/frame GPU→CPU download, and SDL's 12MB re-upload all
share the RTX. **Negative result, documented**: bounding cuvid's surface pool to 6 left
the depth at 6 but starved reference frames (corruption detectors forced 135 keyframes in
40s) — reverted; pool tuning is the wrong lever.

## Remaining work (recorded in task #28 + project memory)

**Zero-copy rendering** (CUDA→GL interop: decoded frames stay on the GPU, no 24MB/frame
round trip) or equivalent contention reduction → the ~100ms collapses; projected E2E
~60–70ms. Fresh-session-sized work; everything needed to start cold is written down.

## How to use it

One pair at a time (the host holds a single-instance lock — always stop the old pair
first). Monitor stays at native 4K portrait permanently.

### Native-4K screen (sharp + input) — the default

```bash
cd ~/Downloads/personal/AGI/remote_extended_screen/mac-host && nohup ./.build/debug/remote-display-host 2160 3840 60 --client 192.168.50.47 > /tmp/resc4k-host.log 2>&1 &
```

```bash
ssh wan@192.168.50.47 'cd ~/resc/remote_extended_screen/ubuntu-client && nohup env DISPLAY=:0 LD_LIBRARY_PATH=$HOME/ffmpeg7/lib ./target/release/remote-display-client -H 192.168.50.125 < /dev/null > /tmp/resc4k-client.log 2>&1 & echo started'
```

Start the host first, wait ~8s, then the client. Picture appears ~instantly (keyframe is
forced at connect).

### 1080p screen (fastest feel, 49ms, softer text)

Same client command; host WITHOUT positional args (defaults to the Retina-supersampled
1080×1920 stream, integer-2× crisp on the 4K canvas):

```bash
cd ~/Downloads/personal/AGI/remote_extended_screen/mac-host && nohup ./.build/debug/remote-display-host --client 192.168.50.47 --hevc > /tmp/resc-host.log 2>&1 &
```

### Stop whatever is running

```bash
pkill -TERM remote-display-host; ssh wan@192.168.50.47 'pkill -TERM remote-display'
```

(OpenDisplay sharp-no-input alternative lives in `../ubuntu_receiver` — see its README.)

### Odds and ends

- Quit the box client from its fullscreen: **Ctrl+Alt+Q** (or the stop command above).
- Logs: `/tmp/resc4k-host.log` (Mac), `/tmp/resc4k-client.log` (box). Host prints capture
  fps + encode ms every ~5s; client prints decoded fps + decode ms.
- If the monitor shows nothing after a cable replug: the DP output name drifts (DP-2→DP-4
  seen); re-set with the connected-output-detecting command in `evidence/demo/DEMO_NOTE.md`
  or just replug and let X restore.
- **Measure latency yourself** (requires dragging during the run):
  `STREAM_SECS=45 RUN_TAG=mytest EVID=$PWD/evidence/demo HOST_ARGS="2160 3840 60 --client 192.168.50.47" bash tools/r4_live_gate.sh`
  then eyeball `evidence/demo/mytest-join-summary.json`; per-segment percentiles come from
  the joined `.jsonl` (analysis snippets in the session records / git log of
  `evidence/demo`).
