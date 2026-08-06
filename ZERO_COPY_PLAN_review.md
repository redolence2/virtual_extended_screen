# Review of `ZERO_COPY_PLAN.md`

Reviewed: `ZERO_COPY_PLAN.md` at local `HEAD 66b9730`

Review date: 2026-08-06

## Executive verdict

**CONDITIONAL GO on the architecture; do not implement W1-W6 verbatim yet. Proceed now with the corrected W0 gates below.**

The central decision is good: keep the decoded image on the GPU, make one bounded CUDA device-to-device copy into GL-owned textures, unmap, and release the decoder surface before waiting for display. For this fixed RTX 4090/Xorg machine, that is simpler and safer than trying to render directly from decoder-owned surfaces across vsync.

The plan is not yet implementation-ready because it treats the cause of the six-frame delay as proved, while the retained trace only shows correlation. Its synthetic W0 also does not exercise the production FFmpeg CUDA context or a real CUVID frame. There are a few concrete source mismatches as well: `AVCUDADeviceContext` is absent from the pinned Rust bindings, there is no existing setup channel, one pitch is insufficient for NV12, the proposed shader is not visually equivalent to the current warm filter, and the current render loop would retain the cloned surface through vsync unless ownership is changed explicitly.

This does **not** justify another long planning cycle. Treat this review as an amendment:

1. take one short current-HEAD legacy baseline;
2. run W0a and W0b as specified below;
3. if both pass, continue with the corrected W1-W5 contracts in this review without requesting another architecture review;
4. if W0a or W0b fails, stop before the GL rewrite and report the measured failure.

## What is accepted

### 1. One bounded device copy is the right design

**Accepted.** Literal zero-copy would couple decoder-surface lifetime to GL drawing and swap timing. The planned map/copy/unmap boundary allows the CUVID `AVFrame` reference to be returned before vsync while GL displays its own texture. That is the right latency/complexity tradeoff for this personal deployment.

CUDA/GL interop supports registered GL textures, mapped arrays, and device-to-array copies. CUDA work must be complete and the resources unmapped before GL uses them; successful unmap provides the required CUDA-to-graphics ordering. See NVIDIA's [OpenGL interop](https://docs.nvidia.com/cuda/cuda-driver-api/group__CUDA__GL.html) and [graphics interop](https://docs.nvidia.com/cuda/cuda-driver-api/group__CUDA__GRAPHICS.html) documentation.

### 2. The baseline numbers are valid

Independent recomputation of `evidence/demo/resc4k-final-joined.jsonl` reproduces the plan's rounded values:

| segment | p50 | p90 where relevant |
|---|---:|---:|
| capture -> encode out | 19.309 ms | |
| encode out -> receive | 5.610 ms | |
| receive -> decode done | 109.894 ms | |
| decode done -> present | 23.267 ms | |
| capture -> present | 157.418 ms | 220.089 ms |

Trace identity is clean: 2,669 joined frames, 2,385 presented frames, and no identity failures or ambiguities. The baseline is suitable as historical evidence.

The `decode_trigger_frame_id - recovered_frame_id == 6` rate is 84.03% **among presented frames** (2,004/2,385), or 81.45% among all joined emissions (2,174/2,669). State the denominator. This gap is an input/output ordinal delay, not a direct measurement of six occupied CUDA surfaces.

### 3. The current path is wasteful

The source does perform the GPU-to-CPU transfer, NV12-to-I420 deinterleave/copy, another full CPU copy in the renderer, scalar chroma adjustment, and an SDL texture upload. For a 2160x3840 NV12 image, four nominal full-payload moves total about 49.8 MB/frame or 2.49 GB/s at 50 fps.

Call that **payload accounting**, not measured PCIe or memory-bus traffic. The current evidence does not establish that this traffic is what causes CUVID's ordinal delay, nor does it prove the 60-70 ms projection. The proposed optimization is plausible; W0a must establish causality cheaply.

## Required amendments

### A. Re-baseline current HEAD once

The sealed latency baseline predates the completed keyframe-storm fix. Run one comparable 45-60 second legacy trace on the current Mac and Ubuntu commits before changing the decoder. Retain the exact host/client SHAs, dirty state, codec, resolution, display mode, driver, and run arguments.

This is not a new test campaign. It is one control run so that removal of the false keyframe storm is not silently credited to zero-copy.

The Ubuntu checkout inspected during this review was still at `767e845`, while this repository is at `66b9730`. Sync the Ubuntu box to the intended current commit before either the baseline or W0 and print its SHA/dirty state in the retained evidence.

