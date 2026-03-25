# Remote Extended Screen — Implementation Plan (v5)

## Context

Build software that lets a Mac use a physical monitor connected to a nearby Ubuntu machine as an extended display — no physical cable between Mac and Monitor B. The Mac ends up with 3 screens: built-in + wired Monitor A + virtual Monitor B (streamed to Ubuntu). The virtual display appears in macOS System Settings > Displays and is arrangeable. When using Mac input, the cursor enters the virtual display naturally (standard multi-monitor behavior). When using Ubuntu input, the user grabs control via a hotkey and interacts with the virtual display through the Ubuntu machine.

## Architecture

```
        macOS (Host)                        LAN                      Ubuntu (Slave)
┌──────────────────────────┐                                ┌──────────────────────────┐
│ CGVirtualDisplay         │                                │                          │
│ (creates OS-level display│    Encoded video (custom UDP)  │  Decode (ffmpeg VAAPI    │
│  in System Settings)     │  ────────────────────────────► │  or H.264 SW fallback)   │
│         ↓                │                                │         ↓                │
│ ScreenCaptureKit         │                                │  SDL2 Fullscreen Render  │
│ (showsCursor: FALSE,     │    Cursor updates (UDP)        │  + local cursor rendering│
│  NV12 GPU frames)        │  ────────────────────────────► │                          │
│         ↓                │                                │         ↓                │
│ Latest-frame slot ──────►│    Mouse/scroll (UDP,          │  Input Capture            │
│         ↓                │    latest-seq-wins)            │  (SDL2 on Xorg)          │
│ Encoder thread           │  ◄──────────────────────────── │                          │
│ (VideoToolbox H.264)     │    Keys (TCP, reliable)        │                          │
│         ↓                │  ◄──────────────────────────── │                          │
│ CGEvent Injection        │                                │                          │
│ (StreamSpace → global    │    Control (TCP+TLS)           │                          │
│  via live CGDisplayBounds│  ◄────────────────────────────►│                          │
│  at injection time)      │                                │                          │
│         ↓                │    mDNS Discovery              │                          │
│ Mouse move coalescer     │  ◄────────────────────────────►│                          │
│ (cap 240Hz, drop stale)  │                                │                          │
└──────────────────────────┘                                └──────────────────────────┘
```

## State Machines

### Session State Machine
```
Idle → Discovered → Pairing → Paired → Negotiating → Streaming → Disconnected
                                                                       ↓
                                                         (grace period: 30-120s)
                                                                       ↓
                                                          reconnect → Negotiating
                                                          timeout   → Idle (destroy display)
```

**Disconnect grace period**: On network loss, keep the virtual display alive for 30-120s (configurable). Windows stay in place. Only destroy the display on explicit user stop or grace period expiry. This prevents window rearrangement thrashing on transient network blips.

**During grace period**:
- Ubuntu: force-release input grab (if RemoteControlGrabbed → transition to RemoteControlReleased)
- Ubuntu: show "Disconnected — reconnecting..." overlay. Do not inject input.
- Ubuntu: render last decoded frame (frozen) or a "disconnected" placeholder
- Host: trigger `PressedKeyState` reset (release all modifiers + buttons)
- Host: stop encoding (save CPU), but keep virtual display alive

**On reconnect**: Always start in **LocalControl/Released** — never auto-resume RemoteControlGrabbed. User must explicitly grab again. This prevents surprising "mouse suddenly hijacked" after network returns.

### Input Ownership State Machine
```
LocalControl ←──grab hotkey──→ RemoteControlGrabbed
                                       ↓ (release hotkey: Ctrl+Alt+Escape)
                               RemoteControlReleased
                                       ↓ (grab hotkey again)
                               RemoteControlGrabbed
```

- **LocalControl**: Mac local input drives cursor. Ubuntu input is **ignored**. Stream still flows (Ubuntu shows display content as a passive viewer). Cursor updates come from **host** (event-driven polling).
- **RemoteControlGrabbed**: Ubuntu input drives cursor. Ubuntu renders its own cursor position **locally and immediately** (authoritative client pointer). Host cursor updates are **ignored/suppressed** to avoid feedback loop. On-screen overlay: "Remote Control Active — Ctrl+Alt+Escape to release".
- **RemoteControlReleased**: Stream continues. Ubuntu input not injected. SDL2 releases grab. Treat cursor like LocalControl (host-driven).

**Dual-input**: When in `RemoteControlGrabbed`, if Mac local cursor enters the virtual display area, the host does NOT suppress it (too fragile). Both inputs can technically coexist — last event wins. Acceptable for a personal tool.

## Display Identity & Rebinding

The virtual display's `CGDirectDisplayID` can change across sleep/wake or display reconfiguration. Reliable identification is critical for capture targeting and input coordinate mapping.

### VirtualDisplayHandle
```swift
struct VirtualDisplayHandle {
    let creationToken: UUID          // set at creation time
    var lastKnownDisplayID: CGDirectDisplayID
    let expectedMode: (width: Int, height: Int, refreshRate: Double)
    let vendorID: UInt32             // set in CGVirtualDisplayDescriptor
    let productID: UInt32
    let serialNum: UInt32
}
```

### resolveDisplayID() — layered fallback
On any display reconfiguration event (`CGDisplayRegisterReconfigurationCallback`):
1. Check if `lastKnownDisplayID` is still active (`CGDisplayIsActive`) → if yes, done
2. **Layer 1**: Enumerate all displays, match by `vendorID + productID + serialNum` via `CGDisplayVendorNumber/ModelNumber/SerialNumber`
3. **Layer 2**: If Layer 1 returns 0/generic (common for virtual displays), match by display name + expected pixel size + "recently created display"
4. **Layer 3**: If still ambiguous, match by "the only new display since creation time T"
5. **Layer 4**: If multiple candidates, pick most likely + log loudly for debugging
6. If no match → virtual display was destroyed unexpectedly; attempt re-creation or enter degraded mode

**MVP constraint**: Only one virtual display at a time. If multiple exist, this is an error state.

**Observability**: Log the resolved identity (displayID + vendor/model/serial + pixel size + name) at creation time AND after every rebinding. If Layer 3/4 heuristics are used, display a warning banner: "Display rebinding used heuristic match — verify correct display targeted." This prevents silent wrong-target capture/injection bugs.

### Sleep/wake handling
- Register for `NSWorkspace.willSleepNotification` / `didWakeNotification`
- On wake: run `resolveDisplayID()`, verify capture is still delivering frames
- If display lost: re-create with same parameters, re-bind SCK capture

## Capture Contract

### Target vs actual FPS
- Request 60fps via `minimumFrameInterval`; actual delivery depends on macOS compositor pacing and virtual display activity
- Track actual FPS via callback cadence. Log if sustained < 50fps.

