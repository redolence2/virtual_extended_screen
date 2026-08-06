# Keyframe-Storm Fix (shipped) + Zero-Copy Render Plan — for review

**Date**: 2026-08-06 · **Owner priority**: latency ("feels 100 ms+; this is what I care about most")
**Baseline (sealed, traced)**: E2E capture→present **p50 157 ms / p90 220 ms** at 2160×3840@~50 fps
**Target**: E2E p50 **≤ 90 ms** acceptance gate (engineering projection 60–70 ms)
**Scope**: Ubuntu client only. Host untouched. Wire untouched. Sharpness untouched.

---

## Part A — Keyframe-storm fix (COMPLETE, commit `cad416d`)

> **Full reviewable completion report: `KEYFRAME_STORM_FIX_REPORT.md`** — root-cause
> mechanics (session-ID varint lottery + 10 Hz Stats arithmetic), wire analysis,
> enumerated behavioral deltas, and the live-verification protocol. The summary below is
> context for Part B only.

**Symptom.** In the 2026-08-06 morning run: 4,246 `IDR requested by client` / 4,247 keyframes
across ~83 K frames — a ~1 MB 4K keyframe every ~19 frames instead of every ~600 (10 s GOP).
Storms burn bandwidth, stretch paced sends, spike decode load, and mask real recovery.

**Mechanism (review finding 6, confirmed in source).** `HostSession.handleStreamingMessage`
scanned every streaming-phase control payload for **byte `0xFA` at any offset** (the varint
tag of Envelope field 31). Any payload byte with value 250 — inside session IDs, stats,
timestamps — forced a keyframe (rate-limit 250 ms → up to 4/s sustained, which matches the
observed 4,246 over ~18 min exactly).

**Fix.** `handleMessage` already decodes `Resc_Control_Envelope` (typed protobuf) for clock
traffic; added `case .requestIdr(let idr)` there → new `forceKeyframeRateLimited(reason:)`
(gated on `sm.state == .streaming`, same 250 ms limit); deleted the byte-scan;
`handleStreamingMessage` now only silently consumes non-IDR traffic.

**Diagnostic upgrade.** The client's `RequestIDR.reason` (DecodeError / CorruptFrame /
ReferenceLoss) is now logged per request. Honest caveat: some of the 4,246 may have been
*real* corruption-driven requests (the client fires on decode errors; ~3% assembler
frame_drops at 4K could drive them). The fix removes the false-positive class **and makes
the remainder attributable** — if reason-tagged storms persist, that is a genuine signal of
the contention/drop pathology that Part B attacks.

**State.** Builds clean; committed `cad416d`; the running pair predates it — picked up
automatically at the next icon relaunch. Post-relaunch, the log discriminates.

---

## Part B — Zero-copy render (PLANNED, awaiting this review)

### B.0 Why this is the latency lever (sealed measurements)

| segment | p50 |
|---|---|
| capture → encode out | 19.3 ms |
| encode → send → network → assemble | ~5.6 ms |
| **receive → decode done** | **109.9 ms** |
| decode done → present | 23.3 ms |

The 110 ms is not decode work (5–9 ms): `decode_trigger` gap analysis shows cuvid's
pipeline running **exactly 6 frames deep for 84% of frames** under GPU contention. Sealed
negative result: bounding the surface pool did NOT reduce depth and starved references
(135 forced KF/40 s) — pool tuning is the wrong lever; **contention reduction is the lever**.

### B.1 Current per-frame data path (verified in source, 2160×3840 NV12 ≈ 12.4 MB)

1. NVDEC decodes into a CUDA surface — `video-decode/src/lib.rs::drain_ready` (line 359).
2. `av_hwframe_transfer_data(sw_frame, decoded, 0)` (line 368) — **12.4 MB GPU→CPU PCIe
   download**, synchronizing against NVDEC.
3. `extract_yuv(&cpu_frame, …)` (line 451) — **CPU copy #2** into
   `DecodedFrame.planes: [Vec<u8>; 3]`, including NV12→I420 UV **deinterleave**.
