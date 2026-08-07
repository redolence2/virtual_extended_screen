# Review of `W0_REPORT_AND_REORDER_FIX_PLAN.md`

Reviewed: `W0_REPORT_AND_REORDER_FIX_PLAN.md` at local `HEAD b4784d8`

Review date: 2026-08-07

## Executive verdict

**GO WITH AMENDMENTS: accept the W0 STOP decision and run Candidate A (4K HEVC) now. Do not implement Candidate B verbatim.**

The retained evidence is sufficient to stop the CUDA/GL bounded-copy project as the current latency fix. W0a removed the hardware-frame download, YUV extraction, publication, rendering, and upload, but receive-to-decode p50 only moved from **113.187 ms to 104.790 ms**, while the ordinal gap remained **6 for 99.6%** of W0a emissions. It failed both pre-agreed gates by a large margin. Copies may still cost roughly 8 ms and substantial bandwidth, but they are not the primary ~100 ms latency lever.

The missing H.264 VUI is a **strong leading explanation**, not yet a proved root cause. The SPS permits a level-derived five-picture reorder allowance, and five plus CUVID's known one-AU parser behavior fits the observed gap of six unusually well. The standard does not require a decoder to hold exactly five pictures, however, and the current trace timestamp spans receipt, queueing, decode submission, and emission rather than measuring only decoder-internal time. Only a controlled H.264 signaling change that moves the gap can establish that causal claim.

There is no reason for another full planning round. Treat this review as the implementation amendment:

1. run the HEVC experiment under the corrected controls below;
2. if it passes, make HEVC the 4K launcher default, apply the two small codec-handling cleanups, smoke-test, and release the demo;
3. if it fails, try the bounded Apple low-latency encoder probe below before writing a custom SPS parser;
4. use corrected Candidate B only if H.264 is still needed and the simpler paths fail.

## Decision summary

| Item | Decision |
|---|---|
| W0a STOP and cancellation of W0b/CUDA-GL work | **Accepted** |
| “Copies are not the dominant latency lever” | **Accepted** |
| “Copy/contention has no causal cost” | **Not established**; W0a improved p50 by 8.397 ms |
| “Root cause is missing VUI” | **Leading hypothesis, not yet proved** |
| Candidate A, HEVC first | **Approved now** |
| Candidate B architecture | **Conditionally reasonable** if H.264 remains necessary |
| Candidate B exactly as written | **Not approved**; required corrections below |
| HEVC as the shipped 4K default | **Approved if the demo gates pass** |
| Further CUDA/GL investigation now | **No** |

## 1. W0 evidence review

### What is valid

Independent recomputation reproduces the retained join and metric results:

| run | joined | presented | receive -> decode p50/p90 | ordinal-gap result |
|---|---:|---:|---:|---|
| B0 H.264 | 3,663 | 3,158 | 113.187 / 123.907 ms | gap 6 for 3,643/3,663 all emissions (99.5%); 3,146/3,158 presented (99.6%) |
| W0a no-download | 3,717 | 0 by design | 104.790 / 111.387 ms | gap 6 for 3,703/3,717 (99.6%) |

Both traces have clean footers and zero identity failures or ambiguities. W0a's join summary says `pass:false` solely because the generic joiner requires presentations; zero presentations are the intended result of this probe. The source also matches the claimed experiment: hardware frames bypass transfer/extraction, identity is resolved, and empty-plane frames never enter the mailbox or renderer.

The earlier review's rule was to stop before any GL implementation if W0a did not reach receive-to-decode p50 at or below 50 ms and did not move the ordinal mode toward 2. It reached neither. Skipping W0b is therefore correct, not an incomplete gate.

### Claims to narrow

1. Change **“the copy hypothesis is refuted”** to **“removing the copy/render path is not the expected primary latency lever and failed the agreed gate.”** The measured 8.397 ms p50 improvement is real, and one manual A/B pair cannot prove that copies have no effect.

2. Change **“content parity verified”** to **“comparable active workload supported by sender volume.”** W0a sent 6.3% more bytes, which rules out a grossly idle stream, but blind manual dragging does not establish identical frame content.

3. Change **“the delay lives between feed and emission”** to **“the remaining delay is within the measured receive-to-decode interval.”** `ts_recv_us` is stamped at assembly completion; frames may then wait in the receiver channel/batch path before decoder submission. There is no separate packet-feed timestamp in this evidence.

4. The B0 report's 99.6% gap-six label is correct for presented frames, but the all-emission value is **99.5%**, not 99.6%.

5. The encoder/sender totals quoted in the prose do not exactly match raw trace event totals, probably because the console counters are process-cumulative. This does not affect the joined latency decision, but those console counts should not be presented as trace-window counts without the missing logs.

