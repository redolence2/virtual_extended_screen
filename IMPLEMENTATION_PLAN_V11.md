# RESC Plan v11 — Final Contract (standalone, terminal)

| | |
|---|---|
| **Date** | 2026-07-30 |
| **Status** | **Terminal.** The single normative document; applies review 10's terminal errata (all nine items, §15). Architecture locked since review 7; reviews 9 and 10 both closed with "the next substantive review should inspect `proto/control.proto`, `docs/WIRE.md`, fixtures, and code — not another architecture plan." **This series ends here: no v12 plan is to be produced. Findings against this contract are recorded as errata in the repo (`CONTRACT_ERRATA.md`) or folded into the generated artifacts — not into a new plan document.** |
| **Prime directive (user)** | As simple as possible · single user · just make it work. Anything not needed for that is out of scope. |
| **Deployment** | One user; Mac `192.168.50.125` ↔ Ubuntu `192.168.50.47`; trusted wired LAN; both ends built from the same commit of this repo. |
| **RESC commit** | `12b87d1` · host on unlisted macOS build `26A5388g` (§11.5 governs) |
| **Normative companions (generated)** | `proto/control.proto` · `docs/WIRE.md` · `proto/fixtures/*` — per the §12 two-stage freeze; divergence from this document is a defect. |

---

## 1. Trust model

The wired LAN between the two profile IPs is **trusted**; no authentication (`auth_mode = trusted_lan_none`, logged at startup by both ends). Source-IP checks are accidental-peer guards only: host control listener accepts only the profile client IP; client video listener validates peer IP == profile Mac IP **before reading `VideoHello`**; both UDP receivers accept only their profile peer.

## 2. PersonalProfile

**The only profile is `moyunfei-desk-1`.** Canonical form: UTF-8 JSON, keys sorted lexicographically, no whitespace, base-10 integers, NFC strings. **The exact placeholder bytes that are hashed (valid JSON, single line — replace `TBD-A00`/measured values at the §12 stage-2 freeze; explanations live here, never in the bytes):**

```json
{"bitrate_bps":20000000,"client_ip":"192.168.50.47","codec":"hevc-main-8bit","control_port":9870,"cursor_udp_port":9873,"decoder_backend":"TBD-A00","decoder_lag_bound_frames":4,"display_index":0,"flow_window_frames":1,"frame_reordering":false,"height_px":1920,"host_ip":"192.168.50.125","max_record_bytes":2097184,"move_udp_port":9872,"output_deadline_ms":200,"profile_id":"moyunfei-desk-1","protocol_version":3,"refresh_hz":60,"rotation":"portrait-neg90-render","video_port":9871,"width_px":1080}
```

Field meanings: `bitrate_bps`, `max_record_bytes`, `flow_window_frames` are A0-measured; `decoder_backend`, `decoder_lag_bound_frames`, `output_deadline_ms` are A0.0-measured. `profile_hash` = first 8 bytes of SHA-256 over the exact canonical bytes, opaque.

**`decoder_backend` is a closed configuration ID** — one of exactly two values, each completely defined (flags, thread count, low-delay setting, surface-pool size) in `docs/WIRE.md` §Backend at stage-1: `"cuvid-lowdelay"` (hevc_cuvid, `AV_CODEC_FLAG_LOW_DELAY`, pinned surface count) or `"sw1-lowdelay"` (software, `threads=1`, low-delay). **Stage 2 selects one of the two IDs; it may not invent new hashed keys or new IDs.** After the freeze the client opens exactly that backend; init failure ⇒ `REQUIRED_NATIVE_API` (the CUVID→software silent fallback is deleted).

`build_commit` = full lowercase `git rev-parse HEAD`. Commit mismatch ⇒ deterministic failure. Dirty ⇒ deterministic failure unless `--allow-dirty` (all records tagged `dirty`).

## 3. Lifecycle

**States:** `Disconnected → Connecting → AwaitingRA → Streaming → Backoff → (Connecting | Failed)`.

**Ownership:** Mac control listener (9870) and Ubuntu video listener (9871) are process-owned, persist across sessions; **an idle listener waits indefinitely and consumes no retry budget**. Teardown closes session sockets, resets UDP state, disposes encoder/decoder/queues, marks the render slot stale; never rebinds listeners; is idempotent (simultaneous failure events coalesce into one reconnect). **Ubuntu alone initiates control connections.** Per-profile advisory `flock` (Mac `~/Library/Application Support/RESC/<profile>.lock`; Ubuntu `~/.local/state/resc/<profile>.lock`); a second instance exits cleanly (`INSTANCE_LOCK_HELD`); nothing is ever killed.

