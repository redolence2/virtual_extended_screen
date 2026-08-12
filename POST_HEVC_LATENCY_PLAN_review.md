# Review of `POST_HEVC_LATENCY_PLAN.md`

Reviewed: `POST_HEVC_LATENCY_PLAN.md` at local `HEAD c7270bb`

Review date: 2026-08-07

## Executive verdict

**GO WITH AMENDMENTS for optional post-demo experiments; do not implement the plan verbatim.**

The measured HEVC result is strong enough to use as the current product baseline: the two retained runs reproduce E2E p50 values of **81.204 ms** and **79.565 ms**, and the old H.264 gap-six pattern has collapsed to gap one. The first priority remains the already-pending 10-minute owner smoke on the shipped launcher. Nothing in this review blocks that demo gate.

For further optimization:

1. **Conditionally approve one same-buffer AUD experiment.** The proposed bytes are syntactically valid, and a trailing AUD is a plausible way to test whether the remaining gap is caused by CUVID waiting for the next access-unit boundary. It is not equivalent to NVIDIA's explicit end-of-picture signal, so approve it as a bounded diagnostic first, not as an already-approved production design.
2. **Reject the separate-packet AUD variant.** It adds packet timestamp, identity-ledger, retry, and drain semantics without adding useful diagnostic power.
3. **Do not revive CUDA/GL yet.** First fix the known renderer inefficiency that reuploads the full 4K texture on cursor-only redraws, then try the already-supported NV12 SDL upload path. Re-measure before deciding whether a custom interop path is worth its complexity.
4. **Approve the ASUS-unplugged A/B and Apple HEVC low-latency probe.** They are cheap, independent tests. The Apple probe needs a fresh compression session and explicit keyframe/recovery validation.
5. **Do not call 45–55 ms an architecture floor.** It is an optimistic scenario estimate. The present evidence cannot establish a lower bound that tight.

There is no need for another rewritten optimization plan. This review can serve as the implementation amendment.

## Decision summary

| Plan item | Decision |
|---|---|
| HEVC a1/a2 latency measurements | **Accepted** |
| “Gap one is consistent with one-AU parser delay” | **Accepted as the leading hypothesis** |
| “17.5 ms structural wait + ~8 ms real decode” | **Not supported as written** |
| Same-buffer trailing AUD probe | **Conditional GO, diagnostic only in this round** |
| Separate AUD packet | **No-go** |
| Proposed AUD bytes ending in `0x50` | **Syntactically valid** after one-time stream checks |
| Direct `CUVID_PKT_ENDOFPICTURE` | **Semantically preferred production control**, but defer the custom FFmpeg work until the cheap probe proves value |
| Zero-copy/CUDA-GL as the next implementation | **No-go for now** |
| Simple renderer and NV12 cleanup first | **Go** |
| ASUS-display A/B | **Go** |
| Apple low-latency HEVC probe | **Go with runtime and recovery gates** |
| Encoder submission depth 2 to 1 | **Remain rejected** |
| 45–55 ms “architecture floor” | **Reject the label; retain only as a stretch projection** |

## 1. What the retained evidence actually establishes

Independent recomputation reproduces the committed results:

| run | E2E p50 / p90 | receive to decode p50 | presented gap-one count |
|---|---:|---:|---:|
| a1 | 81.204 / 86.997 ms | 25.157 ms | 3,156 / 3,157 |
| a2 | 79.565 / 94.895 ms | 29.454 ms | 2,950 / 2,951 |

The trace footers and identity joins are clean. This is sufficient to say that HEVC removed the old fixed gap-six behavior and left a highly repeatable gap-one relationship.

The more detailed decomposition in lines 16–25 of the plan is too confident, however:

- For gap-one samples, the measured interval from receipt of picture N to receipt of the picture that triggers N is approximately **19.202 ms in a1 and 19.188 ms in a2**, not 17.5 ms. The stream's median cadence and its average cadence are different in these runs.
- The remaining trigger-receipt-to-decoded-emission medians are approximately **5.885 ms and 10.514 ms**. That residual is not “real decode” alone.
- `capture -> encode out` covers capture scheduling, the host queue/submission path, VideoToolbox, and the encoder callback. It is not a pure HEVC hardware-encode timer.
- `encode out -> receive` ends only after the complete wire frame has been assembled. It includes sending, chunking, pacing, Wi-Fi, and receiver assembly; “near floor” has not been measured.
- `receive -> decode done` includes receiver queueing, FFmpeg/CUVID parsing and decoding, `av_hwframe_transfer_data`, NV12 extraction/deinterleave, and CPU plane copies. The decode-done trace is recorded only after all of that returns.
- `decode done -> present` includes the mailbox, another three-plane renderer copy, SDL texture upload, rotation/copy, and the wait inside vsynced presentation.

