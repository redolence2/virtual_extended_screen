# Second Review of `LATENCY_GLITCH_ANALYSIS.md`

| | |
|---|---|
| **Review date** | 2026-07-29 |
| **Reviewed document revision** | v1.1 |
| **Reviewed document SHA-256** | `148417737c66f19bce6f2d7c54c1ba4c05d3231af8e4c5a956ac7dd25ab6dd37` |
| **Reviewed RESC commit** | `12b87d1` |
| **Comparison review** | `LATENCY_GLITCH_ANALYSIS_review.md` |
| **Review type** | Static code, protocol, and architecture review |
| **Runtime measurements performed** | None |

## Executive verdict

The v1.1 update is a substantial improvement. It correctly incorporates nearly
all material findings from the first review:

- the deterministic 695,296-byte UDP frame ceiling;
- the cursor-only full-frame texture upload;
- the dead `BitrateAdapter` retraction;
- the corrected IDR-cadence arithmetic;
- a dedicated video TCP connection;
- corrected bind/ready/connect ordering;
- byte- and age-bounded backpressure;
- real capture metadata;
- retained minimal decoder recovery;
- explicit decoded-frame ownership;
- A/B testing rather than blindly copying encoder/capture settings; and
- latency figures stated as goals pending measurement.

I now agree with both the **architectural destination** and most of the revised
Phase A-E sequence.

The plan is still not fully execution-ready. Four load-bearing amendments remain:

1. The current client silently drops fully assembled encoded P-frames when its
   four-slot channel is full. This is an independent reference-chain/glitch
   source and should be added as G7.
2. L7 still incorrectly says the cursor inherits the full video queue/decoder
   delay. Cursor state is latest-wins; it waits for shared-loop work, upload, and
   presentation, not for the age of queued video frames.
3. Pre-encode backpressure must **coalesce one newest pending capture**, not
   simply skip it, or the final static screen update can be lost after
   `FramePacer` is removed.
4. Receiver progress acknowledgements need precise semantics. ACKing socket
   receipt does not bound an undecoded queue; the sender's credit should advance
   only after the decoder accepts an encoded frame and drains available output.

My revised assessment is:

> **Approve the architecture, but add the four invariants above and correct the
> remaining factual inconsistencies before implementation begins.**

## 1. Disposition of the first review

| First-review finding | v1.1 disposition | v2 assessment |
|---|---|---|
| 695,296-byte deterministic frame ceiling | Added as G6 and Phase A telemetry | Correctly adopted |
| Cursor-only redraw re-uploads the complete texture | Added as L9 and Phase A.5 | Correctly adopted |
| `BitrateAdapter` has no production call site | G4 retracted | Correctly adopted |
| Raw `0xFA` parsing | Typed protobuf decoding in Phase A | Correct direction; build wiring still incomplete |
| `pendingForceKeyframe` race | Phase A thread-safety fix | Correctly adopted |
| Dedicated video TCP connection | Phase B; control multiplexing removed | Correctly adopted |
| Bind/listen before `StreamingReady` | Phase B.7 | Correctly adopted |
| Bound bytes and age, not only frame count | Phase B.8 | Correct direction; ACK stage must be specified |
| Explicit keyframe/reconnect contract | Phase B.9 | Correctly adopted |
| Retain minimal decoder recovery | Phase B.10 | Correctly adopted |
| Capture settings require A/B tests | Phase C.12 | Correctly adopted |
| Safe decoded-frame ownership | Phase D.15 | Correct direction |
| NV12 capability test | Phase D.16 | Correctly adopted |
| Targets are goals, not predictions | Reworded in §7/§8 | Correctly adopted |

## 2. Remaining high-priority findings

### R1. The four-slot channel silently drops encoded P-frames

The client creates a four-slot channel between assembly and decode:

- `ubuntu-client/src/main.rs:122-124`

When it is full, the receiver discards a fully assembled non-keyframe:

- `ubuntu-client/crates/net-transport/src/video_receiver.rs:191-203`

Later encoded frames are still delivered to ffmpeg:

- `ubuntu-client/src/main.rs:348-424`

This is not merely a latency queue. A discarded encoded P-frame can be referenced
by later P-frames, producing decoder errors or visible corruption even when:

- every UDP datagram arrived;
- the frame was assembled successfully; and
- the frame was below the 695,296-byte ceiling.

The drop branch only increments the generic frame-drop counter. It does not:

- mark the decoder reference chain discontinuous;
- send a discontinuity item to the decoder;
- request an IDR directly; or
- stop subsequent dependent P-frames from entering the decoder.

This should be added to the root-cause analysis as:

> **G7 — local post-assembly encoded-frame drop on decoder-queue saturation.**

Phase A telemetry must include a separate `decode_queue_full_nonkey` counter.
Until TCP/backpressure removes the condition, a queue-full drop should explicitly
invalidate the decoder chain and cause a rate-limited reset/IDR transition rather
than silently feeding later dependent frames.

The Phase B statement that encoded P-frames must never be arbitrarily dropped is
correct; the missing piece is recognizing that the current code already violates
that invariant.

### R2. L7 still overattributes video-frame age to cursor latency

The cursor receiver stores only the newest sequence and coordinates in atomics:

- `ubuntu-client/src/main.rs:139-182`

The render loop reads the current values:

- `ubuntu-client/src/main.rs:505-515`

Therefore cursor updates do not queue behind older cursor updates and do not
inherit:

- the four-frame encoded channel's temporal age;
- CUVID's internal display-queue age; or
- the presentation timestamp of the displayed video frame.

The cursor can still wait while the shared thread is busy with:

- a batch of decode calls;
- GPU-to-CPU transfer;
- CPU plane extraction;
- texture upload;
- event handling; and
- vsync-blocked presentation.

L9 is a direct cursor-path cost because every cursor redraw currently uploads the
full cached video frame. L2 is also relevant because decode work and presentation
share the cursor's thread.

L1 and L3 are primarily **video-frame age** mechanisms. They should not be added
directly to pointer latency. In particular, an internal decoder display queue can
hold video output for four frames while `send_packet()` itself returns quickly;
that temporal delay does not automatically block a latest-wins cursor redraw.

L7 should be rewritten as:

> The cursor data path is latest-wins, but repaint is serialized behind whatever
> work the shared decode/render loop is currently performing. Its direct costs
> are current-loop work, full-frame texture upload, and presentation/vsync—not
> the accumulated age of queued video frames.

Accordingly, the statement that the pointer "exhibits the full video-pipeline
latency" remains unproven and is mechanically too strong.

### R3. Pre-encode dropping needs a coalesced pending capture

The updated plan adopts opendisplay's `pendingEncodes <= 1` pattern. The reference
implementation records `lastPixelBuffer`, then returns if encode or send capacity
is unavailable:

- `../opendisplay/Mac/MacSender.swift:1088-1097`

Its watchdog replays that retained buffer only when a reconnect needs a keyframe:

- `../opendisplay/Mac/MacSender.swift:817-824`

That is not the same as normal latest-wins coalescing.

After `FramePacer` is removed, ScreenCaptureKit may stop issuing buffers on a
static screen. Consider this sequence:

1. frame N is being encoded or network credit is exhausted;
2. the screen changes once to final state N+1;
3. the capture callback sees N+1 while the gate is closed and simply skips it;
4. no more content changes occur, so no later callback arrives.

The receiver can remain on state N indefinitely even though the encoder and
network later become idle.

RESC already has the right primitive: a latest-wins one-element slot:

- `mac-host/Sources/RemoteDisplayHost/LatestFrameSlot.swift:16-38`

The revised design should retain exactly one newest `CaptureFrame` while either
gate is closed. When encode and send credit become available, it should submit
that pending capture immediately even if ScreenCaptureKit produces no new
callback.

This must be an explicit invariant:

```text
one encode in flight
+ zero or one coalesced newest pending capture
+ bounded unacknowledged encoded bytes
```

Pre-encode coalescing preserves the codec reference chain while also guaranteeing
eventual display of the last static update.

### R4. Receiver ACKs must identify the progress stage

The update correctly rejects the assumption that
`NWConnection.SendCompletion.contentProcessed` means peer-consumed. Apple
documents it as data being processed by the network stack:

- <https://developer.apple.com/documentation/network/nwconnection/sendcompletion/contentprocessed%28_%3A%29>

However, "receiver progress ACK by frame ID" is still underspecified.

Four different progress points answer different questions:

| Progress signal | What it proves | What it does not bound |
|---|---|---|
| `received_frame_id` | Complete frame bytes were deframed | Encoded decode queue, decoder delay, rendering |
| `decoder_accepted_frame_id` | `send_packet()` accepted the encoded frame and all currently available output was drained | Frames still retained by decoder reorder/display delay, rendering |
| `decoded_output_frame_id` | The decoder emitted that frame as output | Render-slot age and presentation |
| `presented_frame_id` | That decoded frame was handed to presentation | Exact scanout/photon time |

Sender credit should be released from cumulative **decoder-accepted** progress,
not socket receipt and not presentation. In the current wrapper, that boundary
is after `send_packet()` succeeds and the `receive_frame()` loop drains all
currently available output—normally until EAGAIN:

- `ubuntu-client/crates/video-decode/src/lib.rs:193-207`
- `ubuntu-client/crates/video-decode/src/lib.rs:279-284`

The wrapper should distinguish EAGAIN from a real receive error before using
this as a protocol boundary. A packet intentionally discarded while waiting for
an IDR is not decoder-accepted progress; that case requires the explicit
reset/new-generation path.

ACKing socket receipt lets the receiver accumulate an encoded queue and recreate
the latency/drop problem behind TCP. Conversely, tying sender credit to
presentation recouples the host to vsync and render stalls. Presentation belongs
in telemetry.

If the age model is intended to include frames retained internally by the
decoder, `decoded_output_frame_id` is also required. If output progress is ever
used as flow-control credit, the byte/frame window must exceed the measured
decoder startup and display depth before low-delay mode is enabled, or the
sender and decoder can deadlock waiting for each other.

Recommended design:

- deframe directly into the decoder thread, or use a bounded encoded queue that
  never silently drops;
- send cumulative `decoderAcceptedFrameID` and `decoderAcceptedBytes`;
- expose `decodedOutputFrameID` and `lastPresentedFrameID` separately;
- cap outstanding encoded bytes and oldest capture age against
  decoder-accepted progress;
- close/reset the video connection and request a fresh IDR if the age bound is
  violated.

All ACKs should be cumulative and identified by session/config generation. This
is especially important if progress ACKs travel over the control connection
rather than the dedicated video connection.

## 3. Remaining medium-priority corrections

### R5. G6's repeat-IDR loop is a hypothesis, not a verified direct transition

The G6 size rejection is verified:

- `ubuntu-client/crates/jitter-buffer/src/lib.rs:137-149`

But the assembler returns no frame and does not directly request an IDR:

- `ubuntu-client/crates/net-transport/src/video_receiver.rs:185-210`

An IDR request may happen later when subsequent dependent frames cause a decoder
error. Therefore:

- deterministic oversize rejection is **VERIFIED**;
- subsequent reference discontinuity is **HIGH**;
- "drop immediately triggers RequestIDR and repeats" is a **runtime hypothesis**.

The G6 wording should distinguish those confidence levels.

### R5a. Cursor fields are not published as one coherent snapshot

The cursor position, shape, and sequence are four independent atomics:

- `ubuntu-client/src/main.rs:50-56`

The receiver stores the fields separately with relaxed ordering:

- `ubuntu-client/src/main.rs:174-182`

The renderer reads position and shape separately and does not validate the
sequence before and after the read:

- `ubuntu-client/src/main.rs:505-515`

This latest-wins scheme can expose a mixed snapshot—for example, a new X with an
old Y or shape—when a read overlaps an update. This is mainly a cursor
correctness/jitter issue rather than a large latency source.

Publish a coherent `CursorUpdate` using a mutex, a seqlock-style double sequence
check with acquire/release ordering, or a safely packed atomic representation.
Add a stress test that proves readers never observe fields from different
sequence numbers.

### R6. Swift protobuf code generation exists but is not wired into the target

`tools/generate_proto.sh` outputs Swift files to:

- `tools/generate_proto.sh:16-18`
- `tools/generate_proto.sh:117-125`

That directory is `mac-host/Sources/Protocol`.

The executable target compiles only `mac-host/Sources/RemoteDisplayHost`:

- `mac-host/Package.swift:32-38`

No generated Swift protobuf files currently exist under `mac-host/Sources`.
Running the script alone therefore does not complete Phase A.3.

The plan should explicitly choose one of:

1. generate into `Sources/RemoteDisplayHost/Generated`; or
2. add a separate Swift `Protocol` target and make `RemoteDisplayHost` depend on
   it.

The generation step is also network/tool dependent when pinned binaries are not
already installed, so generated sources should normally be committed or
reproducibly generated in CI.

### R7. Existing packet-loss statistics do not measure missing UDP packets

`SharedReceiverStats.packets_dropped` is populated from
`local_packets_dropped`:

- `ubuntu-client/crates/net-transport/src/video_receiver.rs:10-25`
- `ubuntu-client/crates/net-transport/src/video_receiver.rs:90-97`

That counter is incremented for datagrams that were received but failed framing,
version, type, parse, or stream/config validation:

- `ubuntu-client/crates/net-transport/src/video_receiver.rs:121-153`

It cannot count UDP datagrams that never reached the process because the wire
format has no global packet sequence. The reported `packet_loss_rate` therefore
is not actual network loss:

- `ubuntu-client/src/main.rs:231-256`

Phase A should separately report:

- received-invalid/misrouted datagrams;
- frames timed out;
- expected-versus-received missing chunks when chunk 0 metadata is available;
- oversize frames;
- slot evictions;
- decoder-queue-full encoded drops; and
- Linux socket overflow counters if available.

This matters because G2's burst-loss magnitude cannot be validated using the
current `packet_loss_rate`.

### R8. Several narrative inconsistencies remain

#### Payload size

The summary and G1 still say 1,362-byte payloads:

- `LATENCY_GLITCH_ANALYSIS.md:52`
- `LATENCY_GLITCH_ANALYSIS.md:108-109`

The actual value is 1,358:

- `mac-host/Sources/RemoteDisplayHost/ProtocolConstants.swift:13-18`

#### "Three CPU copies"

Two explicit CPU copies are verified. `SDL_UpdateYUVTexture` is a full texture
upload, but whether it performs another CPU copy internally depends on the SDL
renderer backend. The document should say:

> two explicit CPU copies plus one full texture upload

rather than "three full-frame CPU copies."

#### Periodic-keyframe wording

The overview/table still says opendisplay has no periodic keyframes, while the
actual configuration sets both a 3,600-frame and 60-second maximum:

- `../opendisplay/Mac/MacSender.swift:1066-1070`

The later Phase B wording—"approximately 60-second maximum when honored"—is the
more accurate statement and should be used consistently. Low-latency rate
control may alter the effective GOP, so runtime cadence should be logged.

#### TCP retransmit/backpressure claim

Section 3 still says wired-LAN retransmission is single-digit milliseconds and
is absorbed by opendisplay's `pendingSends`. That magnitude is not established
by this repository, and opendisplay's counter is driven by local
`.contentProcessed` callbacks:

- `../opendisplay/Mac/MacSender.swift:142-155`
- `../opendisplay/Mac/MacSender.swift:1256-1269`

This contradicts the updated Phase B explanation that local send completions do
not prove peer consumption.

### R9. Quantitative assertions remain stronger than the evidence

The FFmpeg 7.1 source confirms the mechanism:

```c
ulMaxDisplayDelay =
    (avctx->flags & AV_CODEC_FLAG_LOW_DELAY) ? 0 : 4;
```

- <https://www.ffmpeg.org/doxygen/7.1/cuviddec_8c_source.html>
- `ubuntu-client/crates/video-decode/Cargo.toml:6-10`

That supports enabling the flag before opening the decoder. It does not by itself
prove that:

- deployed Ubuntu uses the same ffmpeg/CUVID implementation;
- total decoder output lag is exactly four frames;
- setting the flag makes submit-to-output gap exactly zero; or
- those frames can be added directly to cursor latency.

The tests should assert improvement and an explicit measured upper bound, not
hard-code "gap must go to 0."

Likewise, the following remain runtime hypotheses:

- 0.5-2 MB real-world 4K IDR distribution;
- G6 being the primary observed glitch cause;
- approximately 51 fps for RESC at `minimumFrameInterval=1/60`;
- the 130-200+ ms additive latency total;
- `FramePacer` permanently saturating the deployed pipeline;
- 40-80 ms video and 10-20 ms cursor targets.

The document usually acknowledges this later, but the TL;DR and tables still
present some of these as settled causal measurements.

### R10. The effort estimate is optimistic

Phase B includes:

- a new bidirectional protocol handshake;
- a dedicated TCP lifecycle;
- binary framing and validation;
- reconnect/config-generation behavior;
- decoder-progress ACKs;
- byte/age credit control;
- static-frame replay;
- decoder reset/IDR recovery; and
- load, stall, loss, and reconnect testing.

Treating that as one focused day is not a reliable planning estimate. Phase D's
thread split, safe ffmpeg-frame ownership, SDL renderer ownership, and
NV12/I420 fallback are similarly nontrivial.

The document should either omit the time estimate or label it explicitly as an
initial estimate to revise after Phase A and a TCP spike.

## 4. Recommended amendments to the phase plan

### Phase A0 — record the untouched baseline

Instrumentation should land and record a baseline before changing L9 or other
behavior. Otherwise the original and fixed paths cannot be compared.

Capture at least:

- SCK presentation timestamp and capture-callback monotonic timestamp;
- capture sequence and encoder-output timestamp;
- encoded bytes and chunk count;
- missing-chunk, timeout, oversize, eviction, and queue-full drop reasons;
- decoder-submit and decoder-output sequence;
- texture-upload start/end;
- present call/return;
- cursor sample, receive, and present sequence/timestamps.

