# Review of `IMPLEMENTATION_PLAN_V4.md`

| | |
|---|---|
| **Review date** | 2026-07-29 |
| **Reviewed plan** | `IMPLEMENTATION_PLAN_V4.md` |
| **Plan SHA-256** | `30d8eff883ea33e74c1dd38793da6799510ce070ac8c1f0ee7b39451eb7b6f3a` |
| **Compared with** | `IMPLEMENTATION_PLAN_review_v3.md` |
| **RESC commit** | `12b87d1c7b4d8939267985896adb6c2a4ab0ae2a` |
| **Method** | Static review of the plan, current Rust/Swift implementation, protobuf schemas, and relevant local SDK headers |
| **Runtime status** | No build, deployed-system measurement, or fault injection was performed for this review |

## Verdict

**No-go for declaring the v4 protocol and lifecycle frozen.**

The central architecture is now sound. v4 correctly adopts nearly all of review
3's important recommendations: ordered decode input, in-band discontinuities,
an active host pump, separate replay storage, generation-fenced encoder
callbacks, decoder-accepted credit, a whole-frame credit rule, truthful cursor
bounds, codec negotiation, and explicit SDL lifetime work.

The remaining problems are narrower than those in plan v2, but they sit on
load-bearing boundaries. In particular:

1. reset and connection-generation authority is not yet an executable state
   machine;
2. same-generation TCP recovery can still deadlock behind exhausted credit;
3. the progress/ACK and video wire schemas are internally incomplete;
4. the claim that 4 MiB covers every legal 4K IDR is false;
5. decoder identity and EAGAIN work is scheduled after phases that already need
   it;
6. the interim UDP barrier has incorrect triggers and no purgeable queue
   contract;
7. input and cursor side channels remain unfenced across reconnect;
8. A0.0's identity and clock-sync plan is not implementable as written; and
9. codec negotiation does not yet carry enough capability information or
   control encoder construction.

Only noncontroversial A0.0 scaffolding should start before these contracts are
amended. After the amendments below, I would approve the overall phase
structure.

## 1. What v4 resolves well

| Review-3 concern | v4 disposition | Assessment |
|---|---|---|
| G8 encoded-frame reordering | I1 plus an ordered `DecodeInput` stream and bounded UDP reorder gate | Correct direction |
| Queue-drop discontinuity | In-band `Discontinuity` rather than a shared flag | Correct direction; queue ownership still needs specification |
| Host coalescing/replay liveness | Serial pump, `latestPendingCapture`, `lastReplayableCapture`, gate-open wakeups | Correct |
| Stale VideoToolbox completions | Full generation context on submissions and callback fencing | Correct |
| Static replay age | Separate content and admission timestamps | Correct |
| Decoder EAGAIN/identity/recovery | Structured outcome, PTS identity, `PrimingAfterIDR`, IDR latch | Correct conceptual model; phase placement and bounds remain incomplete |
| Credit deadlock | Sender-enforced maximum, decoder-accepted cumulative credit, bounded spike | Correct model; the chosen maximum and recovery semantics need correction |
| A0 instrumentation dependency | New A0.0 scaffold, monotonic clocks, optical validation | Correct need; proposed clock/wire mechanics need amendment |
| SDL lifetime and cursor bound | Texture lifetime moved to A1; cursor bound includes current upload | Correct |
| Codec mismatch | Capability intersection and removal of invalid H.264 mid-stream fallback | Correct goal; capability and encoder-lifecycle details remain |
| No-credit TCP spike | B0 is non-deployable and credit-bounded from its first frame | Correct |
| Security scope | Plaintext LAN trust and nonce limitations are explicit | Correctly scoped residual risk |

The 30-byte per-frame header arithmetic at
`IMPLEMENTATION_PLAN_V4.md:75-76` is correct. The objection below is about
missing semantics and metadata, not its addition.

## 2. Mandatory amendments

### V4-1 — Reset, Hello, and generation authority are not a state machine

`VideoReset{reason,new_video_generation}` is described as both a client request
and a host-authoritative action at `IMPLEMENTATION_PLAN_V4.md:59-62`. Those are
different messages. A client cannot legitimately choose
`new_video_generation` when the host is the only writer.

There are four related ambiguities:

- `videoGeneration` is bumped on socket establishment **or** reset
  (`IMPLEMENTATION_PLAN_V4.md:65-71`). A reset followed by a new socket can
  therefore mean one increment or two.
- `VideoHello` introduces the nonce itself
  (`IMPLEMENTATION_PLAN_V4.md:73-74`). The client has no control-plane value
  against which to validate that nonce, so it does not bind the accepted socket
  to the negotiated generation.
- the client listen port is left as an alternative: advertised in
  `StreamingReady` **or** fixed in `ModeConfirm`
  (`IMPLEMENTATION_PLAN_V4.md:77`). The current `StreamingReady` has no port,
  while `ModeConfirm.video_port` is documented as a UDP side-channel port
  (`proto/control.proto:120-123,144-147`);
- `sessionID` is said to survive a reconnect grace period, but the current host
  generates a new session, stream, and config tuple for every ModeRequest
  (`mac-host/Sources/RemoteDisplayHost/HostSession.swift:86-103`). No resume
  request or proof-of-prior-session transition exists.

Reset coordination over the control TCP connection while new frames travel over
the video TCP connection also has no cross-connection ordering guarantee. A
host cannot send a reset on one connection and assume the client observes it
before a new-generation frame on the other.

The simplest correct contract is below. An initial connection begins with the
host allocation/offer; a client-triggered reset begins with step 1.

1. client sends
   `VideoResetRequest{session,stream,config,observedVideoGeneration,reason}`;
2. host freezes the pump, closes the old video socket, and allocates exactly one
   new generation for the next connection attempt;
3. host sends
   `StartVideoGeneration{session,stream,config,newVideoGeneration,nonce,maxWireFrameBytes,creditWindowBytes}`;
4. client binds an ephemeral listener and replies
   `StreamingReady{session,stream,config,videoGeneration,nonce,listenPort}`;
5. host connects to the typed remote control address and sends a fixed-layout
   `VideoHello` echoing the committed tuple and nonce;
6. client validates it and returns a fixed-layout `VideoHelloAck`; and
7. host sends nothing until it has a verified random-access access unit for
   that generation.

A failed connection attempt consumes that generation; retry gets a new one.
This removes the ambiguous “reset or establishment” increment and makes a fresh
video socket the ordering barrier. An in-place reset could work, but it would
need an in-band video-socket reset record and acknowledgement; it is more
complex.

Also choose one session rule:

- make `sessionID` control-connection-scoped and allocate a new tuple on every
  reconnect; or
- define an actual resume message and state transition.

The current prose promise of grace-period persistence is not sufficient.

### V4-2 — Same-generation TCP recovery can exhaust the credit needed for its IDR

I4 says a recovery frame is always admissible
(`IMPLEMENTATION_PLAN_V4.md:48`), but the decoder contract says frames discarded
in `WaitingForIDR` are not accepted progress
(`IMPLEMENTATION_PLAN_V4.md:96`). If those P-frames have already crossed the TCP
socket, their bytes consume the window and are never ACKed. The requested IDR
can then sit in the encoded-pending slot with insufficient credit.

The maximum-frame window does not solve this: it guarantees that an IDR fits in
an empty window, not that a window occupied by deliberately unacknowledged
frames can admit another one.

For TCP, make any decoder discontinuity generation-fatal:

- stop accepting old-generation input;
- request/announce a new generation over control;
- close the old video socket;
- recreate or flush-and-reinitialize the decoder and clear its identity table;
  and
- recover with the first verified random-access unit on the new socket.

Keep same-generation `wantIDR` recovery only for the interim UDP path and
healthy periodic keyframe requests. Reserving emergency IDR credit is an
alternative, but it adds a second credit class and more failure states for no
clear benefit here.

The host-flow invariant should explicitly cover all stages:

- `sentWireBytes - acceptedWireBytes <= creditWindowBytes`;
- `encodedPendingWireBytes <= maxWireFrameBytes`;
- no second encode starts while an encoded-pending frame exists;
- application-owned encoded memory is bounded by
  `creditWindowBytes + maxWireFrameBytes`;
- age bounds cover encode-in-flight, encoded-pending, writer-queued, and
  sent-unaccepted frames; and
- progress ACKs have a maximum coalescing delay.

