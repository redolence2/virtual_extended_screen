# Review of `LATENCY_GLITCH_ANALYSIS.md`

| | |
|---|---|
| **Review date** | 2026-07-29 |
| **Reviewed repository** | `remote_extended_screen` at `12b87d1` |
| **Review type** | Static code and architecture review |
| **Runtime measurements performed** | None |

## Executive verdict

I agree with the proposal's **architectural destination**, but I would not implement
the phase plan verbatim.

The right high-level choice is Option A:

- keep RESC rather than build a new Ubuntu receiver around opendisplay;
- move video to a reliable, dedicated TCP connection for the fixed wired-LAN use
  case;
- bound work before encoding instead of dropping encoded reference frames;
- configure the encoder and decoder for low delay;
- separate decode from presentation; and
- make cursor presentation independent of video processing.

However, the analysis misses two potentially stronger causes already present in
the current code:

1. Encoded frames over **695,296 bytes are deterministically rejected** by the
   receiver because the negotiated 512-chunk limit is inconsistent with the
   separately negotiated maximum frame size.
2. A cursor-only redraw currently **re-uploads the complete cached 4K YUV frame**
   before presenting it.

The proposal also needs a more rigorous TCP backpressure design, a corrected
connection handshake, real capture timestamps, explicit decoded-frame ownership
across threads, and a small retained decoder recovery path.

My overall assessment is therefore:

> **Agree with Option A and the broad architecture; revise the implementation
> plan and acceptance criteria before coding.**

## 1. Findings I agree with

### 1.1 UDP loss can corrupt a predictive codec stream

The current sender divides each encoded frame into UDP datagrams and sends them
without FEC, retransmission, or pacing:

- `mac-host/Sources/RemoteDisplayHost/VideoSender.swift:50-113`

The receiver discards incomplete frames, while later P-frames may depend on the
discarded reference:

- `ubuntu-client/crates/jitter-buffer/src/lib.rs:116-228`

The exact visible result depends on whether the lost frame was referenced and on
decoder concealment, so "all following frames are corrupt until the next IDR" is
too absolute. The underlying risk is nevertheless real. A reliable byte stream
removes this entire transport-loss failure class.

### 1.2 Periodic large IDRs are harmful on the current transport

The host requests a 0.5-second keyframe interval:

- `mac-host/Sources/RemoteDisplayHost/main.swift:111-116`

The UDP sender then emits all chunks in an unpaced loop:

- `mac-host/Sources/RemoteDisplayHost/VideoSender.swift:61-113`

Large IDRs therefore create packet bursts and are more exposed to local,
receiver, and network queue pressure. Moving to rare recovery IDRs on reliable
transport is a sound direction.

### 1.3 The raw `0xFA` IDR detector is a real bug

The host does not parse the protobuf `Envelope`. It treats any byte equal to
`0xFA` as evidence of field 31, `request_idr`:

- `mac-host/Sources/RemoteDisplayHost/HostSession.swift:134-160`

Normal client messages contain a random session ID and floating-point statistics,
so false matches are possible:

- `ubuntu-client/crates/net-transport/src/control_channel.rs:135-168`
- `ubuntu-client/src/main.rs:197-260`

Typed protobuf decoding should replace this immediately. The generated Swift
message types are not currently present, so the existing SwiftProtobuf package
dependency alone is not the full implementation:

- `mac-host/Package.swift:12-37`

One numerical claim in the original analysis should be corrected: four forced
requests per second is not eight times a nominal two keyframes per second. It is
approximately two times that cadence before considering overlap with scheduled
keyframes.

### 1.4 Client buffering and serialization can create latency

The current client has a four-frame encoded queue:

- `ubuntu-client/src/main.rs:122-137`

One loop drains and decodes every queued frame, processes SDL events, updates the
renderer, draws the cursor, and performs a vsync presentation:

- `ubuntu-client/src/main.rs:339-532`
- `ubuntu-client/crates/renderer/src/lib.rs:138-147`

Software decode is configured for four-frame threading:

- `ubuntu-client/crates/video-decode/src/lib.rs:132-156`

The decoder and renderer also make large CPU-side frame copies:

- `ubuntu-client/crates/video-decode/src/lib.rs:279-330`
- `ubuntu-client/crates/renderer/src/lib.rs:179-237`

These are credible latency mechanisms. Their exact contributions, and the
original analysis's 130-200 ms sum, remain unverified until measured at runtime.

## 2. Material findings missing from the original analysis

### 2.1 Frames above 695,296 bytes are rejected deterministically

The actual video payload is 1,358 bytes per UDP packet:

- `mac-host/Sources/RemoteDisplayHost/ProtocolConstants.swift:13-18`

