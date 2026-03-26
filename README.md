# Remote Extended Screen

Use a monitor connected to a nearby Ubuntu machine as a wireless extended display for your Mac. The Mac gets a virtual third monitor that streams to the Ubuntu machine over LAN.

```
┌─────────────┐          LAN           ┌─────────────┐
│   Mac Host   │  ───── H.264/HEVC ──► │   Ubuntu     │
│              │  ◄──── cursor/input ── │   Client     │
│ Virtual      │                        │              │
│ Display #3   │   TCP control + mDNS   │ SDL2 Window  │
│ (1920x1080)  │  ◄────────────────────►│ (fullscreen) │
└─────────────┘                        └──────┬───────┘
                                              │ HDMI/DP
                                         ┌────┴────┐
                                         │ Monitor  │
                                         │    B     │
                                         └─────────┘
```

## Requirements

**Mac (host)**
- macOS 14+ (Sonoma) on Apple Silicon
- Screen Recording permission
- Accessibility permission (for input injection)

**Ubuntu (client)**
- Ubuntu 22.04+ with Xorg
- System packages: `sudo apt install libavcodec-dev libavformat-dev libavutil-dev libavfilter-dev libswscale-dev libswresample-dev libsdl2-dev libva-dev protobuf-compiler pkg-config clang libclang-dev`
- Rust toolchain: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- Wired LAN recommended (Wi-Fi works for 1080p)

## Quick Start

### 1. Build

**Mac:**
```bash
cd mac-host
swift build
```

**Ubuntu:**
```bash
cd ubuntu-client
cargo build --release
```

### 2. Run the smoke test (Mac)

```bash
swift smoke_test.swift
```

Verifies all APIs are available on your macOS version.

### 3. Start the host FIRST (Mac)

**Important:** Run from the `mac-host/` directory (where `Package.swift` lives).

```bash
cd <project-root>/mac-host

# HEVC (recommended — 40% less bandwidth)
swift run remote-display-host 1920 1080 60 --hevc --client <UBUNTU_IP>

# H.264 (fallback, 20 Mbps)
swift run remote-display-host 1920 1080 60 --client <UBUNTU_IP>

# 4K
swift run remote-display-host 3840 2160 60 --hevc --client <UBUNTU_IP>
```

A new display called "Remote Extended Screen" appears in **System Settings > Displays**. Arrange it next to your other monitors.

### 4. Start the client SECOND (Ubuntu)

**Important:** Run from the `ubuntu-client/` directory (where `Cargo.toml` lives).

```bash
cd <project-root>/ubuntu-client

# Auto-discover Mac via mDNS
cargo run --release

# Or specify Mac IP directly (recommended)
cargo run --release -- --host <MAC_IP>

# Select which monitor to display on (if multiple)
cargo run --release -- --host <MAC_IP> --display 1
```

The Ubuntu monitor goes fullscreen showing the Mac virtual display content.

### 5. Use it

- **Drag windows** from your Mac onto the virtual display — they appear on the Ubuntu monitor
- **Mouse cursor** tracks automatically when you move it onto the virtual display
- **Cmd+Tab** works, but use it from the Mac built-in display to avoid quirks

### 6. Stop

Press `Ctrl+C` on either the Mac host or Ubuntu client.

## Command Reference

### Mac Host

```
remote-display-host [WIDTH] [HEIGHT] [REFRESH] [OPTIONS]

Arguments:
  WIDTH              Display width in pixels (default: 1920)
  HEIGHT             Display height in pixels (default: 1080)
  REFRESH            Refresh rate in Hz (default: 60)

Options:
  --client <IP>      Ubuntu client IP address (required for streaming)
  --port <PORT>      Control port (default: 9870)
  --hevc             Use HEVC encoding (lower bandwidth, recommended)
  --bitrate <BPS>    Override bitrate in bps (e.g. 30000000 for 30Mbps)
  --dump-h264 <PATH> Dump raw H.264/HEVC stream to file for debugging
  --no-virtual-display  Disable virtual display (kill switch)
```

### Ubuntu Client

