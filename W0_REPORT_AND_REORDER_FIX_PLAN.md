# W0 Gate Report: Zero-Copy STOPPED — Root Cause Is Decoder Reorder Policy, Not Copies

**Date**: 2026-08-07 · **Executes**: `ZERO_COPY_PLAN_review.md` amendments A + B (B0, W0a)
**Gate verdict**: **W0a FAILED its gate → the bounded-copy/GL rewrite is STOPPED** per the
review's stop rule, before any GL code was written.
**New finding**: the ~110 ms decode-stage delay is **H.264 decoder reorder policy** caused
by the encoder emitting **no VUI declaration**, with bitstream evidence and closed
arithmetic. Two candidate fixes proposed (§5) — one requires zero code.
**Code state**: NO fix code written (owner directive: review first). The only code this
cycle was the review-specified W0a probe itself (`f32ba62`, committed before the runs).

---

## 1. B0 — fresh legacy baseline on current HEAD (review amendment A)

Conditions: Mac HEAD `8e8c7ee` (client sources byte-identical to box `f32ba62` — commits
between touch tools/docs/host only), box `f32ba62` clean, NVIDIA driver 570.169, monitor
2160×3840-left, H.264 50 Mbps 2160×3840@60 (no `--hevc`), owner performing continuous
circular window drag, `r4_live_gate.sh` 60 s, joiner `pass: true`, 0 identity failures.

| metric | p50 | p90 |
|---|---:|---:|
| capture → encode out | 19.619 ms | 28.746 ms |
| encode out → receive | 6.240 ms | 12.389 ms |
| **receive → decode done** | **113.187 ms** | 123.907 ms |
| decode done → present | 23.038 ms | 29.039 ms |
| **E2E capture → present** | **161.990 ms** | 177.910 ms |

3,663 joined / 3,158 presented; ordinal gap **6 for 99.6%** of presented frames
(3,146 at 6, 12 at 7); encoder: 4,077 frames / **8 keyframes** (keyframe-storm fix
confirmed live); sender 402,200 KB. Consistent with the sealed pre-fix baseline
(157.4 ms) — the storm fix is **not** silently credited with any latency change.

## 2. W0a — causal no-download probe: FAIL → STOP