Use monotonic clocks on both machines. Record both SCK PTS and callback time so
ScreenCaptureKit delivery delay is not hidden.

### Phase A1 — surgical correctness fixes

1. Wire generated Swift protobuf types into the package target.
2. Replace raw `0xFA` scanning and validate session/stream/config identifiers.
3. Make force-keyframe state encoder-thread-confined or synchronized.
4. Split texture upload from cursor-only recomposition.
5. Add G7 telemetry and an explicit discontinuity/reset path for any current
   encoded-frame queue drop.
6. Decide whether to:
   - fast-track Phase B; or
   - temporarily make UDP `max_total_chunks` consistent with
     `ceil(maxFrameBytes / 1358)` using dynamically sized tracking.

### Phase B — dedicated TCP with defined credit semantics

1. Bind/listen before `StreamingReady`.
2. Use versioned binary framing with length, frame ID, capture sequence/time,
   keyframe flag, session ID, and config generation.
3. Decode every encoded reference frame.
4. ACK cumulative decoder-accepted frame IDs and bytes only after successful
   packet submission and output draining to EAGAIN.
5. Report decoder-output and presented progress separately.
6. Bound unconsumed bytes and oldest capture age.
7. Reset the connection/decoder and start with an IDR when the age bound is
   exceeded.

### Phase C — coalesced host gating and host tuning

1. Keep at most one encode in flight.
2. Keep exactly one newest pending `CaptureFrame` while encoder/network credit is
   closed.
3. Drain it immediately when capacity returns.
4. Verify final-static-state delivery before removing `FramePacer`.
5. Then A/B encoder flags, capture interval, and queue depth.

### Phase D — decode/render split

1. Keep network deframing and decoder consumption coupled or use a no-drop
   bounded encoded queue.
2. Use latest-wins only after decode.
3. Keep SDL events, texture ownership, cursor composition, and presentation on
   one render thread.
4. Pass an owned/refcounted decoded surface or an explicit pooled copy; do not
   send borrowed ffmpeg pointers across threads.
5. Upload only on a new video frame and reuse the GPU texture for cursor-only
   redraws.

Do not treat the existing `unsafe impl Send for Renderer` as proof that SDL
objects may move freely between threads:

- `ubuntu-client/crates/renderer/src/lib.rs:36-50`
- `ubuntu-client/crates/renderer/src/lib.rs:105-112`

SDL initialization, event pumping, texture creation/update/destruction, cursor
composition, and presentation should remain on the same render thread. The
exact `ffmpeg-next` wrapper's cross-thread guarantees are not established here;
the handoff should use a compile-tested RAII reference to an `AVFrame` or an
owned pool of decoded planes with an explicit return path.

## 5. Required validation gates

| Gate | Required test |
|---|---|
| Baseline integrity | Record p50/p95/max video and cursor timing before behavioral fixes. |
| Typed control parsing | A valid Stats envelope whose session varint contains `0xFA` never requests an IDR. |
| G6 | Exercise 512 versus 513 chunks and log real 4K IDR sizes. |
| G7 | Saturate the four-slot queue; every encoded P-frame discontinuity is counted and causes controlled reset/IDR, never silent continuation. |
| Cursor residency | Cursor-only redraw produces zero `SDL_UpdateYUVTexture` calls. |
| ACK semantics | Stall decoder consumption while socket reads continue; sender credit must stop advancing. |
| Bounded TCP age | Stall receiver and saturate network; outstanding bytes and oldest capture age stay within limits or force a clean reset. |
| Final static update | Close encoder/network credit, generate exactly one final capture, stop further captures, reopen credit, and require that exact capture sequence to display with `FramePacer` off. |
| Decoder low delay | Compare numbered submit/output sequences with low-delay off/on; enforce a measured bound rather than an assumed exact zero. |
| Thread ownership | Sustained 4K decode/render/cursor soak with no borrowed-frame lifetime or SDL cross-thread violations. |
| Cursor coherence | Race cursor publication and rendering; every observed X/Y/shape tuple must belong to one published sequence. |

## Conclusion

The updated document is now directionally strong and much closer to an
implementation specification than v1.0. Its central recommendation—retain RESC,
move video to a dedicated reliable stream, apply pre-encode backpressure, decode
all references, and render only the latest decoded result—is sound for the
stated fixed wired-LAN use case.

Before implementation, it should add G7, correct the cursor-latency model,
specify coalesced latest-capture behavior, and define ACKs at the
decoder-acceptance boundary specified above. With those changes, I would
consider the architecture ready for a measured Phase A0/A1 implementation
followed by a bounded TCP prototype.
