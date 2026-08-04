# Implementation Plan V6 Review — Personal Deployment

Reviewed document: `IMPLEMENTATION_PLAN_V6.md`  
Review basis: plan review plus current Swift, Rust, protobuf, launch, and diagnostic code  
Deployment assumption: one user, one known Mac, one known Ubuntu client, updated together  
Verdict: **V6 closes most V5 contract defects, but should not be frozen or implemented unchanged**

## Executive verdict

V6 is technically much better than V5. It resolves nearly all of the concrete V5 review findings: protobuf assignments, reset idempotence, ACK timing, oversize scoping, signed FFmpeg tokens, random-access reseeding, reliable button delivery, encoder-before-confirm ordering, and signed clock arithmetic.

The new requirement changes the appropriate architecture, however. V6 still designs a reusable multi-mode, dual-stack, backward-aware protocol with capability probing, adaptive limits, two generations of video transport, and numerous recovery sub-states. This is unnecessary for a fixed Mac–Ubuntu pair and increases both implementation time and the number of failure modes a future agent must understand.

At the same time, V6 does **not** define the operational diagnostics needed when a private macOS interface, VideoToolbox property, FFmpeg/CUDA decoder, SDL behavior, or system package changes.

The right conclusion is:

| Scope | Decision |
|---|---|
| A0.0 tracing, token experiment, and test harnesses | **Go now**, with diagnostics added |
| A0 measurement | **Go after A0.0 trace/clock checks pass** |
| V6 §§2–10 as a frozen reusable protocol | **No-go** |
| A1 interim UDP-video recovery work | **Skip** |
| A1/B0 implementation | **Go after a short personal-profile simplification amendment** |
| Final architecture | **Implement fixed-profile TCP video directly** |

This is not a request for another large architecture rewrite. It is a request to remove unneeded branches and add evidence-rich failure reporting.

## Actual deployment profile already visible in the repository

The checked-in launchers already describe a single concrete deployment:

- Mac host address: `192.168.50.125`
- Ubuntu client address: `192.168.50.47`
- Virtual display/stream: `1080 × 1920 @ 60 Hz`
- Primary codec: HEVC
- Control port: `9870`
- Ubuntu display index: current default `0`

Evidence:

- `mac-host/launch.sh:5-11`
- `ubuntu-client/launch.sh:5-9`
- `mac-host/RESC-Host.app/Contents/MacOS/resc-host-launcher:4-10`

These values should be centralized into one named `PersonalProfile`, rather than repeated across launch scripts, CLI defaults, `AppConfig`, protocol constants, and negotiation code. Hard-coding is appropriate here; scattering hard-coded values is not.

The profile should contain:

- `profile_id` and a stable hash
- exact IPv4 peers and ports
- exact display width, height, orientation, refresh rate, and SDL display index
- HEVC Main, 8-bit, no frame reordering
- fixed bitrate chosen from A0
- fixed maximum access-unit size
- fixed application flow-control window
- expected Ubuntu decoder backend
- protocol version and build compatibility rule

Both endpoints should print this profile at startup and reject a different profile hash before capture or input injection begins.

## What V6 correctly fixes

The following changes should be retained:

- Exact new protobuf tags and validation rules.
- A nonzero/unspecified transport enum default.
- ACK publication only after exact-once decoder acceptance and drain to `Again`.
- Resume of partial TCP writes instead of treating them as failure.
- Fixed memory bounds and a bounded writer.
- A hard encoded-record size ceiling.
- Decoder send/receive outcome classification and retry-once behavior.
- Random-access NAL verification on both encoder output and client input.
- Encoder construction, property validation, and prepare-before-confirm ordering.
- Button transitions on reliable control rather than UDP.
- Release/fencing of stale video and decoder state on lifecycle changes.
- One SDL/render owner and bounded latest-frame handoff.
- Signed clock arithmetic and bracketed macOS clock calibration for trace mode.

These mechanisms address real correctness risks on this one deployment. They are not multi-user features.

## Remaining correctness defects in V6

### 1. Decoder progress mixes incompatible units

V6 says tokens are monotonic across generations while also saying they are scoped to a decoder instance. Section 2 disposes the decoder on generation replacement, so these statements cannot all be true.

`decoded_retired_through_count` is then described as a token frontier, while `accepted_frame_count` and the inherited watchdog are generation-local frame counts. Subtracting or comparing these values is invalid.

References:

- `IMPLEMENTATION_PLAN_V6.md:40`
- `IMPLEMENTATION_PLAN_V6.md:65-71`
- `IMPLEMENTATION_PLAN_V6.md:128-134`
- `IMPLEMENTATION_PLAN_V5.md:119`

For the personal profile, use generation/session-local decoder ordinals beginning at one. If the final flow window is one frame, decoder retirement does not need to be a wire-level credit field at all.

V6 also claims that emission of a higher token proves a lower token was silently skipped. That is only valid when output ordering is a read-back-and-tested hard invariant. Encoder `AllowFrameReordering = false`, decoder low-delay configuration, and the actual output order must all be validated before making this inference. Otherwise the lower output may legally arrive later.

The simpler rule is:

- Frame identity is session-local and ordered.
- No B-frames/reordering is allowed in the fixed profile.
- A0.0 proves FIFO/token behavior on the selected decoder backend.
- Missing output is handled by a bounded watchdog, not speculative “proven skip” arithmetic.

### 2. The retry budget can still loop forever

The consecutive-failure counter resets after one ACK-accepted frame (`IMPLEMENTATION_PLAN_V6.md:42`). A failure pattern that streams one frame and then resets can therefore repeat forever without reaching eight consecutive failures.

Use a hard rolling/session restart budget instead, for example:

- at most eight video/session restarts per process run, or
- at most five restarts in 60 seconds,
- with a reset only after a genuinely stable interval such as 30 seconds.

For personal software, failing loudly after a small bounded number of attempts is preferable to an elaborate self-healing loop that hides the root cause.

### 3. Oversize/config transitions remain ambiguous

The second oversize step changes bitrate and recreates the encoder, but V6 does not say whether this sends a new `ModeConfirm`, increments `config_id`, or retains the existing configuration. If it creates a new config, a “session + config” streak can reset and recreate the original loop.

The first frame above the hard ceiling is also ambiguous: it is not clear whether the streak becomes one or jumps directly to the bitrate-reduced state, and “already reduced” is not represented explicitly.

References:

- `IMPLEMENTATION_PLAN_V6.md:48`
- `IMPLEMENTATION_PLAN_V6.md:116-122`

The personal profile should remove the adaptive ladder:

- one measured fixed bitrate
- one fixed frame cap
- one fixed flow window
- cap violation ⇒ close the stream/session and emit a structured fatal diagnostic containing actual size, cap, codec properties, frame type, and recent encoder statistics

A future agent can then adjust one profile constant from evidence instead of debugging a hidden automatic fallback.

### 4. Cap/window validation is incomplete

If V6’s dynamic negotiation is retained, the client must validate:

`0 < generationCap <= advertisedCeiling <= localHardCeiling`

and:

`generationCap <= creditWindow <= localWindowCeiling`.

The first `StartVideoGeneration` must also agree with the values in `ModeConfirm`. A remote value must never directly determine allocation size without a local ceiling.

For the recommended fixed profile this becomes simpler: both sides compile/use the same cap and window, exchange the profile hash, and reject any mismatch.

### 5. Reliable input still needs disconnect cleanup

Moving button events to TCP solves UDP loss and reordering, but TCP can still fail after a down event and before its matching up event.

The host must call `releaseAll()` on:

- control disconnect
- session replacement
- input watchdog expiry
- fatal protocol error
- injector teardown
- process shutdown

The action and released key/button counts must be logged.

Current evidence shows the gap:

- `PressedKeyState.releaseAll()` exists at `mac-host/Sources/RemoteDisplayHost/PressedKeyState.swift:32`.
- `HostSession.onReleaseInput` is declared at `mac-host/Sources/RemoteDisplayHost/HostSession.swift:37`.
- There is currently no call site connecting them.

`ButtonEvent.seq` also has no sender/receiver rule in V6. A serialized TCP control stream neither duplicates nor reorders successfully delivered messages, so remove this field unless the application explicitly retries button messages.

### 6. The claimed freeze artifact is not standalone

V6 says it supersedes V5, but essential behavior is still defined as “unchanged from V5/V4”:

- happy-path socket ordering
- complete video record layouts
- credit and queue invariants
- decoder outcome details
- cursor body bytes
- NAL/random-access rules
- phase contents

Examples appear at `IMPLEMENTATION_PLAN_V6.md:28`, `:101-106`, `:124`, `:134`, `:161`, and `:179`.

A future agent should not need to merge V4, V5, V6, and multiple reviews mentally. Produce one small normative personal-deployment contract. Historical plans can remain as rationale, but only one document should define current behavior.

