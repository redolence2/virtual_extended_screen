# Remote Extended Screen -- Technical Review

A comprehensive technical review of the Remote Extended Screen (RESC) project:
software that lets a Mac use a physical monitor connected to a nearby Ubuntu machine as
an extended display over LAN, with full cursor and input forwarding.

---

## 1. Development Journey & Problems Solved

### Phase Summary

The project was built incrementally across eight phases, each adding a layer of the
full pipeline. The git history (`6ba9795` through `9a41ba8`) records the progression
and the significant bug-fixing iterations that followed.

| Phase | Commit | Scope |
|-------|--------|-------|
| 1 | `6ba9795` | Virtual display + decoupled capture pipeline (Mac only) |
| 2 | `ad141b6` | H.264 encoding via VideoToolbox + local .h264 dump validation |
| 3 | `7ba6969` | Protocol + transport + TCP control channel (both sides) |
| 4 | `d838971` | H.264 decode (ffmpeg-next) + SDL2 fullscreen render on Ubuntu |
| 5 | `851623b` | Cursor rendering (host-driven, LocalControl mode) |
| 6 | `eed98ff` | Input forwarding + input ownership state machine |
| 7 | `4f54f98` | Session management, adaptive bitrate, config, smoke test |
| 8 | `d88140f` | HEVC encoding + decoding support |

After Phase 8, more than a dozen targeted bug-fix commits addressed real-world streaming
issues discovered during testing. These are documented in detail below.

---

### Problem 1: Virtual display not appearing in system (missing sizeInMillimeters)

**Symptom**: `CGVirtualDisplay` was created successfully and returned a non-zero
`displayID`, but the display did not appear in macOS System Settings > Displays and
`SCShareableContent` could not find it.

**Root cause**: The `CGVirtualDisplayDescriptor` requires a `sizeInMillimeters` property
to be set. Without a physical size, macOS does not fully register the display in the
compositor. This is undocumented since `CGVirtualDisplay` is a private API.