`old <= ack <= sent` is necessary but not sufficient. Each ACK's frame cursor
and byte cursor must identify the same exact prefix boundary in a retained
host-side ledger.

### V4-3 — The frozen wire/progress schema is not complete

Several fields have no unambiguous semantics yet.

#### Progress presence and presentation

`frameID` starts at zero (`IMPLEMENTATION_PLAN_V4.md:71`), while the proposed
proto3 scalar `accepted_frame_id`, `output_frame_id`, and
`presented_frame_id` also default to zero
(`IMPLEMENTATION_PLAN_V4.md:59-61`). “No progress” and “frame 0 progressed” are
therefore indistinguishable.

Use either proto `optional` presence or, preferably, u64 exclusive counters
starting at zero:

- `acceptedFrameCount`;
- `acceptedWireBytes`;
- `decodedOutputThroughCount`; and
- telemetry-only presentation fields.

The proposed “contiguous presented watermark” is incompatible with Phase D.
Plan v2's inherited latest-wins decoded-frame handoff intentionally permits
decoded frames never to be presented (`IMPLEMENTATION_PLAN.md:90-95`).
Presentation should therefore be represented as
`lastPresentedFrameID/captureSeq`, `renderRetiredThrough`, and intentional-drop
counters. It must never release credit.

`DecoderProgress` also omits `streamID`, despite `VideoHello` binding a stream.
Every progress/reset message should carry the complete tuple that is meaningful
in that state.

#### Byte domain

The plan alternates among encoded bytes, payload bytes, framed bytes, and socket
bytes (`IMPLEMENTATION_PLAN_V4.md:48,61,76,83,101`). Freeze one definition:

`wireBytes(frame) = actual headerLen + payloadLen`.

Then define exactly when a frame enters `sentWireBytes`—for example, when a
credit-admitted record is enqueued to a single bounded writer. A partial or
failed write closes the generation; counters are never rolled into the next
one. `acceptedWireBytes` must equal the byte-prefix boundary associated with
`acceptedFrameCount`, not merely be independently monotonic.

#### Missing or contradictory metadata

The client identity side table is specified to contain
`{captureSeq, contentCaptureTs, admissionTs}`
(`IMPLEMENTATION_PLAN_V4.md:94`), but the 30-byte frame header carries no
`admissionTs` (`IMPLEMENTATION_PLAN_V4.md:75-76`). Either:

- add `admissionTs_us:u64`, making the base header 38 bytes; or
- keep admission age exclusively on the host and remove it from the client-side
  table, retaining a bounded host telemetry ledger after credit is released.

The first option is simpler for output/presentation age telemetry.

Before calling the protocol frozen, also assign:

- protobuf field numbers and directions for all new messages;
- selected versus supported transport fields;
- exact `VideoHello` and `VideoHelloAck` byte layouts;
- magic values, nonce size and origin, minimum/maximum `headerLen`, and unknown
  flag/reserved-bit policy;
- `creditWindowBytes` and `maxWireFrameBytes` negotiation fields;
- codec numeric values and “must equal negotiated codec” validation; and
- payload format: one Annex-B H.264/HEVC access unit per record, including the
  random-access parameter-set rules.

### V4-4 — The 4 MiB “any legal 4K IDR” claim is false

`IMPLEMENTATION_PLAN_V4.md:83,207` treats 4 MiB as at least every legal 4K IDR
under the configured VideoToolbox rate limit. The current encoder actually
sets:

`[bytesPerSec * 2.0, 0.1]`

at `mac-host/Sources/RemoteDisplayHost/VideoEncoder.swift:107-111`. With the
current defaults
(`mac-host/Sources/RemoteDisplayHost/VideoEncoder.swift:29-34`), that is:

- 12,500,000 bytes over 0.1 s for 50 Mbps 4K H.264; and
- 10,000,000 bytes over 0.1 s for 40 Mbps 4K HEVC.

The SDK defines `DataRateLimits` as compressed bytes in a contiguous decode-time
window, not a per-access-unit maximum, and notes that codec support varies
(`/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/System/Library/Frameworks/VideoToolbox.framework/Headers/VTCompressionProperties.h:215-229`).
It does not prove a 4 MiB frame bound.