Consequently, zero-copy work can affect **both** of the latter trace segments. The plan's assignment of the entire copy chain to decode-to-present is incorrect.

The segment p50 values also cannot be added as though each came from the same frame and display phase. Their sums are 78.156 ms for a1 and 82.993 ms for a2, whereas the measured E2E medians are 81.204 ms and 79.565 ms. Projections based on subtracting median components must remain approximate scenarios.

### Delivery must remain part of every result

The E2E values describe frames that survived through presentation. That matters because a1 presented about **95.2%** of its joined frames, while a2 presented about **88.1%**. An optimization must not appear faster merely by dropping more old frames. Every comparison should retain:

- host frames versus joined frames;
- joined frames versus presented frames;
- queue/drop counters;
- p50 and p90 latency;
- identity failures, ambiguities, and footer status.

### Provenance amendment, without blocking the demo

The a1/a2 JSON evidence is internally sound, and the installed launchers now use HEVC at 50 Mbps. The retained files do not, however, include a dedicated a1/a2 run record, exact source/binary hashes, launch logs, HEVC negotiation and `hevc_cuvid` selection, display/driver state, or a small HEVC VPS/SPS/PPS/AUD sample. The existing run record and bitstream sample cover the older H.264 work.

Do **not** rerun a1/a2 merely to beautify the archive. On the 10-minute smoke or the next optimization baseline, retain those small provenance items so a future agent can distinguish a codec, driver, macOS, or Ubuntu change from a performance regression.

## 2. E1: AUD append is a valid falsification test, with narrower approval

### Mechanism

NVIDIA documents the exact parser control for this situation: when each input packet contains exactly one complete picture, setting `CUVID_PKT_ENDOFPICTURE` lets the parser skip NAL boundary detection and trigger the decode callback immediately. See the [NVIDIA NVDEC parser guide](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.1/nvdec-video-decoder-api-prog-guide/index.html).