### Evidence retention

The 30.8 MB H.264 dump and its `trace_headers` output are only on the Ubuntu box under `/tmp`; the repository retains the SPS hex and asserted fields, not the independently auditable artifact. Before that temporary file disappears, retain only:

- the small SPS/PPS header sample or first configuration AU;
- its SHA-256;
- the exact `trace_headers` output;
- the decoder-selection/startup lines showing that `h264_cuvid` was actually used.

There is no need to commit the full screen recording or rerun W0 solely for archival perfection. This is a personal demo, and the committed traces already justify the STOP decision.

The W0a probe currently treats the mere presence of `RESC_W0A_NO_DOWNLOAD` as enabled, so even `RESC_W0A_NO_DOWNLOAD=0` activates the black-screen mode. The simplest shipping cleanup is to remove the probe after this evidence is sealed. If retained, parse exact truthy values and keep the prominent startup warning.

## 2. Reorder diagnosis review

The captured SPS is consistent with the report:

- High profile (`profile_idc=100`), constraint flags byte zero;
- Level 5.2 (`level_idc=52`);
- 135 x 240 = 32,400 macroblocks per picture;
- `max_num_ref_frames=1`;
- `vui_parameters_present_flag=0`.

With no bitstream-restriction declaration, H.264's inference permits `max_num_reorder_frames = MaxDpbFrames` for this profile/constraint combination. Level 5.2 has `MaxDpbMbs=184,320`, so `floor(184,320 / 32,400) = 5`. FFmpeg 7.1.5 implements the same level-table calculation. See [ITU-T H.264 (08/2024)](https://www.itu.int/rec/T-REC-H.264-202408-I) and the official FFmpeg [`h264_ps.c`](https://github.com/FFmpeg/FFmpeg/blob/n7.1.5/libavcodec/h264_ps.c#L538-L550).

That closes the **allowance arithmetic**, not the decoder's exact behavior. The SPS allows five reordered pictures; it does not command every decoder to delay five outputs. The real CUVID path feeds NVIDIA's parser directly, so FFmpeg's generic H.264 parser behavior is evidence of AVC semantics rather than proof of NVDEC's policy.

The extra one-AU explanation is credible: NVIDIA documents one-frame parser latency for NAL-unit codecs when end-of-picture is not explicitly signaled, and this FFmpeg CUVID path does not set `CUVID_PKT_ENDOFPICTURE`. The current client already sets `AV_CODEC_FLAG_LOW_DELAY`, which makes FFmpeg set `ulMaxDisplayDelay=0`; that removes the configurable display queue but does not prove that SPS-derived ordering behavior disappears. See the [NVIDIA NVDEC parser guide](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.1/nvdec-video-decoder-api-prog-guide/index.html) and FFmpeg's [`cuviddec.c`](https://github.com/FFmpeg/FFmpeg/blob/n7.1.5/libavcodec/cuviddec.c#L426-L488).

The accurate conclusion is therefore:

> The H.264 SPS permits a five-picture reorder allowance, and the observed fixed gap of six is consistent with that allowance plus CUVID's one-AU parser behavior. Missing reorder signaling is the leading hypothesis, to be confirmed only if a controlled H.264 signaling change moves the gap.

An HEVC success proves that HEVC solves the product problem. It does **not**, by itself, prove the H.264 VUI mechanism, because codec/parser behavior changes at the same time.

## 3. Candidate A: approved with corrected controls

Run Candidate A immediately. The host and client already contain the complete HEVC encode, parameter-set packaging, protocol codec ID, and `hevc_cuvid` decode path.

### First diagnostic run

Use the same 50 Mbps as B0 so the codec change is the principal variable:

```text
HOST_ARGS="2160 3840 60 --client 192.168.50.47 --hevc --bitrate 50"
```

Without `--bitrate 50`, the current host silently changes the 4K default from H.264 50 Mbps to HEVC 40 Mbps. A 40 Mbps run can still prove that the intended product configuration works, but it is a weaker causal comparison.

Retain logs proving all of the following:

- `ModeConfirm` selected codec 1 / HEVC;
- `hevc_cuvid` actually opened;
- no software or H.264 fallback was used;
- the HEVC SPS value of `sps_max_num_reorder_pics[]` and its matching buffering limit;
- ordinary keyframe cadence, keyframe sizes, and no sustained IDR requests.

The current client incorrectly constructs an H.264 decoder if HEVC decoder initialization fails, even though the incoming stream remains HEVC. Any such fallback invalidates the run; it is not an HEVC result.