### B. Replace W0 with two gates

#### W0a - causal no-download probe

Add a temporary, clearly logged mode that uses the real 4K CUVID decoder but, for hardware frames only:

- reads the emitted frame's PTS/identity;
- skips `av_hwframe_transfer_data` and `extract_yuv`;
- does not publish or render pixels;
- retains the existing trace-ledger resolution and clean footer.

This can emit a metadata-only/non-renderable frame because the current decode loop resolves identity before its empty-plane skip. Run it for 45-60 seconds under the same stream conditions.

Proceed only if receive-to-decode latency falls materially (use p50 <= 50 ms as the working gate) and the ordinal gap shifts toward <=2. If receive-to-decode remains near 110 ms or the gap remains concentrated at six, stop: removing the CPU transfer/render path is not the expected latency lever and the GL rewrite should not begin.

The gap is supporting structural evidence, not a literal surface-pool counter.

#### W0b - real FFmpeg-frame interop probe

The synthetic gradient is useful as a ten-minute environment smoke check, but it is not the production gate. The gate must use:

- the actual FFmpeg-created CUDA context;
- one real H.264 CUVID `AVFrame` with its cloned buffer reference;
- the real frame's two plane addresses and independent pitches;
- the actual SDL GL context on `DISPLAY=:0`;
- `cuGLGetDevices` plus `cuCtxGetDevice` to confirm GL and CUDA use the same RTX;
- map both R8/RG8 resources, copy both planes, unmap, release the cloned frame **before** swap, and render it repeatedly;
- several seconds of streaming to expose pool/lifetime failures, not only one still image.

This matters because CUDA pointers are context-specific, not “context-global,” and real FFmpeg pitches/layouts are exactly what the synthetic `cuCtxCreate` + `cuMemAlloc` pattern bypasses. See NVIDIA's [driver context guidance](https://docs.nvidia.com/cuda/cuda-programming-guide/03-advanced/driver-api.html) and [`cuMemcpy2D` documentation](https://docs.nvidia.com/cuda/cuda-driver-api/group__CUDA__MEM.html).

Target-box preflight was encouraging: the inspected system has direct GL 4.6 on the RTX 4090, NVIDIA driver 570.169, `libcuda.so.1`, the required graphics-interop symbols, and FFmpeg 7.1.5 CUDA headers. W0b still decides feasibility because those facts do not prove the real FFmpeg-context path.

### C. Make ownership and CUDA-context rules explicit

The pinned `ffmpeg-sys-next 7.1.3` generator includes `libavutil/hwcontext.h` but not `hwcontext_cuda.h`, so the W0 instruction to “check whether the binding exists” cannot make `AVCUDADeviceContext` appear. Do not use an undocumented byte-offset read. For this fixed Linux build, the simplest acceptable bridge is a tiny local `#[repr(C)]` prefix definition matching the public FFmpeg CUDA header's first `CUcontext cuda_ctx` field, guarded by `AV_HWDEVICE_TYPE_CUDA`, the expected libavutil major version, and a startup log. If the major/layout expectation changes, reject zero-copy with an actionable legacy-mode message. A tiny C shim compiled against `hwcontext_cuda.h` is also valid but is not required for this one-machine deployment. FFmpeg documents the public structure here: [`AVCUDADeviceContext`](https://www.ffmpeg.org/doxygen/trunk/structAVCUDADeviceContext.html).

Revise `CudaSurface` to own:

- an `av_frame_clone`-owned `AVFrame` reference, with null-clone handling;
- an `av_buffer_ref`-owned lease on the FFmpeg hardware device/context so it cannot disappear first;
- `y_dev` and `uv_dev`;
- separate `y_pitch` and `uv_pitch`;
- visible width/height and any relevant crop/layout metadata.

Before constructing it, validate and log:

- `AVFrame.format == AV_PIX_FMT_CUDA`;
- non-null `hw_frames_ctx`;
- `AVHWFramesContext.sw_format == AV_PIX_FMT_NV12`;
- non-null Y and UV pointers;
- positive pitches with each pitch at least its row width;
- even visible dimensions for this fixed NV12 profile;
- the expected CUDA/GL device.

If any condition changes after an Ubuntu, FFmpeg, or driver upgrade, fail with one actionable diagnostic and tell the owner how to select the legacy path. Do not guess another layout.