The current wire and client already satisfy the important precondition: one fully assembled encoded frame becomes one `AVPacket`, and that packet receives the wire frame ID as its PTS. But FFmpeg 7.1.5's CUVID wrapper sets a timestamp flag and does not expose an ordinary `AVPacket` switch that maps to `CUVID_PKT_ENDOFPICTURE`; its low-delay flag only selects zero display delay. See FFmpeg's [`cuviddec.c`](https://github.com/FFmpeg/FFmpeg/blob/n7.1.5/libavcodec/cuviddec.c#L426-L488).

Appending an AUD is therefore a plausible application-level proxy, but it has different semantics. The AUD starts the **next** access unit; it is not a normative “finish decoding the current picture” command. Its arrival may nevertheless make CUVID close and emit the previous picture, which is exactly what this experiment should test.

### Proposed bytes

`00 00 00 01 46 01 50` is a valid Annex-B HEVC AUD for the common base-layer, temporal-ID-zero stream:

- `00 00 00 01`: four-byte Annex-B start code;
- `46 01`: `AUD_NUT` (NAL type 35), layer ID 0, `nuh_temporal_id_plus1 = 1`;
- `50`: `pic_type = 2`, followed by the RBSP stop bit and padding.

`pic_type=2` permits I, P, or B slices; it does not require the stream to contain B pictures. It is the conservative value when the retained HEVC headers have not proved an I/P-only stream. `pic_type=1` could be used after proving there are no B pictures, but it offers no latency advantage. EOS or EOB NALs are not safer substitutes because they carry real sequence/bitstream termination semantics.

Before hard-coding these bytes, inspect a small current HEVC sample once and establish:

1. the encoder does not already emit an AUD at the start of each AU;
2. the stream uses layer 0 and temporal ID 0;
3. each wire frame contains one complete VCL-bearing AU.

This one-time check is enough for this fixed personal setup. Keep an actionable warning and disable the experiment if a future encoder begins emitting native AUDs, because two AUDs in one AU would be nonconforming.

### Test only the same-buffer variant

Append the AUD to the same `AVPacket` buffer as the real AU. This preserves one application packet, one PTS, and one identity-ledger admission. Capture or log the exact **post-append bytes fed to the decoder**; the current dump occurs before `decoder.decode()` and would otherwise prove only the original input.

Do not test the separate-packet variant. It requires a synthetic PTS policy, creates another `send_packet`/EAGAIN/drain cycle, and risks associating a decoded output with the wrong ledger entry. It cannot answer the boundary hypothesis better than the same-buffer test.

Use an exact value-parsed experiment switch, emit one startup warning, and make rollback trivial. Also exercise EOF flush: the last synthetic AUD may leave a non-VCL next-AU prefix pending, so shutdown must still produce a clean footer and no unresolved identity.

### Acceptance gate

One negative 60-second paired run is enough to stop this line. A positive result needs a repeat and the short stability smoke:

| gate | requirement |
|---|---|
| Structural result | Presented ordinal mode changes from gap one to **gap zero** |
| Latency | Paired receive-to-decode p50 improves by at least about **10 ms**; `<=12 ms` remains a useful target, not the only pass condition |
| E2E | Material paired improvement consistent with the decode change; approximately 62–65 ms is plausible, not guaranteed |
| Identity | Zero failures and ambiguities; trigger/decoded PTS association remains correct |
| Decode stability | No malformed-AU errors, recovery loop, sustained IDR requests, or keyframe regression |
| Delivery | No material joined/presented-rate or queue-drop regression |
| Confirmation | Repeat the positive 60-second result, then perform a 10-minute normal-use smoke |

For this review, that is approval of a **diagnostic**, not blanket approval to ship the workaround. If it works, the owner can make a pragmatic follow-up choice: keep the small, documented fixed-rig workaround after the stability gate, or use a narrowly pinned FFmpeg/CUVID `ENDOFPICTURE` patch as the cleaner long-lived mechanism. Do not start the dependency patch before E1 proves that the missing boundary is worth fixing.

## 3. E2: use the simpler renderer path before CUDA/GL

The current code contains a concrete inefficiency that the plan overlooks: `present_with_cursor()` calls the full `SDL_UpdateYUVTexture` path whenever it redraws, including cursor-only updates. That means cursor motion can upload the cached 4K video frame again even though no new video frame arrived.

The first renderer step should therefore be:

1. upload the texture only when `update_frame()` receives a new video frame;
2. on cursor-only redraws, reuse the resident texture and only clear/copy/draw/present;
3. reuse allocated plane/scratch buffers instead of allocating and copying three fresh vectors per frame.

Then preserve the transferred NV12 layout and use `SDL_UpdateNVTexture` rather than scalar NV12-to-I420 deinterleave followed by three-plane recopy. A working real-frame NV12 texture/update probe already exists in `ubuntu-client/src/doctor.rs`. Preserve the warmth behavior: use the direct path when warmth is zero and a reusable conversion/filter scratch path when it is nonzero, or implement equivalent UV adjustment deliberately.

This remains a CPU download plus GPU upload, but it is much smaller and easier to maintain than CUDA/GL interop. Measure it before assuming the custom path has 10–15 ms left to remove.

### If CUDA/GL is still considered

Do not call the reviewed design literal zero-copy. It performs one bounded CUDA device-to-device copy into GL-owned textures so the decoder surface can be released before vsync.

The previous H.264 W0a result—about 8.4 ms improvement—does not predict the HEVC gain after the ordinal gap changed from six to one. Before W0b:

- restore a value-parsed **HEVC W0a-prime** or add equivalent cheap stage timers;
- stop if the measured HEVC download/extract/render cost is immaterial after the SDL cleanup;
- make W0b use an actual HEVC CUVID frame and revalidate format, pitch, CUDA context, ownership, synchronization, and teardown;
- define exactly when `ts_decode_done` is recorded once GPU work crosses threads, so before/after segment names remain comparable;
- use a relative benefit gate derived from the new baseline, not the old absolute thresholds.

The contracts in `ZERO_COPY_PLAN_review.md` otherwise remain relevant. A defensible expected range before measurement is roughly **5–10 ms**, not a promised 10–15 ms. Proceed only after the simpler path leaves at least about 5–8 ms of clearly removable cost and the owner still wants it.

## 4. E3: encoder probes

### ASUS display A/B

Approved. It is the cheapest test and should happen before interpreting the entire capture-to-encode interval as encoder work. Use the same content, network, and receiver conditions; change only the second Mac display. If the difference is material, the simplest fixed-rig solution may be an operating habit.

### Apple low-latency rate control with HEVC

Approved as a small, env-gated runtime probe. Apple's current [low-latency conferencing sample](https://developer.apple.com/documentation/videotoolbox/encoding-video-for-low-latency-conferencing) includes HEVC Main/Main10 handling, so this is not statically killed for HEVC even though older introductory material emphasized H.264.

Apply `EnableLowLatencyRateControl` at compression-session creation and create a fresh session for each side of the A/B. Record:

- session-creation status and selected encoder/hardware-use information;
- 4K60 throughput and capture-to-encode-out distribution;
- valid VPS/SPS/PPS and ordinary decode behavior;
- bitrate and visual quality;
- scheduled keyframes and a forced-IDR/loss-recovery test.

Measure a successfully created valid stream even if its parameter-set syntax appears unchanged. The encoder selection/rate-control mode can alter timing without changing the visible SPS fields. Conversely, its documented infinite-GOP/temporal behavior may conflict with the current 10-second keyframe and recovery design, so forced-keyframe behavior is a hard gate.

### Depth two to one

Keep this rejected. The plan's wording should not call the 23.5 ms capture-to-encode-out metric pure HEVC encode time, but the previously retained depth-one throughput result already shows that reducing the submission gate is unsafe for this 4K60 setup. Also keep the application's submission-depth limit distinct from VideoToolbox's `MaxFrameDelayCount` property.

## 5. Revised minimal sequence

1. **Finish the pending 10-minute shipped-launcher smoke.** This is the demo gate and remains independent of optional optimization.
2. **Seal provenance on that smoke or the next baseline:** source/binary hashes, exact args, codec negotiation, `hevc_cuvid`, display/driver state, and a small HEVC parameter-set/AUD sample.
3. **Run the zero-code ASUS unplugged A/B.** Keep it only if material.
4. **Run E1 once, same-buffer only.** Stop on a negative result; repeat and smoke a positive result.
5. **Fix resident-texture/cursor-only upload behavior, then try the NV12 SDL path and reusable buffers.** Rebaseline after each small coherent change.
6. **Run the Apple HEVC low-latency fresh-session A/B** with keyframe/recovery and quality gates.
7. **Owner stop/go.** If more work is still worthwhile, run HEVC W0a-prime, then W0b, and only then consider bounded-copy CUDA/GL.

E1, the display test, renderer cleanup, and the Apple probe are technically independent after a clean baseline; their exact order may be adjusted for convenience. What should not happen is jumping directly from the current plan into the 7–9 hour CUDA/GL implementation.

## 6. Projection and floor correction

It is reasonable to describe these as planning scenarios:

| scenario | defensible interpretation |
|---|---|
| Current HEVC | Approximately 80 ms measured E2E p50 |
| Boundary wait removed | Approximately 62–65 ms is plausible |
| Plus small renderer/encoder wins | Roughly 50–60 ms may be possible |
| 45–55 ms | Stretch scenario requiring measurements, not a demonstrated floor |

The trace ends at return from `canvas.present()`; it does not measure physical scanout or photon arrival. Vsync phase, segment covariance, queueing, and survivorship all prevent a tight lower-bound claim from the current medians. A result below 45 ms would not by itself prove that a completely new architecture had been used. Rename “architecture floor” to **optimistic scenario range** and let the gates replace estimates with measurements.

## 7. Direct answers to the plan's questions

1. **Is AUD append sound, and which variant?** It is a plausible and bounded parser-boundary experiment, not equivalent to explicit end-of-picture. Test only the same-buffer variant.
2. **Is `pic_type=2` acceptable?** Yes. The proposed bytes are syntactically valid for layer 0/temporal ID 0, and value 2 safely permits I/P/B. Verify those fixed stream properties and absence of native AUDs once. There is no safer generic NAL; EOS/EOB would be semantically wrong.
3. **Do the zero-copy contracts carry over?** Mostly, but add a HEVC W0a-prime measurement, real HEVC W0b validation, an explicit cross-thread decode-done timestamp definition, and relative gates. Do the simpler renderer/NV12 work first.
4. **Is Apple HEVC low-latency precluded?** No. Current Apple sample material supports an HEVC runtime probe. A fresh session, hardware/throughput checks, valid output, and forced-keyframe recovery decide the result.
5. **Approve E1 -> E3 -> owner -> E2?** Not verbatim. Finish the demo smoke, take the zero-code display measurement, run bounded E1, perform the known SDL cleanup, then the Apple probe; revive CUDA/GL only after a new owner decision and HEVC-specific gates.
6. **Is 45–55 ms a sound architecture floor?** No. Keep it only as an optimistic projection.

## Final recommendation

Ship and smoke the current approximately 80 ms HEVC demo first. If further optimization is still desired, the next code experiment should be the **single same-buffer AUD diagnostic**, followed by the **small renderer cleanup**, not a CUDA/GL rewrite. This gives the highest chance of learning or gaining latency with little code and a clean rollback path.

This was a static review of the retained source, traces, metrics, and primary API documentation. No runtime or hardware experiment was performed as part of the review.