### Fast acceptance sequence

1. Take one comparable 60-second 50 Mbps HEVC run.
2. If the improvement is large and the trace is clean, repeat once to rule out a lucky run.
3. Make the small shipping changes below and perform a 10-minute normal-use smoke at the actual launcher bitrate.
4. Release the demo if the gates pass. Do not require a 30-minute soak or another architecture review.

Demo GO gates:

| gate | requirement |
|---|---|
| E2E latency | p50 <= 90 ms in both candidate runs |
| tail | p90 <= 140 ms |
| receive -> decode | p50 <= 50 ms, or an equally clear structural collapse explained in the result |
| ordinal gap | mode moves from 6 toward <= 2; diagnostic evidence, not a literal surface count |
| integrity | clean footers; zero identity failure/ambiguity |
| stability | no decoder/render failure and no sustained IDR storm |
| delivery | no material regression in joined/host or presented/joined rates |
| usability | owner accepts motion, image quality, warmth, cursor alignment, and ordinary keyframe recovery |

If HEVC passes, ship it as the 4K default and stop investigating H.264 for this demo. The production change is small but is not literally only one launcher flag:

1. add `--hevc` to both installed 4K launcher copies and the maintained source/template used to regenerate them;
2. advertise HEVC in the client's `supported_codecs` list instead of advertising only H.264;
3. replace the invalid HEVC-to-H.264 decoder fallback with a clear fail-fast error that names the requested codec and underlying initialization failure.

The explicit 50 Mbps setting is for a clean comparison. The shipped launcher may use the existing HEVC 40 Mbps default if the 10-minute smoke shows acceptable quality; record the actual value rather than claiming the 50 Mbps experiment and 40 Mbps product run were identical.

## 4. Simpler fallback before custom SPS surgery