The host advertises a maximum of 512 chunks per frame:

- `mac-host/Sources/RemoteDisplayHost/HostSession.swift:260-263`

The receiver rejects metadata whose `total_chunks` exceeds that value:

- `ubuntu-client/crates/jitter-buffer/src/lib.rs:137-150`

Therefore the largest accepted encoded frame is:

```text
512 chunks * 1,358 bytes/chunk = 695,296 bytes
```

This conflicts with `maxFrameBytes()`, which permits 20 times the average frame
size, capped at 2 MB:

- `mac-host/Sources/RemoteDisplayHost/ProtocolConstants.swift:62-67`

At 4K HEVC and 40 Mbps:

```text
average = 40,000,000 / 8 / 60 ~= 83,333 bytes
advertised maxFrameBytes ~= 1,666,667 bytes
actual chunk ceiling       =   695,296 bytes
```

Consequently, an IDR between approximately 695 KB and 1.67 MB passes the
`maxFrameBytes` policy but cannot be assembled. It is dropped by construction,
even on a perfect network.

This may be a primary explanation for glitches during abrupt, high-entropy
changes. TCP migration removes the chunk ceiling, but Phase 0 should first log
encoded frame sizes and reason-coded oversize drops so this hypothesis can be
confirmed.

### 2.2 Cursor-only rendering uploads the full video texture

The main loop correctly notices cursor movement independently of a new video
frame:

- `ubuntu-client/src/main.rs:505-530`

However, every call to `present_with_cursor()` calls `update_and_copy()`:

- `ubuntu-client/crates/renderer/src/lib.rs:256-263`

`update_and_copy()` performs `SDL_UpdateYUVTexture` for all Y, U, and V planes:

- `ubuntu-client/crates/renderer/src/lib.rs:53-61`

At 4K I420 this is roughly 12 MB uploaded for every cursor-driven presentation.
The call is followed by vsync presentation. This is a more direct cursor-path
cost than the original analysis identifies.

The renderer should have separate operations:

1. upload/replace the GPU texture only when a new decoded video frame arrives;
2. copy the already-uploaded texture, draw the cursor overlay, and present when
   only the cursor changes.

This is a small, high-value A/B experiment and should precede the transport
rewrite.

### 2.3 The described bitrate-adaptation effect is not active at HEAD

`BitrateAdapter` exists:

- `mac-host/Sources/RemoteDisplayHost/BitrateAdapter.swift`

But it has no call site, and streaming `Stats` messages are silently consumed by
the raw-byte handler:

- `mac-host/Sources/RemoteDisplayHost/HostSession.swift:134-160`

Therefore G4's claim that glitch episodes currently lower the active bitrate is
false for commit `12b87d1`.

### 2.4 The current IDR flag has a data race

`pendingForceKeyframe` is written from the control/session callback and read and
cleared on the encoder thread without synchronization:

- `mac-host/Sources/RemoteDisplayHost/VideoEncoder.swift:48`
- `mac-host/Sources/RemoteDisplayHost/VideoEncoder.swift:154-163`
- `mac-host/Sources/RemoteDisplayHost/VideoEncoder.swift:206`
- `mac-host/Sources/RemoteDisplayHost/main.swift:200-202`

The typed-IDR work should make this state encoder-queue-confined or protect it
with a lock/atomic mechanism.

## 3. Changes required to the proposed phases

### 3.1 Phase 0: measure actual pipeline and cursor latency

Instrumentation should still come first, but the current proposal understates
the required metadata change.

`DisplayCapturer` drops the sample timing and stores only a `CVPixelBuffer`:

- `mac-host/Sources/RemoteDisplayHost/DisplayCapturer.swift:105-127`
- `mac-host/Sources/RemoteDisplayHost/LatestFrameSlot.swift:10-38`

The encoder thread then synthesizes PTS from an encoded frame counter:

- `mac-host/Sources/RemoteDisplayHost/main.swift:140-145`

Phase 0 should introduce a capture-frame value containing at least:

- pixel buffer;
- capture callback timestamp or mapped ScreenCaptureKit PTS;
- monotonically increasing capture sequence number.

The framed video header should contain:

- frame ID;
- capture timestamp;
- encoder-output timestamp;
- encoded byte length;
- keyframe flag;
- codec/config generation.

The client should record:

- socket frame completion;
- decoder submission;
- decoder output;
- texture upload completion;
- presentation return;
- last observed cursor sequence and cursor presentation.

Capture-to-present software timing is not literally glass-to-glass or
photon-time. The optical 240 fps check remains necessary. Cursor latency must be
measured separately because video timestamps cannot validate the cursor path.

### 3.2 Phase 1: use a dedicated video TCP connection

Do not multiplex raw video onto the current control connection.