### Pixel format
- Preferred: NV12 (`kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange`) — native VideoToolbox input
- Fallback: if SCK delivers BGRA, convert on encoder thread (adds ~1-2ms per frame). Do not block capture callback.

### Session Timebase
Define a single monotonic clock origin on the host: `t0 = mach_absolute_time()` at session start. All timestamps in the protocol are `microseconds since t0`:
- **Video**: SCK `CMSampleBuffer` presentation timestamps converted to session timebase
- **Cursor updates**: `timestamp_us` in same timebase (host-originated)
- **Input events**: `timestamp_us` is **optional/debug-only** in MVP. Host uses **arrival time** for ordering; `seq` field is authoritative for latest-wins. No client-host clock synchronization in MVP.

This ensures video and cursor timestamps are directly comparable (both host-originated). Input ordering relies on `seq`, not timestamps. Clock sync (RTT/2 offset estimation) deferred to post-MVP.

### Backpressure
- Capture callback MUST return immediately (writes to AtomicLatestFrame slot, signals semaphore, returns)
- Never block, never allocate, never do heavy work in the callback

**Key reset on disconnect**: On any control/input channel disconnect, host immediately:
- Injects key-up for all currently-pressed modifiers (Shift, Ctrl, Opt, Cmd)
- Injects mouse-up for any pressed buttons (cancels any in-progress drag)
- Clears internal pressed-key map and drag state
- Host maintains a `PressedKeyState` tracker for this purpose
- Also triggered on reconnect (clear stale state before new session begins)

## Cursor Strategy: Receiver-Side Rendered, Ownership-Aware

Host capture uses `showsCursor: false`. Cursor is rendered locally on Ubuntu.

### Cursor Source Policy (per ownership state)

| State | Cursor source | Behavior |
|-------|--------------|----------|
| **LocalControl** | Host sends `CursorUpdate` over UDP (event-driven, ~120Hz when cursor is on virtual display) | Ubuntu renders cursor at host-reported position |
| **RemoteControlGrabbed** | **Client is authoritative**. Ubuntu renders cursor at its own last-sent input position. Host cursor updates are ignored. | Zero-lag local cursor; no feedback loop |
| **RemoteControlReleased** | Same as LocalControl | Host-driven cursor |

### Why no feedback loop
In `RemoteControlGrabbed`, the naive approach would be: Ubuntu sends mouse → host injects → macOS moves cursor → host polls → sends back to Ubuntu → Ubuntu renders. This loop adds delay and jitter.

Instead: Ubuntu tracks its own cursor position locally and renders immediately. The host injects at the same coordinates but Ubuntu does not wait for host confirmation.

### MVP Cursor Set (no PNG streaming)
Do **not** stream cursor bitmaps in MVP. Instead:
- Define a standard set of cursor shape IDs: `Arrow`, `IBeam`, `Crosshair`, `OpenHand`, `ClosedHand`, `PointingHand`, `ResizeN`, `ResizeS`, `ResizeE`, `ResizeW`, `ResizeNS`, `ResizeEW`, `ResizeNESW`, `ResizeNWSE`, `NotAllowed`, `Wait`
- Host sends `shape_id` + `hotspot_x` + `hotspot_y` + `scale` when cursor shape changes
- Ubuntu has a built-in sprite for each shape ID
- App-specific custom cursors degrade to `Arrow` — acceptable for MVP

### CursorUpdate message
```
CursorUpdate {
  seq: u32              // monotonic sequence number (authoritative for latest-seq-wins)
  timestamp_us: u64     // microseconds since session start (debug/telemetry only in MVP; no smoothing/interpolation)
  x_px: i32             // StreamSpace pixels (signed for edge cases)
  y_px: i32             // StreamSpace pixels
  shape_id: u8          // from standard set (Arrow=0, IBeam=1, ...)
  hotspot_x_px: u16     // pixels from top-left of cursor sprite
  hotspot_y_px: u16     // (u16 supports large accessibility cursors)
  cursor_scale: f32     // for Retina-scaled cursor sprites
}
```

**CursorUpdate binary encoding** (exactly 29 bytes after PacketPrefix, field order is canonical):
```
CursorUpdate (29 bytes, after PacketPrefix):
  seq:            u32    // bytes 0-3
  timestamp_us:   u64    // bytes 4-11
  x_px:           i32    // bytes 12-15
  y_px:           i32    // bytes 16-19
  shape_id:       u8     // byte 20
  hotspot_x_px:   u16    // bytes 21-22
  hotspot_y_px:   u16    // bytes 23-24
  cursor_scale:   f32    // bytes 25-28 (IEEE 754 little-endian)
```
Total cursor packet: PacketPrefix(6) + CursorUpdate(29) = **35 bytes**. No padding, no alignment gaps. Implementations must serialize fields in this exact order.

Sent over the cursor UDP sideband. In LocalControl/RemoteControlReleased, sourced from host. In RemoteControlGrabbed, not sent (client is authoritative; Ubuntu uses its own tracked position).

**Clamp behavior**: Receiver clamps cursor rendering to viewport bounds `[0..stream_width-1, 0..stream_height-1]` even if negative or out-of-bounds values arrive. Host clamps injected coordinates in StreamSpace before mapping to global (safety against malformed input).

## Protocol Design

### Shared definitions in `proto/` directory
All message types defined once in Protobuf `.proto` files, with codegen for both Swift (`swift-protobuf`) and Rust (`prost`). Single source of truth prevents divergence.

```
proto/
├── control.proto     # Session management, pairing, mode negotiation, stats
├── video.proto       # Video frame/chunk headers
├── input.proto       # Mouse, keyboard, scroll events
└── cursor.proto      # Cursor position + shape updates
```

**Build tooling**: `tools/` directory with a `generate_proto.sh` script that invokes `protoc` with both Swift and Rust plugins. Pinned `protoc` version (e.g., 27.x). Generated sources committed to repo (avoids protoc as a build-time dependency for contributors).

**control.proto design principles**:
- `Envelope` wraps all control messages with `session_id` + `protocol_version` + `oneof payload`
- Field numbering: pairing=10-19, mode negotiation=20-29, runtime=30-39, input=40-49, lifecycle=50-59 (room for growth)
- No floats for protocol values — use `uint32` millihertz for refresh rate, `uint32` bps for bitrate
- `Stats` uses `float` only for rates (0.0-1.0) which are inherently approximate
- Protobuf `bool` is fine in .proto (well-defined wire format); binary UDP structs use `u8` instead
- `uint32` for ports (protobuf has no u16; clamp 0-65535 in code)
- `KeyEvent` on TCP (reliable); mouse/scroll/cursor on UDP (lossy OK)