An `unsafe impl Send` can be justified only by this narrower invariant: the cloned frame owns the storage, the wrapper is not `Clone`/`Sync`, the render thread uses its pointers only while the exact FFmpeg CUDA context is current, and no thread mutates the surface concurrently.

All CUDA context push/pop and map/unmap operations need RAII guards so partial errors still restore the previous context and unmap resources. The `libloading::Library` must be stored for at least as long as every loaded function pointer and interop resource. Unregister CUDA resources while both the correct CUDA context and GL context still exist, then delete GL objects/context in order.

Add `cuGLGetDevices` and `cuCtxGetDevice` to the production symbol set. Let W0b test whether ordinary `cuMemcpy2D` accepts the real FFmpeg pitches. Only add `cuMemcpy2DUnaligned` or another copy fallback if that real test demonstrates a need; do not add speculative paths now.

### D. Actually release the surface before vsync

The current render loop borrows the frame for `update_frame` and keeps the `newest` variable alive through event handling and `present_with_cursor`. Merely documenting “immediate release” will therefore not make it true.

Make the CUDA upload consume the frame/pixel representation, or explicitly drop the `CudaSurface` immediately after successful unmap. Retain only ordinary metadata such as `recovered_frame_id` for the later present trace. The surface must be gone before the GL draw/swap can block on vsync.

Also move disposal of the overwritten mailbox frame outside the mailbox mutex. Returning a CUDA/FFmpeg buffer should not execute while the shared mailbox lock is held.

Instrument current external-surface count/high-water and CUDA/EAGAIN failures. `extra_hw_frames = 8` means eight extra frames beyond decoder requirements; it is not proof that the total pool is eight. Do not automatically increase it to 12. Increase only if telemetry shows legitimate bounded demand rather than a leak.

`ReferenceLoss` is not a valid hardware-starvation detector in the current code: it is produced by the software gray-frame check, while hardware transfer failures become `DecodeError`. Count every reason-tagged IDR plus EAGAIN, clone, CUDA, map/unmap, and render failures.

### E. Use one coherent rollback mode

For this one-user application, avoid the proposed backend matrix. Use one switch, for example:

```text
default                     = CUDA NV12 frame + CUDA/GL renderer
RESC_LEGACY_RENDER=1        = GPU->CPU transfer + existing SDL renderer
```

The legacy switch must coordinate decoder representation and renderer selection. `RESC_GL=0` by itself would otherwise send a CUDA-only frame to a renderer that requires CPU planes. Do not add the three-plane I420 GL path in this milestone. If software decode is selected, enter the complete legacy path automatically (or fail fast with a clear message if automatic switching would complicate startup).

There is no existing decode-to-render setup channel to reuse. The simplest implementation is to carry a ref-counted device/context lease with the first CUDA surface and initialize interop lazily on the render thread. A small explicit channel is also valid, but it must be newly designed; do not describe it as existing.

### F. Preserve output behavior exactly first

The proposed RGB warmth multiplier is not equivalent to the current renderer. Reproduce the existing clamped chroma operation in the shader before color conversion:

```text
U += -20 * warm_strength / 255
V +=  15 * warm_strength / 255
```

The current SDL path does not explicitly pin the YUV conversion matrix/range, so “BT.709 limited” cannot also be claimed as automatic visual parity. Use explicit constants, a color-bar/chroma-edge comparison, and owner side-by-side acceptance. Any intentional BT.709 correction should be recorded as a separate visual change rather than hidden inside the latency optimization.

The current cursor is procedurally drawn, not an existing bitmap that can simply be uploaded “on change.” Reproduce its shape and the existing four-corner rotation/input mapping in GL; implementation as a tiny generated RGBA texture or simple GL geometry is an internal choice.

Check and log GL shader compilation/link messages, GL vendor/renderer/version, requested and effective swap interval, FFmpeg/libavutil version, CUDA driver/device, format, dimensions, and both pitches. These logs are the maintainability mechanism for a future macOS/Ubuntu/FFmpeg change.

## Revised minimal work sequence

1. **B0:** sync exact commits and take one current legacy baseline.
2. **W0a:** real CUVID, no download/render causal probe.
3. **W0b:** real FFmpeg context + real retained CUVID frame -> CUDA/GL textures; verify drop before swap.
4. **W1/W2:** add validated `CudaSurface`, context/device lease, dynamic CUDA API, and RAII lifecycle.
5. **W3:** implement NV12-only GL video, exact warm behavior, rotation, cursor, and one coordinated legacy rollback.
6. **W4:** integrate a consume-and-release upload; keep identity and present-trace semantics unchanged.
7. **W5:** run the small acceptance gate below; document results and then demo.