**Handshake:**
1. Client connects (Ubuntu control-connect attempt timeout 3 s). Host validates peer IP.
2. Host generates nonzero random `session_run_id:u64`, **binds it as `activeRun` upon sending the announce**.
3. Host → `HostProfileAnnounce`.
4. Client (no active run) holds the announce's ID as **`candidateRun`** while validating: framing/version → bounded canonical bytes parsed → SHA-256 prefix recomputed == transmitted hash → profile bytes + build vs local.
5. **Accept:** promote `candidateRun → activeRun`, arm the video listener, reply `ProfileResult{accepted}`. **Reject:** reply bounded `ProfileResult{rejected, reject_code}` echoing `candidateRun` (never promoted), close, enter `Failed` locally; the client initiates no further connections.
6. Host validates the echoed client profile/build identically (both sides can log both profiles and differing keys).
7. Host dials video (client peer-IP check) → `VideoHello`/`Ack` (§5.1).
8. Only after `Ack(OK)`: capture + input enabled; `AwaitingRA`.

Post-promotion, every control Envelope must satisfy `session_run_id == activeRun`; ordinary payloads carry no run-ID field.

**Lost-rejection lifecycle (exact — corrects v10):** (1) the client's rejection is lost; (2) the client stays `Failed` and never reconnects; (3) the host observes EOF/announce-timeout and charges **exactly one transient restart**; (4) after backoff the host returns to its process-owned idle listener; (5) it waits **indefinitely, consuming no further budget**, until the user fixes/restarts the client. Persistent logs on both sides plus the client's deterministic failure record provide the diagnosis. (A received rejection is deterministic on both sides and consumes no budget.)

**Deadlines** *(init; expiry transient unless noted)*: Ubuntu control-connect attempt 3 s · announce→result 2 s · result→video-dial 1 s · video connect 2 s · Hello→Ack 2 s · encoder submit→callback 500 ms · first verified-RA frame 2 s · outstanding-frame ACK 250 ms · heartbeat silence 6 s. Every expiry logs timer, bound, observed value, and its `FatalCode`.

**Failure classes:** deterministic ⇒ `Failed` (profile/build/version mismatch incl. received rejection, `MALFORMED_FRAMING`, `PROTOCOL_VIOLATION`, `PROFILE_INVALID`, `RECORD_CAP_VIOLATION`, `ENCODER_PROPERTY`, `REQUIRED_NATIVE_API`, `PERMISSION_DENIED`); transient ⇒ `Backoff` (socket error/EOF, decode fatal, watchdog/deadline expiry).

**Backoff:** 250 ms → 500 ms → 1 s → 2 s thereafter; 30 s uninterrupted `Streaming` resets the schedule and clears the burst deque; the process total is never reset.
**Retry algorithm:** prune deque entries older than 60 s → if `deque.count >= 5` or `processTotal >= 8` ⇒ `Failed` (`RETRY_BUDGET_EXHAUSTED`) → else append + increment, exactly once.

## 4. Control protocol (protobuf v3; generated + typed mandatory)

Framing `u32_le length + Envelope`. Bounds: reject length > 64 KiB **before frame allocation**; decode with generated protobuf; validate field lengths + semantics **immediately post-decode, before any state mutation/logging-of-contents/injection/copying**. Field caps: strings ≤ 256 B (exception: `FatalReport.summary` ≤ 2 KiB), `profile_canonical` ≤ 4096 B, hashes exactly 8 B.

**Schema (verbatim into `proto/control.proto`):**

