# Response to the Zero-Copy Plan Review

**Date**: 2026-08-06 · **Responds to**: `ZERO_COPY_PLAN_review.md` (CONDITIONAL GO)
**Verdict on the verdict**: **ACCEPTED IN FULL.** Every required amendment is adopted
verbatim; no counter-arguments. The plan document is superseded by
plan + review + this response together (per the review: no further full-plan revision;
B0/W0a/W0b decide whether implementation proceeds).

## 1. Corrections owned

- **Causality vs correlation** — the review is right: the 6-frame ordinal gap and the
  49.8 MB/frame payload accounting are correlation and arithmetic, not proof that the
  CPU download path causes the ~110 ms. Adopted wording: "contention/copy removal is the
  hypothesis tested by W0a"; "best-case hypothesis 60–70 ms". W0a is now the causal gate
  and its failure stops the project before any GL code.
- **`ffmpeg-sys-next 7.1.3` does not bind `hwcontext_cuda.h`** — my W0 step "check
  whether the binding exists" could never succeed. Adopted: local `#[repr(C)]` prefix
  struct matching the public `AVCUDADeviceContext` first field (`CUcontext cuda_ctx`),
  guarded by `AV_HWDEVICE_TYPE_CUDA` + expected libavutil major + startup log; any
  mismatch → actionable reject into legacy mode. No byte-offset guessing.
- **"Device pointers are context-global" — wrong**, removed. CUDA pointers are
  context-specific; the interop uses the FFmpeg-created context exclusively, verified
  via `cuGLGetDevices` + `cuCtxGetDevice` (added to the production symbol set).
- **"Piggyback the existing setup channel" — no such channel exists.** Adopted: carry a
  ref-counted device/context lease with the first `CudaSurface`; interop initializes
  lazily on the render thread.
- **Surface-release claim was documentation, not mechanism** — the render loop keeps
  `newest` alive through present. Adopted: the upload **consumes** the frame
  representation (explicit drop after successful unmap; only plain metadata like
  `recovered_frame_id` survives for the present trace), and disposal of
  mailbox-overwritten frames moves outside the mailbox mutex.
- **Pool arithmetic and mitigations withdrawn**: the `1+1+~6 ≤ pool` proof, the
  automatic `extra_hw_frames 8→12` step, and `ReferenceLoss` as a starvation detector
  (it is produced by the software-only gray check; hw transfer failures surface as
  `DecodeError`) are all removed. Adopted instead: instrument external-surface
  count/high-water plus every typed IDR reason, EAGAIN, clone, CUDA, map/unmap, and
  render failure; grow the pool only on telemetry showing bounded legitimate demand.
- **Rollback matrix was incoherent** (`RESC_GL=0` alone would feed CUDA frames to a
  CPU renderer). Adopted: one coordinated switch — default = CUDA NV12 + CUDA/GL
  renderer; `RESC_LEGACY_RENDER=1` = transfer + existing SDL renderer; software decode
  auto-selects the complete legacy path. The I420-GL variant is dropped from this
  milestone.
- **Warm filter and colorimetry**: my RGB multiplier was not equivalent. Adopted: the
  shader reproduces the exact clamped chroma math (`U += −20·s/255`, `V += +15·s/255`)
  before conversion; conversion constants are explicit; parity is judged by color-bar /
  chroma-edge comparison plus owner side-by-side, and any intentional colorimetry change
  would be its own recorded change, not smuggled into a latency patch.
- **Cursor**: procedurally drawn today, not an uploadable bitmap — will be reproduced in
  GL (generated RGBA texture or geometry, internal choice), with the four-corner
  rotation/input mapping checks retained.
- **Estimate**: "7–9 h" replaced by "W0-gated; re-estimate after real-frame interop
  succeeds."
- **Part A staleness**: superseded by the dedicated report trio
  (`KEYFRAME_STORM_FIX_REPORT{,_review,_response(+amendments)}.md`); the storm item is
  closed at `66b9730`. Part A is context only.

## 2. Adopted execution contract (review's revised sequence)

1. **B0** — sync both checkouts to the same commit (box was at `767e845`; being
   fast-forwarded — client sources are unchanged between `767e845` and current HEAD, so
   the installed client binary already matches source) and take ONE 60 s legacy baseline
   on current HEAD with a scripted, reproducible motion workload; retain SHAs/dirty
   state/driver/args in the evidence.
2. **W0a** — causal probe: real 4K CUVID decode, hw frames skip
   `av_hwframe_transfer_data` + `extract_yuv`, nothing published/rendered, identity +
   ledger + clean footer preserved, clearly logged temp mode. Gate:
   receive→decode p50 ≤ 50 ms AND ordinal gap moving toward ≤ 2, else STOP and report.
3. **W0b** — interop probe on the real FFmpeg CUDA context and a real cloned CUVID
   frame's plane pointers/pitches, into R8/RG8 GL textures on the real `DISPLAY=:0` SDL
   GL context; device identity checked; map→copy→unmap→**drop before swap**; several
   seconds of streaming, not one still.
4. **W1–W5** under the review's contracts (validated `CudaSurface` with frame clone +
   device-ctx lease + separate pitches + validation list; narrowed `Send` invariant;
   RAII for ctx push/pop and map/unmap; `libloading` library owned for full resource
   lifetime; ordered teardown; consume-and-release upload; NV12-only GL; exact warm
   math; one legacy switch).
5. Commit granularity per the review: spike commit, implementation commit,
   evidence/report commit.

## 3. Acceptance gate adopted verbatim

Two comparable 60 s legacy/candidate pairs, same everything; vsync interval 1 verified
via the `SDL_GL_SetSwapInterval` result and logged; gates: clean trace integrity;
candidate E2E p50 ≤ 90 ms (both runs) and p90 ≤ 140 ms; receive→decode p50 ≤ 50 ms (or
explained structural equivalent); ordinal-gap mode reported as diagnostic; zero
CUDA/GL/map/unmap/render failures and no rising retained-surface count; IDR totals/rates
by reason with no sustained storms; delivery ratios not materially regressed; owner
visual/input acceptance at four corners. p50 stays at 90 ms — not pre-tightened; an
honest 80–90 ms stable result ships. A small summary checker (or human-readable metrics
summary beside the traces) will state gate results — the shell runner alone claims
nothing it doesn't evaluate. 10-minute smoke then demo; 30-minute soak becomes follow-up
evidence.

## 4. Execution status

- Box fast-forward to current HEAD: in progress with this response's push (client
  binary already equivalent; SHA printed into `evidence/zero_copy/` records at B0).
- B0 + W0a + W0b are ready to run and need a screen window of roughly 30–45 minutes
  (three short measured runs plus one brief GL takeover of the box display for W0b);
  owner names the window per the standing agreement.
- If W0a or W0b fails its gate: stop, retain the measurement, report — per the review,
  that outcome is a finding, not a failure of process.
