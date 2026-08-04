# Third Review of `IMPLEMENTATION_PLAN.md`

| | |
|---|---|
| **Review date** | 2026-07-29 |
| **Reviewed plan revision** | v2 |
| **Reviewed plan SHA-256** | `a83429d09de756585cf7dcfa8bef9f604d11b9d5321ac0f87b2530813a294533` |
| **Review basis** | `LATENCY_GLITCH_ANALYSIS_review_v2.md` |
| **Basis SHA-256** | `e141414ab7a8a8d951e92c4b21302b8b2d5886591abc8ac62d369888f2a75905` |
| **Reviewed RESC commit** | `12b87d1` |
| **Review type** | Static code, protocol, lifecycle, and architecture review |
| **Runtime measurements performed** | None |

## Executive verdict

`IMPLEMENTATION_PLAN.md` incorporates the v2 review carefully and gets the
architectural direction right:

- dedicated video TCP rather than control-channel multiplexing;
- client listen-before-ready ordering;
- pre-encode latest-capture coalescing;
- decoder-accepted rather than socket-received credit;
- separate decoder-output and presentation telemetry;
- texture-resident cursor redraw;
- retained minimal decoder recovery;
- one-thread SDL ownership and owned decoded-frame handoff;
- measured low-delay/capture tuning rather than assumed values; and
- effort estimates explicitly marked for revision.

I still approve that destination. The plan is substantially better than the
analysis it supersedes.

It is not yet ready to freeze into implementation tickets. Seven load-bearing
amendments remain:

1. Add **G8: completed UDP frames can be delivered to ffmpeg out of frame-ID
   order**, even with no loss.
2. Specify the complete versioned video/progress/reset wire contract, including
   a per-video-connection generation in every frame and ACK.
3. Move a real host gate/replay controller ahead of the Phase B recovery
   contract; `LatestFrameSlot` alone does not provide gate-open wakeup or
   post-consumption replay.
4. Fence asynchronous VideoToolbox completions by generation so old output
   cannot enter a replacement connection.
5. Redesign the decoder API around stable frame identity, explicit
   EAGAIN/error outcomes, exact-once ACKs, and a `PrimingAfterIDR` state.
6. Define a hard maximum-frame and credit-reservation rule that always admits
   one complete recovery IDR.
7. Add an A0.0 instrumentation/protocol scaffold and a minimal reconnect
   lifecycle before claiming the baseline and Phase B gates are executable.

My revised decision is:

> **Approve the architecture and the overall A–E ordering, but revise the
> protocol, host scheduler, decoder recovery, and UDP ordering contracts before
> implementation begins.**

## 1. Disposition of the v2 review

| V2 item | Plan disposition | V3 assessment |
|---|---|---|
| G7 queue-full encoded P-frame loss | Added to findings, I1, A1, B, and gates | Correctly adopted; A1 barrier mechanics still incomplete |
| Correct cursor-latency model | Added to findings and I2 | Correctly adopted; I2 overstates the post-D bound |
| Coalesced newest pending capture | Added as I3 and Phase C | Correct invariant; scheduled too late for B.5 and missing a wake/replay controller |
| Decoder-accepted ACK boundary | Added as I4 and B.4 | Correct boundary; wire epoch, retry, and identity semantics remain incomplete |
| G6 confidence discipline | Added to hypotheses and A0 | Correctly adopted |
| Coherent cursor snapshots | Added to A1 and gates | Correct issue; packed-atomic representation is underspecified |
| Swift protobuf target wiring | Concrete target decision in A1 | Correct; generator/runtime version and test scaffolding need alignment |
| Real drop telemetry | Added to A0 | Correct direction; frame-order and disjoint-rate counters are missing |
| Narrative errata | Recorded in §13 | Correctly adopted |
| Measured low-delay bounds | Added to D and gates | Correctly adopted; current decoder cannot yet identify delayed output |
| More realistic effort treatment | Initial estimates plus B spike | Improved; the proposed no-credit spike must be explicitly non-deployable |

## 2. New high-priority findings