`ControlChannel` assumes every length-prefixed payload is a protobuf envelope:

- `mac-host/Sources/RemoteDisplayHost/ControlChannel.swift:83-155`

Multiplexing would require a new typed framing layer and would let large video
frames head-of-line-block keyframes, display settings, stats, and future keyboard
messages.

Use a separately negotiated video TCP connection with:

- `TCP_NODELAY` on both endpoints;
- an explicit bounded frame length;
- session, stream, and configuration IDs in the initial handshake;
- a binary frame header rather than a JSON/Annex-B heuristic;
- a connection generation so stale sockets cannot join a new stream.

The readiness order must also change. The client currently sends
`StreamingReady` before starting its UDP receiver:

- `ubuntu-client/src/main.rs:101-124`

For TCP, the client should bind/listen first, advertise readiness second, and
only then allow the host to connect and send the initial IDR.

### 3.3 TCP backpressure must bound age and bytes

TCP removes corruption but can replace it with a freeze and stale-frame backlog
under congestion. `pendingSends <= 3` is a useful local guard, but it is not a
complete end-to-end bound.

The opendisplay comment treats `NWConnection`'s `.contentProcessed` callback as
equivalent to bytes acknowledged by the peer:

- `../opendisplay/Mac/MacSender.swift:142-155`
- `../opendisplay/Mac/MacSender.swift:1256-1269`

Apple documents the callback as occurring when data is processed by the network
stack, not explicitly when the receiver has consumed or decoded it:

- <https://developer.apple.com/documentation/network/nwconnection/sendcompletion>

For RESC, use:

- pre-encode dropping while an encode is already pending;
- frame IDs and receiver progress acknowledgements;
- an outstanding **byte** budget as well as a frame-count budget, because an IDR
  can be much larger than a P-frame;
- a maximum oldest-frame age;
- a reconnect/reset plus fresh IDR when that age limit is exceeded.

The receive/decode side must consume every encoded reference frame. It may drop
decoded frames from the render handoff, but it cannot arbitrarily discard
encoded P-frames without resetting the decoder and requesting an IDR.

### 3.4 Keyframe policy needs one explicit contract

The intended policy should be stated unambiguously:

- initial IDR when a video connection becomes ready;
- forced IDR after decoder reset/request;
- static-screen replay from a retained pixel buffer when an IDR is needed but no
  new capture arrives;
- optionally a rare safety IDR, if measurements show it is useful.

`MaxKeyFrameInterval=3600` together with
`MaxKeyFrameIntervalDuration=60` describes an approximately 60-second maximum
interval when honored, rather than literally "no periodic IDRs." Low-latency
rate control may impose its own GOP behavior, so the actual property results
must be logged and validated.

The reference implementation proves the encoder settings for its H.264 path,
not necessarily for RESC's HEVC path. Session creation and every important
`VTSessionSetProperty` return value should be checked instead of ignored.

### 3.5 Capture settings should be A/B tested

Requesting `1/120` to avoid ScreenCaptureKit's measured 1/60 beat is plausible
and worth testing:

- `../opendisplay/Mac/MacSender.swift:439-456`

Copying `queueDepth = 8` should not be an unconditional requirement. Select the
smallest depth that prevents ScreenCaptureKit buffer starvation without
increasing frame age. Measure at least:

- `minimumFrameInterval`: 1/60 versus 1/120;
- `queueDepth`: 3, 5, and 8;
- delivered callback FPS;
- p50/p95 capture-to-present age;
- ScreenCaptureKit drop reasons.

Delete `FramePacer` only after:

- cursor-only redraw is independent of new video captures;
- static reconnect can replay the last pixel buffer as an IDR;
- idle, reconnect, and decoder-reset tests pass.

### 3.6 Decoder recovery should be simplified, not deleted

TCP makes UDP-loss-specific recovery unnecessary. The gray-frame heuristic can
be removed, but a small recovery path remains valuable for:

- decoder errors;
- codec/config changes;
- reconnects;
- hardware decoder reset or failure.

Recommended behavior:

```text
decoder error/config change
  -> stop accepting dependent frames
  -> flush or recreate decoder
  -> request IDR once, with rate limiting
  -> resume on matching config-generation IDR
```

### 3.7 Decode/render split must preserve frame ownership

The proposal correctly says the decoder must not be blocked by SDL vsync.
However, raw FFmpeg plane pointers cannot simply be sent to another thread.
Their storage may be reused when the decoder advances.

The handoff must retain a reference-counted `AVFrame` or copy into explicitly
owned pooled buffers. The design should:

- deframe and decode every encoded frame on the decoder thread;
- retain the newest decoded frame in a latest-wins render slot;
- let the render thread discard older **decoded** outputs;
- upload a frame once, then retain the SDL texture for cursor-only redraws;
- preserve the last uploaded texture while no new video arrives.