### 7. The A1 hybrid cannot be implemented as currently described

V6 makes protocol v2 TCP-video-only, while A1 retains v1 UDP video but introduces v2 side-channel epochs and `ButtonEvent`. It does not define which control protocol carries those v2 fields during the hybrid.

The current Swift control path also hand-encodes protobuf tags through `UInt8`. Envelope fields 33–41 exceed one-byte tag values and cannot be emitted safely:

- `mac-host/Sources/RemoteDisplayHost/HostSession.swift:292-311`

Although `SwiftProtobuf` is linked, the package has no generated protocol target and `HostSession` still manually scans/emits protobuf:

- `mac-host/Package.swift:32-39`
- `mac-host/Sources/RemoteDisplayHost/HostSession.swift:109-159`

Make generated Swift/Rust protobuf plus typed dispatch a hard prerequisite. More importantly, skip the transitional UDP-video A1 work and move directly to the final TCP path.

## Recommended smallest reliable architecture

### 1. One fixed profile, exact-match handshake

Keep only:

- one protocol version
- one profile hash
- one build/commit identifier per endpoint
- one random session/run ID

The client and host are updated together. Version/profile mismatch is a fatal, well-logged error. No backward-compatibility matrix or opaque forward extensions are needed.

Remove:

- `CodecCapability`
- `SupportedMode`
- candidate probing/selection
- multi-mode negotiation
- IPv6/scope handling for this IPv4 deployment
- mDNS discovery
- resume/grace-period semantics
- automatic codec switching
- pv1 compatibility after the baseline is recorded

If peer authentication is required, pin one certificate/public-key fingerprint or use one pre-shared key. A fixed IP is not authentication.

### 2. Separate control and video TCP sockets

Retain separate sockets so video cannot head-of-line block input/control.

Use:

- fixed control listener: port `9870`
- fixed video listener: retain `9871` unless A0 shows a reason to change it
- fixed IPv4 peers from the personal profile
- one session ID/profile hash in the video hello

On any video, decoder, framing, or protocol failure:

1. release all input state
2. close both video and control sessions
3. dispose decoder, encoder, queues, and render slot
4. reconnect after bounded backoff

This replaces the multi-generation reset protocol with:

`Disconnected → Connecting → AwaitingRA → Streaming → Backoff → Failed`.

It is acceptable for this personal tool to pause briefly during full reconnection. The simpler lifecycle is easier to test and diagnose.

### 3. Fixed stop-and-wait video flow control

Do not rely on TCP flow control alone; the original latency problem requires an application-level bound.

Start with:

- fixed maximum record size from A0
- at most **one outstanding encoded frame**
- one latest-capture slot while waiting
- `FrameAck(session_id, frame_id)` only after exact-once decoder acceptance and drain to `Again`
- no byte-prefix ledger
- no dynamic credit window
- no adaptive frame-cap or bitrate ladder

This provides a strict memory/age bound with far less machinery. If A0 proves that one outstanding frame cannot sustain 60 Hz, raise the fixed window to two and test it explicitly; do not add general dynamic negotiation.

The first frame of every session must still be a client-validated random-access unit with required parameter sets.

### 4. No interim UDP video

Section 7 is throwaway work because the target transport is TCP. Preserve the current UDP path only long enough to record A0 baseline data, then implement TCP and delete UDP video.

Do not implement:

- `AwaitingRA` for a soon-to-be-deleted UDP video path
- recovery epochs for UDP video
- generalized UDP video comparators
- a second queue/purge architecture solely for the interim

### 5. Simplify input channels

Use:

- control TCP: keyboard, buttons, scroll, grab/release
- UDP: mouse movement and cursor updates only, latest-wins

Moving scroll to control eliminates sequence-dedup and loss semantics for a discrete user action. Keep one tested `u32 isNewer` helper for the two lossy latest-state streams.

## Mandatory diagnostics and upgradeability contract

This section is missing from V6 and is required before implementation.

### Persistent structured logs

Write bounded JSONL logs locally:

- macOS: `~/Library/Logs/RESC/host.jsonl`
- Ubuntu: `~/.local/state/resc/client.jsonl`

Use stable event/error codes. Every record should include:

- local monotonic timestamp
- wall-clock timestamp
- side/component
- run/session ID
- profile hash
- state before/after
- frame ID when applicable
- result
- native error domain
- native numeric code
- native text
- relevant expected/actual values

Log lifecycle transitions and failures, not every packet. Emit a 10–30 second aggregate plus a final summary.