4. Mailbox hand-off (pointer move — fine).
5. `renderer/src/lib.rs::Renderer::update_frame` (line 180) — **CPU copy #3**: allocates
   three fresh `Vec`s *every frame* (12.4 MB), row-copies all planes into `CachedYUV`;
   when Night Shift is on, a **per-byte scalar loop over ~4.1 M chroma bytes** applies the
   warm shift on the CPU (lines 204–215).
6. `PersistentTexture::update_and_copy` (line 53) — `SDL_UpdateYUVTexture` (**12.4 MB
   CPU→GPU upload**), then `SDL_RenderCopyEx(angle = −90°)` rotation+scale blit (SDL
   fullscreen bypasses xrandr rotation; the canvas is physical 3840×2160 landscape), then
   cursor copy, then present.

Total ≈ 50 MB of CPU/PCIe memory traffic per frame (~2.5 GB/s at 50 fps) plus a rotation
blit — all on the same RTX that NVDEC needs. This is the contention that holds the decode
pipeline 6 deep.

### B.2 Target per-frame data path

1. NVDEC decodes into a CUDA surface (unchanged).
2. Render thread: `cuMemcpy2D` **device→device** from the surface's `data[0]`/`data[1]`
   (pitch = `linesize[0]`) into two CUDA-mapped OpenGL textures — Y as `GL_R8`
   2160×3840, interleaved UV as `GL_RG8` 1080×1920. ~0.3–0.6 ms on-GPU; the decoder
   surface is released immediately after (surface-pool protection — see B.5 risk 3).
3. One GL draw: fragment shader does NV12→RGB (BT.709 limited range) **and** the warm
   tint (uniform multiply — deletes the CPU chroma loop); vertex transform does the −90°
   rotation + fit scale (deletes `RenderCopyEx`); cursor drawn as a small textured quad;
   `SDL_GL_SwapWindow`.

Design decision, stated for review: this is "zero-CPU-copy, one bounded on-GPU copy," not
literal zero-copy. Rendering directly from decoder surfaces would hold cuvid surfaces
across vsync — exactly the lifetime pressure the sealed negative result punishes. One
device copy (~0.5 ms) buys immediate surface release. 

### B.3 Work items (function level)

**W0 — interop spike (de-risk first, ~1 h).** New standalone binary
`ubuntu-client/src/bin/glcuda_spike.rs`:
`sdl2::init` → GL window on `DISPLAY=:0` → `gl::load_with(video.gl_get_proc_address)` →
create R8/RG8 textures → dlopen `libcuda.so.1` (via `libloading`; avoids a link-time dep)
→ `cuInit`, `cuCtxCreate` → `cuGraphicsGLRegisterImage(WRITE_DISCARD)` × 2 →
`cuGraphicsMapResources` → `cuGraphicsSubResourceGetMappedArray` → `cuMemcpy2D` from a
`cuMemAlloc`'d test-pattern buffer → shader render → visual gradient check at 60 fps.
**Gate: this must pass on the box's driver/GLX stack before any production code changes.**
Also verifies in-tree: whether `ffmpeg_sys_next` exposes `AVCUDADeviceContext` (for W1's
context extraction) against the installed ffmpeg7 headers.

**W1 — `video-decode` crate: dual-representation frames.**
- `DecodedFrame.planes/strides` → `pub pixels: FramePixels` where
  `enum FramePixels { Cpu { planes: [Vec<u8>; 3], strides: [usize; 3] }, CudaNv12(CudaSurface) }`.
- `pub struct CudaSurface { frame: *mut AVFrame /* av_frame_clone-owned */,
  y_dev: u64, uv_dev: u64, pitch: usize, width: u32, height: u32 }`;
  `impl Drop` → `av_frame_free`; `unsafe impl Send` (device pointers are context-global;
  frame refcount is thread-safe).