**Fix**: In `CGVirtualDisplayBridge.m`, the descriptor now calls
`setSizeInMillimeters:` with a reasonable approximation (531mm x 299mm, corresponding to
a 24" 16:9 monitor). This causes the display to register properly with the system.

```objc
SEL setSizeInMM = NSSelectorFromString(@"setSizeInMillimeters:");
if ([descriptor respondsToSelector:setSizeInMM]) {
    CGSize physicalSize = CGSizeMake(531.0, 299.0);
    ((void (*)(id, SEL, CGSize))objc_msgSend)(descriptor, setSizeInMM, physicalSize);
}
```

---

### Problem 2: Double-init bug in ObjC bridge (alloc+init then initWithDescriptor)

**Symptom**: `CGVirtualDisplay` creation succeeded but the display object was in an
invalid or partially initialized state.

**Root cause**: The original code used the Objective-C pattern `[[Class alloc] init]`
followed by `initWithDescriptor:`, which double-initializes the object. The
`CGVirtualDisplayMode` and `CGVirtualDisplay` classes use designated initializers --
`initWithWidth:height:refreshRate:` and `initWithDescriptor:` respectively -- that must
be called directly on `alloc` without an intermediate `init`.

**Fix**: Changed to `[Class alloc]` followed immediately by the designated initializer:

```objc
// Mode: alloc + designated init (NOT alloc+init+reinit)
mode = ((id (*)(id, SEL, NSUInteger, NSUInteger, double))objc_msgSend)(
    [modeClass alloc], modeInit, width, height, (double)refreshRate);

// Display: alloc + designated init (NOT alloc+init+reinit)
display = ((id (*)(id, SEL, id))objc_msgSend)(
    [displayClass alloc], displayInit, descriptor);
```

---

### Problem 3: SCShareableContent not finding virtual display (retry with delay)

**Symptom**: After creating the virtual display, `SCShareableContent.excludingDesktopWindows`
returned a list that did not include the newly created display. The capture pipeline
failed with "Display not found in SCShareableContent."

**Root cause**: Virtual displays take a moment to register with ScreenCaptureKit
after creation. The system needs time to propagate the display into its internal
display list.

**Fix**: Added a retry loop in `DisplayCapturer.start()` that tries up to 5 times with
a 1-second delay between attempts:

```swift
for attempt in 1...5 {
    let content = try await SCShareableContent.excludingDesktopWindows(false, ...)
    scDisplay = content.displays.first(where: { $0.displayID == targetDisplayID })
    if scDisplay != nil { break }
    try await Task.sleep(nanoseconds: 1_000_000_000) // 1s
}
```

---

### Problem 4: NWConnection UDP silently failing (switched to POSIX sendto)

**Symptom**: The Mac host appeared to be sending video frames, but the Ubuntu client
received zero UDP packets. No errors were logged on the host side.

**Root cause**: Apple's `NWConnection` (Network.framework) for UDP was silently
dropping datagrams without reporting errors. This is a known behavioral issue --
`NWConnection` is optimized for TCP and its UDP support has reliability quirks,
particularly around connection state and readiness.

**Fix**: Replaced `NWConnection`-based UDP sending with raw POSIX sockets (`socket()`,
`sendto()`) in both `VideoSender.swift` and `CursorTracker.swift`. The POSIX API
provides immediate, reliable sendto semantics and reports errors via `errno`. Similarly
on the host side, `InputReceiver.swift` uses POSIX `socket()` + `bind()` + `recv()`.

---

### Problem 5: Protocol header size mismatch (per-packet fields are 16 bytes not 12)

**Symptom**: Video frames received on Ubuntu were corrupted. The first frame appeared
garbled, and subsequent frames failed to decode. Chunk payloads were misaligned.

**Root cause**: The original plan specified per-packet fields as 12 bytes
(`stream_id:u32 + config_id:u32 + frame_id:u32`), but the actual implementation added
`chunk_id:u16 + chunk_size:u16`, making per-packet fields 16 bytes. The Rust receiver
was parsing at offset 12 where the actual data started at offset 16, causing a
systematic 4-byte shift that corrupted every chunk's payload start position.

**Fix** (commit `17457ee`): Updated the protocol constants to reflect the true layout.
Per-packet fields are 16 bytes (not 12). The `VIDEO_CHUNK_HEADER_BYTES` constant was
corrected to 36 (16 + 20), and `VIDEO_TOTAL_HEADER_BYTES` to 42 (6 + 36). Both the
Swift `ProtocolConstants` and Rust `protocol::constants` were synchronized:

```
Per-packet: stream_id(4) + config_id(4) + frame_id(4) + chunk_id(2) + chunk_size(2) = 16 bytes
Per-frame:  timestamp_us(8) + is_keyframe(1) + codec(1) + width(2) + height(2) + total_chunks(2) + total_bytes(4) = 20 bytes
Total chunk header: 16 + 20 = 36 bytes
Total packet header: 6 (prefix) + 36 (chunk header) = 42 bytes
```

---

### Problem 6: Jitter buffer frame assembly corruption (stride-based storage)

**Symptom**: Assembled frames had garbage data interspersed with valid video content.
Decoded output showed visual corruption -- tearing, block artifacts, shifted rows.

**Root cause**: The original jitter buffer stored chunk payloads contiguously in a
single buffer. But chunks can arrive out of order, and their sizes vary (the last
chunk is typically smaller). Storing them contiguously without accounting for ordering
led to overlapping writes and data corruption.

**Fix**: Changed to stride-based storage: each chunk is stored at
`chunk_id * MAX_VIDEO_PAYLOAD_BYTES` within a preallocated buffer, with the actual
payload size tracked per-chunk in a `chunk_sizes` array. When the frame is complete,
a compaction step copies chunks contiguously into the output buffer using the tracked
sizes:

```rust
// Store at stride offset
let offset = cid * stride;
slot.data[offset..offset + payload.len()].copy_from_slice(payload);
slot.chunk_sizes[cid] = payload.len() as u16;

// Compact on completion
for i in 0..meta.total_chunks as usize {
    let chunk_offset = i * stride;
    let chunk_len = slot.chunk_sizes[i] as usize;
    frame_data.extend_from_slice(&slot.data[chunk_offset..chunk_offset + chunk_len]);
}
```

---

### Problem 7: First frame missing SPS/PPS (keyframe gate)

**Symptom**: The client's decoder received the first frames but could not decode them.
The decoder reported "no parameter sets" or produced blank output.

**Root cause**: The encoder starts producing frames immediately. The first frames sent
to the client were P-frames (delta frames) which require SPS/PPS parameter sets to
decode. These parameter sets are only included with keyframes (I-frames). The client
received P-frames before any keyframe.

**Fix**: Two-pronged approach:
1. **Keyframe gate** in `main.swift`: A `hasSentKeyframe` flag suppresses all frame
   transmission until the first keyframe is sent. Non-keyframes before the first
   keyframe are silently dropped.
2. **Force keyframe** on streaming start: When `StreamingReady` is received,
   `onForceKeyframe` triggers `encoder.forceKeyframe()`, ensuring the very first
   frame the client receives contains SPS/PPS.

---

### Problem 8: max_frame_bytes too small for keyframes (increased to 20x average)

**Symptom**: Keyframes (IDR frames) were being dropped by the receiver because they
exceeded `max_frame_bytes`. This caused periodic black/corrupt frames, especially
during rapid content changes.

**Root cause**: The original `max_frame_bytes` calculation used `5x` the average frame
size. But during fast content changes (window dragging, video playback), keyframes
can spike to 10-20x the average. The receiver dropped these oversized frames, losing
the reference needed for subsequent P-frames.

**Fix**: Increased the multiplier from 5x to 20x with a 2MB cap:

```swift
static func maxFrameBytes(bitrateBps: UInt32, fps: Double) -> UInt32 {
    let avgFrameBytes = Double(bitrateBps) / 8.0 / fps
    return UInt32(min(avgFrameBytes * 20.0, 2_000_000)) // 20x avg, cap 2MB
}
```

---

### Problem 9: GPU memory leak from creating SDL2 texture per present() call

**Symptom**: Memory usage on the Ubuntu client grew continuously during streaming.
Eventually the system slowed down or the process was killed by the OOM killer.

**Root cause** (commits `2851484`, `1026f20`): The original renderer created a new
SDL2 streaming texture for every frame presentation. SDL2 textures consume GPU memory
and must be explicitly dropped. Textures were being leaked.

**Fix**: The renderer now caches YUV frame data in `CachedYUV` and creates a short-lived
texture per `present_with_cursor()` call that is dropped at the end of the function
scope. Additionally, the `TextureCreator` is stored as a struct field rather than
recreated, avoiding lifetime issues. A comment explicitly notes the drop behavior:

```rust
if let Ok(mut tex) = self.texture_creator.create_texture_streaming(
    PixelFormatEnum::IYUV, yuv.w, yuv.h
) {
    let _ = tex.update_yuv(...);
    let _ = self.canvas.copy(&tex, None, Some(dst));
    // tex is dropped here -- no GPU memory leak
}
```

Note: while the current approach creates a texture per present call rather than caching
a single reusable texture, the texture is properly dropped each time, so there is no
leak. See Section 6 for discussion of the performance implications.

---

### Problem 10: Gray frames from HEVC error concealment

**Symptom**: After HEVC was enabled (Phase 8), the Ubuntu client intermittently displayed
fully gray frames. These appeared during rapid content changes and sometimes persisted
for several seconds.

**Root cause**: When HEVC reference frames are lost (due to UDP packet loss or frame
drops), ffmpeg's error concealment fills missing macroblocks with neutral gray (Y=128).
With HEVC's longer reference chains, a single lost frame causes gray concealment in all
subsequent frames until the next keyframe.

**Fix** (commits `51a8712`, `5c7f14c`, `e5c32cc`): Three-layer defense:
1. **Disabled ffmpeg error concealment** entirely by setting `error_concealment = 0` on
   the decoder context. This prevents gray fill.
2. **Skip corrupt frames** by checking `decode_error_flags` on each output frame.
3. **Y-plane variance detector**: Samples 16 pixels across the frame. If variance is
   near zero and mean is in the gray range (100-160), the frame is classified as
   error concealment output and skipped:

```rust
if variance < 4 && mean > 100 && mean < 160 {
    self.has_reference = false;
    continue; // skip gray frame
}
```

---

### Problem 11: Frame channel drops breaking HEVC references

**Symptom**: Even with error concealment disabled, gray frames still appeared
intermittently. The decoder lost its reference state.

**Root cause** (commit `81017ff`): The `mpsc::sync_channel` between the video receiver
and decode/render thread had a capacity of 2. When the decoder fell slightly behind
(e.g., during a burst of keyframes), `try_send` dropped frames. With HEVC, every
dropped frame breaks the reference chain, causing all subsequent frames to produce
errors until the next keyframe.

**Fix**: Increased channel capacity from 2 to 64, and switched from non-blocking
`try_send` to blocking `send`:

```rust
let (frame_tx, frame_rx) = mpsc::sync_channel::<AssembledFrame>(64);

// In video receiver: blocking send -- never drop frames
match frame_tx.send(frame) { ... }
```

---

### Problem 12: Drag lag from stale frame queue

**Symptom**: When dragging a window across the virtual display, the Ubuntu client showed
significant lag -- the window position was visibly behind the cursor. Releasing the drag
caused the window to "catch up" in a burst.

**Root cause** (commits `7096287`, `9a41ba8`): The decode thread was processing one
frame per render cycle. When frames queued up (e.g., during a drag that generates many
frames), the renderer displayed them sequentially, always behind the latest content.
The queue grew during activity and drained slowly, creating ever-increasing lag.

**Fix**: Changed the decode loop to drain ALL available frames from the queue each
iteration. Every frame is decoded (maintaining the codec reference chain), but only
the last decoded frame in each batch is actually rendered:

```rust
// Drain all available frames
let mut frames_to_decode: Vec<AssembledFrame> = Vec::new();
match frame_rx.recv_timeout(Duration::from_millis(8)) {
    Ok(frame) => {
        frames_to_decode.push(frame);
        while let Ok(more) = frame_rx.try_recv() {
            frames_to_decode.push(more);
        }
    }
    ...
}
// Decode ALL but render only latest
for (i, assembled) in frames_to_decode.iter().enumerate() {
    let is_last = i == batch_size - 1;
    // ... decode ...
    if is_last { r.update_frame(decoded); }
}
```

---

### Problem 13: Low capture FPS on idle display (FramePacer)

**Symptom**: When the virtual display had static content (e.g., a desktop wallpaper with
no window activity), the capture FPS dropped far below 60fps -- sometimes to single
digits. This caused the stream to appear frozen or unresponsive.

**Root cause**: macOS ScreenCaptureKit only delivers frames when the compositor detects
a display update. On an idle virtual display with no visible changes, the compositor has
nothing to composite and delivers no frames.

**Fix** (commit `e6ff9f0`): Created `FramePacer`, which opens a tiny 1x1 borderless
window on the virtual display that toggles its alpha between 0.01 and 0.02 at 60Hz.
This constant sub-pixel change tricks the compositor into delivering steady frames:

```swift
let timer = DispatchSource.makeTimerSource(queue: .main)
timer.schedule(deadline: .now(), repeating: interval)
timer.setEventHandler {
    self.toggle.toggle()
    let alpha: CGFloat = self.toggle ? 0.01 : 0.02
    view.layer?.backgroundColor = NSColor(white: 0.01, alpha: alpha).cgColor
}
```

The window is positioned in the far corner, is invisible to the user (alpha < 0.02),
ignores mouse events, and joins all spaces.

---

### Problem 14: Screen tearing (re-enabled SDL2 vsync)

**Symptom** (commit `2ddf9bc`): Horizontal tearing artifacts visible during content
motion on the Ubuntu client.

**Root cause**: Vsync was disabled in the SDL2 canvas builder during earlier debugging.
Without vsync, buffer swaps are not synchronized with the monitor's refresh cycle.

**Fix**: Re-enabled `present_vsync()` in the SDL2 canvas builder:

```rust
let mut canvas = window.into_canvas()
    .accelerated()
    .present_vsync()  // prevents tearing
    .build()?;
```

---

### Problem 15: App Switcher quirks with virtual display

**Symptom**: The virtual display interacts with macOS multi-display behaviors in expected
but sometimes inconvenient ways: Cmd+Tab app switcher may appear on the virtual display,
windows can be accidentally moved there, and Spaces behaviors apply.

**Root cause**: macOS treats the virtual display as a real display. All standard
multi-display behaviors apply, including Mission Control, Spaces, and the App Switcher.
This is by design (the virtual display appears in System Settings > Displays and is
fully arrangeable), but creates UX quirks.

**Mitigation**: The FramePacer window uses `collectionBehavior: [.canJoinAllSpaces, .stationary]`
and `.screenSaver` level to minimize interference. The virtual display can be arranged
in System Settings to reduce accidental window placement. No full solution exists
without deeper macOS integration.

---

## 2. Final Architecture

### System Overview

RESC creates a virtual display on macOS using the private `CGVirtualDisplay` API. The
display appears in System Settings as a real monitor and is fully arrangeable. The
content of this virtual display is captured, encoded, and streamed to an Ubuntu client
over LAN, where it is decoded and rendered fullscreen on a physical monitor.

```
 Mac Host                                            Ubuntu Client
+-----------------------------------+               +---------------------------+
| CGVirtualDisplay (private API)    |               |                           |
|         |                         |  UDP video    | Video Receiver (UDP)      |
| ScreenCaptureKit (showsCursor:NO) | ------------> | Jitter Buffer / Assembler |
|         |                         |               |         |                 |
| LatestFrameSlot (lock-free)       |               | FFmpeg Decoder (H264/HEVC)|
|         |                         |  UDP cursor   |         |                 |
| VideoToolbox Encoder (H264/HEVC)  | ------------> | SDL2 Fullscreen Renderer  |
|         |                         |               | + Cursor Overlay          |
| FramePacer (1x1 window, 60Hz)    |               |         |                 |
|         |                         |  UDP input    | Input Capture (SDL2)      |
| CursorTracker (120Hz polling)     | <------------ |   (mouse/scroll over UDP) |
| EventInjector (CGEvent)          |               |                           |
|         |                         |  TCP control  |                           |
| ControlChannel (TCP, protobuf)    | <-----------> | ControlChannel (TCP)      |
| Discovery (mDNS)                 |               | Discovery (mDNS)          |
+-----------------------------------+               +---------------------------+
```

### Mac Host Components

| Component | File | Responsibility |
|-----------|------|---------------|
| **VirtualDisplayManager** | `VirtualDisplayManager.swift` | Creates/destroys the virtual display, resolves display IDs after sleep/wake using layered fallback (vendor+product+serial, then pixel size, then newest heuristic). |
| **CGVirtualDisplayBridge** | `CGVirtualDisplayBridge.m` | Objective-C bridge that wraps the private `CGVirtualDisplay` API. Uses `objc_msgSend` runtime calls. Sets physical size, vendor/product/serial, and mode. |
| **DisplayCapturer** | `DisplayCapturer.swift` | Captures the virtual display at 60fps via ScreenCaptureKit. Outputs NV12 `CVPixelBuffer` to `LatestFrameSlot`. Retries up to 5 times if the virtual display is not yet visible. |
| **LatestFrameSlot** | `LatestFrameSlot.swift` | Lock-free single-producer/single-consumer frame slot. Capture writes, encoder reads. Older unread frames are dropped automatically. Uses `OSAllocatedUnfairLock` + `DispatchSemaphore`. |
| **VideoEncoder** | `VideoEncoder.swift` | Hardware encoder using VideoToolbox. Supports both H.264 (High profile) and HEVC (Main profile). Low-latency configuration: real-time mode, no B-frames, CABAC for H.264, configurable bitrate and keyframe interval. |
| **NALUPackager** | `NALUPackager.swift` | Extracts NAL units from VideoToolbox AVCC/HVCC output, converts to Annex B format. Prepends SPS/PPS (H.264) or VPS/SPS/PPS (HEVC) parameter sets on keyframes. |
| **VideoSender** | `VideoSender.swift` | Chunks encoded Annex B frames into UDP packets fitting within 1400 bytes. Sends via POSIX `sendto()`. Each packet carries the 42-byte header (6-byte prefix + 36-byte chunk header) plus payload. |
| **ControlChannel** | `ControlChannel.swift` | TCP server using `NWListener`. Accepts one client. Framing: `u32_le` length prefix + protobuf Envelope bytes. Handles mode negotiation, start/stop streaming. |
| **HostSession** | `HostSession.swift` | Orchestrates control channel + discovery + mode negotiation. Manages Idle -> Negotiating -> Streaming state transitions. Builds protobuf Envelopes (hand-rolled encoding). |
| **Discovery** | `Discovery.swift` | mDNS advertisement via `NetService`. Publishes `_remotedisplay._tcp.` on the control port. |
| **CursorTracker** | `CursorTracker.swift` | Polls mouse position at 120Hz via `CGEvent`. When cursor is within the virtual display's bounds, sends `CursorUpdate` packets over UDP. Sends heartbeat every 50ms. |
| **InputReceiver** | `InputReceiver.swift` | Listens on a UDP port for binary `InputEvent` packets from the client. Parses mouse/scroll events and dispatches to `EventInjector`. Uses latest-seq-wins for mouse moves. |
| **EventInjector** | `EventInjector.swift` | Injects mouse and keyboard events into macOS via `CGEvent`. Rate-limits mouse moves to 240Hz. Maps HID usage codes to macOS `CGKeyCode` values (~120 key mappings). |
| **CoordinateMapper** | `CoordinateMapper.swift` | Converts StreamSpace pixel coordinates to global macOS coordinates. Queries `CGDisplayBounds` live at injection time (never cached) to handle display rearrangement. |
| **PressedKeyState** | `PressedKeyState.swift` | Tracks currently pressed keys and mouse buttons. On disconnect, releases all pressed keys/buttons to prevent stuck input. |
| **FramePacer** | `FramePacer.swift` | Creates a 1x1 borderless window on the virtual display that toggles alpha at 60Hz. Forces the compositor to deliver steady capture frames even on idle content. |
| **BitrateAdapter** | `BitrateAdapter.swift` | Adjusts encoder bitrate based on receiver Stats. Reduces by 20% on loss, probes up by 5% after 5 stable seconds. Floor 2Mbps, ceiling at initial bitrate. |
| **SessionStateMachine** | `SessionStateMachine.swift` | Formal session state machine: Idle -> WaitingForClient -> Negotiating -> Streaming -> Disconnected (with grace period timer). |
| **Config** | `Config.swift` | Runtime configuration struct, loadable from JSON. CLI argument overrides. Auto-sets 4K bitrate. |

### Ubuntu Client Components

| Component | Crate | Responsibility |
|-----------|-------|---------------|
| **main** | `ubuntu-client/src/main.rs` | Entry point. Discovers host, negotiates mode, spawns threads: video-recv, cursor-recv, stats-reporter, decode-render. Processes SDL2 events for input. |
| **protocol** | `crates/protocol` | Generated protobuf types (`prost`), protocol constants, binary packet parsers (`PacketPrefix`, `VideoChunkPacket`, `CursorUpdate`). |
| **net-transport** | `crates/net-transport` | Network I/O: `VideoReceiver` (UDP recv loop + frame assembly), `ControlChannel` (TCP async client via tokio), `discovery` (mDNS via mdns-sd). |
| **jitter-buffer** | `crates/jitter-buffer` | Frame assembly from UDP chunks. Preallocated slots (4 in-flight frames). Stride-based storage with bitset tracking. Stale frame expiration at 500ms. |
| **video-decode** | `crates/video-decode` | FFmpeg decoder (ffmpeg-next) supporting H.264 and HEVC. Error concealment disabled. Y-plane variance detector skips gray concealment frames. 2 frame-level decode threads. |
| **renderer** | `crates/renderer` | SDL2 fullscreen window with vsync. Creates IYUV streaming texture per present. Caches YUV frame data. Includes cursor overlay rendering. |
| **cursor\_renderer** | `crates/renderer/cursor_renderer.rs` | Renders a simple arrow cursor (white with black outline) using filled rectangles. Supports visibility toggle and position update. |
| **input-capture** | `crates/input-capture` | Captures SDL2 input events. Sends mouse/scroll over UDP as binary `InputEvent` packets. Manages input ownership states (LocalControl, RemoteControlGrabbed, RemoteControlReleased). Grab hotkey: Ctrl+Alt+G; release: Ctrl+Alt+Escape. |

### Network Protocol

The system uses four network channels:

| Channel | Protocol | Port | Direction | Content |
|---------|----------|------|-----------|---------|
| Control | TCP (plaintext, TLS planned) | `controlPort` (default 9870) | Bidirectional | Protobuf Envelope messages: mode negotiation, start/stop streaming, stats, IDR requests |
| Video | UDP | `controlPort + 1` | Host -> Client | Chunked encoded video frames with binary headers |
| Input | UDP | `controlPort + 2` | Client -> Host | Binary mouse/scroll events |
| Cursor | UDP | `controlPort + 3` | Host -> Client | Binary cursor position/shape updates |

### Data Flow: Capture -> Encode -> Chunk -> Send -> Receive -> Assemble -> Decode -> Render

1. **Capture**: `DisplayCapturer` receives `CVPixelBuffer` (NV12) from ScreenCaptureKit at ~60fps. The callback immediately writes to `LatestFrameSlot` and returns.

2. **Encode**: Dedicated encoder thread waits on `LatestFrameSlot`'s semaphore, takes the latest `CVPixelBuffer`, feeds it to `VideoToolbox` via `VTCompressionSessionEncodeFrame`. The output callback receives AVCC/HVCC encoded data.

3. **Package**: `NALUPackager` converts AVCC/HVCC to Annex B format, prepending parameter sets on keyframes.

4. **Chunk & Send**: `VideoSender` splits the Annex B data into chunks of at most 1358 bytes each. Each chunk gets a 42-byte header (6 prefix + 16 per-packet + 20 per-frame). Per-frame fields are only meaningful in chunk 0; other chunks zero-fill those 20 bytes. Sent via POSIX `sendto()`.

5. **Receive**: `VideoReceiver` on Ubuntu reads UDP datagrams, validates prefix (magic, version, packet type), filters by `stream_id`/`config_id`, and feeds chunks to `FrameAssembler`.

6. **Assemble**: `FrameAssembler` stores chunks in preallocated slots using stride-based addressing. A bitset tracks which chunks have arrived. When all chunks are present, the data is compacted contiguously and emitted as an `AssembledFrame`.

7. **Decode**: The assembled Annex B frame is fed to ffmpeg's H.264 or HEVC decoder. Output frames are YUV420P. Gray concealment frames are detected by Y-plane variance and skipped.

8. **Render**: The decode/render thread drains all available frames, decodes them all (maintaining the reference chain), but only updates the SDL2 texture with the last decoded frame. `present_with_cursor()` creates a IYUV streaming texture, uploads the YUV data, composites the cursor overlay, and calls `canvas.present()` (with vsync).

### Cursor Pipeline

The cursor pipeline is separate from video to minimize latency:

- **Host -> Client (LocalControl mode)**: `CursorTracker` polls `CGEvent` at 120Hz. When the cursor is within the virtual display's `CGDisplayBounds`, it converts to StreamSpace coordinates and sends a 35-byte `CursorUpdate` packet over the cursor UDP port.

- **Client rendering**: A dedicated cursor-recv thread reads `CursorUpdate` packets and writes to `SharedCursorState` (atomic integers). The render thread reads this state and draws the cursor overlay.

- **RemoteControlGrabbed mode**: Host cursor updates are ignored. The client renders the cursor at its own last-known mouse position (zero-lag local rendering).

### Threading Model

**Mac Host (6 threads)**:
1. **Main thread**: RunLoop, signal handling, session management
2. **Capture thread**: ScreenCaptureKit callback dispatch queue (`com.resc.capture`)
3. **Encoder thread**: Dedicated `Thread` (`com.resc.encoder`), userInteractive QoS
4. **Control channel queue**: `DispatchQueue` (`com.resc.control`)
5. **Cursor tracker timer**: `DispatchSource` timer on `com.resc.cursor` queue
6. **Input receiver thread**: Dedicated `Thread` (`com.resc.input-recv`)

**Ubuntu Client (5 threads)**:
1. **Main / tokio thread**: Async runtime for TCP control channel + stats reporting
2. **video-recv thread**: Blocking UDP receive loop, frame assembly, sends to channel
3. **cursor-recv thread**: Blocking UDP receive loop, writes to shared atomic state
4. **decode-render thread**: SDL2 event pump, frame decode, rendering, input capture
5. **FramePacer timer** (on main dispatch queue, Mac side only)

---

## 3. API & Protocol Reference

### PacketPrefix (6 bytes, all UDP packets)

All UDP packets (video, cursor, input) share this prefix:

| Offset | Size | Field | Type | Description |
|--------|------|-------|------|-------------|
| 0 | 4 | `magic` | `[u8; 4]` | `b"RESC"` (`0x52, 0x45, 0x53, 0x43`) |
| 4 | 1 | `version` | `u8` | Protocol version (must be `1`) |
| 5 | 1 | `packet_type` | `u8` | `0`=video_chunk, `1`=cursor_update, `2`=input_event |

Packets with wrong magic or version are dropped silently. Wrong `packet_type` for a
given port increments `misrouted_packets`.

### VideoChunkHeader (36 bytes, after PacketPrefix)

**Per-packet fields (16 bytes, always valid):**

| Offset | Size | Field | Type | Description |
|--------|------|-------|------|-------------|
| 6 | 4 | `stream_id` | `u32 LE` | Random per negotiation |
| 10 | 4 | `config_id` | `u32 LE` | Increments on renegotiation |
| 14 | 4 | `frame_id` | `u32 LE` | Monotonic within stream |
| 18 | 2 | `chunk_id` | `u16 LE` | 0-indexed within frame |
| 20 | 2 | `chunk_size` | `u16 LE` | Payload bytes in this packet |

**Per-frame fields (20 bytes, valid when chunk_id==0, zero-filled otherwise):**

| Offset | Size | Field | Type | Description |
|--------|------|-------|------|-------------|
| 22 | 8 | `timestamp_us` | `u64 LE` | Microseconds since session start |
| 30 | 1 | `is_keyframe` | `u8` | 0=false, 1=true |
| 31 | 1 | `codec` | `u8` | 0=H.264, 1=HEVC |
| 32 | 2 | `width` | `u16 LE` | Frame width |
| 34 | 2 | `height` | `u16 LE` | Frame height |
| 36 | 2 | `total_chunks` | `u16 LE` | Total chunks in this frame |
| 38 | 4 | `total_bytes` | `u32 LE` | Total payload bytes for entire frame |

**Payload** starts at offset 42, up to 1358 bytes.

Total packet: 42 (header) + payload (up to 1358) = max 1400 bytes.

### CursorUpdate (29 bytes, after PacketPrefix)

| Offset | Size | Field | Type | Description |
|--------|------|-------|------|-------------|
| 6 | 4 | `seq` | `u32 LE` | Monotonic sequence number |
| 10 | 8 | `timestamp_us` | `u64 LE` | Microseconds (debug/telemetry) |
| 18 | 4 | `x_px` | `i32 LE` | StreamSpace X pixel (-1 = off display) |
| 22 | 4 | `y_px` | `i32 LE` | StreamSpace Y pixel |
| 26 | 1 | `shape_id` | `u8` | Cursor shape (Arrow=0, IBeam=1, ...) |
| 27 | 2 | `hotspot_x_px` | `u16 LE` | Hotspot X offset |
| 29 | 2 | `hotspot_y_px` | `u16 LE` | Hotspot Y offset |
| 31 | 4 | `cursor_scale` | `f32 LE` | IEEE 754 cursor scale factor |

Total cursor packet: 6 + 29 = **35 bytes**.

### InputEvent (22 bytes, after PacketPrefix)

| Offset | Size | Field | Type | Description |
|--------|------|-------|------|-------------|
| 6 | 4 | `seq` | `u32 LE` | Monotonic (latest-seq-wins for moves) |
| 10 | 1 | `event_type` | `u8` | 0=move, 1=button_down, 2=button_up, 3=scroll |
| 11 | 4 | `x_px` | `i32 LE` | StreamSpace X pixel |
| 15 | 4 | `y_px` | `i32 LE` | StreamSpace Y pixel |
| 19 | 1 | `button` | `u8` | 0=left, 1=right, 2=middle |
| 20 | 2 | `scroll_dx` | `i16 LE` | Horizontal scroll delta |
| 22 | 2 | `scroll_dy` | `i16 LE` | Vertical scroll delta |
| 24 | 4 | `modifiers` | `u32 LE` | Modifier bitmask (unused in MVP) |

Total input packet: 6 + 22 = **28 bytes**.

### Protobuf Control Protocol (control.proto)

The control channel uses protobuf `Envelope` messages framed as `u32_le length` +
protobuf bytes over TCP.

```protobuf
message Envelope {
  uint64 session_id = 1;        // 0 until assigned by host
  uint32 protocol_version = 2;  // must equal 1

  oneof payload {
    PairRequest pair_request = 10;
    PairResponse pair_response = 11;
    ModeRequest mode_request = 20;
    ModeConfirm mode_confirm = 21;
    ModeReject mode_reject = 22;
    StartStreaming start_streaming = 23;
    StreamingReady streaming_ready = 24;
    Stats stats = 30;
    RequestIDR request_idr = 31;
    KeyEvent key_event = 40;
    StopStreaming stop_streaming = 50;
  }
}
```

Field numbering bands: pairing=10-19, negotiation=20-29, runtime=30-39, input=40-49,
lifecycle=50-59.

### Mode Negotiation Handshake

```
Client                                    Host
  |                                         |
  |  TCP connect to controlPort (9870)      |
  | --------- ModeRequest ----------------> |
  |   (protocol_version, preferred_modes,   |
  |    supported_codecs)                    |
  |                                         |
  | <-------- ModeConfirm ----------------- |
  |   (session_id, stream_id, config_id,    |
  |    actual_width/height, codec,          |
  |    video_port, input_port, cursor_port) |
  |                                         |
  | <-------- StartStreaming --------------- |
  |   (stream_id, config_id)                |
  |                                         |
  |   [Client opens UDP sockets,            |
  |    initializes decoder]                 |
  |                                         |
  | --------- StreamingReady -------------> |
  |   (stream_id, config_id)                |
  |                                         |
  |   [Host begins UDP video/cursor]        |
  |   [Client sends Stats every 1s]         |
```

### Protocol Constants

| Constant | Value | Description |
|----------|-------|-------------|
| `PROTOCOL_VERSION` | `1` | Must match on both sides |
| `MAGIC` | `b"RESC"` | `[0x52, 0x45, 0x53, 0x43]` |
| `MAX_DATAGRAM_BYTES` | `1400` | Max UDP packet size |
| `PACKET_PREFIX_BYTES` | `6` | magic(4) + version(1) + type(1) |
| `VIDEO_CHUNK_HEADER_BYTES` | `36` | per-packet(16) + per-frame(20) |
| `VIDEO_TOTAL_HEADER_BYTES` | `42` | prefix + chunk header |
| `MAX_VIDEO_PAYLOAD_BYTES` | `1358` | 1400 - 42 |
| `CURSOR_UPDATE_BYTES` | `29` | CursorUpdate payload |
| `CURSOR_TOTAL_PACKET_BYTES` | `35` | prefix + cursor update |
| `INPUT_EVENT_BYTES` | `22` | InputEvent payload |
| `INPUT_TOTAL_PACKET_BYTES` | `28` | prefix + input event |
| Default control port | `9870` | TCP control channel |
| Default video port | `9871` | controlPort + 1 |
| Default input port | `9872` | controlPort + 2 |
| Default cursor port | `9873` | controlPort + 3 |
| mDNS service type | `_remotedisplay._tcp.` | For auto-discovery |
| mDNS domain | `local.` | Standard mDNS domain |

---

## 4. Code Structure

### Project Root

| File | Description |
|------|-------------|
| `Makefile` | Top-level build automation. Targets: `mac-build`, `mac-run`, `ubuntu-build`, `ubuntu-run`, `proto`, `setup`, `clean`. |
| `plan.md` | Comprehensive implementation plan covering all 8 phases, architecture, protocol design, and verification criteria. |
| `smoke_test.swift` | Standalone post-OS-update validation script. Checks for CGVirtualDisplay API, ScreenCaptureKit, VideoToolbox, display enumeration, and Accessibility permission. |
| `.gitignore` | Ignores build artifacts, tools, secrets, editor files, debug output. |

### Proto Definitions (`proto/`)

| File | Description |
|------|-------------|
| `control.proto` | Envelope message with oneof payload covering pairing, mode negotiation, runtime stats, input, and lifecycle messages. |
| `video.proto` | Protocol constants reference and binary packet layout documentation (no protobuf messages -- video uses binary UDP). |
| `cursor.proto` | `CursorShape` enum defining 16 standard cursor shapes (Arrow through Wait). |
| `input.proto` | `MouseButton` and `InputEventType` enums. Documents binary InputEvent layout. |

### Mac Host (`mac-host/`)

| File | Description |
|------|-------------|
| `Package.swift` | Swift Package Manager manifest. Targets: VirtualDisplayBridge (ObjC) and RemoteDisplayHost (Swift executable). Dependencies: swift-protobuf. Requires macOS 14+. |
| `Sources/VirtualDisplay/include/CGVirtualDisplayBridge.h` | ObjC header declaring the CGVirtualDisplayBridge class interface (displayID, vendorID, productID, create/destroy, API availability, OS build version). |
| `Sources/VirtualDisplay/CGVirtualDisplayBridge.m` | ObjC implementation wrapping the private CGVirtualDisplay API via runtime calls. Creates descriptor, mode, settings, and display objects. Sets physical size and identity. |
| `Sources/VirtualDisplay/include/module.modulemap` | Clang module map exposing the ObjC bridge to Swift as `VirtualDisplayBridge`. |
| `Sources/RemoteDisplayHost/main.swift` | Entry point. Parses CLI args, creates virtual display, starts FramePacer, sets up capture/encode/session pipeline, handles graceful shutdown. |
| `Sources/RemoteDisplayHost/VirtualDisplayManager.swift` | Manages virtual display lifecycle. OS version gating (allowlist/denylist). Layered display ID resolution for sleep/wake rebinding. |
| `Sources/RemoteDisplayHost/DisplayCapturer.swift` | ScreenCaptureKit-based capture targeting the virtual display. NV12 pixel format, 60fps, showsCursor=false. Retry logic for SCShareableContent. |
| `Sources/RemoteDisplayHost/LatestFrameSlot.swift` | Thread-safe single-frame buffer using OSAllocatedUnfairLock and DispatchSemaphore. Capture writes, encoder reads. Latest-wins semantics. |
| `Sources/RemoteDisplayHost/VideoEncoder.swift` | VideoToolbox hardware encoder for H.264/HEVC. Real-time mode, no B-frames, CABAC, configurable bitrate/keyframe interval. Force-keyframe support. |
| `Sources/RemoteDisplayHost/NALUPackager.swift` | AVCC/HVCC to Annex B conversion. Extracts SPS/PPS (H.264) or VPS/SPS/PPS (HEVC) parameter sets. Prepends on keyframes. |
| `Sources/RemoteDisplayHost/VideoSender.swift` | Chunks encoded frames into UDP packets with binary headers. Sends via POSIX sendto(). Tracks packet/byte stats. |
| `Sources/RemoteDisplayHost/ControlChannel.swift` | TCP server using NWListener. Length-prefixed protobuf framing (u32_le + Envelope). Single client, handles connect/send/recv/disconnect. |
| `Sources/RemoteDisplayHost/HostSession.swift` | Session orchestrator. Builds hand-rolled protobuf ModeConfirm and StartStreaming envelopes. Manages state transitions and callbacks. |
| `Sources/RemoteDisplayHost/Discovery.swift` | mDNS advertisement using NetService. Publishes `_remotedisplay._tcp.` service. |
| `Sources/RemoteDisplayHost/CursorTracker.swift` | 120Hz cursor position polling via CGEvent. Sends CursorUpdate over UDP when cursor is within virtual display bounds. Heartbeat every 50ms. |
| `Sources/RemoteDisplayHost/InputReceiver.swift` | UDP listener for binary InputEvent packets. Parses mouse/scroll, dispatches to EventInjector. Latest-seq-wins for mouse moves. |
| `Sources/RemoteDisplayHost/EventInjector.swift` | Injects CGEvents (mouse, keyboard) into macOS. HID-to-CGKeyCode mapping table. Rate-limits mouse moves to 240Hz. Accessibility permission check. |
| `Sources/RemoteDisplayHost/CoordinateMapper.swift` | Converts StreamSpace pixel coordinates to global macOS coordinates via live CGDisplayBounds query. |
| `Sources/RemoteDisplayHost/PressedKeyState.swift` | Tracks pressed keys/buttons. releaseAll() injects key-up and button-up events for all tracked state on disconnect. |
| `Sources/RemoteDisplayHost/FramePacer.swift` | 1x1 transparent window on virtual display that toggles alpha at 60Hz to force steady compositor frame delivery. |
| `Sources/RemoteDisplayHost/BitrateAdapter.swift` | Adjusts encoder bitrate based on receiver Stats. Reduces 20% on loss, probes 5% on stability. Floor 2Mbps. |
| `Sources/RemoteDisplayHost/SessionStateMachine.swift` | Formal state machine with grace period timer for disconnect handling. States: idle, waitingForClient, negotiating, streaming, disconnected. |
| `Sources/RemoteDisplayHost/Config.swift` | JSON-loadable configuration struct with CLI override support. |
| `Sources/RemoteDisplayHost/ProtocolConstants.swift` | Protocol constants (magic, version, header sizes, packet types, mDNS service type, codec level computation, max frame bytes formula). |

### Ubuntu Client (`ubuntu-client/`)

| File | Description |
|------|-------------|
| `Cargo.toml` | Workspace manifest with 6 internal crates. Dependencies: sdl2, tokio, anyhow, clap, log. |
| `src/main.rs` | Entry point. mDNS discovery or manual host. TCP mode negotiation. Spawns video-recv, cursor-recv, stats, decode-render threads. SDL2 event loop for input. |
| `crates/protocol/src/lib.rs` | Protocol constants, generated protobuf module includes, binary packet parsers (PacketPrefix, VideoChunkPacket, CursorUpdate). |
| `crates/protocol/build.rs` | prost-build invocation compiling control.proto, cursor.proto, input.proto. |
| `crates/net-transport/src/lib.rs` | Module declarations for video_receiver, control_channel, discovery. |
| `crates/net-transport/src/video_receiver.rs` | UDP receive loop with POSIX socket (SO_REUSEADDR, SO_RCVBUF=2MB). Validates packets, feeds chunks to FrameAssembler, sends completed frames via blocking channel send. |
| `crates/net-transport/src/control_channel.rs` | Async TCP client (tokio). Framed protobuf send/recv. Mode negotiation (ModeRequest/ModeConfirm), wait for StartStreaming, send StreamingReady, send Stats. |
| `crates/net-transport/src/discovery.rs` | mDNS host discovery using mdns-sd crate. Browses for `_remotedisplay._tcp.local.` with configurable timeout. |
| `crates/jitter-buffer/src/lib.rs` | Frame assembly from UDP chunks. 4 preallocated FrameSlots with stride-based storage. Bitset chunk tracking. Stale frame expiration. Contiguous compaction on completion. |
| `crates/video-decode/src/lib.rs` | FFmpeg decoder via ffmpeg-next. H.264 and HEVC. Error concealment disabled. Corrupt frame detection via decode_error_flags. Gray frame filtering via Y-plane variance sampling. |
| `crates/renderer/src/lib.rs` | SDL2 fullscreen renderer. Creates IYUV texture per present. Cached YUV frame data. Flash test on startup. Vsync enabled. Cursor overlay compositing. |
| `crates/renderer/src/cursor_renderer.rs` | Simple arrow cursor sprite rendered as filled rectangles (black outline + white fill). Supports visibility and position updates. |
| `crates/input-capture/src/lib.rs` | SDL2 input capture with UDP sending. InputOwnership state machine. Grab/release hotkeys (Ctrl+Alt+G / Ctrl+Alt+Escape). Binary InputEvent packet building. SDL scancode to HID usage mapping. |

### Tools

| File | Description |
|------|-------------|
| `tools/generate_proto.sh` | Downloads protoc + protoc-gen-swift locally, generates Swift and Rust protobuf code from proto/ definitions. Pinned protoc v27.3. |

---

## 5. Known Issues & Limitations

### Gray frames during rapid content changes

When HEVC reference frames are lost (packet loss, frame drops during bursts), the
decoder loses its reference chain. Despite the three-layer defense (concealment disabled,
corrupt frame check, Y-plane variance filter), brief gray or corrupted frames can still
appear during rapid content changes before the next keyframe arrives. The keyframe
interval (0.5s default) bounds the maximum recovery time.

### Capture FPS depends on compositor activity

ScreenCaptureKit only delivers frames when the macOS compositor detects display updates.
On a truly idle virtual display (no window activity, no cursor motion), frame delivery
drops to near zero. The FramePacer hack mitigates this but does not solve it for all
edge cases.

### FramePacer hack for steady frame rate

The FramePacer creates a 1x1 window that toggles alpha at 60Hz to trick the compositor.
This is a hack that works in practice but has theoretical fragility:
- macOS could optimize away sub-pixel alpha changes in future versions
- The window may interfere with App Switcher or window enumeration
- It adds a small, unnecessary compositing load

### App Switcher behavior with virtual display

macOS treats the virtual display as a real display. The App Switcher (Cmd+Tab),
Mission Control, and Spaces all interact with it normally. Windows can accidentally be
moved to the virtual display via window management. This is expected behavior but can
be surprising to users.

### CGVirtualDisplay is a private API

The core virtual display functionality relies on Apple's undocumented
`CGVirtualDisplay` API, accessed via Objective-C runtime calls. This means:
- No guarantee of API stability across macOS versions
- OS version gating (allowlist/denylist) is required for each macOS release
- API behavior can change without notice (e.g., new required properties, changed selectors)
- The smoke test (`smoke_test.swift`) should be run after every macOS update

### No TLS/pairing implemented yet

The control channel is plaintext TCP. The protocol defines `PairRequest`/`PairResponse`
messages and the plan describes a PIN-based pairing flow with certificate pinning, but
this is not implemented. Any device on the LAN can connect and control the host.

### Input grab (Ubuntu -> Mac) code present but not tested

The input capture and injection pipeline is implemented but has not been thoroughly
tested in a real dual-machine environment. The InputOwnership state machine,
grab/release hotkeys, and mouse/keyboard forwarding code is present but marked as
functionally incomplete.

### Keyboard forwarding over TCP not wired

The protocol defines `KeyEvent` messages for reliable keyboard forwarding over TCP.
The client side generates `KeyEventOut` structs, but the actual TCP send is marked
with `// TODO: send KeyEvent over TCP control channel`. Keyboard input forwarding
is not functional.

### Single client only

The control channel accepts only one client at a time. If a new client connects, the
previous connection is cancelled. There is no multi-client support or session
multiplexing.

---

## 6. Potential Architectural Deficiencies

### Hand-rolled protobuf encoding in HostSession.swift

`HostSession.swift` contains manual protobuf varint and field encoding
(`appendProtoUInt32`, `appendProtoUInt64`, `appendProtoBytes`, `appendVarint`) instead
of using the `swift-protobuf` library that is already a dependency. This is fragile:
- Field encoding bugs are easy to introduce (wrong wire type, wrong field number)
- Proto3 default-value semantics must be manually handled (e.g., not emitting zero values)
- The code comments acknowledge this: "Will be replaced with generated swift-protobuf code"
- The client uses generated prost code (correct approach); the host should match

### No proper error recovery on control channel disconnect

When the TCP control channel disconnects, there is no automatic reconnection attempt.
The `SessionStateMachine` defines a grace period and reconnect path, but it is not
wired into the actual `HostSession` and `ControlChannel` code. A disconnect effectively
requires restarting both sides.

### FramePacer is a hack

As noted in Known Issues, the FramePacer creates a 1x1 window that manipulates alpha
to force compositor updates. A more robust approach would be:
- Using `CVDisplayLink` or `CADisplayLink` to drive frame timing
- Investigating ScreenCaptureKit options for guaranteed frame delivery
- Rendering content directly to the virtual display's surface if the API supports it

### Texture creation per present() call in renderer

`Renderer::present_with_cursor()` creates a new SDL2 streaming texture every frame.
While the texture is properly dropped (no memory leak), this pattern has overhead:
- Texture allocation and deallocation per frame (~60 alloc/dealloc per second)
- A single persistent texture that is updated each frame would be more efficient
- The code comments acknowledge this ("creates one texture per call BUT properly drops it")

### No rate limiting on cursor updates

`CursorTracker` sends UDP packets at 120Hz whenever the cursor is within the virtual
display bounds, regardless of whether the position has changed (heartbeat every 50ms).
At 120Hz with 35-byte packets, this is negligible bandwidth, but there is no throttling
if the cursor is continuously moving.

### Thread safety of shared mutable state

In `main.swift`, `activeVideoSender` and `hasSentKeyframe` are bare mutable variables
accessed from both the main thread and the encoder output callback (which runs on a
VideoToolbox internal thread). These lack any synchronization:
- `activeVideoSender` is set in the `onStreamingStart` callback and read in the encoder output callback
- `hasSentKeyframe` is read and written in the encoder output callback and could theoretically be reset from the main thread

In practice, the access pattern is set-once-then-read, but this is technically a data
race under the Swift memory model.

### Missing proper session state machine integration

`SessionStateMachine` defines formal states (idle, waitingForClient, negotiating,
streaming, disconnected) with a grace period timer, but `HostSession` has its own
independent `State` enum. The two are not connected -- `HostSession` does not use
`SessionStateMachine` at all. The state machine exists but is dead code relative
to the actual streaming logic.

### Lack of unit tests

The project has zero unit tests on both the Swift and Rust sides. The `smoke_test.swift`
is a runtime integration check (verifying API availability), not a unit test. Critical
components that should be tested include:
- Binary packet serialization/deserialization (both sides)
- Jitter buffer frame assembly (chunk ordering, completion, expiration)
- Protocol constant consistency between Swift and Rust
- Coordinate mapping
- Protobuf encoding/decoding

### Hard-coded port offsets

UDP ports are derived from the control port by fixed offsets:
- Video: `controlPort + 1`
- Input: `controlPort + 2`
- Cursor: `controlPort + 3`

These are set in `main.swift` (host side) and communicated in `ModeConfirm` (protocol
side). If the control port changes, the offsets apply unconditionally. There is no
mechanism to assign arbitrary ports or handle port conflicts. The `ModeConfirm` message
does carry `video_port`, `input_udp_port`, and `cursor_udp_port` fields, so the
protocol supports arbitrary ports, but the host implementation always uses fixed offsets.

### Stats reporting is placeholder

The client sends `Stats` messages every second, but `packet_loss_rate` and
`frame_drop_rate` are hardcoded to `0.0`. The comment notes "real implementation reads
from receiver atomics" but this is not implemented. The `BitrateAdapter` on the host
will therefore never reduce bitrate based on actual network conditions.

### No flow control between receiver and decoder

The video receiver thread uses blocking `mpsc::send` with a capacity-64 channel. If
the decoder cannot keep up (e.g., CPU spike, large keyframe burst), the receiver thread
will block, causing UDP packets to queue in the kernel socket buffer. If that overflows,
packets are silently lost at the OS level. There is no explicit backpressure signal
from decoder to receiver.
