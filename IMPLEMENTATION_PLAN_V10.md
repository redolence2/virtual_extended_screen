# RESC Plan v10 — Final Personal-Profile Contract (standalone; supersedes all prior plans)

| | |
|---|---|
| **Date** | 2026-07-29 |
| **Status** | The single normative document; incorporates review 9's V9.1 patch (final conditional approval: *"No further architecture review is warranted; the next review should be of generated artifacts and code."*). Architecture locked since review 7. This is intended as the **terminal paper artifact** — subsequent review targets are `proto/control.proto`, `docs/WIRE.md`, fixtures, and code. |
| **Deployment** | One user, Mac `192.168.50.125` ↔ Ubuntu `192.168.50.47`, trusted wired LAN, endpoints updated together from this repo. |
| **RESC commit** | `12b87d1` · host on unlisted macOS build `26A5388g` (§11.5 doctor-over-allowlist governs) |
| **Normative companions** | `proto/control.proto`, `docs/WIRE.md`, `proto/fixtures/*` — generated per the two-stage freeze in §12; divergence from this document is a defect. |

---

## 1. Trust model

The wired LAN between the two profile IPs is **trusted**; there is no authentication (`auth_mode = trusted_lan_none` logged at startup by both ends). All source-IP checks are accidental-peer guards, not authentication: host control listener accepts only the profile client IP; the client video listener validates the accepted socket's peer IP == profile Mac IP **before reading `VideoHello`**; both UDP receivers accept only their profile peer. Future authentication, if ever needed, will be specified as mutual — never a unilateral proof.

## 2. PersonalProfile

**The only T1 profile is `moyunfei-desk-1`.** (A future profile requires its own canonical artifact, codec/RA rules, launchers, and tests before it exists.)

**Literal canonical schema (normative; placeholder values marked `TBD-A0`/`TBD-A00` are replaced at the §12 stage-2 freeze; canonical form = UTF-8 JSON, keys sorted lexicographically, no whitespace, base-10 integers, NFC strings):**

```json
{
  "bitrate_bps": 20000000,            /* TBD-A0 */
  "codec": "hevc-main-8bit",
  "control_port": 9870,
  "cursor_udp_port": 9873,
  "decoder_backend": "TBD-A00",       /* "hevc_cuvid" | "sw-threads1-lowdelay", plus load-bearing config */
  "decoder_lag_bound_frames": 4,      /* TBD-A00 */
  "display_index": 0,
  "flow_window_frames": 1,            /* TBD-A0: 1 or 2 */
  "frame_reordering": false,
  "height_px": 1920,
  "host_ip": "192.168.50.125",
  "max_record_bytes": 2097184,        /* TBD-A0 */
  "move_udp_port": 9872,
  "output_deadline_ms": 200,          /* TBD-A00 */
  "profile_id": "moyunfei-desk-1",
  "protocol_version": 3,
  "refresh_hz": 60,
  "rotation": "portrait-neg90-render",
  "video_port": 9871,
  "client_ip": "192.168.50.47",
  "width_px": 1080
}
```

`profile_hash` = first 8 bytes of SHA-256 over the exact canonical bytes, used verbatim as opaque bytes. **`decoder_backend` is a categorical A0.0 result and is part of the hashed profile**; after the freeze, the doctor must open exactly that backend, and the client's current CUVID→software silent fallback is removed (backend init failure ⇒ `REQUIRED_NATIVE_API`, not a switch). `build_commit` = the full lowercase object ID from `git rev-parse HEAD` (never a short hash). Build rule: commit mismatch ⇒ deterministic failure; dirty ⇒ deterministic failure unless `--allow-dirty`, which tags every log record `dirty`.

## 3. Lifecycle

**States:** `Disconnected → Connecting → AwaitingRA → Streaming → Backoff → (Connecting | Failed)`.

**Ownership:** Mac control listener (9870) and Ubuntu video listener (9871) are process-owned and persist across sessions; **an idle listener waits indefinitely and consumes no retry budget**. Teardown closes accepted/session sockets, resets UDP session state, disposes encoder/decoder/queues, marks the render slot stale; never rebinds listeners; is idempotent (simultaneous EOF/error/fatal coalesce into one scheduled reconnect). **Ubuntu alone initiates control connections.** Per-profile advisory `flock` (Mac `~/Library/Application Support/RESC/<profile>.lock`, Ubuntu `~/.local/state/resc/<profile>.lock`) makes a second instance exit cleanly (`INSTANCE_LOCK_HELD`); no process is ever killed (the `pgrep`+`SIGKILL` sweeps are deleted).