Moreover, current property-set return values are ignored
(`mac-host/Sources/RemoteDisplayHost/VideoEncoder.swift:94-129`), while v4
delays checking them until Phase C
(`IMPLEMENTATION_PLAN_V4.md:151-152`). B0 cannot base a correctness invariant
on a property whose acceptance is checked later.

Treat 4 MiB as a **provisional application cap**, not a derived fact. Before B0:

- choose a negotiated memory/wire cap from A0 histograms plus an explicit safety
  margin;
- validate every property required to make that cap credible when the encoder
  session is created;
- test both codecs and the actual deployed macOS/hardware;
- define what happens when the encoder cannot honor the cap; and
- define oversize recovery that changes something—larger bounded negotiation,
  lower bitrate/quality, encoder recreation, or session failure.

Resetting an unchanged encoder after an oversized first IDR can produce an
infinite reset loop. Add a repeated-oversize liveness gate.

### V4-5 — Decoder identity, EAGAIN, and priming are scheduled too late and remain unbounded

A1 requires `PrimingAfterIDR` and tests whether output mapped to the recovery
IDR appears (`IMPLEMENTATION_PLAN_V4.md:119-128`). That is impossible without
the identity and structured EAGAIN work described in §4. Yet B2 is where the
plan explicitly says D1 identity/progress is fixed
(`IMPLEMENTATION_PLAN_V4.md:144-148`).

Current code demonstrates the dependency:

- it creates `Packet::copy` without PTS
  (`ubuntu-client/crates/video-decode/src/lib.rs:193-199`);
- it treats every `send_packet` error as fatal
  (`ubuntu-client/crates/video-decode/src/lib.rs:199-202`);
- it drains with `receive_frame(...).is_ok()`, merging EAGAIN, EOF, and real
  errors (`ubuntu-client/crates/video-decode/src/lib.rs:204-207`); and
- it stamps every output with the currently submitted input timestamp
  (`ubuntu-client/crates/video-decode/src/lib.rs:279-284,324-330`).

Move the minimum structured decoder outcome, exact packet retry, PTS identity,
decoder reset, and side-table lifecycle into A1 before priming. B2 should attach
credit and progress reporting to an already-correct decoder API.

The PTS mechanism itself is a hypothesis until tested on both the deployed
CUVID and software paths. The project only constrains `ffmpeg-next` to major
version 7 (`ubuntu-client/crates/video-decode/Cargo.toml:9`). Use a decoder-local
u64 token as packet PTS rather than truncating `videoGeneration` to 15 bits,
then map the token to the full identity tuple. Specify:

- which output timestamp is read (`pts` or a verified fallback);
- behavior for missing/`AV_NOPTS_VALUE` or unknown tokens;
- side-table removal on output, reset, error, and never-output inputs;
- maximum outstanding identity entries and maximum output age; and
- wrap/reuse rules.

Input acceptance remains the credit boundary, but decoder output cannot be
“telemetry only” in the sense of having no safety consequence. A decoder that
keeps accepting packets while producing no output would continually release
credit, grow the identity table, and grow visual age. Add a watchdog:

`acceptedFrameCount - decodedRetiredCount <= maxDecoderLagFrames`

plus a time bound. Exceeding either closes the generation; it does not move the
credit boundary to presentation.

“Retry once-effective” should mean:

1. retain the exact packet;
2. on send-side EAGAIN, drain receive-side output;
3. require progress or fail the generation;
4. retry the same packet until it is accepted exactly once; and
5. drain to a classified EAGAIN afterward.

`PrimingAfterIDR` also needs concrete `maxPrimingFrames` and
`maxPrimingDuration` values, a failure transition, and explicit decoder
flush/recreation plus side-table disposal. Clear the IDR latch only after a
verified random-access unit is accepted into priming, not merely when a packet
whose header says “keyframe” arrives.

### V4-6 — The UDP discontinuity contract resets on harmless packets and cannot clear its current queue

`IMPLEMENTATION_PLAN_V4.md:92` lists every late frame and duplicate as a
discontinuity trigger. A duplicate of an already delivered frame, or a late
copy after a gap has already been linearized, does not imply a new reference
loss. Resetting on ordinary UDP duplication would create unnecessary IDR
storms.

Use these rules instead:

- expected frame completes: release it, then drain consecutive held frames;
- ahead-of-expected completion within the window: hold it;
- behind-expected duplicate/late completion: discard and count it;
- missing expected frame at the deadline, conflicting duplicate, slot eviction,
  or decode-queue admission failure: emit exactly one discontinuity for the
  current recovery epoch.

The current `sync_channel(4)` cannot clear already queued frames
(`ubuntu-client/src/main.rs:122-124`), and the current assembler reports timeout
and eviction only through counters
(`ubuntu-client/crates/jitter-buffer/src/lib.rs:210-255`). A1 must give one actor
ownership of a purgeable queue and surface reason-coded assembler events. That
actor must be able to clear held/queued dependents and place the barrier even
when the former data queue is full.

UDP v1 has no `videoGeneration`, so define a local `recoveryEpoch` for A1. The
epoch fences held frames and IDR-latch state until B2 deletes UDP.

### V4-7 — B1 lifecycle does not fence cursor and input side channels

The plan correctly closes video and releases pressed input on control loss
(`IMPLEMENTATION_PLAN_V4.md:79-80`), but the UDP side channels can invalidate
that cleanup.

Input packets contain only a packet prefix, sequence, and event data
(`proto/input.proto:25-38`). `InputReceiver` binds all interfaces, calls
`recv`, and validates neither source nor session/config generation
(`mac-host/Sources/RemoteDisplayHost/InputReceiver.swift:19-40,58-99`). A stale
mouse-down queued before disconnect can therefore arrive after `releaseAll()`
and press the button again. Sequence rejection is applied only to mouse moves,
not button or scroll events
(`mac-host/Sources/RemoteDisplayHost/InputReceiver.swift:83-99`).

The cursor sender sequence begins at zero in each new tracker
(`mac-host/Sources/RemoteDisplayHost/CursorTracker.swift:17-24,107-123`), while
the client preserves and compares its previous sequence
(`ubuntu-client/src/main.rs:139-182`). Without an epoch/reset, a new cursor
stream can be rejected against a high old sequence; stale old packets can also
overwrite new state.

At the v2 protocol bump, fence input and cursor packets by a session or
side-channel generation and validate the remote source. On lifecycle change,
atomically close/rebind fresh sockets (preferably fresh negotiated ports), reset
sequence state, drain old kernel queues by closing the old descriptors, and
only then enable the new epoch. Add stale cursor/input gates alongside stale
ACK and stale encoder-callback gates.

This does not require tying cursor operation to every video reset. It does
require tying both UDP side channels to the active control session/config
lifetime.

### V4-8 — A0.0 identity and clock sync are not implementable as written

A0.0 is described as additive v1 scaffolding with behavioral typed parsing
disabled, yet it requires `captureSeq` to travel
capture→encode→wire→decode→present
(`IMPLEMENTATION_PLAN_V4.md:107-110`).

The current v1 UDP header is fixed and has no capture sequence
(`proto/video.proto:27-41`;
`ubuntu-client/crates/protocol/src/lib.rs:148-188`). Appending a field would move
the payload offset and corrupt the Annex-B data for an unchanged v1 parser.

For the baseline, use the existing encoded `frameID` as the wire identity and
keep a host trace mapping `frameID -> captureSeq/contentCaptureTs`, or introduce
a separately versioned telemetry packet. Do not silently mutate the v1 video
layout. End-to-end `captureSeq` belongs naturally in the v2 header.

Clock handling also needs four-event semantics. The proposed
`ClockPing{t1}` / `ClockPong{t1,t2}` cannot remove responder processing delay.
Use:

- requester send `t1`;
- responder receive `t2`;
- responder send `t3`; and
- requester receive `t4` locally.

Record the offset formula, sign, uncertainty, rejection criteria, resampling
interval, and suspend/reconnect invalidation. A0.0 must enable a narrowly scoped
typed ClockPing/Pong handler even if the rest of behavioral parsing waits until
A1; the current host runtime parser only scans for RequestIDR-like bytes
(`mac-host/Sources/RemoteDisplayHost/HostSession.swift:134-159`).

