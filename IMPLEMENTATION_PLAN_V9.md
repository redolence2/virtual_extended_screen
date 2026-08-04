# RESC Plan v9 — Complete Personal-Profile Contract (standalone; supersedes all prior plans)

| | |
|---|---|
| **Date** | 2026-07-29 |
| **Status** | **The single normative document.** Nothing herein is defined by reference to v2–v8 — every retained rule is restated (review-8 §1). Prior plans/reviews are rationale-history only. Architecture is locked (reviews 7 & 8): fixed profile · control TCP (Mac listens) + video TCP (Ubuntu listens) · UDP only for latest-state mouse-move/cursor · fixed 1–2-frame stop-and-wait · full-session reconnect · reliable discrete input · diagnostics-first. Do not restore negotiation, generations, credit ledgers, adaptive ladders, or UDP-video recovery. |
| **Incorporates** | `IMPLEMENTATION_PLAN_review_v8.md` (conditional go). All twelve V8.1 checklist items resolved (§14). |
| **Normative companions (generated in A0.0, frozen per §12)** | `proto/control.proto` (protocol v3) · `docs/WIRE.md` (binary layouts) · `proto/fixtures/*` (golden bytes). Divergence between them and this document is a defect. |
| **RESC commit** | `12b87d1` · host on unlisted macOS build `26A5388g` (see §11.5 upgrade policy) |

---

## 1. Trust model (normative)

The physical wired LAN between `192.168.50.125` (Mac) and `192.168.50.47` (Ubuntu) is **trusted**. There is no authentication: `psk_proof` is **removed** (its field number reserved); both endpoints log `auth_mode = trusted_lan_none` at startup. Source-IP validation everywhere below is an **accidental-peer guard, not authentication**: the host accepts control connections only from the profile's client IP; **the client's video listener accepts a socket only if the peer IP equals the profile's Mac IP (checked before reading `VideoHello`)**; both UDP receivers accept only their profile peer. If authentication is ever needed, it will be specified as mutual authentication in a future revision — never a unilateral proof.

## 2. PersonalProfile

| Field | Value |
|---|---|
| `profile_id` | `moyunfei-desk-1` — **the only profile in T1**. (Additional profiles are out of scope until each has its own canonical artifact, codec/RA rules, launchers, and tests; naming ghost profiles defines nothing.) |
| `protocol_version` | `3` |
| Mac / Ubuntu IPv4 | `192.168.50.125` / `192.168.50.47` |
| Control / video TCP | `9870` (Mac listens) / `9871` (Ubuntu listens) |
| UDP move / cursor | `9872` (host receives) / `9873` (client receives) |
| Stream | `1080×1920@60`, portrait; client SDL display `0`, −90° render path |
| Codec | HEVC Main, 8-bit, `AllowFrameReordering = false` (read back, asserted) |
| `bitrate_bps` · `max_record_bytes` · `flow_window_frames` · `decoder_lag_bound` · `output_deadline_ms` | **A0-measured** (placeholders until then: 20 Mbps · 2 MiB+32 · 1 · 4 frames · 200 ms) |

**Canonicalization:** UTF-8 JSON, keys sorted lexicographically, no whitespace, base-10 integers, NFC strings → `proto/fixtures/profile.canonical.json`. `profile_hash` = first 8 bytes of SHA-256 over those exact bytes, **used verbatim as opaque bytes everywhere** (never an integer).

**Fixture ordering (corrects v8):** A0.0 builds and tests the canonicalization *mechanism* against a clearly-marked placeholder artifact. The **final** artifact and golden hash are frozen only after A0 commits all five measured constants; Swift and Rust fixtures then run against the final bytes. T1 cannot start before that (§12).

**Build rule:** endpoints exchange `build_commit` + `build_dirty`. Commit mismatch ⇒ deterministic failure. Dirty ⇒ deterministic failure unless `--allow-dirty`, which tags every log record `dirty`.

## 3. Lifecycle

**States:** `Disconnected → Connecting → AwaitingRA → Streaming → Backoff → (Connecting | Failed)`.