### V3-1. G8 — the UDP assembler does not preserve encoded-frame order

`FrameAssembler` returns whichever slot becomes complete:

- `ubuntu-client/crates/jitter-buffer/src/lib.rs:174-204`

`VideoReceiver` immediately forwards that result:

- `ubuntu-client/crates/net-transport/src/video_receiver.rs:185-210`

`frame_id` is used for slot lookup and numeric eviction, not for an ordered
delivery watermark:

- `ubuntu-client/crates/jitter-buffer/src/lib.rs:230-255`

The decode loop then processes channel arrival order:

- `ubuntu-client/src/main.rs:348-424`

Therefore the following is possible with ordinary UDP reordering:

1. all chunks of frame N+1 arrive;
2. one chunk of frame N is delayed;
3. N+1 completes and is decoded;
4. the delayed chunk arrives;
5. N completes and is decoded after N+1.

No datagram needs to be permanently lost. No frame needs to exceed G6. The
encoded reference sequence is nevertheless supplied to ffmpeg in the wrong
order.

This should be recorded as:

> **G8 — post-assembly encoded-frame reordering before decode.**

I1 should become:

> Every encoded access unit is either decoded exactly once in source order
> within its video generation, or a linearized discontinuity barrier invalidates
> that generation before any later dependent access unit reaches the decoder.

For the interim UDP path, add a bounded completed-frame reorder gate with
wrap-safe frame-ID comparison. On a gap deadline, slot eviction, late frame, or
duplicate:

- emit reason-coded telemetry;
- insert a discontinuity marker in the same ordered stream as frames;
- clear held and queued dependent frames;
- enter decoder recovery once;
- latch one rate-limited IDR request; and
- resume only through the matching recovery-IDR path.

A separate shared Boolean is not a sufficient G7/G8 barrier: later frames may
already be queued when the decoder observes it. Prefer a channel such as
`DecodeInput::{Frame, Discontinuity}` whose ordering is linearized by the
receiver/reorder stage.

This also corrects A0's statement that missing chunks after chunk 0 are the only
wire-level loss proxy. Every packet carries `frame_id`, and the host increments
it per encoded frame:

- `ubuntu-client/crates/protocol/src/lib.rs:118-126`
- `mac-host/Sources/RemoteDisplayHost/VideoSender.swift:50-55`

A gap after a bounded reorder interval detects an entirely missing encoded
frame, although it still cannot count the exact number of missing datagrams.

### V3-2. Phase B needs a complete wire and generation contract

The plan calls the frame format "versioned," but the listed header has no
version, magic, header length, byte order, codec, stream identifier, or video
connection generation:

- `IMPLEMENTATION_PLAN.md:69-76`

The current control schema still defines protocol version 1 and describes the
ports as UDP side channels:

- `proto/control.proto:17-23`
- `proto/control.proto:89-124`

It has no decoder-progress, reset, or clock-sync payload:

- `proto/control.proto:20-46`
- `proto/control.proto:149-180`

I4 says ACKs are tagged with session and config generation, while B separately
mentions a connection generation:

- `IMPLEMENTATION_PLAN.md:41`
- `IMPLEMENTATION_PLAN.md:72-74`

That is insufficient. A delayed ACK from video connection K can arrive over the
still-live control connection after video connection K+1 starts. If both share
the same session and codec configuration, the stale ACK can release K+1 credit.

Before the B spike, specify:

- a control-protocol version bump or an explicit video-transport capability;
- mixed-version rejection behavior;
- exact fixed-width frame header bytes and endianness;
- negotiated hard frame/header limits;
- the lifetimes of `sessionID`, `streamID`, `configGeneration`, and
  `videoGeneration`;
- frame-ID and cumulative-byte-offset reset/wrap rules;
- a video hello/ready handshake containing a nonce or `videoGeneration`;
- `DecoderProgress` with session, config, video generation, cumulative accepted
  frame ID, and a **u64** cumulative accepted-byte offset;
- separate decoder-output and presented watermarks;
- reset/close reasons and which endpoint increments the generation;
- monotonic ACK validation (`old <= ack <= sent`) and duplicate handling; and
- a state-by-message validation matrix.