### Transport Channels

| Channel | Protocol | Content | Reliability |
|---------|----------|---------|-------------|
| Video | Custom UDP | Encoded frames (chunked) | Unreliable; latest-frame-wins; IDR on loss |
| Mouse + Scroll input | UDP | High-freq position/delta events | Unreliable; latest-seq-wins; lossy is fine |
| Keyboard input | TCP | Key down/up events | Reliable; prevents stuck keys |
| Cursor sideband | UDP | Cursor position + shape (LocalControl only) | Unreliable; latest-seq-wins |
| Control | TCP + TLS | Session mgmt, pairing, mode negotiation, IDR requests, stats | Reliable, encrypted |

**Control channel framing**: Messages are framed as `u32_le length` (4 bytes, little-endian) followed by `length` bytes of Protobuf-encoded `Envelope` message. Both Swift and Rust implementations must use this exact framing.

**Control channel message model**: Single framed TCP stream carrying an `Envelope` message with `oneof payload`. One parser, easy logging, trivially extensible.

**Session ID**: The `Envelope` carries `session_id: u64` (random, generated by host). Set to 0 by client before `ModeConfirm` is received; host assigns in `ModeConfirm` and both sides use it thereafter. Persists across reconnects within the same grace period, enabling log correlation.

**Who sends what** (prevents invalid state transitions):
- **Client → Host**: `PairRequest`, `ModeRequest`, `StreamingReady`, `Stats`, `RequestIDR`, `KeyEvent`
- **Host → Client**: `PairResponse`, `ModeConfirm`, `ModeReject`, `StartStreaming`, `StopStreaming`
- Receiving a message from the wrong direction is a protocol error → log + ignore (do not crash).

### Custom UDP Video Framing

### Serialization Split
- **Control channel (TCP+TLS)**: Protobuf messages (session, pairing, mode negotiation, stats, StartStreaming)
- **UDP channels**: Fixed **binary framing** (NOT Protobuf) — simple packed structs for speed, zero allocations, predictable size
- **Separate UDP ports per channel** (video, cursor, input) — allocated during negotiation via `ModeConfirm`. `packet_type` in the prefix is for **validation only** (reject misrouted packets):
  - Video port accepts only `packet_type=0` — drop + count any other value
  - Cursor port accepts only `packet_type=1` — drop + count any other value
  - Input port accepts only `packet_type=2` — drop + count any other value
  - Mismatched `packet_type` increments a `misrouted_packets` counter in stats (aids debugging cross-wiring bugs).

**All UDP packets use little-endian byte order.** All fields are explicitly sized — **do not serialize by dumping in-memory structs; write/read fields explicitly** (avoids cross-language layout bugs with padding, alignment, and bool representation). Booleans are represented as `u8` (0 or 1). Each packet starts with a common prefix:
```
PacketPrefix (exactly 6 bytes, all UDP packets):
  magic:        [u8; 4]  // b"RESC" (Remote Extended SCreen)
  version:      u8       // protocol version (1 for MVP)
  packet_type:  u8       // 0=video_chunk, 1=cursor_update, 2=input_event (for validation)
```

**Version handling (UDP)**: If received `version != SUPPORTED_VERSION` (1 in MVP), **drop packet** and increment `unsupported_version_packets` counter. No backward/forward compat in MVP — strict equality.

**Version handling (TCP control channel)**: Include `protocol_version: u32` in `ModeRequest` (client→host). Host checks compatibility in `ModeConfirm`:
- If mismatch → host sends `ModeReject { reason: "incompatible protocol version" }` and closes connection.
- This surfaces version errors early with a clear message, rather than silent packet drops.

**Protocol version constant**: `PROTOCOL_VERSION = 1` (single source of truth, used in both UDP `version` field and TCP `protocol_version` field).
### Protocol Constants (v1)
```
PROTOCOL_VERSION              = 1
MAX_DATAGRAM_BYTES            = 1400   // total UDP packet size cap (avoids IP fragmentation)

// Derived from struct definitions — do NOT hardcode separately.
// Single source of truth: the struct byte layouts below.
PACKET_PREFIX_BYTES           = 6      // magic(4) + version(1) + packet_type(1)
VIDEO_CHUNK_HEADER_BYTES      = 32     // per-packet(12) + per-frame(20) — see VideoChunkHeader
VIDEO_TOTAL_HEADER_BYTES      = 38     // PACKET_PREFIX_BYTES + VIDEO_CHUNK_HEADER_BYTES
MAX_VIDEO_PAYLOAD_BYTES       = 1362   // MAX_DATAGRAM_BYTES - VIDEO_TOTAL_HEADER_BYTES
CURSOR_UPDATE_BYTES           = 29     // see CursorUpdate binary layout
CURSOR_TOTAL_PACKET_BYTES     = 35     // PACKET_PREFIX_BYTES + CURSOR_UPDATE_BYTES
INPUT_EVENT_BYTES             = TBD    // defined in Phase 6
```
These are **spec constants** — both implementations must use the same values. In code, derive `MAX_VIDEO_PAYLOAD_BYTES` from the other constants (not hardcoded independently) to prevent drift when fields are added in future versions.

**Unified VideoChunkHeader** — every video UDP packet uses this header (after PacketPrefix). Fields marked "(chunk_id==0 only)" are present in all packets for fixed-size parsing but only meaningful in the first chunk:
```
VideoChunkHeader (after PacketPrefix, all video packets):
  // === Per-packet fields (always valid) ===
  stream_id:    u32    // random per negotiation; receiver drops mismatched
  config_id:    u32    // increments on renegotiation; reject old config_id packets
  frame_id:     u32    // monotonic within stream
  chunk_id:     u16    // 0-indexed within frame
  chunk_size:   u16    // payload bytes in this packet

  // === Per-frame fields (valid when chunk_id==0; zero-filled otherwise) ===
  timestamp_us: u64    // microseconds since session start (SCK-sourced)
  is_keyframe:  u8     // 0=false, 1=true (NOT native bool — explicit u8)
  codec:        u8     // 0=H.264, 1=HEVC
  width:        u16
  height:       u16
  total_chunks: u16
  total_bytes:  u32    // total encoded frame PAYLOAD bytes (sum of all chunk payloads,
                       // excluding headers). Used for receiver buffer preallocation.

  // === Payload ===
  [payload: up to MAX_VIDEO_PAYLOAD_BYTES (1362) bytes]
```
Total header size: 6 (prefix) + 12 (per-packet) + 20 (per-frame) = 38 bytes. Max payload = 1400 - 38 = **1362 bytes per chunk**.

### Receiver Chunk Validation Rules (mandatory bounds checks)