**Handshake and run-ID adoption (candidate vs active — exact wording per review 9):**
1. Client connects to control (**per-attempt timeout: Ubuntu control-connect 3 s**). Host validates peer IP.
2. Host generates a nonzero random `session_run_id:u64` and **binds it as its `activeRun` upon sending the announce**.
3. Host → `HostProfileAnnounce` (Envelope run ID = that value).
4. Client, holding no active run, treats the announce's run ID as **`candidateRun`** while validating: framing/version → bounded canonical profile bytes parsed → SHA-256 prefix recomputed and required equal to the transmitted hash → profile bytes and build compared against local values.
5. **Accept:** client promotes `candidateRun → activeRun`, arms the video listener, replies `ProfileResult{accepted}`. **Reject:** client replies a bounded `ProfileResult{rejected, reject_code}` echoing `candidateRun` (never promoted), closes, and enters `Failed` locally — **the client does not initiate further connections after a deterministic rejection**.
6. Host validates the echoed client profile/build identically (the result carries the client's canonical bytes — both sides can log both profiles and differing keys).
7. Host dials video; client peer-IP check (§1); `VideoHello`/`Ack` per §5.1.
8. Only after `Ack(OK)`: capture transmission and input injection enabled; state `AwaitingRA`.

After promotion, **every control Envelope must satisfy `session_run_id == activeRun`**; ordinary payloads carry no run-ID field.

**Delivery honesty (corrects v9):** a *received* valid rejection is deterministic on both sides and consumes no retry budget. If the rejection is **lost**, the host observes EOF/timeout and can only classify that observation as transient — it may burn one transient attempt; the client, already `Failed`, will not reconnect, so the host's next attempts time out into its budget and terminate boundedly.

**Deadlines** *(init; expiry transient unless noted)*: Ubuntu control-connect attempt 3 s · announce→result 2 s · result→video-dial 1 s · video connect 2 s · `VideoHello`→`Ack` 2 s · encoder submit→callback 500 ms · first verified-RA frame 2 s · outstanding-frame ACK 250 ms · heartbeat silence 6 s. Every expiry logs timer, bound, observed value, and its `FatalCode` (§4 table).

**Failure classes:** deterministic ⇒ `Failed` immediately (profile/build/version mismatch incl. received `ProfileResult{rejected}`, `MALFORMED_FRAMING`, `PROTOCOL_VIOLATION`, `PROFILE_INVALID`, `RECORD_CAP_VIOLATION`, `ENCODER_PROPERTY`, `REQUIRED_NATIVE_API`, `PERMISSION_DENIED`); transient ⇒ `Backoff` (socket error/EOF, decode fatal, watchdog/deadline expiry).

**Backoff schedule:** 250 ms → 500 ms → 1 s → 2 s thereafter; 30 s of uninterrupted `Streaming` resets the schedule and clears the burst deque; the process total is never reset.

**Retry algorithm (defensive `>=` guards per review 9):** before each transient restart: prune deque entries older than 60 s → if `deque.count >= 5` **or** `processTotal >= 8` ⇒ `Failed` (`RETRY_BUDGET_EXHAUSTED`) → else append now and increment `processTotal`, exactly once.

## 4. Control protocol (protobuf v3; generated + typed mandatory; hand-rolled encode/scan forbidden)

Framing: `u32_le length` + `Envelope`. **Bounds rule (enforceable with generated protobuf, corrects v9):** (1) read the 4-byte prefix; (2) reject length > 64 KiB **before allocating the frame buffer**; (3) decode with generated protobuf; (4) validate field byte-lengths and semantic constraints **immediately after decode and before any state mutation, logging of contents, injection, or further copying**. Per-field caps: strings ≤ 256 B (**except `FatalReport.summary` ≤ 2 KiB, an explicit exception**), `profile_canonical` ≤ 4096 B, `profile_hash` exactly 8 B.

**Literal schema (lands verbatim in `proto/control.proto`):**

```proto
syntax = "proto3";
package resc.v3;

message Envelope {
  uint64 session_run_id  = 1;
  uint32 protocol_version = 2;   // field number 2; runtime value MUST equal 3
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
  reserved 67;                    // removed StatsSummary slot (correcting v9's mislabel)
}

message DisplaySettings { float warm_strength = 1; }
message KeyEvent  { uint32 hid_usage = 1; bool is_down = 2; uint32 modifiers = 3; }
message HostProfileAnnounce {
  bytes  profile_canonical = 1;   // <= 4096 B
  bytes  profile_hash = 2;        // exactly 8 B
  string build_commit = 3;        // full lowercase git object id
  bool   build_dirty = 4;
  reserved 5;                     // removed psk_proof (v8 field 5)
}
message ProfileResult {
  bool   accepted = 1;
  bytes  profile_canonical = 2;   // client's canonical bytes, always present
  bytes  profile_hash = 3;        // exactly 8 B
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
                      sint64 native_code = 4; string summary = 5; }  // summary <= 2048 B
message ReleaseInput { }
message Heartbeat    { uint64 t_mono_us = 1; }  // diagnostics; liveness uses receipt time

enum FatalCode {
  FATAL_UNSPECIFIED        = 0;
  PROFILE_MISMATCH         = 1;   // deterministic
  BUILD_MISMATCH           = 2;   // deterministic
  VERSION_MISMATCH         = 3;   // deterministic
  MALFORMED_FRAMING        = 4;   // deterministic
  RECORD_CAP_VIOLATION     = 5;   // deterministic
  ENCODER_PROPERTY         = 6;   // deterministic
  PROFILE_INVALID          = 7;   // deterministic — invalid local profile constants
  PROTOCOL_VIOLATION       = 8;   // deterministic — wrong direction/state, unknown enum/status/flag values
  REQUIRED_NATIVE_API      = 9;   // deterministic — failed required native probe (VD/SCK/VT/CoreBrightness/FFmpeg/SDL)
  PERMISSION_DENIED        = 21;  // deterministic — OS permission prevents the fixed profile from operating
  SOCKET_FAILURE           = 10;  // transient
  DECODE_FATAL             = 11;  // transient
  WATCHDOG_ACK_AGE         = 12;  // transient
  WATCHDOG_OUTPUT          = 13;  // transient
  WATCHDOG_HANDSHAKE       = 14;  // transient — control-connect / announce→result / result→dial / video-connect / Hello→Ack
  WATCHDOG_ENCODER         = 15;  // transient — encoder submit→callback
  WATCHDOG_HEARTBEAT       = 16;  // transient
  IDENTITY_FAILURE         = 17;  // transient
  RA_VERIFICATION          = 18;  // transient (one re-force, then session-fatal)
  WATCHDOG_FIRST_RA        = 22;  // transient — first verified-RA deadline
  RETRY_BUDGET_EXHAUSTED   = 19;  // terminal
  INSTANCE_LOCK_HELD       = 20;  // terminal
}
```

**Timer→code map (complete):** control-connect/announce/result/dial/Hello→Ack ⇒ `WATCHDOG_HANDSHAKE` · encoder callback ⇒ `WATCHDOG_ENCODER` · first-RA ⇒ `WATCHDOG_FIRST_RA` · ACK age ⇒ `WATCHDOG_ACK_AGE` · output ⇒ `WATCHDOG_OUTPUT` · heartbeat ⇒ `WATCHDOG_HEARTBEAT`. The classification table is machine-tested in both languages.

**Direction/state table (normative; any known payload in the wrong direction or state ⇒ `PROTOCOL_VIOLATION`; input received before video Ack is never injected):**

| Payload | Direction | Legal state |
|---|---|---|
| `HostProfileAnnounce` | host→client | first bootstrap message only |
| `ProfileResult` | client→host | bootstrap response only |
| `FrameAck` | client→host | `AwaitingRA`/`Streaming`, must name the **oldest outstanding ordinal (both window sizes)** |
| `DisplaySettings` | host→client | post-video-Ack |
| `KeyEvent`, `ButtonEvent`, `ScrollEvent`, `ReleaseInput` | client→host | post-video-Ack |
| `Heartbeat` | both | post-video-Ack |
| `ClockPing`/`ClockPong` | request/response | trace/doctor mode, after profile acceptance |
| `FatalReport` | both | once a candidate run ID is known |

**Message invariants (value-matrix — enforceable despite proto3 enum non-presence):** accepted `ProfileResult` ⇒ `reject_code == FATAL_UNSPECIFIED ∧ video_listener_ready == true`; rejected ⇒ `reject_code` is a known nonzero deterministic value ∧ `video_listener_ready == false`; any other combination, and any unknown enum numeric anywhere, ⇒ `PROTOCOL_VIOLATION`. An inbound `FatalReport` is classified by its code's frozen class to choose `Failed` vs `Backoff`. Decode semantics: generated protobuf's unknown-field ignoring and last-one-wins oneof are accepted; an absent payload ⇒ `PROTOCOL_VIOLATION`; no raw tag scanner exists.

**Boundary value validation (fail-closed, logged):** `button ∈ {0,1,2}` (the `CGMouseButton` force-unwrap is removed); unknown `hid_usage` ⇒ never injected + aggregated `unsupported_hid` diagnostic; `warm_strength` finite ∧ ∈ [0,1]; hashes exactly 8 B; run IDs nonzero; cursor `shape_id ∈ 0…15`, `cursor_scale` finite ∧ > 0.

## 5. Fixed binary wire (verbatim into `docs/WIRE.md`)

**Global rules:** all fixed-width integers and IEEE-754 floats in non-protobuf records are **little-endian**; magics are the literal bytes shown; reserved fields are zero on send, validated zero on receive; every UDP datagram has exactly its declared length (truncated/trailing ⇒ reject + count).

### 5.1 Video handshake and binding (completes review-9 §1)

`VideoHello` (32 B, host→client): `52 53 43 56` · `ver:u8=3` · `res:u8=0` · `len:u16=32` · `session_run_id:u64` · `profile_hash[8]` · `res:u64=0`.
`VideoHelloAck` (16 B): `52 53 43 41` · `ver:u8=3` · `status:u8` · `len:u16=16` · `session_run_id:u64`.

**Client returns `OK` only if all hold:** peer IP == profile Mac IP · magic/version/length exact · reserved zero · `session_run_id == activeRun` · `profile_hash == activeProfileHash` · no other video socket active for this run. A **structurally valid** Hello with wrong run/hash ⇒ `MISMATCH` (close; deterministic). A malformed Hello ⇒ `PROTOCOL_VIOLATION`. `BUSY` only for an already-active socket (transient); `INTERNAL` only for a local preparation failure (transient).
**Host accepts the Ack only if:** magic/version/length exact · status is a known value · Ack run ID == the dialed run. Unknown status or run mismatch ⇒ `PROTOCOL_VIOLATION`. **Only `OK` opens capture/input.**

### 5.2 Frame records

Header (exactly 32 B; `headerLen` must equal 32): `56 46` · `headerLen:u8=32` · `flags:u8` (bit0 keyframe-claim; unknown bits ⇒ `PROTOCOL_VIOLATION`) · `frameOrdinal:u64` (1…i64::MAX, session-local, +1 per AU) · `captureSeq:u32` · `contentCaptureTs_us:u64` (**the host continuous-monotonic microsecond epoch** — stated explicitly) · `res:u32=0` · `payloadLen:u32`. Validate `u64(headerLen)+u64(payloadLen)` checked/widened before allocation; > `max_record_bytes` ⇒ `RECORD_CAP_VIOLATION`. Payload = one Annex-B HEVC AU.

### 5.3 UDP records

Prefix (14 B): `52 45 53 43` · `ver:u8=3` · `type:u8` (`1` cursor, `2` move) · `session_run_id:u64`.
Mouse-move (26 B, client→host): prefix · `seq:u32` · `x:i32` · `y:i32`.
Cursor (43 B, host→client): prefix · body at absolute offsets 14–42: `seq:u32_le`(14) · `timestamp_us:u64_le`(18) · `x_px:i32_le`(26) · `y_px:i32_le`(30) · `shape_id:u8`(34) · `hotspot_x_px:u16_le`(35) · `hotspot_y_px:u16_le`(37) · `cursor_scale:f32_le`(39).

### 5.4 Input/cursor semantics (standalone; completes review-9 §8)

- **Coordinate space:** all mouse/button coordinates are **portrait StreamSpace pixels** (`0…1079 × 0…1919`). The client applies the inverse of its −90° render transform (including letterbox scale/offset) before sending; the host clamps to profile bounds and maps StreamSpace → global display coordinates using live `CGDisplayBounds` of the virtual display.
- **Scroll:** `ScrollEvent.dx/dy` are StreamSpace-oriented line/pixel deltas with natural-scrolling sign as produced by the client's SDL wheel events; under rotation the client swaps axes (`dx' = −dy, dy' = dx`) before sending; the host injects via pixel-unit scroll events, converting points via the display's pixel scale.
- **Cursor shapes 0–15 (frozen names):** 0 arrow · 1 ibeam · 2 crosshair · 3 openHand · 4 closedHand · 5 pointingHand · 6 resizeN · 7 resizeS · 8 resizeE · 9 resizeW · 10 resizeNS · 11 resizeEW · 12 resizeNESW · 13 resizeNWSE · 14 notAllowed · 15 wait.
- **Cursor fields:** `x_px/y_px` = hot-point position in StreamSpace; **hidden/off-display convention: `x_px == −1 ∧ y_px == −1`**; `hotspot_*` = offset of the hot point within the sprite (StreamSpace px); `cursor_scale` = sprite scale multiplier (1.0 in this profile).
- **Grab state machine (client-local; the host does not track grab):** initial state released/local → `Ctrl+Alt+G` consumed locally ⇒ **grabbed** (relative-mouse on) → only the grabbed state emits key/button/scroll/move traffic → `Ctrl+Alt+Esc` consumed locally ⇒ sends reliable `ReleaseInput`, returns to released; quit sends `ReleaseInput` before teardown when possible. Accepted looseness (recorded): a delayed pre-release UDP move may still move the pointer once after release — harmless disposable state; strict post-release pointer fencing is a non-goal.

### 5.5 Golden fixtures

Accepted + rejected `ProfileResult`; `VideoHello` + all four `VideoHelloAck` statuses; frame headers at min/max legal values (max uses the **measured** cap — stage-2, §12); move + cursor datagrams; malformed cases (bad length, nonzero reserved, unknown flag/status, length overflow) — each consumed by Swift **and** Rust tests.

## 6. Host pipeline

- **Capture:** ScreenCaptureKit at profile size, NV12, `showsCursor=false`; the callback only stores into the pending slot. FramePacer remains until T3's gates pass.
- **Slots:** `latestPendingCapture` (overwritten by every capture; consumed only when the flow gate opens; copied on consumption to `lastReplayableCapture`). Replays keep original `contentCaptureTs` with a fresh admission time. Gate-open drains the pending capture immediately — no new SCK callback required.
- **Pump:** one serial actor owns slots, `encodeInFlight ∈ {0,1}`, the outstanding-frame ledger, and the (queue-confined) force-keyframe flag. Wake events: capture arrival, encoder callback, `FrameAck`, connection transition. **Encoder callbacks are tagged with the `session_run_id` captured at submission and discarded when the tag no longer equals the active run** (corrected wording — no "generation" concept exists).
- **Encoder lifecycle:** create VT session → set **and read back** every load-bearing property (`RealTime=true`, `AllowFrameReordering=false`, profile/level, `AverageBitRate`, `MaxKeyFrameInterval=3600`, `MaxKeyFrameIntervalDuration=60`; from T3: `PrioritizeEncodingSpeedOverQuality=true`, `MaxFrameDelayCount=0`) → `PrepareToEncodeFrames` → only then connect-ready. Unsupported required property ⇒ `ENCODER_PROPERTY`. Honest keyframe statement: this configuration produces a ~60 s periodic safety IDR at 60 Hz, retained deliberately; cadence logged. Forced IDRs only at session start and RA re-force.
- **RA verification (encoder-output AU, pre-send):** RA iff NAL 19/20 (CRA 21 rejected); session-first AU must carry VPS(32)+SPS(33)+PPS(34); header claim must equal the parse; a forced non-RA output ⇒ one re-force then session failure (`RA_VERIFICATION`).
- **Cap enforcement:** an AU whose record would exceed `max_record_bytes` is rejected at the callback (never enters pending storage) ⇒ `RECORD_CAP_VIOLATION` with actual size, cap, frame type, read-back properties, recent AU-size stats.
- **Host watchdogs:** encoder submit→callback 500 ms; oldest-outstanding-frame age 250 ms — **scope: transport/acceptance stalls only** (accepted-but-never-emitted still ACKs; that case is the client's §8 output watchdog).

## 7. Flow control

- `flow_window_frames` init 1 (fixed 2 only if the A0 trial — real pair, selected backend, via the §12 measurement harness — cannot sustain 60 Hz; loopback is supplementary). **For both window sizes, `FrameAck.frame_ordinal` must equal the oldest outstanding ordinal**; anything else ⇒ `PROTOCOL_VIOLATION`; the sender frees oldest-first only.
- ACK point (final): after (a) exact-once decoder acceptance and (b) drain to classified `Again`. Not after output — decoders may need several accepted packets before first output; ACK-after-output at window 1 would deadlock startup. Drain `Error` ⇒ teardown, no ACK.
- **Send-EAGAIN retry (exact):** retain the exact packet → drain receive side → require drain progress or fail the session → resubmit the same packet until accepted exactly once → drain to `Again`; ACK exactly once, after both.
- Partial TCP writes are resumed by the single bounded writer per connection; only EOF/error tears down. `TCP_NODELAY` on both ends of both TCP connections.
- **Memory:** `flow_window_frames × max_record_bytes` bounds encoded transport records. Separately bounded: 2 raw capture buffers, 1 encode-in-flight, decoder internals (§8's honest statement), 1 owned decoded frame in the render slot.

## 8. Client pipeline

- **Deframe → decode:** one reader validates each record (§5) and feeds the decoder directly; no drop-capable intermediate queue.
- **Identity:** packet `pts = frameOrdinal`; output identity from `AVFrame.pts`, fallback `best_effort_timestamp`; both missing ⇒ unknown. FIFO/pts fidelity of the **selected backend** proven in A0.0; the backend + config are hashed into the profile (§2).
- Unknown/duplicate/reordered-ordinal output ⇒ discarded (never presented) + counted; 4th per session ⇒ `IDENTITY_FAILURE`.
- EAGAIN/EOF/error are distinct classified outcomes on both send and receive sides; `error_concealment` off; no gray-frame heuristics or recovery state machine — session-first frame is client-validated RA (§6 rules), everything after arrives in order exactly once; decode fatal ⇒ teardown.
- **Output watchdog:** per accepted ordinal, track emission; fail (`WATCHDOG_OUTPUT`) when `acceptedCount − emittedCount > decoder_lag_bound` or the oldest unresolved ordinal exceeds `output_deadline_ms` (both A0.0-measured).
- **Decoder memory (honest statement, corrects v9):** `decoder_lag_bound` bounds *unresolved ordinals*, not allocation; decoder surfaces/memory are bounded by the selected backend's configured/tested pool, whose size and observed usage are logged by doctor and soak runs.
- **Render handoff:** owned/ref-counted decoded frames into a latest-wins slot; older decoded frames dropped freely (counted; presentation is telemetry, never flow control).
- **T2 designs (to implement):** single render thread owning the one SDL context/event-pump/window/canvas/textures (double `sdl2::init` removed; `unsafe impl Send` and the `mem::forget` texture trick deleted; textures destroyed before the canvas); upload-once per new frame from plane pointers + strides; texture-resident cursor (zero uploads on cursor-only presents); `SDL_UpdateNVTexture` where available, I420 fallback; **packed cursor snapshot** — one `AtomicU64` `x:i16|y:i16|shape:u8|seq:u24` (low 24 bits of wire seq; newer ⇔ `d≠0 ∧ d<2²³`, `d=(a−b)&0xFFFFFF`), with **hotspot/scale held outside the atomic as per-shape constants** (declared immutable per `shape_id` in this profile — coherence is claimed for the (x, y, shape, seq) tuple only; corrects v9's overclaim).

## 9. Input channels & liveness

Control TCP: `KeyEvent` (client send wired; host HID→keycode map exists), `ButtonEvent` (validated), `ScrollEvent`, `ReleaseInput`, `Heartbeat` every 2 s both ways. Liveness: nothing on control for 6 s ⇒ `WATCHDOG_HEARTBEAT` teardown (⇒ release). Held keys/buttons with a quiet mouse are legal while heartbeats flow; **UDP contributes nothing to liveness**; there is no input-inactivity watchdog. `releaseAll()` triggers (wired, counts logged): control disconnect · heartbeat timeout · `ReleaseInput` · fatal error · injector teardown · process shutdown (signal handler sets a flag/wakes the loop — no CoreGraphics from async-signal context). UDP latest-wins per §5.3/§5.4 with comparator `newer(a,b) ⇔ d≠0 ∧ d<2³¹, d=(a−b) mod 2³²` and per-session `lastSeq` reset.

## 10. Clock & trace contract (diagnostics/A0 mode only)

Four-timestamp exchange: requester t1 → responder t2 (receive) → t3 (send) → requester t4. `offset = ((t2−t1)+(t3−t4))/2` (responder−requester); `delay = (t4−t1)−(t3−t2)`; i128 intermediates; signed i64 µs offset + u32 µs uncertainty (= delay/2); reject `delay < 0` or ≥ 5 ms; keep min-delay sample; resample every 10 s (trace mode), on reconnect, after wake (wake invalidates). Host: continuous-monotonic epoch; CoreMedia host time via bracketed calibration (`t_c1 → t_a → t_c2`, require `t_c2−t_c1 < 50 µs` else retry; anchor midpoint, uncertainty half-width); SCK PTS via `CMSyncConvertTime` to `CMClockGetHostTimeClock()`; nil `synchronizationClock` ⇒ labeled callback-time fallback. Software present-return ≠ photon time; final acceptance includes one optical comparison. Normal streaming consumes no cross-machine offsets.

## 11. Diagnostics & upgradeability

1. **JSONL logs** — Mac `~/Library/Logs/RESC/host.jsonl`, Ubuntu `~/.local/state/resc/client.jsonl`; 5 × 10 MiB rotation; `0600`; fatal events flushed synchronously (bounded) before exit. Fields per record: monotonic ts, wall ts, component, `session_run_id`, `profile_hash`, state before/after, frame ordinal when applicable, result, native domain/code/text, expected-vs-actual. Content: lifecycle transitions, failures, every timeout/retry decision, 10–30 s aggregates, final summary; never per-packet. No secrets, no frame payload bytes.
2. **Startup environment record** — commit (full object ID)/dirty, protocol version, full effective profile + hash, `auth_mode`, OS release/build/arch, peers/ports, codec/bitrate/cap/window/backend; Mac: SCK+VT availability; Ubuntu: kernel, FFmpeg/libavcodec, SDL, NVIDIA/CUDA versions.
3. **Native-call evidence** — every load-bearing call checked and logged: `CGVirtualDisplay` class/selector/creation; SCK permission/config/format/clock/start; every `VTSessionSetProperty` requested-vs-read-back; `PrepareToEncodeFrames`; `CoreBrightness`/Night Shift class/selector/returns (retained feature); FFmpeg discovery/creation/hw-device + `av_strerror` on send/receive; SDL init/renderer/texture/update; sockets with errno. **No silent fallback anywhere** (including the deleted decoder-backend fallback, §2).
4. **Doctors** — host: create/destroy the profile virtual display, create the profile encoder, set/read-back all properties, encode a bundled frame, verify RA NALs. Client: open **exactly the profile backend**, decode a bundled RA sample, verify pts/FIFO, create the required SDL texture, report input capability. `--diagnose-peer`: version/profile/build exchange + one frame + one ACK + correlated logs, injection disabled. Reports: `doctor_report_v: 1`; exit codes 0 pass · 2 environment · 3 native-API · 4 peer. T1-exit checks: forced native failures ⇒ actionable records; all timeouts ⇒ their stable codes; lock denial kills nothing; lockfiles tracked.
5. **Upgrade policy (doctor-over-allowlist):** unlisted OS versions are never rejected on version string; startup/doctor probes required APIs/selectors/properties/returns; success ⇒ operate + log unlisted version; a failed required probe stops the feature/process with `REQUIRED_NATIVE_API` (or `PERMISSION_DENIED`) and persistent evidence; no native failure may silently change the fixed profile. The OS allowlist is advisory logging.
6. **Dependencies** — remove the `.gitignore` lockfile rules; commit `Cargo.lock` + `Package.resolved`; pin `ffmpeg-next`/`-sys` exactly; doctor records runtime library versions.

## 12. Phases & the two-stage freeze

- **A0.0 — go now.** Codegen toolchain (generated code as a normal package target; CI regen-clean) + typed dispatch; §11 logging/environment/native-evidence; doctors; lockfiles + instance lock; clock bridges (trace mode); trace joining on the untouched v1 wire (frameID + host map); decoder pts/FIFO experiment selecting `decoder_backend` and measuring `decoder_lag_bound`/`output_deadline_ms`; canonicalization mechanism vs the marked placeholder; **the A0 measurement harness** — a narrow framed-TCP/ACK rig exercising the §5.2/§7 path on the real encoder + selected decoder (built here so the window measurement is not circular).
  **Stage-1 freeze (end of A0.0):** `control.proto` · structural `WIRE.md` · generated Swift/Rust verified · placeholder canonicalization tests · profile-independent malformed fixtures.
- **A0 — go after trace-joining + clock-uncertainty evidence.** On the real pair: AU histogram → `max_record_bytes`; latency (software + one optical spot-check); capture fps; stop-and-wait window trial via the harness → `flow_window_frames`; commit bitrate.
  **Stage-2 freeze (end of A0):** the three A0 constants + two A0.0 bounds + `decoder_backend` committed → final canonical profile + golden hash → profile-bearing `ProfileResult` fixtures and measured-cap max-record fixtures regenerated → all final fixtures pass in Swift and Rust.
- **T1 — entry gate (all):** stage-2 freeze complete · `control.proto` + `WIRE.md` match this document at the same commit · both doctors pass on the real endpoints. **Scope:** §§1–9, 11; delete UDP video (chunking, assembler, jitter-buffer crate, gray detector, recovery machinery), mDNS, kill sweeps, `--client` (debug override only); FramePacer survives until T3.
- **T2 (approved):** §8's designs. **T3:** capture/encoder A/B (1/60 vs 1/120; queueDepth 3/5/8; speed-priority + `MaxFrameDelayCount=0` read back); final-static gate (window closed → one last capture → window opens → that exact `captureSeq` displays with no new SCK callback); then FramePacer removal. **T4:** VirtualDisplay resilience (HiDPI re-assert, mirror-set detach, arrangement memory), sleep/wake, long soak.

## 13. Validation gates (delta on v9's table, which carries forward; changed/added rows only)

| Gate | Result required | Phase |
|---|---|---|
| Hello binding | structurally-valid Hello with wrong run/hash ⇒ `MISMATCH` + deterministic; malformed ⇒ `PROTOCOL_VIOLATION`; unknown Ack status / run mismatch ⇒ `PROTOCOL_VIOLATION`; only `OK` opens capture/input | T1 |
| Direction/state | every §4-table violation ⇒ `PROTOCOL_VIOLATION`; pre-Ack input never injected | T1 |
| ProfileResult matrix | all four accept/reject field combinations enforced; unknown enum numerics rejected | T1 |
| Control bounds (reworded) | frame cap rejected **before frame allocation**; per-field caps rejected **immediately after generated decode, before state mutation/logging/injection/copying**; 2 KiB summary exception honored | T1 |
| Candidate/active run | rejection echoes candidateRun without promotion; post-promotion Envelope equality enforced; lost-rejection path: host burns ≤ 1 transient attempt, client stays `Failed`, host terminates boundedly | T1 |
| Retry guards | `deque.count >= 5` / `processTotal >= 8` boundary behavior exact (incl. restored/corrupted-state defensiveness) | T1 |
| Profile literalness | canonical artifact matches the §2 schema exactly; backend field present; doctor opens exactly that backend; no decoder fallback path exists | A0→T1 |
| Two-stage freeze | stage-1 artifacts exist at A0.0 end; stage-2 regeneration after constants; T1 blocked on stage-2 | A0.0/A0 |
| Harness | the A0 window trial runs on the A0.0 harness against the real encoder+backend | A0 |
| ACK oldest (w=1 too) | a `FrameAck` naming anything but the oldest outstanding ordinal ⇒ `PROTOCOL_VIOLATION` at both window sizes | T1 |
| Grab semantics | released state emits no input traffic; G/Esc consumed locally; Esc/quit sends `ReleaseInput`; delayed pre-release move is accepted-harmless (documented) | T1 |
| Coordinate/scroll transform | inverse-rotation mapping and scroll axis-swap verified against a reference fixture set | T1 |
| Cursor snapshot scope | coherence asserted for (x,y,shape,seq); hotspot/scale served from per-shape constants | T2 |
| Timer→code | every named timer emits its mapped `FatalCode`; classification table passes in both languages | T1 |

## 14. Review-9 checklist dispositions

| V9.1 item | Resolved |
|---|---|
| Hello/Ack binding | §5.1 + gate |
| Direction/state table | §4 + gate |
| ProfileResult/FatalReport/unknown-enum semantics | §4 invariants + gate |
| Reservations (`HostProfileAnnounce` field 5; Envelope 67 relabel) | §4 schema |
| Complete fatal-code mapping | §4 enum additions (7/8/9/21/22) + timer→code map |
| Enforceable bounds | §4 bounds rule + reworded gate |
| Candidate/active, lost rejection, connect-timeout wording, `>=` guards | §3 |
| Literal profile schema + backend + `build_commit` definition | §2 |
| Two-stage freeze | §12 |
| Named A0 harness | §12 (A0.0 deliverable) |
| Grab/coordinate/scroll/shape/hidden-cursor semantics | §5.4 |
| Overclaim corrections (decoder memory, callback wording, T2 cursor coherence) | §8, §6, §8 |

## 15. Go/no-go

- **A0.0: go now.** Stage-1 freeze at its end, against this document.
- **A0: go** on trace/clock evidence; stage-2 freeze at its end.
- **T1: go** on the §12 entry gate. T2–T4 approved.
- Per review 9, this patch closes the contract series. **The next review target is generated artifacts and code — not another plan.**