"Validate session/stream/config IDs on every message" should read "on every
message where those identifiers are defined and valid in the current state."
Pre-session negotiation legitimately uses session zero, and the existing
`Stats` and `DisplaySettings` messages do not carry stream/config identifiers.

The host also needs a concrete peer-address rule. It currently exposes only a
debug endpoint string to `HostSession`, while streaming still relies on the
`--client` argument:

- `mac-host/Sources/RemoteDisplayHost/ControlChannel.swift:50-80`
- `mac-host/Sources/RemoteDisplayHost/HostSession.swift:57-67`
- `mac-host/Sources/RemoteDisplayHost/main.swift:168-199`

Phase B should derive the IPv4/IPv6 peer from the accepted control connection,
define who chooses the listening port, validate the video peer/hello, and remove
the correctness dependency on `--client`.

Finally, minimal disconnect handling belongs in B, not only E. The host clears a
failed control connection without notifying `HostSession`, and the client exits
its control task while decode/render continues:

- `mac-host/Sources/RemoteDisplayHost/ControlChannel.swift:63-76`
- `ubuntu-client/src/main.rs:197-264`

If control carries progress and reset messages, control loss must atomically
stop video credit, close the video generation, release input state, and either
renegotiate or terminate. Phase E may polish sleep/wake behavior, but it cannot
own the first correct reconnect lifecycle.

### V3-3. `LatestFrameSlot` is a coalescing store, not the required gate/replay controller

The plan correctly says to gate slot consumption rather than capture storage.
The existing slot is helpful, but it is not sufficient by itself:

- every store overwrites the buffer and signals the semaphore;
- `waitAndTake()` clears the only retained buffer;
- `tryTake()` also clears it; and
- no encode-completion or ACK path currently wakes a gate-open drain.

Evidence:

- `mac-host/Sources/RemoteDisplayHost/LatestFrameSlot.swift:16-47`

Because each overwrite signals, semaphore permits can accumulate while only one
buffer exists. After one real take, later `waitAndTake()` calls may consume stale
permits and return `nil`.

More importantly, B.5 requires static-screen IDR replay, but I3's implementation
is deferred to Phase C:

- `IMPLEMENTATION_PLAN.md:75`
- `IMPLEMENTATION_PLAN.md:80-84`

Once the current slot is consumed, there is no buffer left to replay. A pending
capture and a replayable last capture are distinct concepts.

Introduce one serial host pump/actor before Phase B recovery is implemented. It
should own:

- `latestPendingCapture`: overwritten by capture, consumed only when all gates
  allow;
- `lastReplayableCapture`: retained after consumption for reconnect/reset IDR;
- `encodeInFlight`: zero or one;
- one bounded encoded-but-not-yet-admitted result, if that design is chosen;
- outstanding accepted-credit accounting;
- current session/config/video generation;
- `awaitingFirstIDR`; and
- wakeups from capture arrival, encode completion, ACK arrival, connection
  transition, and reset request.

On every wakeup, the pump should reevaluate capacity and drain the newest pending
capture without waiting for another ScreenCaptureKit callback.

VideoToolbox output is asynchronous:

- `mac-host/Sources/RemoteDisplayHost/VideoEncoder.swift:154-204`
- [Apple `VTCompressionSessionEncodeFrame` documentation](https://developer.apple.com/documentation/videotoolbox/vtcompressionsessionencodeframe%28_%3Aimagebuffer%3Apresentationtimestamp%3Aduration%3Aframeproperties%3Ainfoflagsout%3Aoutputhandler%3A%29)

The current encoder loop submits again immediately:

- `mac-host/Sources/RemoteDisplayHost/main.swift:135-146`

Therefore the pump must increment the in-flight gate before submission and
release it on every completion or synchronous submission failure.

### V3-4. Socket generation fencing must extend through the asynchronous encoder

The plan mentions a connection generation only at the socket layer. Current
VideoToolbox submissions and callbacks carry no session/config/video generation:

- `mac-host/Sources/RemoteDisplayHost/VideoEncoder.swift:166-199`

The callback sends through whichever global `StreamingState` sender is current:

- `mac-host/Sources/RemoteDisplayHost/main.swift:129-133`
- `mac-host/Sources/RemoteDisplayHost/StreamingState.swift:23-30`
- `mac-host/Sources/RemoteDisplayHost/StreamingState.swift:42-61`

An output submitted under generation K can therefore finish after generation
K+1 installs a new sender and be transmitted on K+1. It may even be mistaken for
the first valid keyframe.

Every encode submission must capture:

```text
sessionID
configGeneration
videoGeneration
captureSeq
contentCaptureTs
admissionTs
forceIDR intent
```

The completion path must discard generation mismatches. A new video generation
must remain `awaitingFirstIDR` until an access unit that was submitted with that
generation and verified as a random-access/keyframe output is ready.

The plan also needs two age concepts:

- `contentCaptureTs`: original screen-content age, retained for telemetry;
- `admissionTs`: when this frame was admitted to the current flow-control
  generation.

A minutes-old static buffer replayed as a new IDR must not use its original
capture timestamp as the 250 ms unaccepted-flow timeout. Otherwise every static
reconnect immediately violates the age bound and reset-loops. Mark replays
explicitly and start flow age at replay admission while retaining the original
content timestamp for observability.

### V3-5. Decoder identity, EAGAIN, and recovery need one structured contract

The current decoder cannot produce correct output watermarks.

It creates an `AVPacket` without frame identity or PTS:

- `ubuntu-client/crates/video-decode/src/lib.rs:193-202`

It may receive zero or more delayed outputs:

- `ubuntu-client/crates/video-decode/src/lib.rs:204-284`

It labels every output with the timestamp of the *current input* packet:

- `ubuntu-client/crates/video-decode/src/lib.rs:279-284`
- `ubuntu-client/crates/video-decode/src/lib.rs:324-330`

With CUVID display delay or software frame threading, that output may belong to
an earlier packet. A0's submit-to-output measurement, B's
`decodedOutputFrameID`, and D's presentation telemetry would all be wrong.

Set an input packet PTS/identity that maps to:

```text
videoGeneration + frameID + captureSeq + captureTs
```

Recover the decoder-emitted PTS for each `AVFrame`, preserve it through the
owned decoded-frame handoff, and report contiguous output/presentation progress
rather than the highest number merely observed.

The wrapper also conflates normal backpressure and real errors:

- `send_packet()` treats every error, including EAGAIN, as a decode failure:
  `ubuntu-client/crates/video-decode/src/lib.rs:198-202`
- `receive_frame(...).is_ok()` treats EAGAIN, EOF, and real errors as the same
  loop termination:
  `ubuntu-client/crates/video-decode/src/lib.rs:204-207`

FFmpeg's send/receive contract requires draining output after EAGAIN before
retrying the same packet:

- [FFmpeg send/receive API overview](https://ffmpeg.org/doxygen/trunk/group__lavc__encdec.html)

Return a structured outcome such as:

```text
DecodeOutcome {
  inputAcceptedExactlyOnce,
  acceptedFrameID,
  acceptedBytes,
  outputs[],
  drainState: Again | EndOfStream | Error,
  recoveryTransition
}
```

Only `Again` is normal drain completion. A send-side EAGAIN retains the exact
encoded frame, drains, and retries; it is neither dropped nor ACKed twice.

There is also a recovery liveness bug. While `WaitingForIDR`, non-keyframes are
rejected before `send_packet()`:

- `ubuntu-client/crates/video-decode/src/lib.rs:193-196`

The state changes only after an IDR produces decoder output:

- `ubuntu-client/crates/video-decode/src/lib.rs:263-275`

If an accepted IDR initially yields EAGAIN because the decoder needs more input,
the next dependent frames are withheld, so the decoder may never emit that IDR.
The current frequent periodic IDRs can mask this; the planned ~60-second maximum
can make it persistent.

Add `PrimingAfterIDR`:

1. flush or recreate the decoder;
2. accept the matching-generation IDR;
3. transition to priming immediately after successful IDR input acceptance;
4. feed a bounded sequence of subsequent access units;
5. suppress unsafe presentation until the mapped IDR/output boundary is seen;
6. then enter recovering/healthy.

The IDR request itself must be an idempotent generation-scoped latch with retry.
The current `try_send` result is ignored, so a full four-slot request channel can
lose the only recovery request:

- `ubuntu-client/src/main.rs:379-387`

### V3-6. The credit window needs a hard whole-frame liveness rule

B.4 proposes an initial byte window of at least twice the largest IDR observed
in A0:

- `IMPLEMENTATION_PLAN.md:74`

That is a tuning heuristic, not a protocol invariant. A later legal IDR can be
larger. If one complete encoded frame does not fit within total or remaining
credit, the receiver cannot deframe and decode it, no ACK can be produced, and
the stream deadlocks.

The current advertised frame cap is at most 2 MB, but the sender does not enforce
that cap:

- `mac-host/Sources/RemoteDisplayHost/ProtocolConstants.swift:62-67`
- `mac-host/Sources/RemoteDisplayHost/VideoSender.swift:50-114`

The new protocol should:

- negotiate and enforce one hard `maxEncodedFrameBytes` on both ends;
- make total credit at least one complete maximum-sized frame plus framing;
- admit a frame only when its full encoded length fits, or define an explicit
  one-frame emergency reservation;
- use a u64 connection-local cumulative byte offset;
- reset that offset only with a new `videoGeneration`; and
- reject ACKs beyond the last sent boundary.

Pre-encode gating cannot know the actual encoded size. Choose one explicit
policy:

1. reserve `maxEncodedFrameBytes` before encoding and return unused reservation
   after output; or
2. allow exactly one encoded-pending frame, count its bytes and age, and do not
   encode another until it is admitted or the generation resets.

Because credit is released on decoder **input acceptance**, decoder display
depth is not itself a credit-liveness requirement. It belongs in output-age
telemetry. A window only needs to exceed decoder startup/display depth if output
progress is used for credit, which this plan correctly rejects.

### V3-7. A0 needs an explicit instrumentation scaffold and clock contract

A0 is titled "no behavior changes" but requires timestamped ping/pong on the
control protocol:

- `IMPLEMENTATION_PLAN.md:45-53`

No such protobuf messages or handlers exist:

- `proto/control.proto:20-46`
- `mac-host/Sources/RemoteDisplayHost/HostSession.swift:74-83`
- `mac-host/Sources/RemoteDisplayHost/HostSession.swift:134-160`

The generated Swift protobuf target is not scheduled until A1. A0 would
otherwise require more handwritten protobuf—the mechanism A1 is intended to
remove.

Add **Phase A0.0 — instrumentation scaffolding**:

1. wire generated Swift protobuf into the build without yet enabling the typed
   IDR behavior fix;
2. add additive `ClockPing`, `ClockPong`, and structured trace/counter schemas;
3. establish buffered/low-overhead structured logging;
4. define one host monotonic epoch and one client monotonic epoch;
5. preserve frame identity through capture, encode, transport, decode, and
   present; and
6. then record the behaviorally unchanged baseline before enabling A1 fixes.

Current timestamps are not a common monotonic trace:

- capture discards sample timing:
  `mac-host/Sources/RemoteDisplayHost/DisplayCapturer.swift:105-127`
- video timestamp is synthesized from frame count:
  `mac-host/Sources/RemoteDisplayHost/main.swift:129-145`
- cursor timestamp uses `CFAbsoluteTimeGetCurrent()`:
  `mac-host/Sources/RemoteDisplayHost/CursorTracker.swift:107-123`

Apple explicitly says `CFAbsoluteTimeGetCurrent()` is not guaranteed to increase
monotonically:

- [Apple `CFAbsoluteTimeGetCurrent` documentation](https://developer.apple.com/documentation/corefoundation/cfabsolutetimegetcurrent%28%29)

Use host-time/continuous monotonic timestamps for latency. Keep the SCK PTS as
diagnostic metadata and explicitly map its `SCStream` synchronization clock to
host time rather than subtracting unrelated timebases:

- [Apple `CMSyncConvertTime` documentation](https://developer.apple.com/documentation/coremedia/cmsyncconverttime%28_%3Afrom%3Ato%3A%29)

Repeat clock-offset sampling periodically and after reconnect; record
lowest-RTT offset and uncertainty. Software `present()` return is still not
photon time, so the final performance gate should include a high-speed-camera or
optical validation.

A0 should use disjoint counters instead of a repaired aggregate
`frame_drop_rate`. An assembled-then-queue-dropped frame currently increments
both completed and dropped, so the existing denominator double-counts the same
frame:

- `ubuntu-client/crates/jitter-buffer/src/lib.rs:193-204`
- `ubuntu-client/crates/net-transport/src/video_receiver.rs:191-203`
- `ubuntu-client/src/main.rs:231-256`

If `SO_RXQ_OVFL` is required, specify `recvmsg` ancillary-data collection; the
current `UdpSocket::recv` path cannot read that control message:

- `ubuntu-client/crates/net-transport/src/video_receiver.rs:100-119`

`/proc/net/snmp` may remain a labeled system-wide fallback, not a per-socket
counter.

## 3. Remaining medium-priority amendments

### M1. Fix raw SDL texture destruction order during A1.4

`PersistentTexture` takes a raw pointer with `mem::forget` and destroys it
manually:

- `ubuntu-client/crates/renderer/src/lib.rs:36-50`
- `ubuntu-client/crates/renderer/src/lib.rs:105-108`

But `Renderer.canvas` is declared before `persistent_tex`:

- `ubuntu-client/crates/renderer/src/lib.rs:13-23`

Rust drops struct fields in declaration order, while `SDL_DestroyRenderer`
frees its associated textures:

- [Rust destructor order](https://doc.rust-lang.org/reference/destructors.html)
- [SDL `SDL_DestroyRenderer`](https://wiki.libsdl.org/SDL2/SDL_DestroyRenderer)

The canvas/SDL renderer can therefore be destroyed before
`PersistentTexture::drop()` calls `SDL_DestroyTexture` on the raw pointer.
Treat this as a likely double-destroy/use-after-free risk. The texture-resident
A1 refactor should replace the raw ownership trick or explicitly destroy/take
the texture before the canvas, rather than waiting for the Phase D thread soak.

### M2. I2 promises more cursor isolation than Phase D provides

After Phase D, video upload remains on the render thread. A cursor update that
arrives during one 4K upload still waits behind that current upload:

- `ubuntu-client/crates/renderer/src/lib.rs:53-61`
- `ubuntu-client/crates/renderer/src/lib.rs:256-289`

The defensible invariant is:

> Cursor updates inherit no queued-video temporal age and trigger no upload on a
> cursor-only redraw; under simultaneous video work they remain bounded by at
> most the current render/upload operation plus presentation.

If the plan wants the stronger "input cadence + present only" claim under
saturated video, it must add cursor-priority scheduling or a different upload
architecture and test it.

The packed `AtomicU64` cursor decision also needs a documented layout. Two i16
coordinates plus an eight-bit shape leave only 24 bits for sequence, not the
current u32. Define wrap comparison and the supported coordinate range, or use a
small mutex/seqlock. The i16 claim should not silently conflict with the plan's
"arbitrary resolutions" goal.

### M3. Make codec negotiation real before claiming HEVC preservation

The client currently advertises only H.264:

- `ubuntu-client/crates/net-transport/src/control_channel.rs:69-78`

The host does not decode `ModeRequest`; it selects HEVC from its command line:

- `mac-host/Sources/RemoteDisplayHost/HostSession.swift:86-132`
- `mac-host/Sources/RemoteDisplayHost/HostSession.swift:251-257`

If the HEVC decoder fails, the client locally constructs an H.264 decoder while
the host continues sending HEVC:

- `ubuntu-client/src/main.rs:278-292`

That is not a valid fallback. A1/B must implement capability intersection and
either renegotiate/recreate the encoder or reject the mode. This is required by
the plan's stated goal of retaining 4K HEVC.

### M4. Align code generation, encoder creation, and test scaffolding

The protobuf target choice is correct:

- `tools/generate_proto.sh:16-18`
- `tools/generate_proto.sh:117-125`
- `mac-host/Package.swift:31-38`

But the generator is pinned to SwiftProtobuf 1.28.1 while the resolved runtime is
1.36.1:

- `tools/generate_proto.sh:12-15`
- `mac-host/Package.resolved:3-10`

Pin compatible matching versions or add a generated-code compatibility/CI check.
Replace the remaining handwritten outbound encoders as well as the inbound
parser:

- `mac-host/Sources/RemoteDisplayHost/HostSession.swift:109-129`
- `mac-host/Sources/RemoteDisplayHost/HostSession.swift:197-220`
- `mac-host/Sources/RemoteDisplayHost/HostSession.swift:230-321`

`EnableLowLatencyRateControl` is an encoder-specification key supplied when the
compression session is created, so each A/B variant requires a new session:

- `mac-host/Sources/RemoteDisplayHost/VideoEncoder.swift:72-88`
- `/Library/Developer/CommandLineTools/SDKs/MacOSX15.4.sdk/System/Library/Frameworks/VideoToolbox.framework/Versions/A/Headers/VTCompressionProperties.h:1119-1134`

Keep the plan's correct requirements to check every property result and verify
HEVC behavior.

Finally, the repository has no SwiftPM test target. The plan should budget and
name test harnesses for:

- protobuf state/message validation;
- host pump/coalescing/replay;
- generation fencing with delayed fake encoder completions; and
- credit/reset state-machine tests.

### M5. Constrain the B spike and revise estimates after the contract spike

I4 is declared invariant across every phase, yet §10 proposes a TCP spike with
no credit:

- `IMPLEMENTATION_PLAN.md:36-41`
- `IMPLEMENTATION_PLAN.md:119-121`

Either include minimal hard-bounded credit in the spike, or label it a
non-deployable laboratory branch with a fixed one-frame write/read bound. It
must not become an intermediate release that recreates unbounded TCP age.

The revised estimates are better framed than the analysis's one-day Phase B,
but A0, the new wire schema, lifecycle tests, host scheduler, and decoder
state-machine work make the current lower bounds aggressive. Revise again after
the contract-first spike, as the plan already promises.

The plan should also state whether TLS/pairing remains out of scope. A random
generation token on a plaintext control channel prevents accidental stale joins;
it does not authenticate the peer.

## 4. Recommended phase-plan amendments

### Phase A0.0 — additive instrumentation scaffold

1. Wire generated Swift protobuf and align codegen/runtime versions.
2. Add additive clock-sync and structured trace messages without enabling G3's
   behavioral fix.
3. Define stable identity and monotonic time domains end to end.
4. Add test targets/harnesses.

### Phase A0 — behaviorally unchanged baseline

1. Add disjoint packet, frame, ordering, assembly, queue, decoder, upload, and
   present counters.
2. Add frame-gap, late, duplicate, and out-of-order telemetry.
3. Record the baseline before enabling typed IDR parsing, cursor, upload, or
   discontinuity behavior changes.
4. Include software timing uncertainty and an optical baseline.

### Phase A1 — UDP correctness and surgical fixes

1. Enable typed protobuf parsing with a state/message validation matrix.
2. Fix force-keyframe synchronization.
3. Implement texture-resident cursor redraw **and** safe texture lifetime.
4. Publish a coherent cursor snapshot with a documented representation.
5. Add the ordered frame gate and a linearized
   `Frame | Discontinuity` decode-input stream for G7/G8.
6. Add the idempotent IDR-request latch and `PrimingAfterIDR`.
7. Keep UDP explicitly non-release-quality unless the G6 ceiling is patched.

### Phase B0 — contract-first, non-deployable spike

1. Freeze the control/video protocol version and exact schemas.
2. Define peer/port ownership and acquire the control peer address.
3. Define generation lifetimes and generation-fenced fake encoder completions.
4. Enforce a hard maximum frame and one-whole-frame credit rule.
5. Prove one frame and one ACK end to end under fixed memory bounds.

### Phase B1 — host pump, replay, and lifecycle

1. Implement the serialized capture/encode/credit pump.
2. Retain both pending-latest and last-replayable capture state.
3. Fence every encode submission/completion by generation.
4. Implement video reset plus minimal control-disconnect behavior.
5. Prove minutes-old static replay with separate content/admission timestamps.

### Phase B2 — complete reliable transport

1. Add cumulative decoder-accepted byte/frame progress.
2. Add exact EAGAIN retry and decoder-output identity.
3. Add `PrimingAfterIDR` and matching-generation first-IDR gating.
4. Run stall, stale-ACK, delayed-callback, reconnect, oversize, and malformed
   framing tests.
5. Delete UDP assembly only after the TCP path passes the full gates.

### Phase C

Keep final-static verification, FramePacer removal, and encoder/capture tuning
here. The minimum coalescing/replay machinery moves to B1.

### Phase D

Keep the decode/render split, owned handoff, low-delay A/B, and NV12 capability
test. Add:

- frame identity through present;
- explicit SDL texture destruction order;
- separate NV12 texture/update API;
- a GPU Night Shift path or explicit I420 fallback; and
- the corrected cursor bound under simultaneous upload.

## 5. Additional required validation gates

| Gate | Required test |
|---|---|
| UDP order/G8 | Complete N+1 before N, then complete N; decoder receives neither reversed nor duplicate access units. A gap produces one ordered discontinuity. |
| Discontinuity linearization | Saturate the encoded queue while injecting a drop; no later dependent P-frame crosses the barrier. |
| IDR request liveness | Fill the request channel, trigger recovery, then drain it; one generation-scoped request is eventually delivered. |
| Decoder identity | Submit numbered packets with induced output delay; every emitted frame retains the correct input identity and capture metadata. |
| Decoder EAGAIN | Force send-side EAGAIN; the identical packet is drained/retried once and ACKed exactly once. |
| IDR priming | Make the first recovery IDR produce no immediate output; dependent inputs are admitted until the mapped IDR appears without presenting pre-boundary output. |
| Protocol version | A v1/UDP peer cannot enter the v2/TCP state accidentally; malformed/oversize frames fail closed. |
| Stale ACK | Deliver generation-K ACKs after K+1 starts; K+1 credit does not move. |
| Stale encode callback | Complete a generation-K encode after K+1 installs its sender; no old access unit enters K+1. |
| Whole-frame credit | A maximum legal IDR fits and recovers when available credit is at its minimum legal state. |
| Static replay age | Replay a minutes-old retained capture after reset; flow age starts at replay admission and does not reset-loop. |
| Gate-open wake | Close credit, store one final capture, stop callbacks, reopen credit; the capture is encoded without another SCK event. |
| Control disconnect | Drop control while video is active; both ends close/reset the video generation and release input state deterministically. |
| Codec negotiation | Exercise H.264-only, HEVC-capable, and HEVC-init-failure peers; both endpoints always use the same codec. |
| Renderer lifetime | Repeated create/destroy under sanitizers; texture destruction precedes renderer destruction and every SDL call stays on the render thread. |
| Optical latency | Compare software timing with a high-speed-camera/optical measurement before and after the final changes. |

## Conclusion

The plan is thoughtful, evidence-driven, and faithful to the v2 review. Its
central architecture remains the correct one for this fixed wired-LAN use case.

The new findings do not argue for a different architecture. They expose missing
contracts at its boundaries:

- encoded input must be ordered, not merely non-dropped;
- video generations must cover sockets, ACKs, encoder callbacks, and decoder
  state together;
- a coalescing slot needs an active pump and a separate replay lifetime;
- decoder acceptance must be identity-preserving and EAGAIN-correct; and
- byte credit must admit one whole legal recovery frame.

After those amendments are written into the plan, I would consider it ready for
an A0.0/A0 instrumentation pass followed by a contract-first Phase B spike.