**Out-of-order chunk handling**: Chunks may arrive before chunk 0 (which carries per-frame metadata). The receiver must accept out-of-order chunks. Per-frame fields (`total_chunks`, `total_bytes`, etc.) are zero-filled in non–chunk-0 packets — **receiver MUST ignore per-frame fields when `chunk_id != 0`** and MUST NOT treat zeros as real values.

**Checks applied to every video packet** (before storing payload):
1. If `chunk_size > MAX_VIDEO_PAYLOAD_BYTES` → **drop packet**
2. If `chunk_id >= max_total_chunks_per_frame` (from ModeConfirm) → **drop packet** (out of preallocated range)

**Checks applied only when chunk 0 arrives** (establishes frame metadata):
3. If `total_chunks == 0` → **drop frame** (invalid)
4. If `total_chunks > max_total_chunks_per_frame` from ModeConfirm → **drop entire frame**, count toward IDR hysteresis
5. If `total_bytes > max_frame_bytes` from ModeConfirm → **drop entire frame**, count toward IDR hysteresis

**Checks applied once frame metadata is known** (after chunk 0 received for this frame_id):
6. If `chunk_id >= total_chunks` for already-buffered chunks → **discard those chunks** (arrived before metadata, now known invalid)

This design tolerates UDP reordering while still enforcing bounds. All drops increment per-type counters in stats.

`stream_id` prevents stale packets from a previous session confusing the decoder. `config_id` is in **every packet** (not just chunk 0), so the receiver can immediately reject packets from an old config even if chunk 0 hasn't arrived yet or arrives out of order. This is critical for UDP reordering resilience during renegotiation.

**Renegotiation boundary rule**: Receiver only switches decoder config when a `ModeConfirm` with new `(stream_id, config_id)` is received over the **control channel** (TCP). On receiving new `ModeConfirm`:
1. Receiver **resets depacketizer state** (clear all in-flight frame assemblies)
2. Receiver **discards all UDP packets** with old `stream_id/config_id`
3. Receiver reinitializes decoder with new parameters
4. First frame accepted must be a keyframe with new IDs
This prevents "half-switched" corruption during mode changes.

**Receiver chunk loss policy**:
- Collect chunks per frame_id in jitter buffer
- If all chunks received → decode
- If newer frame_id arrives and current frame incomplete → drop incomplete
- **IDR request**: Sent via control channel (TCP) as:
  ```
  RequestIDR { stream_id: u32, config_id: u32, reason: enum(FrameLoss=0, DecodeError=1, ParameterSetLoss=2) }
  ```
  **Rate limit**: Host ignores RequestIDR if one was processed within the last 250ms (prevents IDR storms). Client-side hysteresis: only send if ≥3 frames dropped within 500ms, OR if decoder reports parameter set loss / decode error.
- Send SPS/PPS (H.264) periodically (~every 5s) to allow recovery without full IDR in some cases. (Post-MVP HEVC adds VPS/SPS/PPS.)
- **Receiver must tolerate repeated parameter sets** without resetting the decoder (treat as non-fatal no-op).

### Jitter Buffer: Placement and Caps

**Placement**: At the **frame-assembly level**, between chunk reception and decode.

```
UDP recv thread → chunk collection (per frame_id) → jitter buffer → decode thread
```

**Behavior**:
- Hold assembled frames for 0–1 frame period (0–16ms at 60Hz) based on observed inter-frame jitter
- Adaptive: if jitter is consistently < 2ms, hold time → 0 (pass-through). If jitter spikes, hold up to 1 frame.
- **Max in-flight frames tracked**: 4. Older frame_ids are dropped.
- **Max frame assembly timeout**: 30ms (assumes 60Hz; frame interval = 16.7ms, so 30ms allows ~1.8 frame periods). If not all chunks received within 30ms of first chunk → drop frame, count toward IDR hysteresis.
- **Decode queue cap**: 1 frame. If decoder is behind and a new frame is ready, drop the queued frame (latest wins).

### Mode Negotiation

```
Client → Host:  ModeRequest {
  protocol_version: u32             // must match host's PROTOCOL_VERSION; mismatch → ModeReject
  preferred_modes: [(width, height, refresh_rate_millihz)]  // ordered by preference
  rotation: 0 | 90 | 180 | 270
  supported_codecs: [H264, HEVC]
}

Host → Client:  ModeConfirm {
  session_id: u64          // random, long-lived across reconnects (log correlation)
  stream_id: u32          // random, matches VideoChunkHeader.stream_id
  config_id: u32          // matches VideoChunkHeader.config_id
  actual_width: u32
  actual_height: u32
  actual_refresh_rate_millihz: u32  // millihertz (e.g., 60000 = 60Hz). Integer avoids
                                    // cross-language float determinism issues.
  actual_rotation: u16
  stream_width: u32       // = actual pixel resolution
  stream_height: u32
  codec: enum Codec        // H264=0, HEVC=1. Always H264 in Phases 1-7.
  codec_profile: enum      // H264_BASELINE=0, H264_MAIN=1, H264_HIGH=2,
                           // HEVC_MAIN=10, HEVC_MAIN10=11 (extensible)
  codec_level_idc: u32     // H.264 level_idc convention: 41=Level 4.1, 51=Level 5.1
                           // Computed from resolution (NOT hardcoded).
  max_payload_bytes_per_chunk: u16  // e.g., 1362 (receiver pre-allocates based on this)
  max_total_chunks_per_frame: u16   // e.g., 256 (receiver allocates bitset + assembly buffer)
  max_frame_bytes: u32              // MUST be non-zero for 4K60 (prevents unbounded alloc);
                                    // 0 allowed only for 1080p. Recommended: 5× avg frame size.
  video_port: u16
  input_udp_port: u16
  cursor_udp_port: u16
}

Host → Client:  StartStreaming {
  stream_id: u32
  config_id: u32
}

Client → Host:  StreamingReady {
  stream_id: u32
  config_id: u32
}

// Handshake ordering:
// 1. Host sends ModeConfirm
// 2. Host sends StartStreaming
// 3. Client opens UDP sockets, initializes decoder, replies StreamingReady
// 4. Host begins sending UDP video ONLY after receiving StreamingReady
// This prevents the receiver missing the first keyframe and sitting black until next IDR.
```

Host attempts modes in client's preferred order. If none work, picks closest available. **Client never assumes requested mode was applied** — always uses `ModeConfirm` values.

**DPI/HiDPI policy (MVP)**: Virtual display is **non-Retina (scale 1.0)** by default. Stream resolution equals virtual display pixel resolution (no extra scaling). HiDPI modes deferred to post-MVP to avoid "everything too small/large" confusion.

### Input Coordinate Space