- `drain_ready()` cuvid branch: **delete** the `av_hwframe_transfer_data` call and
  `extract_yuv` for hw frames; instead `av_frame_clone(decoded)` → `CudaSurface`.
  Corruption check reads `decode_error_flags` off the hw frame directly.
  `recovered_frame_id` logic reads `decoded` — **unchanged** (C3/identity semantics
  preserved verbatim). Software branch: unchanged (`FramePixels::Cpu`).
- `pub fn cuda_context(&self) -> Option<*mut c_void>` — from
  `hw_device_ctx → AVHWDeviceContext.hwctx → AVCUDADeviceContext.cuda_ctx` (binding
  presence verified in W0; fallback = documented first-field offset read).
- Escape hatch: env `RESC_NO_ZEROCOPY=1` keeps the old transfer path (it must keep
  compiling anyway — the software backend uses it).

**W2 — new module `crates/renderer/src/cuda_gl.rs` (FFI + interop, ~150 lines).**
- `libloading` dlopen of `libcuda.so.1`; typed wrappers for exactly:
  `cuCtxPushCurrent_v2, cuCtxPopCurrent_v2, cuGraphicsGLRegisterImage,
  cuGraphicsUnregisterResource, cuGraphicsMapResources, cuGraphicsUnmapResources,
  cuGraphicsSubResourceGetMappedArray, cuMemcpy2D_v2, cuGetErrorString`.
- `pub struct GlInterop { y_res, uv_res, ctx }`;
  `fn register(ctx, y_tex: GLuint, uv_tex: GLuint) -> Result<Self>`;
  `fn upload(&mut self, s: &CudaSurface) -> Result<()>` = push ctx → map → get arrays →
  `cuMemcpy2D` ×2 (src DEVICE with `srcPitch = s.pitch`; dst ARRAY;
  Y: WidthInBytes = width, Height = height; UV: WidthInBytes = width, Height = height/2)
  → unmap → pop ctx. Any CUresult ≠ 0 → error with `cuGetErrorString` text.

**W3 — `renderer` crate: GL pipeline (largest item).**
- `Renderer::new`: window gains `.opengl()`; `gl_create_context()`;
  `gl::load_with(...)`; `gl_set_swap_interval(1)` (vsync ON for acceptance; interval 0
  measured as a data point). Old `Canvas`/`SDL_Renderer` path retained behind env
  `RESC_GL=0` for one release as instant rollback.
- `struct GlPipeline { prog, vao, vbo, y_tex, uv_tex, i420_u_tex, i420_v_tex, cursor_tex,
  u_warm, u_transform, u_mode }` + `fn compile()`:
  - vertex shader: fullscreen quad × `u_transform` mat3 — encodes the −90° rotation +
    fit-scale currently done by `RenderCopyEx` (same math as renderer lib.rs lines 63–79,
    moved to a uniform computed once per canvas/stream geometry).
  - fragment shader: `u_mode` selects NV12 (Y=R8 + UV=RG8) or I420 (three R8 planes —
    the software-decode path); BT.709 limited-range matrix; warm tint as
    `rgb *= mix(vec3(1.0), vec3(1.06, 0.98, 0.90), u_warm)` — replaces the CPU chroma
    loop AND the old UV-shift math (visual parity check in W5).
- `update_frame(&mut self, frame)` branches on `FramePixels`:
  `CudaNv12(s)` → `interop.upload(s)`; `Cpu{..}` → `glTexSubImage2D` per plane.
  `CachedYUV` and `PersistentTexture` are deleted on the GL path.
- `present_with_cursor(&mut self, cursor)`: video quad draw → cursor quad draw
  (`CursorRenderer` bitmap uploaded to `cursor_tex` on change only) → swap. The
  present-timestamp trace call site in `main.rs` is unchanged (records after swap —
  C3/R4 joiner semantics preserved).
- `canvas_size()` / `is_rotated()` APIs unchanged (input mapping depends on them).

**W4 — `main.rs` integration (small).**
- Decode thread sends `decoder.cuda_context()` to the render thread once at startup
  (piggybacked on the existing setup channel).