### Startup environment record

Each endpoint must record:

- application commit/build and whether the tree was dirty
- protocol version and full effective personal profile
- OS release/build and CPU architecture
- peer addresses and ports
- selected display/mode/codec/bitrate/frame cap
- macOS ScreenCaptureKit and VideoToolbox availability
- Ubuntu kernel, FFmpeg/libavcodec, SDL, CUDA/NVIDIA driver, and selected decoder backend versions

At review time, the Mac is already running macOS `27.0`, build `26A5388g`, while `VirtualDisplayManager.allowedBuilds` does not contain that build. The current behavior is only to warn and proceed:

- `mac-host/Sources/RemoteDisplayHost/VirtualDisplayManager.swift:34-44`
- `mac-host/Sources/RemoteDisplayHost/main.swift:66-78`

That makes runtime diagnostics more valuable than a growing OS allowlist.

### Native API failure evidence

Every load-bearing native call must check and log its result:

- private `CGVirtualDisplay` class, selector, and expected method signature
- ScreenCaptureKit permission, configuration, output pixel format, clock, and start error
- every required `VTSessionSetProperty`
- requested-versus-read-back VideoToolbox values
- `VTCompressionSessionPrepareToEncodeFrames`
- FFmpeg decoder discovery/creation and hardware-device setup
- FFmpeg `send_packet`/`receive_frame` errors using `av_strerror`
- SDL initialization, renderer, texture format, and update failures
- socket bind/connect/read/write failures with `errno`

The current encoder ignores every property-set and prepare status at `mac-host/Sources/RemoteDisplayHost/VideoEncoder.swift:94-131`; V6 says to validate properties but does not define the diagnostic payload.

Automatic fallback must never be silent. If a manual H.264/software safe mode is retained, the log must state why the expected HEVC/CUVID profile failed and which fallback was activated.

### Post-upgrade self-test

Replace the shallow class/framework smoke check with:

- `remote-display-host --doctor`
- `remote-display-client --doctor`
- optional `--diagnose-peer`

The Mac doctor should actually create/destroy the fixed virtual display, create the fixed VideoToolbox encoder, set/read all required properties, encode a bundled test frame, and verify its random-access NALs.

The Ubuntu doctor should open the selected decoder, decode the bundled random-access sample, verify token/FIFO behavior, create the required SDL texture format, and report input capability.

The peer test should exchange version/profile/build IDs, one test frame, one ACK, and correlated logs without enabling input injection.

On failure, print one human-readable summary and the path to the machine-readable report.

The existing `smoke_test.swift` checks only class/framework existence and display enumeration (`smoke_test.swift:23-76`); it does not prove the private selectors, encoder properties, actual encode, or decoder path still work.

### Reproducible dependency state

- Commit `ubuntu-client/Cargo.lock`; it is currently absent.
- Keep `mac-host/Package.resolved` committed.
- Record runtime system-library versions in the doctor report.
- Prefer exact tested FFmpeg crate versions over the current broad `ffmpeg-next = "7"` and `ffmpeg-sys-next = "7"` constraints.

This lets a future agent distinguish a source regression from a dependency or OS upgrade.

## Revised phase boundary

### A0.0 — go now

Add first:

- generated protobuf and typed handlers
- effective-profile/startup logging
- persistent JSONL lifecycle/error logs
- improved host/client doctor probes
- trace joining and selected-decoder token/FIFO experiment

### A0 — go after A0.0

Measure the actual fixed profile:

- HEVC access-unit histogram
- end-to-end latency
- one-frame stop-and-wait throughput
- encoder/decoder time
- fixed frame-cap margin

Commit the resulting bitrate, cap, and window to the profile.

### Next implementation phase

Implement the fixed-profile TCP path directly. Do not implement V6 §7 or generalized capability negotiation.

Entry gates:

- profile constants and hash frozen
- generated wire fixtures pass in Swift and Rust
- selected encoder/decoder self-tests pass
- one-frame window sustains the required rate or a fixed window of two is justified by measurement
- disconnect tests always release all keys/buttons
- native failure injection produces actionable diagnostics

## Final recommendation

V6 is a good resolution of the V5 review when judged as a reusable protocol design. It is not the right implementation plan for this personal deployment.

Publish a short personal-profile amendment, keep the load-bearing latency and decoder invariants, remove negotiation/backward-compatibility/interim-UDP machinery, and make diagnostics a first-class deliverable. Then proceed directly to the TCP implementation.