Client sends input in **StreamSpace** pixel coordinates: `(x_px, y_px)` within `(stream_width, stream_height)`.

Host converts to global macOS coordinates **at injection time** (never cached):
```swift
let bounds = CGDisplayBounds(virtualDisplayID)  // live query every time
let globalX = bounds.origin.x + (Double(x_px) / Double(streamWidth)) * bounds.width
let globalY = bounds.origin.y + (Double(y_px) / Double(streamHeight)) * bounds.height
// Apply rotation transform if rotation != 0
```

**Edge crossing behavior (RemoteControlGrabbed)**:
- Cursor coordinates are **clamped** to `[0..stream_width-1, 0..stream_height-1]`
- Do NOT auto-release grab on edge crossing (too magical, confusing)
- Release is always explicit via `Ctrl+Alt+Escape` hotkey

**Host-side mouse move coalescer**:
- Drop intermediate mouse moves if a newer move arrives before injection
- Cap injection rate at 240Hz max
- Ensure drag sequences are consistent: mouseDown → move* → mouseUp (never reorder)

### Keyboard Mapping Policy

- **MVP**: US keyboard layout supported reliably. Other layouts are best-effort.
- **Canonical physical key identifier in protocol**: **USB HID Usage ID** (not SDL scancode — they are similar but not identical). SDL scancodes can be converted to HID usage on the client side.
- Client sends **both** fields per key event:
  - `hid_usage: u16` (USB HID usage code — layout-independent, canonical)
  - `logical_keysym: u32` (SDL keysym / Unicode codepoint — layout-dependent, where available)
- Host mapping table: ~120 entries mapping HID usage → macOS `CGKeyCode` values
- For unmatched keys: log a debug warning with the raw HID usage, do not inject (prevents phantom input)
- **Debug overlay**: Host logs all received HID usage + keysym pairs. Useful for iterating on mapping table.
- IME and non-US text input: **deferred to post-MVP** (complex, requires text input protocol)
- Media keys / function keys: map where HID usage is defined; best-effort

### Pairing Protocol

**Identity model**: Pinned self-signed certificates (simplest for personal LAN).

First-time pairing:
1. Host generates a self-signed TLS certificate on first launch, stores it persistently
2. Host displays 6-digit PIN on screen
3. Client sends `PairRequest { pin, client_device_id, nonce, timestamp }` (nonce + timestamp for replay protection; reject if timestamp > 60s old)
4. Host verifies PIN, replies `PairResponse { host_cert_fingerprint }`
5. Client stores `(host_device_id, host_cert_fingerprint)` to disk
6. Host stores `(client_device_id)` to its allowed-devices list

Subsequent connections:
- TLS with pinned certificate fingerprint — client verifies host cert matches stored fingerprint
- If fingerprint mismatch → reject, require re-pair

## Capture → Encode Pipeline (Decoupled)

**NOT on the capture callback queue.** Instead:

```
ScreenCaptureKit callback thread:
  → writes CVPixelBuffer ref to AtomicLatestFrame slot (lock-free, single-producer)
  → signals encoder semaphore

Encoder thread (dedicated):
  → waits on semaphore
  → reads latest CVPixelBuffer from slot (automatically drops older)
  → VTCompressionSessionEncodeFrame
  → on output callback: packetize + UDP send
```

This prevents capture backpressure (SCK callback returns immediately), avoids blocking the WindowServer, and ensures the encoder always works on the freshest frame.

## Codec Strategy

**All phases (1-7): H.264 only** (High profile). Reasons:
- Universal VAAPI support on Linux (Intel/AMD)
- Software decode fallback is fast (even 4K on modern CPU)
- Easier to debug bitstreams
- Simpler to get all features working end-to-end with one codec

**Phase 8 (after all features validated): HEVC upgrade.** Negotiation fields (`supported_codecs`, `codec` enum) are present in the protocol from the start, but **`codec` in ModeConfirm is always H.264 through Phase 7. `supported_codecs` from client is stored but ignored until Phase 8.** This prevents accidental half-implementation of HEVC while features are still being built and validated.

**HEVC is not touched until**: virtual display, capture, encode, transport, decode, render, cursor, input, session management, and pairing are all working and stable with H.264. Only then does Phase 8 add the HEVC codec path.

Both codecs use VideoToolbox hardware encoding on Mac (Apple Silicon Media Engine).

## Project Structure

```
remote_extended_screen/
├── proto/                           # Shared protocol definitions (Protobuf)
│   ├── control.proto
│   ├── video.proto
│   ├── input.proto
│   └── cursor.proto
├── tools/
│   └── generate_proto.sh           # protoc invocation, pinned version
├── mac-host/                        # Swift Package (macOS app)
│   ├── Package.swift
│   └── Sources/
│       ├── RemoteDisplayHost/       # main.swift, App.swift, Config.swift
│       ├── VirtualDisplay/          # CGVirtualDisplay private API bridge
│       │   ├── CGVirtualDisplayBridge.h/m  (Obj-C, private API)
│       │   ├── VirtualDisplayManager.swift
│       │   └── include/module.modulemap
│       ├── ScreenCapture/           # DisplayCapturer.swift, LatestFrameSlot.swift
│       ├── VideoEncoder/            # VideoEncoder.swift (H.264; HEVC added in Phase 8), NALUPackager.swift
│       ├── InputInjector/           # EventInjector.swift, CoordinateMapper.swift,
│       │                            # MouseCoalescer.swift, PressedKeyState.swift
│       ├── CursorTracker/           # CursorTracker.swift (host-driven, LocalControl only)
│       ├── Networking/              # VideoSender.swift, ControlChannel.swift, Discovery.swift
│       ├── Session/                 # SessionStateMachine.swift, InputOwnership.swift
│       └── Protocol/               # Generated protobuf Swift code (committed)
├── ubuntu-client/                   # Rust workspace
│   ├── Cargo.toml
│   ├── src/main.rs
│   └── crates/
│       ├── video-decode/            # decoder.rs, nalu_parser.rs (ffmpeg VAAPI + SW)
│       ├── renderer/                # window.rs, texture.rs, cursor_renderer.rs (SDL2)
│       ├── input-capture/           # lib.rs, sdl_backend.rs (MVP), evdev_backend.rs (future)
│       ├── net-transport/           # video_receiver.rs, control_channel.rs, discovery.rs
│       ├── jitter-buffer/           # lib.rs (adaptive 0.5-1 frame)
│       └── protocol/               # Generated protobuf Rust code (committed)
├── Makefile
└── smoke_test.swift                 # Post-OS-update validation script
```

## MVP Scope Lock (prevent scope creep)