- `FrameMailbox` unchanged structurally; newest-wins `put()` drops the older
  `DecodedFrame` → its `CudaSurface` Drop frees the surface promptly (leak = pool
  starvation, so W5 asserts this).
- Surface budget: outstanding hw refs = 1 (mailbox) + 1 (uploading) + decoder internal
  ~6 ≤ pool (default surfaces + `extra_hw_frames = 8`). If starvation appears it is now
  observable (host logs `reason=ReferenceLoss` storms): bump `extra_hw_frames` 8→12 —
  the sealed lesson says never shrink, growing is the safe direction.

**W5 — measurement + acceptance (same rig as the sealed baseline).**
- `RUN_TAG=zerocopy STREAM_SECS=45 HOST_ARGS="2160 3840 60 --client 192.168.50.47"
  bash tools/r4_live_gate.sh` with owner dragging during the run; join traces; compare
  segment percentiles against baseline (157/109.9/23.3).
- Acceptance gates: E2E p50 ≤ 90 ms; `decode_trigger` depth mode ≤ 2 (from 6);
  ReferenceLoss-tagged IDR ≤ 1/min steady state; 30-min soak with 0 render_failures and
  frame_drops ≤ baseline; owner visual: sharpness unchanged, no tearing, no color shift
  (side-by-side screenshot vs `RESC_GL=0` path for BT.709/warm parity).
- Rollback ladder: `RESC_GL=0` env → old renderer; `RESC_NO_ZEROCOPY=1` → old decode
  transfer; `git revert` — host and wire never changed.

**W6 — seal.** Separate commits per W-item; update `NATIVE_4K.md` measured table; project
memory update; task #28 closes on owner verdict.

### B.4 Effort and conditions

| item | estimate |
|---|---|
| W0 spike | ~1 h |
| W1 decode crate | ~1–1.5 h |
| W2 FFI/interop | ~1 h |
| W3 GL renderer | ~2–3 h |
| W4 integration | ~0.5 h |
| W5 measure + soak | ~1 h + 30 min soak |

Total ~7–9 h wall-clock, all box-side builds over ssh, **repeated client restarts** — the
owner's screen bounces throughout; a dedicated downtime window is agreed before starting.

### B.5 Risk register

1. **GLX/CUDA interop failure on this driver stack** — mitigated by W0 spike gating
   everything; if the spike fails, stop and report (fallback direction: CUDA→EGLImage or
   Vulkan interop — new plan, new review).
2. **Color/gamma mismatch in the shader** (BT.709 limited vs SDL's conversion) — W5
   side-by-side parity check; matrix constants unit-stated in code.
3. **Surface-pool starvation from held hw frames** — bounded by design (1+1 outstanding,
   immediate release after device copy); observable via the new reason-tagged IDR log;
   growth-only mitigation (`extra_hw_frames` 8→12). Sealed negative result respected:
   never bound the pool down.
4. **`ffmpeg_sys_next` lacks `AVCUDADeviceContext`** — checked in W0; documented
   first-field offset fallback.
5. **Software-decode regression** — I420 GL path kept + `RESC_GL=0` full fallback;
   doctor's existing IYUV/NV12 texture checks remain green.
6. **Input/rotation mapping drift** — `canvas_size()`/`is_rotated()` contracts frozen;
   W5 includes cursor-alignment check at all four screen corners.

### B.6 Questions for the reviewer

1. Is "one bounded device copy, immediate surface release" the right call vs. true
   zero-copy rendering from decoder surfaces (surface-lifetime risk)?
2. Acceptance at E2E p50 ≤ 90 ms — agree, or tighten to ≤ 80 ms before sealing?
3. Vsync ON for acceptance (tear-free, up to +16.7 ms present wait) with interval-0
   measured only as a data point — agree?
4. Software-decode path: keep the three-plane I420 GL variant (plan), or retain the whole
   legacy SDL_Renderer solely for the software backend?
5. Any objection to `libloading`/dlopen of `libcuda.so.1` over link-time `-lcuda`?