Finally, the chosen host clock bridge is incomplete. `SCStream.synchronizationClock`
is nullable
(`/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/System/Library/Frameworks/ScreenCaptureKit.framework/Headers/SCStream.h:446-453`).
CoreMedia host time uses `mach_absolute_time`
(`/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/System/Library/Frameworks/CoreMedia.framework/Headers/CMSync.h:123-154`),
whereas `mach_continuous_time` advances through sleep
(`/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/include/mach/mach_time.h:53-62`).
Specify the `CMSyncConvertTime` target, the calibrated
absolute-to-continuous bridge, nil-clock fallback, and epoch invalidation after
sleep. Otherwise the implementation can reintroduce the unrelated-timebase
subtractions v4 intends to eliminate.

### V4-9 — Codec intersection is too coarse and the encoder starts before negotiation

A1 says the client probes a decoder set and the host selects the codec
intersection (`IMPLEMENTATION_PLAN_V4.md:126-128`). Current `ModeRequest`
expresses only coarse H.264/HEVC enum values
(`proto/control.proto:69-74,217-232`); it cannot express profile, level, bit
depth, maximum resolution/rate, or hardware/software constraints.

The host also constructs and starts an immutable encoder before ModeRequest is
handled (`mac-host/Sources/RemoteDisplayHost/main.swift:111-151`;
`mac-host/Sources/RemoteDisplayHost/VideoEncoder.swift:42-50,59-80`). Parsing a
codec intersection later cannot change that already-running session.

Add enough capability data to choose a decodable mode, then create/recreate the
encoder only after `ModeConfirm` fixes the codec/config generation. A decoder
initialization failure must renegotiate a new config generation or fail the
session; it cannot switch codec under an existing generation.

Random-access verification also needs an exact compressed-side contract.
Current `NALUPackager` derives `isKeyframe` from VideoToolbox's `NotSync`
attachment and defaults to keyframe if attachments are absent
(`mac-host/Sources/RemoteDisplayHost/NALUPackager.swift:75-90`). It does not
parse NAL unit types.

Define:

- the host component that parses each Annex-B access unit;
- accepted H.264 and HEVC random-access NAL types;
- required SPS/PPS or VPS/SPS/PPS presence for a generation's first frame;
- whether CRA is accepted and, if so, its leading-picture rule;
- client validation of the header flag against the payload; and
- behavior when a forced encoder output is not actually random-access.

The phrase “verified as random-access on output” should explicitly mean
**encoder output access unit**, not decoded `AVFrame`.

## 3. Smaller corrections

### Cursor sequence comparison

The proposed u24 comparison at `IMPLEMENTATION_PLAN_V4.md:162-164` treats an
equal sequence as newer because delta zero is less than half-range. Use:

`delta = (new - old) & 0xFF_FFFF; 0 < delta && delta < 0x80_0000`.

Document the half-range assumption: the consumer must not go more than
`2^23 - 1` publications without observing an update, or the epoch must reset.

### Render ownership

Phase D should say that one render actor owns exactly one SDL context, video
subsystem, event pump, window, canvas, and all textures. Current code initializes
SDL in both the decode-render closure and `Renderer::new`
(`ubuntu-client/src/main.rs:275-303`;
`ubuntu-client/crates/renderer/src/lib.rs:114-176`). Name the NV12 update
mechanism (`SDL_UpdateNVTexture` when available, or a tested lock/update path),
not only `SDL_PIXELFORMAT_NV12`.

### Code generation verification

Changing the pinned SwiftProtobuf version is not enough. The generator accepts
any already-installed `protoc-gen-swift` without checking its version
(`tools/generate_proto.sh:12-15,82-87`). A0.0 should verify/rebuild the plugin
and make CI regeneration run from a clean, pinned toolchain.

### Effort

The new estimates are more honest than plan v2, and the B0 re-estimation
commitment is good. They are still best-case implementation estimates. A0.0's
current 1–1.5 days includes cross-language codegen integration, two clock-domain
bridges, end-to-end identity, two test harnesses, and structured logging.
Soak/optical wall time and fault-suite development should be budgeted
separately.

## 4. Recommended phase correction

### Paper amendment before implementation

Freeze the following first:

1. generation offer/reset/Hello state machine;
2. complete protobuf field assignments and fixed video records;
3. exact credit byte domain, progress presence, prefix ledger, and age bounds;
4. provisional maximum-frame policy plus oversize recovery;
5. decoder token/side-table/failure bounds;
6. v2 cursor/input session fencing; and
7. codec capability and encoder-creation lifecycle.

### A0.0