- One virtual display at a time (no multi-display)
- **Supported resolutions: 1920x1080@60fps (1080p60) and 3840x2160@60fps (4K60)**
  - 1080p60: universally supported, works on any monitor. H.264 Level 4.1. Default bitrate 20Mbps.
  - 4K60: for user's 4K monitor. H.264 Level 5.1. Default bitrate 50Mbps.
  - Virtual display mode selected via mode negotiation (client reports preferred modes).
- **H.264 only through Phases 1-7.** HEVC added in Phase 8 only after all features are validated end-to-end with H.264. Negotiation plumbing carries codec fields from the start but they are ignored until Phase 8. H.264 High profile.
- **Bitrate**: 20Mbps (1080p) / 50Mbps (4K). Adaptive down to 5Mbps on loss.
- No IME / no non-US keyboard layout guarantee
- No HDR, no color management, no wide color
- No audio forwarding
- Ubuntu 22.04 Xorg only (Wayland unsupported)
- Wired gigabit LAN required for 4K60 (50Mbps sustained). 1080p60 may work on good Wi-Fi.
- Non-Retina (scale 1.0) only
- US keyboard layout primary

**Resolution-dependent implications** (avg chunks/frame = bitrate / 8 / 60 / 1362; peaks higher on IDR frames):
- **1080p60**: 20Mbps → ~41KB/frame avg → **~35 chunks/frame**. VAAPI optional; H.264 SW decode is a viable fallback. Universal compatibility.
- **4K60**: 50Mbps → ~104KB/frame avg → **~87 chunks/frame**. **VAAPI required**; SW decode is best-effort only (may drop to very low fps on many Ubuntu boxes — do not default to 4K60 without confirmed VAAPI). Verify Ubuntu GPU supports VAAPI H.264 Level 5.1. Gigabit wired LAN required. Video packet rate: ~5,200 UDP packets/sec.
- Apple Silicon Media Engine handles both 1080p60 and 4K60 H.264 encode without issues.
- Codec level is computed from negotiated resolution (4.1 for 1080p, 5.1 for 4K) and set in `ModeConfirm`.
- Encoder parameters must stay within level limits (reference frames, DPB, etc.). Keep conservative: no B-frames (already set), max 4 reference frames.
- **Default to 1080p60 on first connection.** User can switch to 4K60 after confirming VAAPI availability + gigabit network. If VAAPI is unavailable, never choose 4K60.

**IDR spike guardrails**: IDR (keyframe) frames can be several times the average frame size (e.g., 3-5x at 4K60). To handle this:
- Set `max_frame_bytes` in ModeConfirm to a realistic cap for worst-case IDR at the negotiated bitrate. Recommended: `5 × (bitrate / 8 / fps)` (5x average frame). E.g., for 4K60@50Mbps: `5 × 104KB = ~520KB`.
- If a single frame exceeds `max_frame_bytes`: receiver drops it and counts toward IDR hysteresis (yes, this may drop an IDR — but the frame was too large to assemble safely, and the next IDR will be requested at a lower bitrate after adaptive bitrate kicks in).
- Host encoder should set VBV buffer / max frame size to stay within `max_frame_bytes` (best-effort — hardware encoders may occasionally exceed).

### Receiver Performance Constraints (critical for 4K60)

At 4K60 with ~87 chunks/frame × 60fps = ~5,200 packets/sec, the receiver must be a high-performance packet processor:
- **No per-packet heap allocations** in the hot path (UDP recv → chunk store → bitset mark)
- Use **preallocated frame assembly buffers** sized by `max_total_chunks_per_frame` and `max_frame_bytes` from `ModeConfirm` — allocate once per session, no runtime reallocation
- Track received chunks via a **bitset** (e.g., `[u64; 4]` for up to 256 chunks), NOT a hash map
- Assembly storage: **contiguous byte buffer** per frame (sized by `max_frame_bytes`) + chunk offset table (minimize copying)
- Limit in-flight frames to 4 (already specified)
- **SO_RCVBUF tuning (do on day one for 4K60)**: At ~5,200 packets/sec, default Linux socket buffers (212KB) can overflow during burst. Set `SO_RCVBUF` to at least 2MB on the video socket. Log actual granted size (kernel may cap at `net.core.rmem_max`).
- Post-MVP: consider `recvmmsg` for batch datagram reads and CPU affinity for the UDP recv thread if packet loss persists

## Platform Support Statement

- **macOS host**: macOS 14+ (Sonoma) on Apple Silicon. Intel Macs not targeted.
- **Ubuntu client (MVP)**: Ubuntu 22.04+ with **Xorg**. Wayland is **unsupported** in MVP (compositor restrictions on global input grab, hotkey interception, and fullscreen behavior). May work partially but not tested or guaranteed.
- **Network**: Wired LAN recommended. Wi-Fi functional but higher latency/jitter.

## Implementation Phases

### Phase 1: Virtual Display + Decoupled Capture Pipeline (Mac only, no network)
**Files**: `CGVirtualDisplayBridge.h/m`, `VirtualDisplayManager.swift`, `module.modulemap`, `Package.swift`, `DisplayCapturer.swift`, `LatestFrameSlot.swift`
- Obj-C bridge for CGVirtualDisplay private API (create/destroy, configurable resolution)
- OS version gating (allowlist/denylist) + kill switch config flag
- `VirtualDisplayHandle` with `resolveDisplayID()` for sleep/wake rebinding
- `CGDisplayRegisterReconfigurationCallback` to detect display ID changes
- ScreenCaptureKit capture targeting the virtual display (`showsCursor: false`, NV12 preferred with BGRA fallback, 60fps)
- Decoupled pipeline: capture thread → AtomicLatestFrame slot → encoder thread reads latest
- Use SCK timestamps (not wall-clock) for all timing
- **Verify**: Display appears in System Settings, cursor enters its region, captured frames saved as PNGs, sleep/wake → display re-resolves correctly, log actual FPS + dropped-frame stats

### Phase 2: Encoding + Local Validation (Mac only, no network)
**Files**: `VideoEncoder.swift`, `NALUPackager.swift`
- VideoToolbox H.264 (High profile) only. No HEVC code in this phase.
- Low-latency: real-time mode, no B-frames, low-latency rate control, **50Mbps** (4K60 needs higher bitrate than 1080p), keyframe every 1s (faster recovery on lossy UDP)
- Include SPS/PPS on every keyframe (standard practice for streaming). Also emit periodically (~every 5s) as backup.
- Force-keyframe on demand
- NAL unit extraction, AVCC → Annex B conversion
- **Verify**: Write .h264 file, play with `ffplay`. Measure encode latency (expect 2-8ms).