**Ownership:** the Mac's control listener and the Ubuntu video listener are process-owned and persist across sessions (an idle listener consumes no retry budget and may wait indefinitely). Teardown closes accepted/session sockets, resets UDP session state, disposes encoder/decoder/queues, marks the render slot stale — it never rebinds the fixed listeners. Teardown is idempotent: simultaneous EOF/error/fatal events coalesce into one scheduled reconnect. **Ubuntu alone initiates each control connection.** A per-profile advisory `flock` (Mac `~/Library/Application Support/RESC/<profile>.lock`, Ubuntu `~/.local/state/resc/<profile>.lock`) rejects a second instance with a clear message; **no process killing** (the current `pgrep`+`SIGKILL` sweeps are deleted).

**Handshake:**
1. Client connects to control (per-attempt timeout 3 s). Host validates peer IP.
2. Host generates a **nonzero** random `session_run_id:u64` (regenerated per accepted connection).
3. Host → `HostProfileAnnounce` (Envelope run ID = new ID; §4).
4. Client, holding no active run: accepts only a valid announce with a nonzero Envelope run ID → validates framing/version → parses the **bounded** canonical profile bytes → recomputes the SHA-256 prefix and requires it to equal the transmitted hash → compares profile bytes and build against local values.
5. Success ⇒ client binds `activeRun := that ID`, arms the video listener, replies `ProfileResult{accepted…}`. Failure ⇒ client replies a bounded `ProfileResult{rejected, code…}` **under the proposed run ID**, then closes, deterministically (no retry-budget consumption on either side — the explicit rejection is what lets the host classify it deterministically instead of as an announce timeout).
6. Host validates the client's echoed profile/build the same way (the response carries the client's canonical bytes — **both** sides can log both profiles and the differing keys).
7. Host dials video (client peer-IP check per §1) → `VideoHello`/`Ack`.
8. Only after `Ack(OK)`: capture transmission and input injection enabled; state `AwaitingRA`.

**Run-ID rule:** after step 3, staleness is enforced solely by `Envelope.session_run_id == activeRun`. Ordinary payloads carry no run-ID field.

**Deadlines** *(init values; expiry = transient unless noted)*: control connect/accept 3 s · announce→result 2 s · result→video-dial 1 s · video connect 2 s · `VideoHello`→`Ack` 2 s · encoder submit→callback 500 ms · first verified-RA frame 2 s · outstanding-frame ACK 250 ms · heartbeat silence 6 s. Every expiry logs the timer, bound, and observed value. `VideoHelloAck` status mapping: `MISMATCH` deterministic; `BUSY`, `INTERNAL` transient.

**Failure classes:** **Deterministic ⇒ `Failed` immediately** (profile/build/version mismatch incl. `ProfileResult{rejected}`, malformed framing, invalid profile constant, record-cap violation, required encoder property unsupported). **Transient ⇒ `Backoff`** (socket error/EOF, decoder error, watchdog/deadline expiry).

**Backoff schedule (new):** 250 ms → 500 ms → 1 s → 2 s for every subsequent attempt. 30 s of uninterrupted `Streaming` resets the schedule to 250 ms **and clears the burst deque; the process-total counter is never reset**.

**Retry algorithm (exact):** before each transient restart: prune deque entries older than 60 s → if 5 entries remain **or** processTotal == 8 ⇒ `Failed` (fatal report) → else append now to the deque and increment processTotal, exactly once per restart.

## 4. Control protocol (protobuf v3; generated + typed mandatory — hand-rolled encode/scan forbidden)

Framing: `u32_le length` + `Envelope` bytes; max frame **64 KiB**, checked before allocation. Field bounds: strings ≤ 256 B, `FatalReport.summary` ≤ 2 KiB, profile bytes ≤ 4 KiB.

**Literal declaration (normative; lands verbatim in `proto/control.proto`):**

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
  reserved 67;                    // psk era, never shipped
}