```proto
syntax = "proto3";
package resc.v3;

message Envelope {
  uint64 session_run_id  = 1;
  uint32 protocol_version = 2;   // runtime value MUST equal 3
  oneof payload {
    DisplaySettings     display_settings      = 32;
    KeyEvent            key_event             = 40;
    HostProfileAnnounce host_profile_announce = 60;
    ProfileResult       profile_result        = 61;
    FrameAck            frame_ack             = 62;
    ButtonEvent         button_event          = 63;
    ScrollEvent         scroll_event          = 64;
    ClockPing           clock_ping            = 65;
    ClockPong           clock_pong            = 66;
    FatalReport         fatal_report          = 68;
    ReleaseInput        release_input         = 69;
    Heartbeat           heartbeat             = 70;
  }
  reserved 67;                    // removed StatsSummary slot
}

message DisplaySettings { float warm_strength = 1; }
message KeyEvent  { uint32 hid_usage = 1; bool is_down = 2; uint32 modifiers = 3; }
message HostProfileAnnounce {
  bytes  profile_canonical = 1;   // <= 4096 B
  bytes  profile_hash = 2;        // exactly 8 B
  string build_commit = 3;        // full lowercase git object id
  bool   build_dirty = 4;
  reserved 5;                     // removed psk_proof
}
message ProfileResult {
  bool   accepted = 1;
  bytes  profile_canonical = 2;
  bytes  profile_hash = 3;
  string build_commit = 4; bool build_dirty = 5;
  FatalCode reject_code = 6;
  bool   video_listener_ready = 7;
}
message FrameAck    { uint64 frame_ordinal = 1; }
message ButtonEvent { uint32 button = 1; bool is_down = 2; sint32 x_px = 3; sint32 y_px = 4; uint32 modifiers = 5; }
message ScrollEvent { sint32 dx = 1; sint32 dy = 2; }
message ClockPing   { uint64 t1_mono_us = 1; uint32 seq = 2; }
message ClockPong   { uint64 t1_mono_us = 1; uint64 t2_mono_us = 2; uint64 t3_mono_us = 3; uint32 seq = 4; }
message FatalReport { FatalCode code = 1; string component = 2; string native_domain = 3;
                      sint64 native_code = 4; string summary = 5; }
message ReleaseInput { }
message Heartbeat    { uint64 t_mono_us = 1; }

enum FatalCode {
  FATAL_UNSPECIFIED        = 0;
  PROFILE_MISMATCH         = 1;   // deterministic
  BUILD_MISMATCH           = 2;   // deterministic
  VERSION_MISMATCH         = 3;   // deterministic
  MALFORMED_FRAMING        = 4;   // deterministic
  RECORD_CAP_VIOLATION     = 5;   // deterministic
  ENCODER_PROPERTY         = 6;   // deterministic
  PROFILE_INVALID          = 7;   // deterministic
  PROTOCOL_VIOLATION       = 8;   // deterministic
  REQUIRED_NATIVE_API      = 9;   // deterministic
  SOCKET_FAILURE           = 10;  // transient
  DECODE_FATAL             = 11;  // transient
  WATCHDOG_ACK_AGE         = 12;  // transient
  WATCHDOG_OUTPUT          = 13;  // transient
  WATCHDOG_HANDSHAKE       = 14;  // transient
  WATCHDOG_ENCODER         = 15;  // transient
  WATCHDOG_HEARTBEAT       = 16;  // transient
  IDENTITY_FAILURE         = 17;  // transient
  RA_VERIFICATION          = 18;  // transient (one re-force, then session-fatal)
  RETRY_BUDGET_EXHAUSTED   = 19;  // terminal
  INSTANCE_LOCK_HELD       = 20;  // terminal
  PERMISSION_DENIED        = 21;  // deterministic
  WATCHDOG_FIRST_RA        = 22;  // transient
}
```

**Timer→code map:** control-connect/announce/result/dial/Hello-Ack ⇒ `WATCHDOG_HANDSHAKE` · encoder callback ⇒ `WATCHDOG_ENCODER` · first-RA ⇒ `WATCHDOG_FIRST_RA` · ACK age ⇒ `WATCHDOG_ACK_AGE` · output ⇒ `WATCHDOG_OUTPUT` · heartbeat ⇒ `WATCHDOG_HEARTBEAT`. Classification machine-tested in both languages.

**Direction/state table** (violation ⇒ `PROTOCOL_VIOLATION`; pre-Ack input never injected):

| Payload | Direction | Legal state |
|---|---|---|
| `HostProfileAnnounce` | host→client | first bootstrap message only |
| `ProfileResult` | client→host | bootstrap response only |
| `FrameAck` | client→host | `AwaitingRA`/`Streaming`; must name the oldest outstanding ordinal (both window sizes) |
| `DisplaySettings` | host→client | post-video-Ack |
| `KeyEvent`/`ButtonEvent`/`ScrollEvent`/`ReleaseInput` | client→host | post-video-Ack |
| `Heartbeat` | both | post-video-Ack |
| `ClockPing`/`ClockPong` | request/response | trace/doctor mode after profile acceptance |
| `FatalReport` | both | once a candidate run ID is known |

**Message invariants:** accepted `ProfileResult` ⇒ `reject_code == FATAL_UNSPECIFIED ∧ video_listener_ready == true`; rejected ⇒ known nonzero deterministic `reject_code ∧ ready == false`; any other combination or unknown enum numeric ⇒ `PROTOCOL_VIOLATION`. **`FatalReport.code` must be a known nonzero `FatalCode`; `FATAL_UNSPECIFIED` or unknown numerics in a `FatalReport` ⇒ `PROTOCOL_VIOLATION`** (inbound transition is thereby total: deterministic/terminal ⇒ `Failed`; transient ⇒ `Backoff`; zero/unknown ⇒ `PROTOCOL_VIOLATION` ⇒ `Failed`). Generated protobuf's unknown-field ignoring and last-one-wins oneof are accepted; absent payload ⇒ `PROTOCOL_VIOLATION`; no raw scanner.

**Boundary validation (fail-closed, logged):** `button ∈ {0,1,2}` (force-unwrap removed); unknown `hid_usage` ⇒ never injected + aggregated diagnostic; `warm_strength` finite ∈ [0,1]; hashes exactly 8 B; run IDs nonzero; cursor `shape_id ∈ 0…15`; `cursor_scale` finite > 0.