NV12 upload should be capability-tested against the deployed SDL version and
renderer backend, with I420 as a fallback.

## 4. Revised implementation order

### Phase A — observable baseline and surgical correctness fixes

1. Carry real capture timestamp and sequence metadata through the pipeline.
2. Add encoded bytes/chunks and reason-coded drop counters.
3. Add decoder-output, texture-upload, presentation, and cursor-presentation
   timing.
4. Replace raw `0xFA` scanning with typed protobuf parsing.
5. Make the force-keyframe flag thread-safe.
6. Log and test the 695,296-byte chunk ceiling.
7. Stop uploading the full video texture on cursor-only redraws.

Acceptance:

- a Stats envelope whose session ID contains `0xFA` never requests an IDR;
- keyframe size and oversize-drop logs explain every large-frame rejection;
- cursor-only presentation performs zero video texture uploads;
- baseline p50/p95/max video and cursor latency are visible.

### Phase B — dedicated reliable video transport

1. Add a separately negotiated TCP video socket.
2. Correct the bind/listen/ready/connect order.
3. Add binary framing with bounded length and stream/config generation.
4. Enable `TCP_NODELAY`.
5. Add pre-encode gating plus receiver progress, byte-budget, and frame-age
   bounds.
6. Reconnect and force a fresh IDR if the age bound is violated.
7. Retain a minimal decoder reset/IDR recovery path.

Acceptance:

- zero transport-loss-induced visual corruption;
- no unbounded catch-up after induced loss or receiver stalls;
- bounded p95 and maximum frame age under `iperf3` saturation;
- control and cursor/input traffic remain responsive during large video frames.

### Phase C — host latency tuning

1. A/B low-latency rate control per codec.
2. Set and verify `PrioritizeEncodingSpeedOverQuality`.
3. Set and verify `MaxFrameDelayCount=0`.
4. Enforce one pending encode.
5. A/B ScreenCaptureKit interval and queue depth.
6. Implement last-pixel-buffer IDR replay.
7. Remove `FramePacer` after idle/reconnect acceptance tests pass.

### Phase D — client pipeline redesign

1. Set decoder low-delay flags before opening the decoder.
2. A/B CUVID low-delay and software thread configurations.
3. Split decode and render ownership.
4. Decode every encoded frame; render only the newest decoded frame.
5. Retain/reference decoded frame storage safely across the handoff.
6. Upload video only on new frames.
7. Reuse the existing GPU texture for cursor-only presentation.
8. Move Night Shift off the full per-frame CPU UV pass if measurements justify
   it.

### Phase E — polish

After latency and glitch acceptance gates pass:

- virtual-display HiDPI/mirroring/arrangement resilience;
- keyboard forwarding;
- sleep/wake and reconnect lifecycle;
- longer soak and fault-injection testing.

## 5. Verification matrix

| Hypothesis/change | Discriminating test |
|---|---|
| Raw `0xFA` false IDR | Send a valid Stats envelope with `session_id = 250`; current code should false-trigger, corrected parsing must not. |
| 512-chunk ceiling | Unit-test 512 versus 513 chunks and log real 4K IDR sizes during window drags/full-screen changes. |
| Cursor texture upload | Count `SDL_UpdateYUVTexture` calls separately for new video and cursor-only presents. |
| CUVID display delay | Feed numbered frames and measure decoder-submit sequence versus decoder-output sequence with low-delay off/on. |
| Software threading delay | Compare thread counts 1/2/4 and slice versus frame threading using latency and sustained decode throughput. |
| ScreenCaptureKit beat | Compare 1/60 and 1/120 using callback cadence, drop reasons, and capture-to-present age. |
| TCP backlog | Stall receiver reads and inject loss/saturation; verify maximum outstanding bytes and frame age stay bounded. |
| Static reconnect | Reconnect without changing screen content; receiver must promptly display a fresh IDR. |
| Cursor independence | Move cursor on an idle screen; verify cursor latency and CPU/GPU load without new captured video frames. |

## 6. Expected outcome

The proposal's target ranges—approximately 40-80 ms video p50 and 10-20 ms
pointer latency—are plausible goals, not currently supported predictions. They
should become acceptance targets only after Phase A establishes a baseline and
the deployed Mac/Ubuntu hardware is measured.

For the stated fixed, wired-LAN use case, a dedicated TCP video stream with
bounded age, pre-encode dropping, low-delay decode, latest-decoded-frame
rendering, and a texture-resident cursor path is the most reasonable architecture.
It should eliminate transport-loss corruption while substantially reducing
perceived pointer and window-drag latency.