message DisplaySettings { float warm_strength = 1; }                 // Night Shift
message KeyEvent  { uint32 hid_usage = 1; bool is_down = 2; uint32 modifiers = 3; }
message HostProfileAnnounce {
  bytes  profile_canonical = 1;   // ≤ 4096 B
  bytes  profile_hash = 2;        // exactly 8 B
  string build_commit = 3; bool build_dirty = 4;
}
message ProfileResult {           // client → host, two-sided diagnostics
  bool   accepted = 1;
  bytes  profile_canonical = 2;   // client's canonical bytes, always present
  bytes  profile_hash = 3;        // exactly 8 B
  string build_commit = 4; bool build_dirty = 5;
  FatalCode reject_code = 6;      // set iff !accepted
  bool   video_listener_ready = 7; // true iff accepted
}
message FrameAck    { uint64 frame_ordinal = 1; }
message ButtonEvent { uint32 button = 1; bool is_down = 2; sint32 x_px = 3; sint32 y_px = 4; uint32 modifiers = 5; }
message ScrollEvent { sint32 dx = 1; sint32 dy = 2; }
message ClockPing   { uint64 t1_mono_us = 1; uint32 seq = 2; }
message ClockPong   { uint64 t1_mono_us = 1; uint64 t2_mono_us = 2; uint64 t3_mono_us = 3; uint32 seq = 4; }
message FatalReport { FatalCode code = 1; string component = 2; string native_domain = 3;
                      sint64 native_code = 4; string summary = 5; }
message ReleaseInput { }
message Heartbeat    { uint64 t_mono_us = 1; }  // diagnostics; liveness uses receipt time