## 5. Fixed binary wire (verbatim into `docs/WIRE.md`)

**Global:** all fixed-width integers/floats little-endian; magics are the literal bytes shown; reserved fields zero (validated); every UDP datagram exactly its declared length.

### 5.1 Video handshake

`VideoHello` (32 B, host→client): `52 53 43 56` · `ver=3:u8` · `res=0:u8` · `len=32:u16` · `session_run_id:u64` · `profile_hash[8]` · `res=0:u64`.
`VideoHelloAck` (16 B): `52 53 43 41` · `ver=3:u8` · `status:u8` · `len=16:u16` · `session_run_id:u64`.

**Ack status bytes (frozen):** `0` OK (accepted; video/input may open) · `1` MISMATCH (structurally valid Hello, wrong active run or profile — deterministic) · `2` BUSY (a video socket is already active — transient) · `3` INTERNAL (local preparation failed — transient). **Any other byte ⇒ `PROTOCOL_VIOLATION`.**

Client returns `OK` only if all hold: peer IP == Mac IP · magic/version/length exact · reserved zero · `session_run_id == activeRun` · `profile_hash == activeProfileHash` · no other active video socket for this run. Malformed Hello ⇒ `PROTOCOL_VIOLATION`. Host accepts the Ack only if magic/version/length exact · known status · Ack run == dialed run; only `OK` opens capture/input.

**Stale-dial run tagging (new):** every asynchronous video connect/Hello/Ack operation captures its run ID at initiation. Host requires `ack.run == dialedRun == activeRun` before mutating state; client requires `hello.run == activeRun`. A late socket/response for an old run is closed and logged **without failing the newer active run**; after answering a wrong-run Hello with `MISMATCH`, the client keeps awaiting the correct current-run Hello until the handshake deadline.

### 5.2 Frame records

Header (exactly 32 B; `headerLen` must equal 32): `56 46` · `headerLen=32:u8` · `flags:u8` (bit0 keyframe-claim; unknown bits ⇒ `PROTOCOL_VIOLATION`) · `frameOrdinal:u64` (1…i64::MAX, session-local, +1 per AU) · `captureSeq:u32` · `contentCaptureTs_us:u64` (host continuous-monotonic µs epoch) · `res=0:u32` · `payloadLen:u32`. Validate `u64(headerLen)+u64(payloadLen)` checked/widened pre-allocation; > `max_record_bytes` ⇒ `RECORD_CAP_VIOLATION`. Payload = one Annex-B HEVC AU.

### 5.3 UDP records

Prefix (14 B): `52 45 53 43` · `ver=3:u8` · `type:u8` (`1` cursor · `2` move) · `session_run_id:u64`.
Move (26 B, client→host): prefix · `seq:u32` · `x:i32` · `y:i32`.
Cursor (43 B, host→client): prefix · `seq:u32`(14) · `timestamp_us:u64`(18) · `x_px:i32`(26) · `y_px:i32`(30) · `shape_id:u8`(34) · `hotspot_x_px:u16`(35) · `hotspot_y_px:u16`(37) · `cursor_scale:f32`(39).

### 5.4 Input/cursor semantics

- **Coordinates:** portrait StreamSpace pixels (`0…1079 × 0…1919`). Client applies the inverse −90° render transform (incl. letterbox scale/offset) before sending; host clamps to profile bounds and maps via live `CGDisplayBounds`.
- **Scroll (one exact unit, frozen):** one wire unit = **one raw SDL wheel step**; the Mac interprets one step as a fixed pixel-scroll quantum of **N = 10 pixels** (constant in `WIRE.md`), injected as pixel-unit scroll events converted to points via the display's pixel scale. Rotation: client swaps axes (`dx' = −dy, dy' = dx`) before sending. Sign: client forwards SDL's sign unchanged (natural scrolling as SDL reports it); host applies as-is. Integer arithmetic; saturate at i32. The sign/axis convention is pinned by reference fixtures.
- **Cursor shapes 0–15:** 0 arrow · 1 ibeam · 2 crosshair · 3 openHand · 4 closedHand · 5 pointingHand · 6 resizeN · 7 resizeS · 8 resizeE · 9 resizeW · 10 resizeNS · 11 resizeEW · 12 resizeNESW · 13 resizeNWSE · 14 notAllowed · 15 wait.
- **Cursor fields:** `x/y` = hot-point in StreamSpace; hidden ⇔ `x == −1 ∧ y == −1`; **hotspot/scale are per-shape constants in this profile — the constant table is: hotspot (0, 0) and scale 1.0 for every shape** (matching the current sender, which only ever emits those values). The wire fields remain (the 43-B layout is frozen); the client validates received values against the constants and, on mismatch, counts + substitutes the constants. The T2 packed snapshot claims coherence for `(x, y, shape, seq)` only.
- **Grab (client-local; host does not track it):** initial released/local → `Ctrl+Alt+G` consumed locally ⇒ grabbed (relative mouse) → only grabbed emits key/button/scroll/move → `Ctrl+Alt+Esc` consumed locally ⇒ sends reliable `ReleaseInput`, returns to released; quit sends `ReleaseInput` when possible. Accepted looseness: one delayed pre-release UDP move may move the pointer once — harmless.

