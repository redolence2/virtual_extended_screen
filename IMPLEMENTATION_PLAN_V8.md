# RESC Plan v8 — Personal-Profile Contract (supersedes v7; single normative document)

| | |
|---|---|
| **Date** | 2026-07-29 |
| **Status** | The only document defining current intended behavior. Supersedes `IMPLEMENTATION_PLAN_V7.md`; v2–v7 and reviews are history/rationale. Architecture is **locked** per review 7 ("keep the V7 architecture… no additional architectural redesign is warranted"): fixed profile, control+video TCP, no video UDP in the final system, stop-and-wait, full reconnect, reliable discrete input, diagnostics-first. v8 resolves review 7's twelve V7.1 contract corrections (§12 maps each). |
| **RESC commit** | `12b87d1` · macOS host on unlisted build `26A5388g` (doctor-over-allowlist stands) |
| **New verifications** | `.gitignore` ignores `Cargo.lock`; `EventInjector.mouseEventParams` force-unwraps `CGMouseButton(rawValue:)` for button bytes ≥ 3 (crash on malformed input); host and client both `pgrep`+`SIGKILL` sibling processes at startup. |
| **Normative wire sources (named, per review 7 §3)** | After A0.0 generates them: `proto/control.proto` (protocol v3) + `docs/WIRE.md` (all fixed binary layouts) + golden byte fixtures under `proto/fixtures/` consumed by Swift **and** Rust tests. This document defines the rules; those three artifacts are the machine-checked truth once generated, and divergence between them and this document is itself a defect. |

---

## 1. PersonalProfile

Values unchanged from v7 (verified from launchers): host `192.168.50.125`, client `192.168.50.47`, control `9870`, video `9871`, UDP move/cursor `9872/9873`, stream `1080×1920@60` portrait (SDL display 0, −90° render path), HEVC Main 8-bit `AllowFrameReordering=false`, `protocol_version = 3`; `bitrate_bps`, `max_record_bytes`, `flow_window_frames` filled by A0.

**Canonical profile bytes (new, exact):** the profile is serialized as a checked-in canonical artifact — UTF-8 JSON, keys sorted lexicographically, no whitespace, integers base-10, strings NFC — stored at `proto/fixtures/profile.canonical.json`. `profile_hash` = **first 8 bytes of SHA-256 over those exact bytes, used verbatim** (an opaque 8-byte string everywhere — never reinterpreted as an integer). One golden fixture test in each language recomputes the hash from the artifact.

**Mismatch reporting (honest):** `HostProfileAnnounce` carries the host's canonical profile **bytes** (≈200 B), not just the hash — so on mismatch each side logs its full local profile, the remote bytes, and the differing keys. (An 8-byte hash alone cannot name differing fields.)

**Build rule (exact):** both endpoints exchange `build_commit` + `build_dirty`. Differing commits ⇒ fatal (deterministic failure, §2). Dirty builds ⇒ fatal unless `--allow-dirty` is passed, in which case every log record is tagged `dirty`. **Safe modes are separate named profiles with their own hashes** (`moyunfei-desk-1-h264sw`, `moyunfei-desk-1-althost`): a host-only switch can never send H.264 under the HEVC profile's hash — both ends must be launched into the same named profile.

Deleted machinery (unchanged from v7): negotiation, generations, credit ledger, adaptive ladders, interim-UDP recovery, mDNS, IPv6, resume, pv1-compat after A0, `ButtonEvent.seq`. Fixed IPs ≠ authentication; optional PSK proof in the announce, off by default, redacted from logs.

## 2. Connections, lifecycle, deadlines, retry