### Phase 3: Protocol + Transport + Control Channel (Both sides, video-only)
**Files**: `proto/*.proto`, `tools/generate_proto.sh`
**Files (Mac)**: `VideoSender.swift`, `ControlChannel.swift`, `Discovery.swift`, `Protocol/` (generated)
**Files (Ubuntu)**: `protocol/`, `net-transport/video_receiver.rs`, `net-transport/control_channel.rs`, `net-transport/discovery.rs`, `jitter-buffer/lib.rs`
- Protobuf definitions + codegen for Swift and Rust
- Custom UDP framing (FrameHeader + ChunkHeader)
- TCP control channel with TLS
- Mode negotiation: client proposes modes + codecs → host confirms actual
- DPI policy: non-Retina (1.0) by default
- mDNS discovery (`_remotedisplay._tcp.`)
- Adaptive jitter buffer (0.5-1 frame)
- IDR request with hysteresis (≥3 drops in 500ms)
- **Verify**: UDP video chunks flow Mac→Ubuntu, control handshake completes, mode negotiation works

### Phase 4: Ubuntu Decode + Render (video-only, passive viewer)
**Files**: `video-decode/decoder.rs`, `video-decode/nalu_parser.rs`, `renderer/window.rs`, `renderer/texture.rs`, `src/main.rs`
- H.264 decode only via ffmpeg-next. VAAPI hardware accel preferred; SW fallback supported for 1080p (best-effort only for 4K60 — VAAPI effectively required at 4K).
- No HEVC decode code in this phase (added in Phase 8)
- SDL2 fullscreen window on target monitor (Xorg), selected by **display index** (CLI arg `--display N`). On startup, run a "flash test": render a colored screen on the chosen monitor for 2s before starting stream, so user can confirm correct monitor. Hide system cursor.
- NV12 → texture upload, `SDL_RenderPresent`
- Frame dropping: if decoder can't keep up, drop oldest queued frames
- **Verify**: Move a window onto virtual display on Mac → appears on Ubuntu's Monitor B. Measure end-to-end latency.

### Phase 5: Cursor Rendering (ownership-aware)
**Files (Mac)**: `CursorTracker.swift`
**Files (Ubuntu)**: `renderer/cursor_renderer.rs`
**Files (proto)**: `cursor.proto`
- Mac `CursorTracker`: 120Hz timer, enabled only when ownership is LocalControl/Released AND `CGEventGetLocation()` is within `CGDisplayBounds(virtualDisplayID)`. Cursor shape detection at 20Hz (lower rate). Sends `CursorUpdate` only when active. Timer is disabled otherwise (no CPU waste).
- Ubuntu `cursor_renderer`: built-in sprite set for standard cursor shapes (Arrow, IBeam, Crosshair, etc.). Renders at reported position.
- **Cursor source policy**: In LocalControl → host-driven. In RemoteControlGrabbed → client-authoritative (local, immediate). In RemoteControlReleased → host-driven.
- App-specific custom cursors degrade to Arrow in MVP.
- **Verify**: In LocalControl, move Mac cursor onto virtual display → cursor visible on Ubuntu with correct shape. Verify hotspot alignment (click target matches visual).

### Phase 6: Input Forwarding + Input Ownership
**Files (proto)**: `input.proto`
**Files (Ubuntu)**: `input-capture/lib.rs`, `input-capture/sdl_backend.rs`
**Files (Mac)**: `EventInjector.swift`, `CoordinateMapper.swift`, `MouseCoalescer.swift`, `PressedKeyState.swift`, `Session/InputOwnership.swift`
- Input Ownership state machine: LocalControl / RemoteControlGrabbed / RemoteControlReleased
- Ubuntu SDL2 backend: mouse (StreamSpace pixel coords), keyboard (SDL scancode), scroll
- `Ctrl+Alt+Escape` releases grab; configurable grab hotkey to re-engage
- **Critical**: Release hotkey must be handled in the SDL event loop BEFORE forwarding keys to host. SDL2's event loop still receives events while grabbed — verify this works and add a test.
- On-screen overlay when grabbed ("Remote Control Active")
- Mouse + scroll: UDP, latest-seq-wins
- Keyboard: TCP, reliable
- Mac `MouseCoalescer`: drop intermediate moves, cap injection at 240Hz, preserve drag sequence ordering (down → move* → up)
- Mac `CoordinateMapper`: StreamSpace → global via **live** `CGDisplayBounds` at injection time, rotation-aware
- Mac `PressedKeyState`: tracks pressed keys/buttons. On disconnect → inject release for all pressed modifiers + buttons.
- Mac: check `AXIsProcessTrusted` on startup, prompt for Accessibility permission
- In RemoteControlGrabbed: cursor source switches to client-authoritative (Ubuntu renders locally, no host cursor feedback)
- **Verify**: Grab → move mouse → cursor moves on virtual display. Type → text appears. Disconnect TCP → all keys/buttons released. Release grab → Ubuntu input works locally.

### Phase 7: Session Management + Pairing + Polish
**Files (Mac)**: `Session/SessionStateMachine.swift`, `App.swift`, `Config.swift`
**Files (Ubuntu)**: `net-transport/discovery.rs` (pairing flow)
- PIN-based first-time pairing with nonce + timestamp replay protection
- Self-signed cert pinning for subsequent connections
- Session state machine with disconnect grace period (30-120s, configurable)
- On grace period: keep virtual display alive, show "Disconnected" on Ubuntu, attempt reconnect
- On timeout/explicit stop: destroy virtual display
- Display arrangement change monitoring (`CGDisplayRegisterReconfigurationCallback`)
- Adaptive bitrate (see algorithm below)
- Health checks: frames arriving + format valid + timestamps monotonic + parameter sets present
- `smoke_test.swift`: standalone post-OS-update validation
- **Degraded fallback**: If CGVirtualDisplay fails → mirror mode (capture existing display, public APIs only)
- **Second fallback**: If capture blocked → input-sharing-only mode (Barrier-like)