### 5.5 Golden fixtures

Accepted + rejected `ProfileResult`; `VideoHello` + all four Ack statuses; frame headers at min/max legal values (max uses the measured cap — stage 2); move + cursor datagrams; scroll sign/axis reference fixtures; malformed cases (bad length, nonzero reserved, unknown flag/status, length overflow) — all consumed by Swift **and** Rust tests.

## 6. Host pipeline

Capture: SCK at profile size, NV12, `showsCursor=false`; callback only stores. FramePacer survives until T3. Slots: `latestPendingCapture` (latest-wins; consumed when the flow gate opens; copied to `lastReplayableCapture` on consumption; replays keep original `contentCaptureTs`, fresh admission time; gate-open drains immediately with no new SCK callback). Pump: one serial actor owns slots, `encodeInFlight ∈ {0,1}`, the outstanding ledger, and the queue-confined force-KF flag; wakes on capture/callback/ACK/connection events; **encoder callbacks are tagged with the submission's `session_run_id` and discarded when it no longer equals the active run**. Encoder lifecycle: create → set **and read back** every load-bearing property (`RealTime=true`, `AllowFrameReordering=false`, profile/level, `AverageBitRate`, `MaxKeyFrameInterval=3600`, `MaxKeyFrameIntervalDuration=60`; from T3 `PrioritizeEncodingSpeedOverQuality=true`, `MaxFrameDelayCount=0`) → `PrepareToEncodeFrames` → connect-ready; unsupported required property ⇒ `ENCODER_PROPERTY`. Honest keyframe statement: ~60 s periodic safety IDR at 60 Hz, deliberate; cadence logged; forced IDRs only at session start and RA re-force. RA verification (encoder-output AU, pre-send): RA iff NAL 19/20 (CRA rejected); session-first AU carries VPS+SPS+PPS; header claim == parse; forced non-RA ⇒ one re-force then `RA_VERIFICATION`. Cap enforcement at the callback (oversize never enters pending storage) ⇒ `RECORD_CAP_VIOLATION` with size/cap/type/read-back properties/recent stats. Watchdogs: encoder 500 ms; oldest-outstanding age 250 ms (**transport/acceptance stalls only** — accepted-but-never-emitted is the client's watchdog).

## 7. Flow control

`flow_window_frames` init 1 (2 only if the A0 harness trial on the real pair + selected backend cannot sustain 60 Hz). **Both window sizes: `FrameAck` must name the oldest outstanding ordinal**; else `PROTOCOL_VIOLATION`; oldest-first freeing only. ACK after (a) exact-once decoder acceptance + (b) drain to classified `Again` — never after output (multi-packet-before-first-output decoders would deadlock at window 1); drain `Error` ⇒ teardown, no ACK. Send-EAGAIN retry: retain exact packet → drain → require progress or fail → resubmit until accepted exactly once → drain to `Again`; ACK once, after both. Partial TCP writes resumed by the single bounded writer; only EOF/error tears down. `TCP_NODELAY` both ends of both TCP connections. Memory: `flow_window_frames × max_record_bytes` bounds encoded transport records; separately bounded: 2 raw capture buffers, 1 encode-in-flight, decoder internals (§8), 1 owned decoded frame.

## 8. Client pipeline

Deframe → decode directly (no drop-capable queue). Identity: `pts = frameOrdinal`; output identity from `AVFrame.pts` (fallback `best_effort_timestamp`); unknown/duplicate/reordered output ⇒ discarded + counted, 4th ⇒ `IDENTITY_FAILURE`. EAGAIN/EOF/error classified both directions; `error_concealment` off; no gray heuristics or recovery machine — session-first is client-validated RA, then in-order exactly-once; decode fatal ⇒ teardown. Output watchdog: `acceptedCount − emittedCount > decoder_lag_bound` or oldest unresolved > `output_deadline_ms` ⇒ `WATCHDOG_OUTPUT`. Decoder memory honesty: the lag bound limits unresolved ordinals; surfaces/memory are bounded by the backend's pinned pool (logged by doctor/soak). Render handoff: owned frames → latest-wins slot; older decoded frames dropped freely (counted; telemetry only). **T2 designs:** one render thread owning the single SDL context/pump/window/canvas/textures (double-init removed; `unsafe impl Send` + `mem::forget` deleted; textures die before canvas); upload-once from planes+strides; texture-resident cursor (zero uploads on cursor-only presents); `SDL_UpdateNVTexture` where available, I420 fallback; packed cursor snapshot `x:i16|y:i16|shape:u8|seq:u24` (low 24 bits of wire seq; newer ⇔ `d≠0 ∧ d<2²³`), hotspot/scale from the §5.4 constant table.