Do not make a separate commit for every tiny internal step merely to satisfy the plan. A spike commit, production implementation commit, and evidence/report commit are enough if each remains reviewable.

## Demo acceptance gate

Use vsync interval 1 for acceptance and verify/log that it took effect. Interval 0 is diagnostic only. [`SDL_GL_SetSwapInterval`](https://wiki.libsdl.org/SDL2/SDL_GL_SetSwapInterval) can fail, so do not ignore its result.

Run two comparable 60-second legacy/candidate pairs with the same codec, resolution, host settings, warm strength, motion/drag workload, and commits. Because W0a separately tests the causal premise, this simple A/B is sufficient for the personal demo; a three-arm renderer experiment is unnecessary.

Required for demo GO:

| gate | requirement |
|---|---|
| trace integrity | clean footer; no identity failure/ambiguity |
| E2E latency | candidate p50 <= 90 ms in both candidate runs |
| tail latency | candidate p90 <= 140 ms |
| decode segment | receive -> decode p50 <= 50 ms, or a clearly explained equivalent structural reduction |
| ordinal delay | mode should move from 6 toward <=2; report it as a diagnostic, not surface occupancy |
| stability | zero CUDA/GL/map/unmap/render failures; no rising retained-surface count |
| recovery | no sustained reason-tagged IDR requests; report total/rate by reason |
| delivery | joined/host and presented/joined rates do not materially regress from the paired legacy runs |
| visual/input | owner accepts sharpness/color/warmth; cursor aligns at all four corners; no tearing |

Keep the plan's **p50 <= 90 ms** threshold. Do not tighten it to 80 ms before measurement. An 80-90 ms result that is stable and feels good is a successful demo; record the observed number honestly. If it remains above 90 ms, W0/W5 telemetry should identify the next segment instead of redefining success.

After the gate passes, do a 10-minute normal-use smoke run and release the demo. A 30-minute soak is useful follow-up evidence but should not block this first personal demo unless errors appear in the short run.

The existing `r4_live_gate.sh` does not enforce all of these numeric/error conditions. Either add a small summary checker or retain a human-readable metrics summary beside the traces; do not say the shell command itself passed gates it never evaluates.

## Answers to the five questions

1. **One bounded copy versus literal zero-copy:** one bounded copy is the right call.
2. **90 ms versus 80 ms:** keep p50 <= 90 ms for the demo; add p90 <= 140 ms. Do not pre-tighten to 80 ms.
3. **Vsync:** yes, interval 1 for acceptance; interval 0 only for diagnosis. Verify the effective setting.
4. **Software decode:** retain the complete legacy SDL path. Skip I420 GL in this milestone.
5. **`libloading` versus `-lcuda`:** `libloading` is appropriate on this fixed system. Own the library for the full symbol/resource lifetime, resolve exact ABI symbols once at startup, and produce actionable missing-symbol errors.

## Part A and wording corrections

Part A is stale context, not a blocker for Part B. The raw-byte keyframe defect is now closed at current `HEAD 66b9730`; the plan still describes the earlier `cad416d` state and says the running pair predates the fix. Update or shorten Part A rather than reopening it.

Also revise these claims:

- change “contention reduction is the lever” to “contention/copy removal is the hypothesis tested by W0a”;
- change “engineering projection 60-70 ms” to “best-case hypothesis 60-70 ms”;
- remove “device pointers are context-global”;
- remove the exact `1 + 1 + ~6 <= pool` proof;
- remove automatic `extra_hw_frames 8 -> 12` mitigation;
- replace `ReferenceLoss <= 1/min` with total typed IDR/error telemetry;
- replace “piggyback existing setup channel” because no such channel exists;
- change the 7-9 hour estimate to “W0-gated; re-estimate after real-frame interop succeeds.”

## Final decision

**GO now for the corrected B0/W0a/W0b. If those gates pass, GO for the bounded-copy implementation under the contracts above; no additional full-plan revision is required.**

The design is appropriately simple for a fixed personal machine: NV12 only, one CUDA/GL route, one complete legacy rollback, hard validation, and actionable startup/runtime logs. The only uncertainty worth stopping for is whether the actual FFmpeg/CUVID context and removal of the download produce the latency collapse that the plan predicts. W0 answers that before substantial code is written.