**Ownership (one rule, fixes v7's contradiction):**
- Mac host **listens** on control `9870` (as today). Ubuntu client connects.
- Ubuntu client **listens** on video `9871` (fixed — `VideoReady` carries no port). Mac host connects.
- Both listeners are **process-owned** and stay open across sessions; teardown closes only accepted/session sockets and session state — no rebind race.
- The client alone initiates each control connection (first and after backoff).

**Handshake (exact order; host allocates the run ID):**
1. Client connects to control; host validates peer IP == profile client IP.
2. Host generates nonzero random `session_run_id:u64`.
3. Host → `HostProfileAnnounce` (Envelope run ID = the new ID; body: canonical profile bytes, `profile_hash[8]`, `build_commit`, `build_dirty`, optional `psk_proof`).
4. Client validates version/profile/build (+PSK); on success →
5. Client → `ProfileAckVideoReady` (echoes hash + its own commit/dirty; video listener confirmed armed).
6. Host dials video; `VideoHello`/`Ack` (§4).
7. Only after Ack: capture transmission and input injection are enabled.

**Run-ID rule (fixes v7's impossible per-payload rule):** after step 3, staleness is enforced by **`Envelope.session_run_id == activeRun` only**; ordinary payloads (`KeyEvent`, `ButtonEvent`, `ScrollEvent`, `DisplaySettings`, clock, acks) carry **no** run-ID field.

**Deadlines (all *(init)* values; expiry = transient failure unless noted):** control connect/accept 3 s · announce→ack 2 s · video-ready→dial 1 s · video connect 2 s · `VideoHello`→`Ack` 2 s · encoder submit→callback 500 ms · first verified-RA frame 2 s · outstanding-frame ACK 250 ms (the §5 age bound). No state can hang without consuming budget.

**Failure classification (new):**
- **Deterministic ⇒ `Failed` immediately, no retry:** profile/build/version mismatch, malformed framing, invalid profile constant, record-cap violation, required encoder property unsupported, PSK failure.
- **Transient ⇒ `Backoff` + budget:** socket errors/EOF, decoder errors, watchdog/deadline expiries, handshake timeouts.

**Retry accounting (exact, replaces prose):** keep a deque of transient-restart timestamps; a new restart is rejected (⇒ `Failed`) if 5 fall within the last 60 s; keep a separate process-total counter capped at 8 (never cleared). 30 s of uninterrupted `Streaming` clears the **deque only** — it supplements, and never overrides, the process-total cap.

**Transport details:** `TCP_NODELAY` on **both ends of both** TCP connections (ACKs, button/key edges, scroll must not sit behind Nagle); one serialized writer per connection. Teardown is idempotent — simultaneous EOF/error/fatal events coalesce into one scheduled reconnect.

**Instance lock (replaces the verified `pgrep`+`SIGKILL` startup sweeps on both ends):** a per-profile advisory lock (`flock` on `~/.local/state/resc/<profile>.lock` / `~/Library/Application Support/RESC/<profile>.lock`); if held, the new process exits with a clear message. No process killing.

## 3. Control protocol (protobuf v3; generated, typed — hand-rolled encode/scan forbidden)

Framing: `u32_le length + Envelope`; **max control frame 64 KiB** (checked before allocation — replaces today's 1 MB limit); per-field bounds: strings ≤ 256 B, `FatalReport.summary` ≤ 2 KiB, profile bytes ≤ 4 KiB.

**Unknown-field rule (corrected):** generated protobuf ignores unknown fields by design; no raw scanner is added to detect them. Compatibility is enforced by exact `protocol_version`, profile hash, and build-commit equality, plus "exactly one recognized payload per Envelope" (zero or ambiguous ⇒ protocol error).

Payloads (numbers frozen; bodies complete — nothing "defined elsewhere"):

```proto
// Envelope: session_run_id = 1 (u64), protocol_version = 2 (=3), oneof payload:
message KeyEvent          { uint32 hid_usage = 1; bool is_down = 2; uint32 modifiers = 3; }        // 40 (body frozen here)
message DisplaySettings   { float warm_strength = 1; }                                             // 32 (Night Shift, retained)
message HostProfileAnnounce { bytes profile_canonical = 1; bytes profile_hash = 2;  // exactly 8 B // 60
                              string build_commit = 3; bool build_dirty = 4; bytes psk_proof = 5; }
message ProfileAckVideoReady { bytes profile_hash = 1; string build_commit = 2; bool build_dirty = 3; } // 61
message FrameAck          { uint64 frame_ordinal = 1; }                                            // 62
message ButtonEvent       { uint32 button = 1;  // MUST be 0 left · 1 right · 2 middle; any other  // 63
                            bool is_down = 2; sint32 x_px = 3; sint32 y_px = 4; uint32 modifiers = 5; }
message ScrollEvent       { sint32 dx = 1; sint32 dy = 2; }                                        // 64
message ClockPing         { uint64 t1_mono_us = 1; uint32 seq = 2; }                               // 65
message ClockPong         { uint64 t1_mono_us = 1; uint64 t2_mono_us = 2; uint64 t3_mono_us = 3; uint32 seq = 4; } // 66
message FatalReport       { uint32 code = 1;         // stable enum, §9                            // 68
                            string component = 2; string native_domain = 3; sint64 native_code = 4;
                            string summary = 5; }    // detail lives in local JSONL, correlated by run ID
message ReleaseInput      { }                                                                      // 69 (§7)
message Heartbeat         { uint64 t_mono_us = 1; }                                                // 70 (§7)
```

Removed from the wire: `StatsSummary` (aggregates live in local JSONL only); all pv1 payloads. `ButtonEvent.button` is validated **before** injection — the current `CGMouseButton(rawValue:)!` force-unwrap for bytes ≥ 3 (verified) is unreachable under this rule and is also replaced by a non-crashing guard.

## 4. Video socket records (all layouts land verbatim in `docs/WIRE.md`)

`VideoHello` (32 B): `52 53 43 56` | `ver:u8=3` | `reserved:u8=0` | `len:u16=32` | `session_run_id:u64` | `profile_hash[8]` (8 opaque bytes, not an integer) | `reserved:u64=0`. `VideoHelloAck` (16 B): `52 53 43 41` | `ver` | `status:u8 (0 OK · 1 MISMATCH · 2 BUSY · 3 INTERNAL)` | `len:u16=16` | `session_run_id:u64`.

Frame record header (**exactly 32 B — `headerLen` must equal 32**; the opaque-extension rule is deleted since version/build mismatch is already fatal): magic `56 46` | `headerLen:u8=32` | `flags:u8` (bit0 keyframe-claim; unknown bits ⇒ protocol error) | `frameOrdinal:u64` (**1…i64::MAX**, session-local) | `captureSeq:u32` | `contentCaptureTs_us:u64` | `reserved:u32=0` | `payloadLen:u32`.

**Size rule (named precisely):** the profile constant is **`max_record_bytes`** (= A0-measured max AU + 32 + margin). Validation computes `total = u64(headerLen) + u64(payloadLen)` in widened checked arithmetic **before any allocation**; `total > max_record_bytes` ⇒ record-cap violation (deterministic failure, evidence-rich report). Payload: one Annex-B HEVC AU. Partial writes resumed; EOF/error ⇒ transient teardown.

## 5. Flow control (stop-and-wait; ACK point defended)

- `flow_window_frames` *(init 1; fixed 2 only if the A0 trial on the **real wired pair with the selected decoder** cannot sustain 60 Hz — loopback is supplementary evidence only)*.
- **If the window is 2:** ACKs are strictly in order — a `FrameAck.frame_ordinal` must equal the **oldest** outstanding ordinal; anything else is a protocol error. The sender never frees a later ordinal first.
- `FrameAck` is sent after (a) exact-once decoder acceptance and (b) drain to classified `Again`. **This point is deliberate and must not move to after-output**: some decoders need multiple accepted packets before their first output, so ACK-after-output with window 1 would deadlock decoder startup. Drain `Error` ⇒ teardown with no ACK. Retry-once-effective on send-EAGAIN unchanged.
- Host age bound: oldest outstanding frame (`admissionTs`) > 250 ms ⇒ transient teardown. **Scope correction (review 7):** this watchdog detects transport/acceptance stalls only — it *cannot* see "accepted but never emitted", because that ACK still arrives. Output liveness is the client's job (§6).
- Latest-wins capture slot + replayable last capture + gate-open-wake unchanged.
- **Memory statement (scoped honestly):** `flow_window_frames × max_record_bytes` bounds **encoded transport records only**. Separately bounded: 2 retained raw capture buffers (pending + replayable), 1 encode-in-flight, decoder-internal frames (bounded by the §6 lag bound), 1 owned decoded frame in the render slot.
- **Keyframes (honest wording):** `MaxKeyFrameInterval=3600` + `MaxKeyFrameIntervalDuration=60` **is a ~60-second periodic safety IDR at 60 Hz, and is retained deliberately** (cheap insurance; actual cadence logged). Session-first frame is a verified RA AU; static-screen replay unchanged.
- Encoder order (create → set+read-back → prepare → then connect-ready) and the pump-confined force-KF flag unchanged.

## 6. Decoder contract (+ output watchdog, new)

Identity by `pts = frameOrdinal` (1…i64::MAX), FIFO/pts behavior proven in A0.0 on the selected backend; unknown/duplicate-ordinal output discarded (never presented), 4th ⇒ teardown; EAGAIN/EOF/error classified; RA verification both ends (HEVC 19/20, VPS/SPS/PPS on session-first, claim-vs-parse agreement, CRA rejected) — all unchanged from v7.

**Client output watchdog (fixes the v7 gap):** track, per accepted ordinal, whether its output was emitted. Fail (transient teardown, `WATCHDOG_OUTPUT`) when `acceptedCount − emittedCount > decoder_lag_bound` **or** the oldest unresolved ordinal exceeds `output_deadline_ms`. Both bounds are **measured in A0.0** on the selected backend *(init: lag 4 frames, deadline 200 ms — placeholders until measured)*.

Additional watchdogs (from §2's deadline table): encoder submit→callback; first-RA-output; video-handshake. Every expiry logs which timer fired, with expected/actual.

## 7. Input channels (long-hold fix)

- Control TCP: keyboard, buttons (validated `{0,1,2}`), scroll, grab/release, and **`ReleaseInput`** — sent by the client on local ungrab/release/quit; host calls `releaseAll()` immediately on receipt.
- **Partition detection is heartbeat-based, not input-absence-based (fixes the v7 watchdog that would release a legitimate long hold):** both ends send `Heartbeat` every 2 s on control; nothing received for 6 s ⇒ transient teardown (which triggers `releaseAll`). A held key/button with a quiet mouse is perfectly legal as long as heartbeats flow. Mouse-move UDP traffic contributes **nothing** to liveness (it must not keep a broken reliable path alive).
- `releaseAll()` triggers (wired, counts logged): control disconnect · heartbeat timeout · `ReleaseInput` received · fatal error · injector teardown · process shutdown — where the signal handler only sets a flag/wakes the main loop; **no CoreGraphics work from async-signal context**.
- UDP (latest-wins only): mouse-move client→host and cursor host→client, now carrying the **full `session_run_id:u64`** (move 26 B: `52 45 53 43`|ver|type|`run:u64`|`seq:u32`|`x:i32`|`y:i32`; cursor 43 B: same 14-B prefix + existing 29-B body, offsets/endianness enumerated in `WIRE.md`). Validation: magic/version/type/run/source-IP. One shared comparator `newer(a,b) ⇔ d≠0 ∧ d<2³¹, d=(a−b) mod 2³²`; per-session reset. Internal packed cursor snapshot (u24 rule) unchanged.

## 8. Clocks

Unchanged four-timestamp/i128/bracketed-calibration contract — **scoped as a diagnostics/A0 mode**: normal streaming operation consumes no cross-machine offsets; resampling runs only under `--trace` or doctor/measurement sessions (review 7 §8).

## 9. Diagnostics & upgradeability (operational bounds completed)

Everything from v7 §9 (JSONL logs, startup environment record, native-call evidence with no silent fallback, doctor commands, dependency pinning) **plus**:
- Rotation/retention: 5 × 10 MiB per endpoint; permissions `0600`; fatal events flushed synchronously (bounded) before exit.
- **Stable code enums** for events and fatals, checked into both languages (single source: `proto/control.proto` enum) — `FatalReport.code` uses it.
- Doctor report: versioned JSON schema (`doctor_report_v: 1`) + documented exit codes (0 pass · 2 environment fail · 3 native-API fail · 4 peer fail).
- Redaction: PSK, proofs, and frame payload bytes never logged.
- **Night Shift probes**: `CoreBrightness` private-class/selector presence and return values checked+logged like every other native dependency (it is a retained feature).
- Every timeout and retry decision logs the timer, bound, and observed value.
- Lockfiles: **remove the `.gitignore` rules** (line 27 ignores `Cargo.lock`; audit line 20's rule as well) and commit `Cargo.lock` + `Package.resolved`; pin `ffmpeg-next`/`-sys` exactly.
- The per-profile instance lock (§2) replaces the kill sweeps; its acquisition/denial is logged.

## 10. Phases

**A0.0 — go now (non-wire parts unconditionally; the v3 schema is generated only after this document):** codegen toolchain + typed dispatch scaffolding; JSONL/environment/native-evidence logging; doctors; dependency pinning + lockfiles + instance lock; clock bridges (trace mode); trace joining on the v1 wire (frameID + host map, wire untouched); decoder pts/FIFO experiment **and the §6 lag/deadline measurement**; golden profile fixture + hash tests.
**A0 — go after trace/clock evidence:** unchanged baseline; AU histogram → `max_record_bytes`; latency; capture-fps; **stop-and-wait window trial on the real pair** → `flow_window_frames`; commit the three constants + the two decoder bounds to the profile.
**T1 — hold until this document's rules are reflected in the generated schema/fixtures and A0 constants land:** implement §§1–7,9; delete UDP video, assembler/jitter-buffer, gray detector, recovery machinery, mDNS, kill sweeps, `--client` (debug override only); FramePacer survives until T3.
**T2 (approved direction):** decode/render split, single SDL owner, texture-resident cursor, upload-once, NV12-capability, coherent snapshot.
**T3:** capture/encoder A/B, final-static gate, FramePacer removal.
**T4:** VirtualDisplay resilience, sleep/wake, soak.

## 11. Validation gates (v7 set, plus)

| New gate | Result | Phase |
|---|---|---|
| Handshake order | Client-initiated control, host-allocated run ID, announce→ack→video-ready→hello strictly ordered; step-skipping rejected | T1 |
| Deadline coverage | Every lifecycle state exits by success or its named deadline; no state hangs without consuming budget | T1 |
| Deterministic-vs-transient | Profile/build/cap violations go straight to `Failed` (no retry burn); socket/decoder failures consume the deque per the exact rule | T1 |
| Output watchdog | Decoder that accepts+drains but never emits trips the **client** watchdog at the measured bound; host age watchdog demonstrably does *not* see it | T1/T2 |
| ACK ordering (w=2) | Out-of-order ack ordinal ⇒ protocol error; oldest-first freeing verified | T1 |
| Button validation | button ∉ {0,1,2} rejected pre-injection; no force-unwrap path reachable | T1 |
| Long-hold safety | A 60 s held key with heartbeats flowing is never auto-released; heartbeat loss releases within 6 s; `ReleaseInput` releases immediately | T1 |
| Instance lock | Second instance exits cleanly; no process is ever killed | A0.0 |
| Control frame bounds | 64 KiB+1 frame and oversized fields rejected before allocation | T1 |
| Profile diffing | Hash mismatch logs local profile, remote bytes, and differing keys; safe-mode profile cannot handshake with the primary profile | T1 |
| Golden fixtures | Swift and Rust independently reproduce the profile hash and all wire fixtures byte-exactly | A0.0 |

## 12. Review-7 checklist dispositions

| V7.1 item | Resolved in |
|---|---|
| Listener ownership + handshake order | §2 (client→control, host-allocated run ID, 7 steps) |
| Envelope-only run IDs | §2/§3 (payload run-ID fields removed) |
| Canonical profile bytes, hash verbatim, build/safe-mode rules | §1 |
| Normative schemas designated; bodies completed | header + §3 (`control.proto` + `WIRE.md` + fixtures; KeyEvent/DisplaySettings/cursor bodies in-document) |
| `StatsSummary` deleted | §3 |
| Control-frame + field bounds | §3 (64 KiB; per-field caps) |
| Exact 32-B header + checked lengths | §4 (`max_record_bytes` named; widened checked arithmetic) |
| All deadlines | §2 table + §6 watchdogs |
| Transient/deterministic + exact retry rule | §2 |
| `ReleaseInput` + heartbeat liveness | §7 |
| Frozen buttons + full UDP run ID | §3/§7 (force-unwrap verified and neutralized) |
| Diagnostics bounds, Night Shift probes, lockfiles | §9 (`.gitignore` verified) |

Also adopted: ACK stays at accept+drain (with the recorded anti-deadlock rationale); host-watchdog scope corrected; keyframe wording honest (~60 s safety IDR retained); clock resampling scoped to diagnostics; real-pair window trial; kill sweeps → instance lock.

## 13. Go/no-go

- **A0.0 (non-wire parts): go today.** Schema generation follows this document immediately — the corrections it needed are now written.
- **A0: go** on trace/clock evidence.
- **T1: go** when the generated schema/fixtures match this contract and A0 commits the profile constants (`bitrate`, `max_record_bytes`, `flow_window_frames`) and decoder bounds (`decoder_lag_bound`, `output_deadline_ms`).
- T2–T4 direction already approved by review 7. Remaining risk is empirical, gated by A0.0/A0 measurement; the contract itself is complete.