- add the Swift protobuf target and verify the generator/runtime pair;
- add only the narrow typed clock/telemetry handlers needed by A0;
- implement the absolute/continuous clock bridge and suspend invalidation;
- use v1 `frameID` plus a host trace mapping instead of changing the v1 video
  header;
- add decoder-token experiments on both CUVID and software; and
- build fake encoder, scripted decoder, and protocol-state test scaffolding.

### A0

Run the behaviorally unchanged baseline only after trace joining and clock
uncertainty are demonstrated. Label software and optical measurements
separately.

### A1

In addition to v4's A1 list, implement the minimum structured decoder outcome,
PTS/token identity, retry-on-EAGAIN, bounded priming, local UDP recovery epoch,
purgeable decode-input queue, and reason-coded assembler events. Codec
negotiation must control encoder creation.

### B0

Implement the v2 generation-offer/listen/Hello handshake, v2 side-channel
fencing, exact wire/progress schemas, a bounded writer/reader, the prefix
ledger, provisional maximum-frame enforcement, and one accepted frame/ACK.

### B1 and B2

Retain v4's host pump, replay, lifecycle, cumulative progress, output watchdog,
fault suite, and UDP deletion, with one change: any TCP decoder discontinuity
creates a new video generation rather than waiting for an in-generation IDR.

Phases C and D can then proceed substantially as written.

## 5. Additional validation gates

| Gate | Required result |
|---|---|
| Generation allocation | Simultaneous reset, reconnect, and failed dial allocate one authoritative generation per attempt, never a double increment |
| Nonce binding | A Hello whose nonce was not precommitted on control is rejected |
| Cross-connection ordering | Delayed reset/control messages cannot make a new-generation frame enter old decoder state |
| Frame-zero progress | “No progress” and “frame 0 accepted/output” are distinguishable |
| ACK prefix | Mismatched valid-looking frame and byte cursors are rejected without releasing credit |
| Credit recovery | A decoder fault after all old credit is consumed still establishes a new generation and displays its first random-access frame |
| Output watchdog | Decoder accepts input but emits no output; identity memory/age remains bounded and the generation resets |
| PTS identity | Induced CUVID and software output delay preserves identity; missing/unknown PTS fails through the defined path |
| Priming timeout | Recovery IDR never appears at output; frame/time bounds terminate priming cleanly |
| Random access | Header keyframe flag, actual NAL type, and required parameter sets must agree for both codecs |
| Repeated oversize | Repeated oversized first IDRs renegotiate/reconfigure or terminate; they never reset-loop |
| UDP duplicate | Late/duplicate old frames are counted and discarded without a second discontinuity |
| UDP full queue | A full data queue can still purge stale dependents and linearize exactly one barrier |
| Stale input | Old-session mouse/button/scroll datagrams cannot act after disconnect cleanup |
| Stale cursor | Old-session cursor datagrams and restarted sequences cannot replace the new epoch's state |
| Clock processing delay | Injected responder delay is removed by t1/t2/t3/t4 math and reflected in uncertainty |
| Suspend clock | Sleep/wake invalidates and recalibrates the host clock bridge before timestamps are compared |
| Codec/config | Capability matrix includes profile/level/bit depth/mode; encoder creation matches the confirmed config |
| Presentation gaps | Intentional latest-wins render drops do not stall credit or falsely claim contiguous presentation |
| Cursor wrap | Equal, forward, wrap-forward, stale, and half-range-ambiguous u24 sequences behave as specified |

## Conclusion

v4 is a substantial improvement and preserves the right architecture for the
fixed wired-LAN use case. I agree with its major decisions.

I do **not** agree that §3 and §4 are frozen yet. The remaining issues are not a
reason to return to UDP or abandon decoder-accepted credit; they are the final
contracts needed to implement those choices safely:

- one authoritative generation handshake;
- new-generation recovery for TCP decoder faults;
- exact progress/byte-prefix semantics;
- a truthful bounded-frame policy;
- identity and output-lag bounds before priming depends on them;
- session fencing for all side channels; and
- implementable clock and codec negotiation paths.

After those changes are made on paper, the plan is ready for A0.0/A0 and then a
contract-first B0 spike. Until then, implementing the current reset/credit
prose would risk replacing the original latency glitch with reconnect
deadlocks, reset loops, or stale-session input.