### Phase 8: HEVC Upgrade (after all H.264 features validated)
**Prerequisite**: Phases 1-7 complete, all verification steps passing with H.264.
**Files (Mac)**: `VideoEncoder.swift` (add HEVC path), `NALUPackager.swift` (VPS/SPS/PPS handling)
**Files (Ubuntu)**: `video-decode/decoder.rs` (HEVC decode path), `video-decode/nalu_parser.rs` (HEVC NAL types)
**Files (proto)**: `control.proto` (enable `supported_codecs` negotiation)
- **Host encoder**: Add HEVC (Main profile) to `VideoEncoder.swift` via VideoToolbox. Same low-latency settings as H.264 (no B-frames, real-time mode). Apple Silicon Media Engine handles HEVC natively.
- **Host packager**: Update `NALUPackager.swift` to handle HEVC NAL unit types (VPS + SPS + PPS on every keyframe, periodic emission ~every 5s).
- **Protocol negotiation**: Enable `supported_codecs` from client `ModeRequest`. Host now considers client's codec list when selecting codec in `ModeConfirm`. Prefer HEVC when both sides support it (better compression → lower bitrate for same quality, or better quality at same bitrate).
- **Ubuntu decoder**: Add HEVC decode path in `decoder.rs` — VAAPI hardware accel (Intel/AMD) with software fallback. HEVC VAAPI is widely supported on Intel Skylake+ and AMD GCN3+.
- **Bitrate adjustment**: HEVC achieves ~30-40% better compression than H.264 at equivalent quality. Adjust default bitrates: 12Mbps (1080p) / 30Mbps (4K) for HEVC. Adaptive bitrate floor: 1.5Mbps.
- **Fallback**: If HEVC encode/decode fails at runtime, fall back to H.264 automatically (renegotiate via control channel with HEVC removed from supported codecs).
- **Verify**: Full end-to-end pipeline with HEVC: encode → transport → decode → render. Compare visual quality and latency against H.264 at same bitrate. Verify VAAPI HEVC decode on Ubuntu. Verify fallback to H.264 when HEVC unavailable. Run all Phase 1-7 verification steps with HEVC active.

## Target Latency (Realistic)

| Stage | Typical Range |
|-------|---------------|
| Capture (ScreenCaptureKit) | 4–12ms |
| Encode (VideoToolbox HW) | 2–8ms |
| LAN transit (UDP) | <1ms |
| Jitter buffer | 0–8ms |
| Decode (VAAPI HW / SW) | 2–10ms |
| Render + vsync | 0–16ms |
| **Total (video)** | **20–45ms** |

Cursor (receiver-rendered in RemoteControlGrabbed): **~0ms** (client-authoritative, local render).
Cursor (host-driven in LocalControl): **~2-5ms** (host poll → UDP → local render).

## Dependencies

**Mac**: System frameworks (CoreGraphics, ScreenCaptureKit, VideoToolbox, Network, Foundation). `swift-protobuf` for protobuf codegen.

**Ubuntu**:
```bash
# System packages:
sudo apt install libavcodec-dev libavformat-dev libavutil-dev \
    libsdl2-dev libva-dev protobuf-compiler pkg-config

# Rust crates:
ffmpeg-next = "4.4"       # matches Ubuntu 22.04's FFmpeg 4.4; upgrade if using PPA
sdl2 = "0.37"
crossbeam-channel = "0.5"
prost = "0.13"
prost-build = "0.13"
mdns-sd = "0.11"
rustls = "0.23"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
anyhow = "1"
log = "0.4"
```

## Required macOS Permissions
- **Screen Recording** (ScreenCaptureKit)
- **Accessibility** (CGEvent injection)

## Bitrate Adaptation Algorithm (MVP)

Every 1 second, receiver sends `Stats` message over control channel:
```
Stats {
  packet_loss_rate: f32     // 0.0–1.0
  frame_drop_rate: f32      // frames dropped / frames expected
  decode_ms_p95: u32        // 95th percentile decode time
}
```

Host adjusts:
- If `frame_drop_rate > 0.05` OR `packet_loss_rate > 0.02` → `bitrate *= 0.8` (reduce)
- If stable (both below thresholds) for 5 consecutive seconds → `bitrate *= 1.05` (probe up)
- Floor: 2 Mbps. Ceiling: 50 Mbps (or negotiated max).
- On IDR request: also reduce by 0.9x (large frames are more likely to suffer loss)

## Key Risk: CGVirtualDisplay is a Private API

**Commitment**: Speed to working product + maximum UX control, in exchange for ongoing maintenance.

**Resilience measures**:
1. **OS version gating**: Per-version allowlist/denylist. Allowlist known-good builds (e.g., 14.6, 15.0, 15.1). Denylist known-bad. Unknown versions → warn + attempt.
2. **Kill switch**: Config flag (`virtual_display_enabled: bool`) to disable CGVirtualDisplay without rebuilding. When disabled, falls back to mirror mode.
3. **Runtime health checks**: Frames arriving + pixel format valid + timestamps monotonic + parameter sets at expected cadence
4. **Degraded fallback**: Mirror mode (capture existing display, public APIs only) if virtual display fails. Documented as "Viewer/KVM fallback" — input mapping changes since it targets an existing display.
5. **Second fallback**: Input-sharing-only if capture is also blocked (Barrier-like)
6. **Smoke test script**: Validates full pipeline post-OS-update

## Appendix A: control.proto (finalized)

See `proto/control.proto`. Key design decisions:
- **Envelope + oneof**: Single framed TCP stream, one parser, easy logging
- **Field numbering bands**: pairing=10-19, negotiation=20-29, runtime=30-39, input=40-49, lifecycle=50-59
- **Belt-and-suspenders**: `protocol_version` in both `Envelope` and `ModeRequest` (require they match); `session_id` in both `Envelope` and `ModeConfirm` (keep consistent)
- **No floats for protocol values**: `refresh_rate_millihz`, `bitrate_bps`, `codec_level_idc` are all integers
- **`Stats.packet_loss_rate`/`frame_drop_rate`**: float is OK here (inherently approximate)
- **`uint32` for ports**: protobuf has no u16; clamp 0-65535 in code

## Appendix B: Remaining proto files to materialize

- `cursor.proto`: cursor shape enum (shared between host cursor tracker and Ubuntu renderer)
- `video.proto`: shared `Codec`/`CodecProfile` enums (referenced by both control.proto and binary UDP headers); protocol constants as comments for human reference
- `input.proto`: mouse button enum, scroll units, input event types (for UDP binary input packets; shared enum definitions)

## Verification (End-to-End)
1. Launch Mac host → "Remote Display" appears in System Settings > Displays
2. Launch Ubuntu client (Xorg) → auto-discovers Mac via mDNS, connects
3. Mode negotiation: client proposes, host confirms actual mode applied
4. Drag a window onto the virtual display → appears on Ubuntu's Monitor B
5. LocalControl: Mac cursor enters virtual display → receiver-rendered cursor tracks with correct shape + hotspot
6. Grab input on Ubuntu → cursor switches to client-authoritative (zero-lag local render)
7. Mouse/keyboard on Ubuntu controls the virtual display
8. Release input (Ctrl+Alt+Escape) → Ubuntu input works locally, cursor reverts to host-driven
9. Kill network → display stays alive (grace period), reconnects when network returns
10. Kill TCP mid-keypress → all keys/buttons released on host (no stuck keys)
11. Glass-to-glass video latency: 20-45ms on wired LAN
12. Run `smoke_test.swift` → all checks pass