If HEVC fails, test Apple's purpose-built low-latency encoder selection before building a general SPS parser. `kVTVideoEncoderSpecification_EnableLowLatencyRateControl=true` is specifically documented for ultra-low-latency conferencing/cloud gaming and selects a low-latency encoder with no B-frame reordering or lookahead. The current `encoderSpecification:nil` experiment compared against an ordinary hardware-preference dictionary; it did not test this specific low-latency selector. See Apple's [low-latency encoder specification](https://developer.apple.com/documentation/videotoolbox/kvtvideoencoderspecification_enablelowlatencyratecontrol).

Keep this a small, env-gated, time-boxed host probe:

- enable the specification at `VTCompressionSessionCreate`;
- dump and inspect the resulting H.264 SPS before doing more work;
- run the same short latency measurement only if the emitted stream changes in a promising way;
- check forced-keyframe behavior and visual quality, because Apple documents infinite GOP and temporal-layer behavior for this mode;
- remove the probe if it does not change the signaling/gap.

This is not guaranteed to emit the desired VUI, but it is a much smaller supported API experiment than approximately 250 lines of custom bitstream surgery. It should not become another planning phase.

## 5. Candidate B corrections

Only implement Candidate B if HEVC and the small Apple probe fail and keeping H.264 is still worthwhile.

For this one fixed Mac/Ubuntu pair, first capture the SPS across a few cold starts. If it is byte-identical, prefer an **exact-known-SPS replacement** with a logged hash match over a general parser. A changed SPS after a macOS upgrade then fails open to the slower path with an actionable diagnostic. Build the general walker only if the SPS actually varies.

Whether exact-match or general, apply these corrections.

### Bitstream values and conformance

1. Set `log2_max_mv_length_horizontal` and `log2_max_mv_length_vertical` to **15, not 16**. FFmpeg's current H.264 coded-bitstream implementation notes that the current standard constrains these values to 0..15; 16 belonged to older editions. See FFmpeg's [`cbs_h264_syntax_template.c`](https://github.com/FFmpeg/FFmpeg/blob/n7.1.5/libavcodec/cbs_h264_syntax_template.c#L190-L215).

2. `max_num_reorder_frames=0` and `max_dec_frame_buffering=1` are the right target values **conditionally**, not universally. The buffering value 1 is numerically valid because it is at least `max_num_ref_frames=1` and no greater than `MaxDpbFrames=5`. Before claiming conformance, confirm the produced stream actually has monotonic output order/no B frames and needs no more than one DPB frame. The existing `AllowFrameReordering=false`, monotonic host capture/output evidence, and a successful live decoder run provide a practical check for this fixed deployment. Do not signal 5 merely as a defensive value; five is the level ceiling and would weaken the intended low-delay declaration.

3. Do not flip `constraint_set3_flag` as a shortcut. For High profile it carries different profile constraints and would misdescribe the emitted P-frame stream.

4. Stock FFmpeg `h264_metadata` is not a substitute: it can manipulate several VUI fields but exposes no option for `max_num_reorder_frames` or `max_dec_frame_buffering`. See the official [`h264_metadata` options](https://ffmpeg.org/ffmpeg-bitstream-filters.html#h264_005fmetadata).

### Rewrite behavior

- Copy bits **before** the original zero `vui_parameters_present_flag`, emit one new `1`, append the VUI, discard the old RBSP trailing bits, and regenerate valid trailing bits. “Copy through the flag” is ambiguous and can leave an invalid zero flag before the new data.
- Rewrite every type-7 NAL in an access unit atomically or rewrite none; never partially modify a multi-SPS AU.
- Support both three- and four-byte Annex-B start codes, preserve the NAL header, and correctly unescape/re-escape emulation-prevention bytes.
- A VUI-present stream is not necessarily an already-safe stream: its VUI may omit `bitstream_restriction` or declare nonzero reorder. Distinguish already-safe, unsupported-existing-VUI, no-SPS, malformed, rewritten, and error outcomes. `Option<Vec<u8>>` hides too much state.
- Bound parser input, Exp-Golomb reads, cache entries, and allocation sizes. A malformed network packet must not cause an unbounded read or allocation.
- Apply the rewrite before the first original SPS reaches CUVID. On a changed SPS mid-session, clear the cache and recover/reinitialize explicitly or pass it through with a prominent warning; do not silently splice half of a reconfiguration.

### Failure behavior and diagnostics

Use **fail-open for runtime availability, but fail the candidate acceptance gate** if the expected first SPS was not rewritten. This gives the personal demo a working, if slower, fallback after an OS/encoder change instead of a black screen.

Log once per session:

- applied/skipped/error outcome and counters;
- input SPS hash and relevant parsed fields;
- exact unsupported field or parse offset;
- rewritten output hash and declared reorder/buffering values;
- explicit activation of `RESC_NO_SPS_FIX=1`.

These diagnostics are the maintainability contract for a future macOS, FFmpeg, or Ubuntu upgrade.

### Required verification

Unit reparse plus a latency collapse is not quite enough. The existing `--dump-h264` tap writes bytes before `VideoDecoder::decode`, so it would capture the original SPS rather than the bytes fed to CUVID. Add a small post-rewrite tap for the first configuration AU and retain:

- unit reparse showing all pre-VUI fields unchanged and `reorder=0`, `buffering=1`;
- post-rewrite parameter-set bytes and SHA-256;
- independent `trace_headers` output from those fed bytes;
- enabled/disabled live A/B evidence: enabled gap toward <=2, rollback gap returning toward 6;
- clean decode, identity, keyframe, and visual results.

This enabled/disabled H.264 comparison—not the HEVC switch—is the experiment that can turn the VUI hypothesis into a causal conclusion.

## 6. Answers to the report's seven questions

1. **Accept W0a STOP?** Yes. It failed both agreed pre-GL gates. Say copies are not the primary lever, not that they have zero cost.

2. **Approve A then B?** Approve A immediately. If A fails, insert one small official Apple low-latency encoder probe, then use corrected B only if needed.

3. **Buffering 1 or level-derived 5?** Use 1, conditional on confirming this stream truly needs one reference/no output reordering. Five is a ceiling, not a safer low-latency declaration.

4. **Fail open or closed?** Fail open to the legacy H.264 bytes with a prominent once-per-session diagnostic and counters; make the candidate test fail if rewriting was expected but skipped.

5. **Require fed-byte inspection?** Yes. Retain one small post-rewrite configuration artifact and verify it independently with `trace_headers`.

6. **Make HEVC the 4K default?** Yes, after the demo gates pass. Also advertise HEVC and remove the invalid H.264-decoder fallback.

7. **M1 receiver revisit?** Keep one low-priority backlog note. Later inspect that stream's SPS first; do not start that task before this demo ships.

## Final implementation order

1. Preserve the small missing bitstream evidence and correct the report wording; do not rerun W0 or start W0b.
2. Run the controlled 4K HEVC test at 50 Mbps.
3. If it passes, repeat once, update launcher/codec handling, run the 10-minute shipping smoke, and release the demo.
4. If it fails, run the bounded Apple low-latency encoder-specification probe.
5. Only if both paths fail, implement the corrected smallest H.264 SPS replacement/rewriter and verify it enabled versus disabled.
6. Leave zero-copy and the M1 revisit out of the demo milestone.

**Final decision: GO now for Candidate A. The W0 STOP is accepted. No further plan revision or architecture review is required before running it.**