## 9. Input & liveness

Control TCP: `KeyEvent` (client send wired; host HID map exists), `ButtonEvent`, `ScrollEvent`, `ReleaseInput`, `Heartbeat` every 2 s both ways. Nothing on control for 6 s ⇒ `WATCHDOG_HEARTBEAT` teardown (⇒ release). Held keys with a quiet mouse are legal while heartbeats flow; UDP contributes nothing to liveness; no input-inactivity watchdog. `releaseAll()` triggers (wired, counts logged): control disconnect · heartbeat timeout · `ReleaseInput` · fatal error · injector teardown · process shutdown (signal handler sets a flag only — no CoreGraphics in async-signal context). UDP latest-wins comparator `newer(a,b) ⇔ d≠0 ∧ d<2³¹, d=(a−b) mod 2³²`; per-session `lastSeq` reset.

## 10. Clock & trace (diagnostics/A0 mode only)

Four-timestamp exchange; `offset=((t2−t1)+(t3−t4))/2`, `delay=(t4−t1)−(t3−t2)`; i128 intermediates; signed i64 µs offset + u32 µs uncertainty (=delay/2); reject `delay<0` or ≥5 ms; keep min-delay sample; resample 10 s (trace mode)/reconnect/wake (wake invalidates). Host: continuous-monotonic epoch; CoreMedia host time via bracketed calibration (`t_c1→t_a→t_c2`, `t_c2−t_c1<50 µs` else retry; midpoint anchor, half-width uncertainty); SCK PTS via `CMSyncConvertTime` → `CMClockGetHostTimeClock()`; nil sync-clock ⇒ labeled callback-time fallback. Software present ≠ photon time; final acceptance includes one optical check. Normal streaming consumes no cross-machine offsets.

## 11. Diagnostics & upgradeability

1. **JSONL logs** — `~/Library/Logs/RESC/host.jsonl` / `~/.local/state/resc/client.jsonl`; 5×10 MiB rotation; `0600`; fatal events flushed synchronously before exit. Per record: monotonic ts, wall ts, component, run ID, profile hash, state before/after, frame ordinal where applicable, result, native domain/code/text, expected-vs-actual. Lifecycle/failures/every timeout+retry decision/10–30 s aggregates/final summary; never per-packet; no secrets or frame bytes.
2. **Startup environment record** — full commit/dirty, protocol version, effective profile + hash, `auth_mode`, OS release/build/arch, peers/ports, codec/bitrate/cap/window/backend; Mac SCK+VT availability; Ubuntu kernel/FFmpeg/SDL/NVIDIA versions.
3. **Native-call evidence** — every load-bearing call checked+logged: `CGVirtualDisplay` class/selector/creation; SCK permission/config/format/clock/start; every `VTSessionSetProperty` requested-vs-read-back; `PrepareToEncodeFrames`; `CoreBrightness` (Night Shift retained); FFmpeg discovery/creation/hw-device + `av_strerror`; SDL init/renderer/texture/update; sockets with errno. **No silent fallback anywhere.**
4. **Doctors** — host: create/destroy profile display, create profile encoder, set/read-back all properties, encode a bundled frame, verify RA NALs. Client: open exactly the profile backend, decode a bundled RA sample, verify pts/FIFO, create the required SDL texture, report input capability. `--diagnose-peer`: profile/build exchange + one frame + one ACK + correlated logs, injection disabled. `doctor_report_v: 1`; exit codes 0/2/3/4 (pass/environment/native/peer). T1-exit: forced native failures ⇒ actionable records; all timeouts ⇒ stable codes; lock denial kills nothing; lockfiles tracked.
5. **Upgrade policy (doctor-over-allowlist):** unlisted OS versions never rejected on version string; probes decide; success ⇒ operate + log; failed required probe ⇒ `REQUIRED_NATIVE_API`/`PERMISSION_DENIED` with persistent evidence; no native failure silently changes the profile. OS allowlist = advisory logging.
6. **Dependencies** — remove `.gitignore` lockfile rules; commit `Cargo.lock` + `Package.resolved`; pin `ffmpeg-next`/`-sys` exactly; doctor records runtime versions.

## 12. Phases & two-stage freeze