```
remote-display-client [OPTIONS]

Options:
  -H, --host <IP>       Mac host IP (skip mDNS discovery)
  -p, --port <PORT>     Control port (default: 9870)
      --width <W>       Preferred width (default: 1920)
      --height <H>      Preferred height (default: 1080)
      --display <N>     SDL2 display index (default: 0)
      --no-flash        Skip 2-second blue flash test on startup
      --dump-h264 <PATH>  Dump received bitstream to file
      --headless        No rendering, just receive + decode (for testing)
  -h, --help            Show help
```

## Network Ports

| Port | Protocol | Purpose |
|------|----------|---------|
| 9870 | TCP | Control channel (mode negotiation, stats) |
| 9871 | UDP | Video stream (chunked H.264/HEVC frames) |
| 9872 | UDP | Input events (mouse, scroll) |
| 9873 | UDP | Cursor position updates |

## Codecs

| Codec | Bitrate (1080p) | Bitrate (4K) | Encode | Decode | Flag |
|-------|----------------|-------------|--------|--------|------|
| H.264 High | 20 Mbps | 50 Mbps | ~6ms | ~5ms | (default) |
| HEVC Main | 12 Mbps | 30 Mbps | ~6ms | ~2ms | `--hevc` |

HEVC is recommended — same encode speed, faster decode, 40% less bandwidth.

## Troubleshooting

**"CGVirtualDisplay API not available"**
- Requires macOS 14+ on Apple Silicon. Run `swift smoke_test.swift` to check.

**"Screen Recording permission not granted"**
- System Settings → Privacy & Security → Screen Recording → enable the app/Terminal.

**"Accessibility permission NOT granted"**
- System Settings → Privacy & Security → Accessibility → enable the app/Terminal.
- Required for cursor tracking and input injection.

**Display doesn't appear in System Settings**
- The virtual display needs `sizeInMillimeters` to register properly. This is set automatically.
- Check the log for "Display X in CG online list: YES".

**No video on Ubuntu / "Address already in use"**
- Kill stale processes: `pkill -9 remote-display` on both machines.
- Wait a few seconds for ports to release, then restart.

**Mosaic/corruption in video**
- Ensure both sides are built from the same commit (header size mismatch causes corruption).
- Try H.264 if HEVC has issues: remove `--hevc` flag.

**Low FPS on virtual display**
- macOS only sends new frames when content changes. Move windows or play video on the virtual display for higher FPS.
- Idle desktops typically get 2-10 fps; active content gets 30-60 fps.

**Cmd+Tab selects wrong app**
- This is a macOS multi-display behavior. Keep cursor on the built-in display when using Cmd+Tab.

## Architecture

```
Mac Host                          Ubuntu Client
─────────                         ─────────────
CGVirtualDisplay (private API)    UDP Video Receiver
       ↓                                ↓
ScreenCaptureKit (NV12, 60fps)    Jitter Buffer (frame assembly)
       ↓                                ↓
VideoToolbox H.264/HEVC           ffmpeg H.264/HEVC decode
       ↓                                ↓
UDP Chunked Sender ──────────►    SDL2 Fullscreen Render
       ↑                                ↓
CursorTracker (120Hz) ──────►    Cursor Overlay
       ↑                                │
TCP Control Channel ◄────────►   mDNS Discovery
```

## Project Structure

```
remote_extended_screen/
├── proto/                    # Protobuf definitions (shared)
│   ├── control.proto         # Session, pairing, mode negotiation
│   ├── cursor.proto          # Cursor shape enums
│   ├── input.proto           # Input event types
│   └── video.proto           # Video constants reference
├── mac-host/                 # Swift package (macOS)
│   ├── Package.swift
│   └── Sources/
│       ├── RemoteDisplayHost/  # Main app
│       └── VirtualDisplay/     # CGVirtualDisplay Obj-C bridge
├── ubuntu-client/            # Rust workspace
│   ├── src/main.rs
│   └── crates/
│       ├── protocol/         # Protobuf + binary packet types
│       ├── net-transport/    # UDP/TCP networking
│       ├── jitter-buffer/    # Frame assembly
│       ├── video-decode/     # ffmpeg H.264/HEVC decode
│       ├── renderer/         # SDL2 fullscreen + cursor
│       └── input-capture/    # SDL2 input (grab/release)
├── smoke_test.swift          # Post-OS-update validation
├── Makefile                  # Build orchestration
└── plan.md                   # Full architecture spec (v5)
```

## License

Personal use tool. Not affiliated with Apple or Canonical.