Probe (per review spec, env `RESC_W0A_NO_DOWNLOAD=1`): real 4K CUVID decode; hw frames
skip `av_hwframe_transfer_data` + `extract_yuv`; metadata-only emissions keep identity /
ledger / clean footer; nothing published or rendered. Run conditions identical to B0;
owner repeated the same drag blind (screen black by design). Content parity **verified by
numbers**: sender 427,534 KB vs B0's 402,200 KB (+6.3%). Joiner joined 3,717 emissions
with 0 identity failures; its `pass:false` is solely the zero-presents predicate — not
applicable to a non-rendering probe (stated here because the runner must not be claimed
to pass gates it didn't evaluate).

| metric | B0 legacy | W0a probe | gate |
|---|---:|---:|---|
| receive → decode p50 | 113.187 ms | **104.790 ms** | required ≤ 50 ms → **FAIL** |
| receive → decode p90 | 123.907 ms | 111.387 ms | — |
| ordinal gap mode (all) | 6 (99.6%) | **6 (3,703/3,717 = 99.6%)** | required → ≤ 2 → **FAIL** |

**Interpretation (review's own stop rule)**: removing the *entire* CPU
transfer/extract/render/upload path — every byte of the 49.8 MB/frame payload accounting —
was worth **8.4 ms of 113**. The copy-traffic/contention hypothesis is **refuted**. The
delay lives inside the decode stage, between feed and emission, and is ordinal: exactly
6 emissions behind, at any load.

## 3. Root cause — evidence chain

1. **The lag counts frames, not work.** Gap locked at exactly 6 in both runs (99.6%),
   downstream load irrelevant; 6 × ~17.5 ms effective feed period ≈ the measured ~105 ms.
   A queueing/contention delay would vary with load; this does not.
2. **Bitstream evidence.** 30.8 MB live-stream dump (`--dump-h264`, box
   `/tmp/stream4k.h264`). First SPS NAL is 11 bytes: `27 64 00 34 ac 56 80 21 c0 78 64`.
   ffmpeg 7.1.5 `trace_headers`: `profile_idc=100`, `level_idc=52`,
   `seq_scaling_matrix_present_flag=0`, `pic_order_cnt_type=0`, `max_num_ref_frames=1`,
   `gaps_in_frame_num_allowed_flag=0`, `frame_mbs_only_flag=1`, and —
   **`vui_parameters_present_flag=0`**. No VUI ⇒ no `bitstream_restriction` ⇒ no
   `max_num_reorder_frames` declaration anywhere in the stream.
3. **Spec arithmetic closes exactly.** With the declaration absent, H.264 defaults
   `max_num_reorder_frames = MaxDpbFrames = min(MaxDpbMbs / picture-MBs, 16)`.
   Level 5.2: MaxDpbMbs = 184,320. Picture 2160×3840: 135 × 240 = 32,400 MBs.
   184,320 / 32,400 = 5.68 → **5 frames** of DPB output delay, **plus** cuvid's
   previously-documented 1-AU parser gap = **exactly the observed 6**.
4. **The encoder truly never reorders** — `kVTCompressionPropertyKey_AllowFrameReordering
   = false` is set (`VideoEncoder.swift:170`); VideoToolbox simply does not *advertise*
   it (it emitted no VUI at all). The decoder is spec-correct to buffer 5 frames for
   reordering that can never occur.
5. **Why LOW_DELAY didn't cover this**: `AV_CODEC_FLAG_LOW_DELAY` zeroes the cuvid
   *display-queue* delay (`ulMaxDisplayDelay`), which is additive; the DPB output delay
   is derived from the SPS and unaffected.
6. **Why the 1080p demo felt fast**: it runs **HEVC** (`--hevc`), a different codec path
   with different (and differently-defaulted) reorder semantics — not a contradiction,
   and the basis of Candidate A below.
7. **Alternative explanations left open for the reviewer**: a fixed internal
   ffmpeg-cuvid queue depth coincidentally equal to 6, or NVDEC-internal pipelining.
   Both are directly falsified or confirmed by the same cheap experiments in §5 — the
   arithmetic in (3) hitting 5+1 exactly is strong but remains correlational until a
   candidate fix moves the number.
8. **Side observation for later**: the M1 OpenDisplay-receiver's "unfixable Mac-side"
   latency verdict predates this finding; if OpenDisplay's H.264 stream also omits the
   declaration, part of that latency may be this same receiver-side DPB wait,
   misattributed. Out of scope here; recorded for a future revisit.

## 4. What was abandoned

The bounded-copy CUDA/GL renderer (plan §B.2–B.3, W1–W6). Projected gain measured by
W0a: ~8 ms — not worth its complexity even if flawless. The W0b interop probe was
rendered moot and was **not** run (no GL/CUDA feasibility question remains relevant).
The W0a probe mode stays in the tree (env-gated, harmless) as a reusable measurement
tool; removal is at the reviewer's discretion.

## 5. Candidate fixes (for review — nothing implemented)

### Candidate A — codec switch to HEVC (ZERO code; measure first)

Run the identical B0 protocol with `HOST_ARGS="2160 3840 60 --client 192.168.50.47
--hevc"` (both ends already support HEVC end-to-end — it is the 1080p demo's daily
path; the old "42 ms HEVC encode wall" was disproven earlier: it was the VT session
dicts, not the codec). Then:

- `zc_metrics`: if receive→decode p50 lands ≤ ~40 ms and the gap mode ≤ 2 →
  the DPB theory is confirmed *and* the fix is a **launcher flag** (one-line
  `resc4k.sh` change, both app copies).
- Also dump + `trace_headers` the HEVC stream (VPS/SPS `sps_max_num_reorder_pics`)
  for the record, and check keyframe sizes/encode ms for regressions.
- Risks: hevc_cuvid behavioral differences under loss/recovery (the IDR-request path
  is codec-agnostic on the wire but recovery behavior should be smoke-checked);
  HEVC keyframe sizes at 50 Mbps.

### Candidate B — client-side SPS VUI injection (H.264 stays; ~250 lines + tests)

New module `ubuntu-client/crates/video-decode/src/sps_fix.rs`:

- `pub fn rewrite_au(au: &[u8]) -> Option<Vec<u8>>` — Annex-B start-code scan; for a
  type-7 NAL: if `vui_parameters_present_flag == 1` → `None` (never touch a declaring
  stream); else rewrite and splice. Per-session cache: byte-identical input SPS →
  cached output (the SPS is constant per session; scan-only cost on non-SPS AUs).
- `BitReader`/`BitWriter` with ue(v) read/write and emulation-prevention
  unescape/re-escape (implemented generally, not assumed absent).
- SPS field walk: full branches (chroma_format 3, scaling lists, poc types 0/1/2,
  cropping). Any shape outside the walker's competence (e.g. scaling lists present) →
  `None` + one rate-limited warn = **fail-open to current behavior** (reviewer: §7 Q4).
- Rewrite: copy all bits through the position of `vui_parameters_present_flag`, write
  `1`, append minimal VUI: eight zero flags (aspect…pic_struct), then
  `bitstream_restriction_flag=1`, `motion_vectors_over_pic_boundaries_flag=1`,
  `max_bytes_per_pic_denom=ue(2)`, `max_bits_per_mb_denom=ue(1)`,
  `log2_max_mv_length_horizontal=ue(16)`, `log2_max_mv_length_vertical=ue(16)`,
  **`max_num_reorder_frames=ue(0)`**, `max_dec_frame_buffering=ue(max_num_ref_frames
  as parsed — 1 for this stream)`, `rbsp_trailing_bits`, re-escape.
- Wiring: in `VideoDecoder::decode()` before `send_packet`; env `RESC_NO_SPS_FIX=1`
  restores byte-exact current behavior.
- Unit tests in-crate: (1) the real captured SPS above → re-parse output: every
  pre-VUI field identical, `vui=1`, `reorder=0`, buffering=1, valid trailing bits;
  (2) already-declaring SPS → `None`; (3) scaling-list SPS → `None`; (4) own output
  fed back → `None` (idempotence); (5) emulation-prevention round-trip vector.
- Live verification: B0-protocol traced run + `zc_metrics`; optionally a fed-bytes
  dump tap (reviewer: §7 Q5).

### Candidates C/D (recorded, not recommended)

C: the same surgery host-side in Swift at SPS emission — fixes the stream for every
receiver (including a possible M1 revisit) but builds a bit-writer from scratch in the
stability-critical host. D: accept 162 ms — rejected by the owner's stated priority.

**Recommended order: A first (free, doubles as the theory's confirmation experiment);
B only if A fails its numbers or regresses stability/quality. Acceptance for either =
the zero-copy review's own gate table (p50 ≤ 90 ms, p90 ≤ 140 ms, integrity/stability/
visual rows unchanged) — expected to pass with large margin if the theory holds.**

## 6. Predicted outcome (hypothesis, to be measured)

Removing 5 DPB periods ≈ −87 ms from receive→decode → segment lands ~18–26 ms
(1 parser gap + 6–9 ms decode + jitter/queue); E2E p50 ≈ 162 − 87 ≈ **~75 ms**
(range 65–85). The gate table, not this prediction, decides.

## 7. Questions for the reviewer

1. Accept the W0a STOP verdict and the refutation record for the copy-contention
   hypothesis?
2. Approve the A-then-B order (free codec probe before bitstream surgery)?
3. Candidate B sets `max_dec_frame_buffering = max_num_ref_frames (=1)`. Spec-safe, or
   should it be the level-derived MaxDpbFrames for defensive compatibility?
4. Fail-open (`None` → legacy path) on unexpected SPS shapes — acceptable, or demand
   fail-closed with a visible startup error?
5. For B's verification: unit re-parse + measured collapse sufficient, or require a
   fed-bytes dump re-inspected with `trace_headers`?
6. If A passes: any objection to `--hevc` becoming the launcher default at 4K?
7. The M1-receiver revisit note (§3.8): worth a task, or drop?

## 8. Evidence retained (committed with this report)

`evidence/zero_copy/`: `b0-legacy-*` and `w0a-probe-*` (host/client traces, joined,
join-summary, metrics.json) + `run-record.md` (SHAs, driver, workload, parity numbers,
joiner-predicate caveat). Bitstream dump stays on the box at `/tmp/stream4k.h264`
(30.8 MB; SPS hex + parsed fields preserved above and in the run record).

## 9. Amendments (per `W0_REPORT_AND_REORDER_FIX_PLAN_review.md`, all accepted)

The reviewed version of this file is preserved at commit `b4784d8`; corrections below
supersede the corresponding statements above.

1. **Wording narrowed as ordered**: "the copy hypothesis is refuted" → *removing the
   copy/render path is not the expected primary latency lever and failed the agreed
   gate* (the 8.397 ms p50 improvement is real; one manual A/B pair cannot prove copies
   cost nothing). "Content parity verified" → *comparable active workload supported by
   sender volume* (+6.3% bytes rules out an idle stream, not identical content).
   "The delay lives between feed and emission" → *the remaining delay is within the
   measured receive→decode interval* (`ts_recv_us` stamps assembly completion; channel/
   batch wait before decoder submission is inside the interval, unmeasured separately).
   B0's all-emission gap-6 rate is **99.5%** (3,643/3,663); 99.6% applies to presented
   frames. Console encoder/sender totals are process-cumulative, not trace-window
   counts.
2. **Root-cause status**: missing VUI is the **leading hypothesis, not a proved root
   cause** — the SPS closes the *allowance* arithmetic (5 + 1 = 6), but only a
   controlled H.264 signaling change that moves the gap proves NVDEC's policy. An HEVC
   success solves the product problem without proving the H.264 mechanism.
3. **Evidence preservation completed** (`evidence/zero_copy/bitstream/`): 23-byte
   SPS+PPS sample (SHA-256 verified box-side and post-transfer), full-dump SHA-256,
   complete `trace_headers` output, and the client log lines proving `h264_cuvid` was
   the active decode path.
4. **Execution order adopted**: Candidate A now at **50 Mbps explicitly**
   (`--bitrate 50` — without it the host silently defaults HEVC 4K to 40 Mbps, which
   would confound the comparison), with proof-of-codec requirements (ModeConfirm
   codec 1, `hevc_cuvid` opened, no silent H.264-decoder fallback — the client
   currently falls back invalidly on HEVC init failure and any such run is void);
   repeat once if the improvement is large; then launcher/codec shipping changes
   (launcher `--hevc` in both copies, client advertises HEVC in supported_codecs,
   fallback replaced with fail-fast) + 10-minute smoke at the *shipped* bitrate
   (recorded as such). If A fails → bounded env-gated
   `kVTVideoEncoderSpecification_EnableLowLatencyRateControl` host probe (dump + SPS
   inspect first, measure only if signaling changes). Corrected Candidate B only after
   both fail: prefer exact-known-SPS replacement over a general parser;
   `log2_max_mv_length_* = 15` (not 16); buffering=1 conditional on the confirmed
   no-reorder stream; no `constraint_set3_flag` shortcut; copy bits *before* the old
   VUI flag and regenerate trailing bits; atomic multi-SPS handling; 3- and 4-byte
   start codes; typed outcome enum instead of `Option`; bounded reads/allocations;
   fail-open at runtime but fail the acceptance gate if the expected rewrite didn't
   happen; post-rewrite fed-bytes tap + independent `trace_headers` verification; the
   enabled/disabled H.264 A/B is the causal experiment.
5. **W0a probe hygiene**: env presence (not value) currently activates it —
   `RESC_W0A_NO_DOWNLOAD=0` would still enable the probe. To be removed after this
   cycle seals (or value-parsed if kept).