- **A0.0 — go now.** Codegen (normal package target, CI regen-clean) + typed dispatch; §11 logging/doctors; lockfiles + instance lock; clock bridges; trace joining on the untouched v1 wire (frameID + host map); decoder experiment selecting the backend ID and measuring `decoder_lag_bound`/`output_deadline_ms`; canonicalization vs the §2 placeholder bytes; the **A0 measurement harness** (framed-TCP/ACK rig on the real encoder + selected backend). **Stage-1 freeze (end):** `control.proto` · structural `WIRE.md` (incl. the two backend config IDs and scroll constant) · generated Swift/Rust verified · placeholder canonicalization tests · profile-independent malformed fixtures.
- **A0 — go after trace-joining + clock-uncertainty evidence.** Real-pair baseline: AU histogram → `max_record_bytes`; latency (software + one optical spot-check); capture fps; harness window trial → `flow_window_frames`; commit bitrate. **Stage-2 freeze (end):** all six profile values committed (three A0 + two A0.0 bounds + backend ID) → final canonical bytes + golden hash → profile-bearing and measured-cap fixtures regenerated → all fixtures pass in both languages.
- **T1 — entry: stage-2 complete · `control.proto` + `WIRE.md` match this document at one commit · both doctors pass on the real endpoints.** Scope: §§1–9, 11; delete UDP video (chunking/assembler/jitter-buffer/gray detector/recovery), mDNS, kill sweeps, `--client` (debug override only).
- **T2:** §8 designs. **T3:** capture/encoder A/B (1/60 vs 1/120; queueDepth 3/5/8; speed-priority + `MaxFrameDelayCount=0` read back); final-static gate; FramePacer removal. **T4:** VirtualDisplay resilience (HiDPI re-assert, mirror-set detach, arrangement memory), sleep/wake, soak.

## 13. Validation gates (complete and standalone)

| Gate | Result required | Phase |
|---|---|---|
| Trace joining + clock | joined capture→present traces; signed-arithmetic/negative-delay tests; bracketed calibration bounds | A0.0 |
| Token/FIFO | selected backend proven pts-faithful (or FIFO under threads=1) with induced delay | A0.0 |
| Lag bounds | `decoder_lag_bound` + `output_deadline_ms` measured | A0.0 |
| Canonicalization | Swift and Rust reproduce the §2 placeholder hash byte-exactly | A0.0 |
| Instance lock | second instance exits cleanly; nothing killed | A0.0 |
| Harness | the window trial runs on the A0.0 harness against the real encoder+backend | A0 |
| Baseline + constants | all six profile values measured/selected on the real pair | A0 |
| Two-stage freeze | stage-1 artifacts at A0.0 end; stage-2 regeneration after constants; T1 blocked on stage-2 | A0.0/A0 |
| Final fixtures | final profile artifact + every §5.5 fixture passes in both languages | A0→T1 |
| Profile literalness | canonical artifact matches §2 bytes-modulo-measured-values; backend is one of the two closed IDs; no decoder fallback path exists | T1 |
| Handshake | client-initiated; host-allocated nonzero run; announce→result→dial→Hello strictly ordered; step-skipping rejected | T1 |
| Candidate/active run | rejection echoes candidateRun unpromoted; post-promotion Envelope equality; lost rejection ⇒ host charges exactly one transient then idles indefinitely; client stays `Failed` | T1 |
| Two-sided mismatch | rejection carries client profile bytes; both sides log both profiles + differing keys; deterministic (no retry burn on receipt) | T1 |
| Hello binding | §5.1 equality list gates `OK`; wrong run/hash ⇒ `MISMATCH` deterministic; malformed ⇒ `PROTOCOL_VIOLATION`; unknown Ack status/run ⇒ `PROTOCOL_VIOLATION`; only `OK` opens capture/input | T1 |
| Ack status bytes | exactly bytes 0–3 accepted with frozen meanings/classes; others ⇒ `PROTOCOL_VIOLATION` | T1 |
| Stale dial | late old-run sockets/Hello/Ack closed+logged without failing the newer run; client keeps awaiting the current-run Hello after answering `MISMATCH` | T1 |
| Video peer IP | non-Mac peer closed before `VideoHello` processing | T1 |
| Legacy rejection | any pv1 message/datagram ⇒ protocol error, never acted on | T1 |
| Decode semantics | absent payload ⇒ error; duplicate oneof last-one-wins without scanner; version ≠ 3 rejected | T1 |
| Direction/state | every §4-table violation ⇒ `PROTOCOL_VIOLATION`; pre-Ack input never injected | T1 |
| ProfileResult/FatalReport matrix | all accept/reject combinations enforced; zero/unknown `FatalReport.code` ⇒ `PROTOCOL_VIOLATION`; inbound classification total | T1 |
| Control bounds | frame cap pre-allocation; per-field caps immediately post-decode before mutation/log/inject/copy; 2 KiB summary exception | T1 |
| ACK-after-drain | drain `Error` ⇒ teardown, no ACK; ACK names the accepted ordinal | T1 |
| ACK oldest | non-oldest ordinal ⇒ `PROTOCOL_VIOLATION` at both window sizes; oldest-first freeing | T1 |
| EAGAIN retry | identical packet drained/resubmitted, accepted exactly once, ACKed exactly once | T1 |
| Age watchdog | transport/acceptance stalls trip 250 ms; no unbounded catch-up | T1 |
| Output watchdog | accept-drain-no-emit trips the client bound; host watchdog demonstrably blind to it | T1/T2 |
| Cap violation | oversize AU ⇒ structured fatal, never enters pending storage, no loop | T1 |
| RA/session-first | claim == parse; parameter sets present; forced non-RA ⇒ one re-force then fail; client rejects an invalid first record | T1 |
| Deadline coverage | every state exits by success or its named deadline; idle listeners consume no budget | T1 |
| Retry accounting | prune→check→append order; `>=5`/`>=8` boundaries exact; streaming clears deque only; `RETRY_BUDGET_EXHAUSTED` emitted | T1 |
| Backoff | 250/500/1000/2000 ms; reset only after 30 s streaming | T1 |
| Timer→code | every named timer emits its mapped code; classification passes in both languages | T1 |
| Stuck input | drop/delay/kill injection leaves zero pressed keys/buttons; all six release triggers exercised with counts | T1 |
| Long-hold safety | 60 s held key with heartbeats never auto-released; heartbeat loss releases ≤ 6 s; `ReleaseInput` immediate | T1 |
| Grab semantics | released state emits nothing; G/Esc consumed locally; Esc/quit sends `ReleaseInput`; delayed pre-release move accepted-harmless | T1 |
| Value validation | out-of-range button/hid/warm-strength/shape/scale/hash-length/zero-run rejected fail-closed with diagnostics | T1 |
| Coordinate/scroll transform | inverse-rotation mapping, axis swap, sign, and the N=10 quantum verified against reference fixtures | T1 |
| UDP hygiene | wrong length/reserved/magic/run/source rejected; comparator edges (equal/forward/wrap/stale) exact | T1 |
| Diagnostics ops | rotation, `0600`, fatal flush, stable codes, doctor schemas/exit codes, forced-failure evidence | T1 |
| Ordinal identity | induced decoder delay preserves mapping; unknown-ordinal output never presented; 4th ⇒ teardown | T1/T2 |
| Cursor residency + coherence | cursor-only presents: zero uploads; packed snapshot never yields mixed-sequence tuples; hotspot/scale served from constants | T2 |
| Render ownership | one SDL owner; sanitizer-clean soak; texture-before-canvas destruction | T2 |
| Final static | last capture before idle displays with FramePacer off | T3 |
| Latency acceptance | software e2e + optical vs A0 baseline; pointer bounded by current-op + present | T3 |