enum FatalCode {                  // stable; values frozen; classification in comments
  FATAL_UNSPECIFIED        = 0;
  PROFILE_MISMATCH         = 1;   // deterministic
  BUILD_MISMATCH           = 2;   // deterministic
  VERSION_MISMATCH         = 3;   // deterministic
  MALFORMED_FRAMING        = 4;   // deterministic
  RECORD_CAP_VIOLATION     = 5;   // deterministic
  ENCODER_PROPERTY         = 6;   // deterministic
  SOCKET_FAILURE           = 10;  // transient
  DECODE_FATAL             = 11;  // transient
  WATCHDOG_ACK_AGE         = 12;  // transient
  WATCHDOG_OUTPUT          = 13;  // transient
  WATCHDOG_HANDSHAKE       = 14;  // transient
  WATCHDOG_ENCODER         = 15;  // transient
  WATCHDOG_HEARTBEAT       = 16;  // transient
  IDENTITY_FAILURE         = 17;  // transient
  RA_VERIFICATION          = 18;  // transient (one re-force then session-fatal)
  RETRY_BUDGET_EXHAUSTED   = 19;  // terminal
  INSTANCE_LOCK_HELD       = 20;  // terminal
}
```

**Decode semantics (enforceable, corrects v8):** generated protobuf ignores unknown fields and applies last-one-wins to duplicate oneof members — both accepted; **no raw tag scanner exists**. Rules: `protocol_version == 3`; profile/build equality per §3; the decoded oneof must contain **one recognized payload** — an absent payload is a protocol error.

**Value validation at every untrusted boundary (fail-closed, logged):** `ButtonEvent.button ∈ {0 left, 1 right, 2 middle}` — anything else rejected before injection (the current `CGMouseButton(rawValue:)!` force-unwrap is removed); unknown `KeyEvent.hid_usage` (no map entry) ⇒ never injected, aggregated `unsupported_hid` diagnostic; `DisplaySettings.warm_strength` finite ∧ ∈ [0,1]; `profile_hash` fields exactly 8 bytes; run IDs nonzero; cursor `shape_id` ∈ defined range (0–15), `cursor_scale` finite ∧ > 0.

## 5. Fixed binary wire (all layouts land verbatim in `docs/WIRE.md`)

**Global rules:** every fixed-width integer and IEEE-754 float in non-protobuf RESC records is **little-endian**; magic values are the literal bytes listed; every reserved field is zero on send and validated zero on receive; **every UDP datagram must have exactly its declared length** — truncated or trailing bytes ⇒ reject + count.

`VideoHello` (32 B, host→client): `52 53 43 56` · `ver:u8=3` · `res:u8=0` · `len:u16=32` · `session_run_id:u64` · `profile_hash[8]` · `res:u64=0`.
`VideoHelloAck` (16 B): `52 53 43 41` · `ver:u8=3` · `status:u8` (`0 OK · 1 MISMATCH · 2 BUSY · 3 INTERNAL`) · `len:u16=16` · `session_run_id:u64`.

Frame record header (**exactly 32 B; `headerLen` must equal 32**): `56 46` · `headerLen:u8=32` · `flags:u8` (bit0 keyframe-claim; any unknown bit ⇒ protocol error) · `frameOrdinal:u64` (1…i64::MAX, session-local, +1 per AU) · `captureSeq:u32` · `contentCaptureTs_us:u64` · `res:u32=0` · `payloadLen:u32`. Validation computes `u64(headerLen) + u64(payloadLen)` checked/widened **before allocation**; > `max_record_bytes` ⇒ `RECORD_CAP_VIOLATION`. Payload = one Annex-B HEVC access unit.

**UDP prefix (14 B, both channels):** `52 45 53 43` · `ver:u8=3` · `type:u8` (`1` cursor, `2` move) · `session_run_id:u64`. Receivers validate magic/version/type/run/source-IP/length.
**Mouse-move (26 B, client→host):** prefix · `seq:u32` · `x:i32` · `y:i32`.
**Cursor (43 B, host→client):** prefix · body at absolute offsets 14–42:

| Body off | Type | Field |
|---:|---|---|
| 0 | `u32_le` | `seq` |
| 4 | `u64_le` | `timestamp_us` |
| 12 | `i32_le` | `x_px` |
| 16 | `i32_le` | `y_px` |
| 20 | `u8` | `shape_id` |
| 21 | `u16_le` | `hotspot_x_px` |
| 23 | `u16_le` | `hotspot_y_px` |
| 25 | `f32_le` | `cursor_scale` |

**Golden fixtures required:** accepted + rejected `ProfileResult`; `VideoHello` and every `VideoHelloAck` status; frame headers at minimum/maximum legal values; move + cursor datagrams; malformed cases (bad length, nonzero reserved, unknown flag, length overflow) — each consumed by Swift **and** Rust tests.

## 6. Host pipeline (restated in full)

- **Capture:** ScreenCaptureKit at profile size, NV12, `showsCursor=false`; capture callback only stores into the slot below (no heavy work). FramePacer remains until T3's gates.
- **Slots (latest-wins + replay):** `latestPendingCapture` — overwritten by every capture, consumed only when the flow gate is open; on consumption it is copied to `lastReplayableCapture`, which persists for session-first replay after reconnect on a static screen (replayed frames keep their original `contentCaptureTs` but get a fresh admission time). When the gate opens, the pump submits the pending capture immediately — **no new SCK callback required**.
- **Pump:** one serial actor owns the slots, `encodeInFlight ∈ {0,1}`, outstanding-frame ledger, and the force-keyframe flag (queue-confined — the current cross-thread race is thereby removed). Wake events: capture arrival, encoder callback, `FrameAck`, connection transition. A generation of encoder output submitted before teardown is discarded by run-ID check on completion.
- **Encoder lifecycle:** create VT session → set **and read back** every load-bearing property (`RealTime=true`, `AllowFrameReordering=false`, profile/level, `AverageBitRate`, `MaxKeyFrameInterval=3600`, `MaxKeyFrameIntervalDuration=60`, plus `PrioritizeEncodingSpeedOverQuality=true` and `MaxFrameDelayCount=0` from T3 onward) → `PrepareToEncodeFrames` → only then connect-ready. Any required property unsupported ⇒ `ENCODER_PROPERTY` (deterministic). **Keyframe policy, honestly stated:** the configuration yields a ~60-second periodic safety IDR at 60 Hz, retained deliberately; actual cadence logged. Forced IDRs occur only at session start and RA re-force.
- **RA verification (encoder output, before send):** parse NAL types — RA iff IDR_W_RADL(19)/IDR_N_LP(20); CRA(21) rejected; session-first AU must contain VPS(32)+SPS(33)+PPS(34). Header keyframe-claim must equal the parse. A forced output that is not RA ⇒ one re-force, then session failure (`RA_VERIFICATION`).
- **Cap enforcement:** an AU whose record would exceed `max_record_bytes` is rejected at the encoder callback (never enters the pending slot) ⇒ `RECORD_CAP_VIOLATION` with actual size, cap, frame type, read-back properties, and recent AU-size stats.
- **Host watchdogs:** encoder submit→callback 500 ms; oldest outstanding frame age (admission-based) 250 ms — **scope: transport/acceptance stalls only** (an accepted-but-never-emitted frame still ACKs; that case belongs to the client's output watchdog, §8).

## 7. Flow control

- `flow_window_frames` init 1 (fixed 2 only if the A0 trial on the real pair + selected decoder cannot sustain 60 Hz; loopback is supplementary evidence only). Window 2 ⇒ **ACKs strictly in order**: `FrameAck.frame_ordinal` must equal the oldest outstanding ordinal, else protocol error; the sender never frees a later ordinal first.
- `FrameAck` is sent after (a) exact-once decoder acceptance and (b) drain to classified `Again`. **This point is final — not after output**: decoders may need several accepted packets before their first output; ACK-after-output with window 1 would deadlock startup. Drain `Error` ⇒ teardown, **no ACK**.
- **Send-EAGAIN retry (exact):** (1) retain the exact packet; (2) drain receive side; (3) require drain progress or fail the session; (4) resubmit the same packet until accepted exactly once; (5) drain to classified `Again`; ACK exactly once, after (4)+(5).
- Partial TCP writes are resumed by the single bounded writer per connection; only EOF/error tears down. `TCP_NODELAY` on both ends of **both** TCP connections.
- **Memory statement:** `flow_window_frames × max_record_bytes` bounds encoded transport records only. Also bounded, separately: 2 raw capture buffers (pending + replayable), 1 encode-in-flight, decoder-internal frames (≤ `decoder_lag_bound`), 1 owned decoded frame in the render slot.

## 8. Client pipeline (restated in full)

- **Deframe → decode:** single reader validates each record per §5 and feeds the decoder directly (no intermediate drop-capable queue).
- **Identity:** packet `pts = frameOrdinal` (1…i64::MAX). Output identity from `AVFrame.pts`, fallback `best_effort_timestamp`; both missing ⇒ unknown. FIFO/pts fidelity of the selected backend is **proven in A0.0** (CUVID and SW `threads=1` low-delay candidates); the profile records the chosen backend.
- **Unknown/duplicate/reordered-ordinal output:** discarded — never presented — and counted; the 4th in a session ⇒ `IDENTITY_FAILURE`.
- **EAGAIN/EOF/error are distinct classified outcomes** (send and receive sides); `error_concealment` off; no gray-frame heuristics, no recovery state machine — session-first frame is client-validated RA (same NAL rules as §6), everything after arrives in order exactly once, and any decode fatal ⇒ teardown.
- **Output watchdog (client-owned):** per accepted ordinal, track emission. Fail (`WATCHDOG_OUTPUT`) when `acceptedCount − emittedCount > decoder_lag_bound` or the oldest unresolved ordinal exceeds `output_deadline_ms` (both A0.0-measured).
- **Render handoff:** decoded frames pass as owned/ref-counted frames into a latest-wins slot; the render side may drop older *decoded* frames freely (intentional drops counted; presentation is telemetry, never flow control).
- **T2 designs (to be implemented — not current invariants):** dedicated render thread owning the single SDL context/event-pump/window/canvas/textures (SDL is currently initialized twice; the `unsafe impl Send` and `mem::forget` texture trick are deleted; texture destroyed before canvas); upload-once per new frame from plane pointers + strides; texture-resident cursor redraw (zero uploads on cursor-only presents); `SDL_UpdateNVTexture` where available, I420 fallback; **packed cursor snapshot** — one `AtomicU64`, layout `x:i16 | y:i16 | shape:u8 | seq:u24` (low 24 bits of wire seq), newer ⇔ `d≠0 ∧ d<2²³` with `d=(a−b)&0xFFFFFF` (the current code uses four independent atomics; this replaces it).

## 9. Input & UDP channels

- **Control TCP (reliable):** `KeyEvent` (client send finally wired; host HID→keycode map exists), `ButtonEvent` (validated), `ScrollEvent`, `ReleaseInput` (client sends on local ungrab/release/quit; host releases immediately), `Heartbeat` every 2 s both ways.
- **Liveness:** nothing received on control for 6 s ⇒ `WATCHDOG_HEARTBEAT` teardown (which triggers release). A held key/button with a quiet mouse is legal while heartbeats flow; **UDP traffic contributes nothing to liveness**. There is no input-inactivity watchdog (it would break legitimate long holds).
- **`releaseAll()` triggers (wired, counts logged):** control disconnect · heartbeat timeout · `ReleaseInput` · fatal error · injector teardown · process shutdown (signal handler sets a flag / wakes the loop — **no CoreGraphics from async-signal context**).
- **UDP latest-wins:** mouse-move and cursor per §5; shared comparator `newer(a,b) ⇔ d≠0 ∧ d<2³¹, d=(a−b) mod 2³²`; per-session `lastSeq` reset; move applies only in grabbed mode (grab/release semantics unchanged: Ctrl+Alt+G / Ctrl+Alt+Esc client-side).

## 10. Clock & trace contract (diagnostics/A0 mode only — normal streaming consumes no cross-machine offsets)

Four-timestamp exchange (`ClockPing/Pong`): requester t1 → responder receive t2 → responder send t3 → requester receive t4. `offset = ((t2−t1)+(t3−t4))/2` (responder − requester); `delay = (t4−t1)−(t3−t2)`. All cross-machine arithmetic in **i128 intermediates**; offset stored signed i64 µs + u32 µs uncertainty (= delay/2); reject `delay < 0` or ≥ 5 ms; keep the minimum-delay sample; resample every 10 s in trace mode, on reconnect, and after wake (wake invalidates). Host latency clock: continuous-monotonic epoch; CoreMedia host time bridged via **bracketed calibration** — read continuous `t_c1`, absolute `t_a`, continuous `t_c2`; require `t_c2−t_c1 < 50 µs` else retry; anchor `t_a ↔ (t_c1+t_c2)/2`, uncertainty half-width. SCK PTS mapped with `CMSyncConvertTime` to `CMClockGetHostTimeClock()`; `SCStream.synchronizationClock == nil` ⇒ callback-time fallback, labeled. Software present-return is not photon time; the final acceptance includes one optical (high-speed-camera) comparison.

## 11. Diagnostics & upgradeability

1. **JSONL logs** — Mac `~/Library/Logs/RESC/host.jsonl`, Ubuntu `~/.local/state/resc/client.jsonl`; rotation 5 × 10 MiB; permissions `0600`; fatal events flushed synchronously (bounded) before exit. Required fields per record: monotonic ts, wall ts, component, `session_run_id`, `profile_hash`, state before/after, frame ordinal when applicable, result, native error domain/code/text, expected-vs-actual. Lifecycle transitions, failures, every timeout/retry decision (timer, bound, observed), plus a 10–30 s aggregate and final summary — never per-packet spam. Redaction: no secrets, no frame payload bytes.
2. **Startup environment record** — commit/dirty, protocol version, full effective profile + hash, `auth_mode`, OS release/build/arch, peers/ports, codec/bitrate/cap/window, SCK+VT availability (Mac); kernel, FFmpeg/libavcodec, SDL, NVIDIA/CUDA versions, selected decoder backend (Ubuntu).
3. **Native-call evidence** — every load-bearing call checks and logs its result: `CGVirtualDisplay` class/selector presence and creation; SCK permission/config/format/clock/start; every `VTSessionSetProperty` requested-vs-read-back; `PrepareToEncodeFrames`; **`CoreBrightness`/Night Shift class, selector, and returned values** (it is a retained feature); FFmpeg decoder discovery/creation/hw-device and `send_packet`/`receive_frame` via `av_strerror`; SDL init/renderer/texture/update; socket ops with errno. **No silent fallback anywhere.**
4. **Doctors** — `remote-display-host --doctor`: create/destroy the profile virtual display; create the profile encoder; set/read-back all properties; encode a bundled frame; verify RA NALs. `remote-display-client --doctor`: open the selected decoder; decode a bundled RA sample; verify pts/FIFO; create the required SDL texture; report input capability. `--diagnose-peer`: exchange version/profile/build, one frame + one ACK, correlated logs, **injection disabled**. Reports: versioned JSON (`doctor_report_v: 1`); exit codes `0` pass · `2` environment · `3` native-API · `4` peer. Doctor failure-injection checks (T1 exit): forced native failures produce actionable records; all timeouts produce their stable codes; instance-lock denial kills nothing; lockfiles tracked.
5. **Upgrade policy (doctor-over-allowlist, normative):** an unlisted macOS/Ubuntu release is never rejected for its version string alone; startup/doctor **probes** the required native APIs/selectors/properties/return-values; successful probes permit operation (unlisted version logged); a failed required probe stops the affected feature or process with a stable `FatalCode` and persistent evidence; no native failure may silently fall back to behavior that changes the fixed profile. The OS allowlist becomes advisory logging.
6. **Dependencies** — remove the `.gitignore` lockfile rules; commit `Cargo.lock` and `Package.resolved`; pin `ffmpeg-next`/`-sys` exactly; doctor records runtime library versions.

## 12. Phases & entry gates

- **A0.0 — go now:** codegen toolchain (generated code compiled as a normal package target, CI-regen-checked) + typed dispatch; JSONL/environment/native-evidence logging; doctors; lockfiles + instance lock; clock bridges (trace mode); trace joining on the untouched v1 wire (frameID + host map); decoder pts/FIFO experiment **and** `decoder_lag_bound`/`output_deadline_ms` measurement; canonicalization mechanism + placeholder fixture. **Schema/fixture freeze happens at the end of A0.0, against this document.**
- **A0 — go after trace-joining + clock-uncertainty evidence:** behaviorally-unchanged baseline on the real pair: AU histogram → `max_record_bytes`; latency (software + one optical spot-check); capture fps; **stop-and-wait window trial on the real wired pair** → `flow_window_frames`; commit all five constants; regenerate the final profile artifact + golden hash.
- **T1 — entry gate (all required):** five constants committed · final profile artifact + hash regenerated · Swift and Rust golden fixtures pass · generated `control.proto` + `WIRE.md` match this document at the same commit · both doctors pass on the real endpoints. **Scope:** implement §§1–9, 11; delete UDP video (sender chunking, assembler, jitter-buffer crate, gray detector, recovery machinery), mDNS, kill sweeps, `--client` (debug override only). FramePacer survives until T3.
- **T2 (direction approved):** §8's T2 designs.
- **T3:** capture/encoder A/B (`minimumFrameInterval` 1/60 vs 1/120, `queueDepth` 3/5/8, speed-priority + `MaxFrameDelayCount=0` read back); final-static gate (window closed → one last capture → window opens → that exact `captureSeq` displays with no new SCK callback); then FramePacer removal.
- **T4:** VirtualDisplay resilience (HiDPI re-assert, mirror-set detach, arrangement memory), sleep/wake, long soak.

## 13. Validation gates (complete, standalone)

| Gate | Result required | Phase |
|---|---|---|
| Trace joining + clock | joined capture→present traces; signed-arithmetic/negative-delay tests pass; bracketed calibration bounds honored | A0.0 |
| Token/FIFO | selected backend proven pts-faithful (or FIFO under threads=1) with induced delay | A0.0 |
| Lag bounds | `decoder_lag_bound` + `output_deadline_ms` measured | A0.0 |
| Canonicalization | Swift and Rust reproduce the placeholder hash byte-exactly | A0.0 |
| Instance lock | second instance exits cleanly; nothing killed | A0.0 |
| Baseline + constants | five constants measured on the real pair; window decision made | A0 |
| Final fixtures | final profile artifact + all §5 golden fixtures pass in both languages | A0→T1 |
| Handshake | client-initiated; host-allocated nonzero run ID; announce→result→video-dial→hello strictly ordered; step-skipping rejected | T1 |
| Two-sided mismatch | profile mismatch yields `ProfileResult{rejected}`; both sides log both profiles + differing keys; classified deterministic (no retry burn) | T1 |
| Video peer IP | a video connection from any IP but the Mac's is closed before `VideoHello` processing | T1 |
| Legacy rejection | any pv1 message/datagram ⇒ protocol error, never acted on | T1 |
| Decode semantics | absent payload ⇒ error; duplicate oneof follows last-one-wins without a scanner; version≠3 rejected | T1 |
| Control bounds | 64 KiB+1 frames and oversized fields rejected pre-allocation | T1 |
| ACK-after-drain | drain `Error` ⇒ teardown with no ACK; ACK carries exactly the accepted ordinal | T1 |
| ACK order (w=2) | out-of-order ordinal ⇒ protocol error; oldest-first freeing verified | T1 |
| EAGAIN retry | identical packet drained/resubmitted, accepted exactly once, ACKed exactly once | T1 |
| Age watchdog | transport/acceptance stalls trip host 250 ms bound; no unbounded catch-up | T1 |
| Output watchdog | accept-drain-no-emit trips the client bound; host watchdog demonstrably blind to it | T1/T2 |
| Cap violation | oversize AU ⇒ structured fatal, never enters pending storage, no loop | T1 |
| RA/session-first | claim = parse; parameter sets present; non-RA forced output ⇒ one re-force then fail; client rejects invalid first record | T1 |
| Deadline coverage | every state exits by success or its named deadline; idle listeners consume no budget | T1 |
| Retry accounting | prune→check→append order verified; 5/60 s and 8/process enforced; streaming clears deque only; `Failed` emits `RETRY_BUDGET_EXHAUSTED` | T1 |
| Backoff | 250/500/1000/2000 ms schedule; reset only after 30 s streaming | T1 |
| Stuck input | drop/delay/kill injection leaves zero pressed keys/buttons; all six triggers exercised with counts | T1 |
| Long-hold safety | 60 s held key with heartbeats never auto-released; heartbeat loss releases ≤ 6 s; `ReleaseInput` immediate | T1 |
| Value validation | out-of-range button/hid/warm-strength/shape/scale/hash-length/zero-run each rejected fail-closed with diagnostics | T1 |
| UDP hygiene | wrong length/reserved/magic/run/source rejected; comparator edges (equal/forward/wrap/stale) exact | T1 |
| Diagnostics ops | rotation, `0600`, fatal flush, stable codes, doctor schemas/exit codes, forced-failure evidence | T1 |
| Ordinal identity | induced decoder delay preserves mapping; unknown-ordinal output never presented; 4th ⇒ teardown | T1/T2 |
| Cursor residency + coherence | cursor-only presents: zero uploads; packed snapshot never yields mixed-sequence tuples | T2 |
| Render ownership | one SDL owner; sanitizer-clean soak; texture-before-canvas destruction | T2 |
| Final static | last capture before idle displays with FramePacer off | T3 |
| Latency acceptance | software e2e + optical check vs A0 baseline; pointer bounded by current-op + present | T3 |

## 14. Review-8 checklist dispositions

| V8.1 item | Resolved |
|---|---|
| Standalone restatement | §§6–11 restate every retained rule; no "unchanged from vN" remains; packed-cursor moved to §8 T2 designs (correctly no longer described as existing) |
| Literal Envelope + typed `FatalCode` | §4 (verbatim proto; enum with frozen values + classifications) |
| Enforceable oneof semantics | §4 (last-one-wins accepted; absent payload = error; no scanner) |
| Run adoption + two-sided rejection | §3 steps 4–6 + `ProfileResult` carrying client canonical bytes |
| Ghost safe profiles removed | §2 (single profile in T1; future-profile procedure stated) |
| PSK removed + trust model | §1 (field 67 reserved; `auth_mode=trusted_lan_none`) |
| Video peer-IP validation | §1/§3 step 7 + gate |
| LE rule, exact UDP lengths, cursor body | §5 (incl. the full 29-B body table at offsets 14–42) |
| Backoff + exact retry operation | §3 (schedule; prune→check→append; status mapping) |
| Fixture ordering after A0 | §2 + §12 (mechanism in A0.0; freeze after constants) |
| HID/strength/shape/scale/hash/run validation | §4 |
| Doctor-over-allowlist normative + final diagnostic gates | §11.5 + §13 |

## 15. Go/no-go

- **A0.0: go now** (logging, doctors, pinning, locks, tracing, decoder experiments, codegen scaffolding). Schema + fixture freeze at A0.0's end, against this document.
- **A0: go** on trace/clock evidence; commits the five constants and the final profile hash.
- **T1: go** when §12's five-item entry gate holds. T2–T4 direction approved.
- Per review 8: after this edit, "the plan is good to implement — no further architectural review is needed." The next artifact is code.