## 14. Convergence declaration

Ten plans and ten reviews have converged: architecture locked (reviews 7–10), contract complete (this document), and the reviewer's own instruction — twice — is that the next review inspects generated artifacts and code. **This plan series is closed.** Any future finding against this contract is recorded as a dated entry in `CONTRACT_ERRATA.md` beside the generated artifacts, or fixed directly in `control.proto`/`WIRE.md`/fixtures/code with a log-visible rationale. Simplicity remains the tiebreak for anything this document underspecifies: choose the smallest behavior that keeps the gates green.

## 15. Review-10 errata dispositions

| Item | Resolved |
|---|---|
| Valid canonical profile bytes | §2 (exact minified sorted JSON; explanations outside the bytes) |
| Backend config schema | §2 (two closed IDs, fully defined in `WIRE.md` at stage-1; stage-2 selects, never invents) |
| Ack status byte mapping | §5.1 (0–3 frozen; others ⇒ violation) |
| Standalone gate table | §13 (complete merge; no external table referenced) |
| Lost-rejection lifecycle | §3 (one host transient charge, then indefinite idle listening; "bounded termination" claim removed) |
| `FatalReport{FATAL_UNSPECIFIED}` | §4 (zero/unknown ⇒ `PROTOCOL_VIOLATION`; inbound transition total) |
| Stale video-dial run tags | §5.1 + gate |
| One scroll unit | §5.4 (1 SDL wheel step = 10-pixel quantum; sign/axis pinned by fixtures) |
| Cursor hotspot/scale constants | §5.4 (all-shapes constant table (0,0)/1.0; wire fields validated against it; coherence scope unchanged) |

## 16. Go

**A0.0: go now.** Stage-1 freeze at its end against this document · **A0: go** on trace/clock evidence; stage-2 freeze at its end · **T1: go** on the §12 entry gate · T2–T4 approved. The next artifact is code.
