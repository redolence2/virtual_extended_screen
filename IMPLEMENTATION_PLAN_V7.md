# RESC Plan v7 — Personal-Profile Contract (single normative document)

| | |
|---|---|
| **Date** | 2026-07-29 |
| **Status** | **This is the only document that defines current intended behavior.** Plans v2–v6 and the analysis remain as history/rationale only; nothing below says "unchanged from vN" — everything normative is stated here (review-6 requirement). |
| **Deployment assumption (normative)** | One user, one Mac host, one Ubuntu client, updated together from this repo. Verified from the checked-in launchers: host `192.168.50.125`, client `192.168.50.47`, stream **1080×1920@60 (portrait)**, HEVC, control port 9870, SDL display 0. |
| **Incorporates** | `IMPLEMENTATION_PLAN_review_v6.md` — verdict adopted in full: keep the load-bearing correctness invariants, delete the reusable-protocol generality (negotiation, generations, interim-UDP recovery, adaptive ladders), add first-class diagnostics. Remaining v6 defects fixed here (§12 dispositions). |
| **RESC commit** | `12b87d1` (macOS host currently on Darwin 27.0 / build `26A5388g`, which is *not* in `VirtualDisplayManager.allowedBuilds` — a fact that motivates the doctor-over-allowlist rule in §9). |
| **Honest correction** | Earlier analysis quoted some 4K magnitudes; the actual profile is 1080×1920. All causal mechanisms stand; all magnitudes are re-measured in A0. |
| **New verifications this revision** | Launcher profile values (above); `ubuntu-client/Cargo.lock` absent; `releaseAll()`/`onReleaseInput` have no connecting call site; hand-rolled protobuf helpers cannot emit field numbers ≥ 32 (one-byte tag construction in `HostSession.swift`). |

**Goal (unchanged since v2):** zero transport/drop-induced corruption and low video+pointer latency on this fixed wired pair, keeping HEVC, Night Shift sync, cursor overlay, and full input — now with the smallest architecture that achieves it, and with diagnostics good enough that a future agent can localize any OS/driver/dependency breakage from logs alone.

---

## 1. PersonalProfile (compile-time constants, one definition per language, hash-checked)

One named `PersonalProfile` replaces the values currently scattered across `launch.sh`, `resc-host-launcher`, CLI defaults, and negotiation code:

| Field | Value | Source |
|---|---|---|
| `profile_id` | `"moyunfei-desk-1"` | chosen |
| `protocol_version` | `3` (v1 = legacy UDP as built; v2 = unimplemented paper protocol; 3 = this) | decision |
| Host / client IPv4 | `192.168.50.125` / `192.168.50.47` | launchers |
| Control / video TCP ports | `9870` / `9871` (host-side client-listener stays 9871 unless A0 shows conflict) | launchers + decision |
| UDP ports (mouse-move, cursor) | `9872`, `9873` | existing |
| Stream | `1080×1920 @ 60 Hz`, portrait; SDL display index `0`; client renders with the existing −90° path on the landscape framebuffer | launchers + code |
| Codec | HEVC Main, 8-bit, `AllowFrameReordering = false` (read back and asserted) | launchers + §6 |
| `bitrate_bps` | **A0-measured** (current default 20 Mbps is the starting point) | A0 |
| `max_wire_frame_bytes` | **A0-measured** (AU histogram + margin) | A0 |
| `flow_window_frames` | `1` (raise to fixed `2` only if the A0 stop-and-wait test cannot sustain 60 Hz) | A0 gate |
| Expected decoder backend | recorded outcome of the A0.0 token/FIFO experiment (`hevc_cuvid` or SW `threads=1`) | A0.0 |
| Build rule | both endpoints built from the same commit; commit hash + dirty flag exchanged and logged | §9 |

`profile_hash` = first 8 bytes of SHA-256 over the canonical serialized profile. Both endpoints print the full effective profile at startup and **exchange and verify the hash before any capture, injection, or streaming begins**; mismatch is fatal with both hashes and the differing fields logged. Hard-coding is correct for this deployment; *scattered* hard-coding is the defect being fixed.

**Deleted from all prior plans** (review-6 simplification, adopted): `CodecCapability`/`SupportedMode`/mode probing and all negotiation; `videoGeneration` and the offer/reset handshake; credit windows, byte-prefix ledger, `DecoderProgress`; the oversize/bitrate adaptive ladder; interim-UDP recovery (`AwaitingRA`, `recoveryEpoch`, reorder gate); mDNS discovery; IPv6/scope handling; resume/grace semantics; pv1 compatibility after A0; `ButtonEvent.seq`. Security scope unchanged: fixed IPs are not authentication; an optional PSK check inside `ProfileAnnounce` is the single supported hardening, off by default.

## 2. Architecture and lifecycle

Two TCP connections (control 9870, video 9871 — video **cannot** head-of-line-block input/control) plus two UDP latest-wins streams (mouse-move client→host, cursor host→client). The client listens on both TCP ports; the host connects (matches today's control direction… **decision: host listens on control as today; client listens on video** — i.e., control keeps its current direction, video adopts the listen-before-ready client role from v5, minus generations).

**One lifecycle, no generations:**

```
Disconnected → Connecting → AwaitingRA → Streaming → Backoff → (Connecting | Failed)
```

- `Connecting`: control TCP up → `ProfileAnnounce` both ways (fatal on mismatch) → host learns client video listener is ready (`VideoReady` message) → host dials video TCP → `VideoHello`/`Ack`.
- `AwaitingRA`: host sends nothing until it holds a **verified** random-access AU (§6); the first record must validate on the client (NAL types + parameter sets) or the session fails.
- `Streaming`: stop-and-wait flow (§5).
- **Any failure anywhere** (socket error/EOF, decode fatal, watchdog, framing violation, cap violation, profile/protocol error): (1) host `releaseAll()` input, (2) both endpoints close **both** TCP sockets and their UDP sockets, (3) dispose encoder, decoder, queues, render-slot staleness mark, (4) `Backoff`, reconnect. Staleness across restarts is fenced by `session_run_id` alone (host-generated random u64 per control acceptance; its low 32 bits are the UDP epoch field). A brief visible pause during full reconnect is accepted behavior for this tool.
- **Retry budget (fixes the v6 loophole where one ACKed frame reset the counter):** hard limits — max **5 restarts per rolling 60 s** and max **8 per process run**; the rolling count decays only after **30 s of continuous `Streaming`**; exceeding either ⇒ `Failed`: process exits nonzero with a structured fatal report. Personal software fails loudly rather than self-healing in a loop.

## 3. Control protocol (protobuf, `protocol_version = 3`)

Generated Swift + Rust protobuf with typed dispatch is a **hard prerequisite** — the current hand-rolled Swift encoders build tags via `UInt8((field << 3) | wire)`, which traps/corrupts for field numbers ≥ 32, and the inbound path is the `0xFA` byte-scan; neither may survive into T1. Framing stays `u32_le length + Envelope`.

Envelope: `session_id` is replaced by `session_run_id:u64` (field 1, reuse); `protocol_version` (field 2) must equal 3. Retained payloads: `KeyEvent(40)`, `DisplaySettings(32)` (Night Shift — a working feature, kept). All other pv1 payloads (`ModeRequest/Confirm/Reject`, `StartStreaming`, `StreamingReady`, `Stats`, `RequestIDR`, pairing) are **removed**; receiving an unknown/removed field ⇒ protocol error ⇒ teardown.

New payloads (numbers frozen):

```proto
message ProfileAnnounce   { // 60, both directions, first message on control
  bytes  profile_hash = 1;      // exactly 8 bytes
  string build_commit = 2;      // git hash
  bool   build_dirty  = 3;
  uint64 session_run_id = 4;    // host→client carries the authoritative value; client echoes it
  bytes  psk_proof = 5;         // optional; empty when PSK disabled
}
message VideoReady        { uint64 session_run_id = 1; uint32 listen_port = 2; }        // 61, client→host
message FrameAck          { uint64 session_run_id = 1; uint64 frame_ordinal = 2; }      // 62, client→host (§5)
message ButtonEvent       { uint32 button = 1; bool is_down = 2; sint32 x_px = 3;       // 63, client→host
                            sint32 y_px = 4; uint32 modifiers = 5; }                    // no seq: TCP neither drops nor reorders
message ScrollEvent       { sint32 dx = 1; sint32 dy = 2; }                             // 64, client→host (moved off UDP)
message ClockPing         { uint64 t1_mono_us = 1; uint32 seq = 2; }                    // 65, either
message ClockPong         { uint64 t1_mono_us = 1; uint64 t2_mono_us = 2;
                            uint64 t3_mono_us = 3; uint32 seq = 4; }                    // 66, either
message StatsSummary      { /* 10–30 s aggregate; schema owned by §9 telemetry */ }     // 67, either
message FatalReport       { uint32 code = 1; string detail_json = 2; }                  // 68, either, best-effort before close
```

Validation: `session_run_id` in every payload must equal the Envelope's; mismatch ⇒ protocol error. Unknown Envelope fields ⇒ protocol error (this pair is updated together; leniency only hides skew).

## 4. Video socket records (little-endian; exact bytes)

`VideoHello` (32 B, host→client): bytes `52 53 43 56` ("RSCV") | `version:u8 = 3` | `reserved:u8 = 0` | `length:u16 = 32` | `session_run_id:u64` | `profile_hash:u64` | `reserved:u64 = 0`. Client validates magic/version/length/run-id/hash; reserved must be zero.
`VideoHelloAck` (16 B): `52 53 43 41` ("RSCA") | `version:u8 = 3` | `status:u8 (0 OK · 1 MISMATCH · 2 BUSY · 3 INTERNAL)` | `length:u16 = 16` | `session_run_id:u64`. Nonzero status ⇒ teardown.

Per-frame record header (**32 B**): 

| Off | Field |
|---|---|
| 0 | magic `56 46` ("VF") |
| 2 | `headerLen:u8 = 32` (bytes 32..headerLen opaque-ignored if ever larger) |
| 3 | `flags:u8` — bit0 keyframe-claim; **any unknown bit set ⇒ protocol error** |
| 4 | `frameOrdinal:u64` — session-local, starts at 1, +1 per AU (§6 identity) |
| 12 | `captureSeq:u32` |
| 16 | `contentCaptureTs_us:u64` — host monotonic (replays keep original) |
| 24 | `reserved:u32 = 0` |
| 28 | `payloadLen:u32` — `headerLen + payloadLen ≤ max_wire_frame_bytes` else protocol error |

Payload: exactly one Annex-B HEVC access unit. No codec byte (the profile fixes the codec; the hash guards it). Malformed/oversize records fail the session — never resync-scan. Partial TCP writes are resumed by the single bounded writer; only EOF/error tears down.

## 5. Flow control — fixed stop-and-wait

- At most **`flow_window_frames` (init 1) outstanding un-ACKed frames**; while blocked, exactly one `latestPendingCapture` is retained (latest-wins) plus one `lastReplayableCapture` for post-reconnect static screens; when the window opens the pump submits immediately with no new SCK callback (the gate-open-wake behavior).
- `FrameAck{run, ordinal}` is sent on control **only after** the client (a) accepted the packet into the decoder exactly once and (b) drained decoder output to a classified `Again`. A drain `Error` ⇒ teardown with **no** ACK. Retry-once-effective on send-side EAGAIN: retain the exact packet → drain → require progress or fail → resubmit until accepted once → drain to `Again`.
- No byte ledger, no credit window, no `DecoderProgress` — with window 1 (or fixed 2), the ordinal itself is the complete flow state; memory bound = `flow_window_frames × max_wire_frame_bytes` by construction.
- **Age bound:** oldest outstanding frame (by `admissionTs`, tracked host-side; replays get a fresh one) > **250 ms** *(init)* ⇒ teardown. This single watchdog also covers "decoder accepts but never emits" — the ACK never arrives, age trips.
- **Cap violation is fatal, not adaptive** (replaces the ladder): an encoder AU exceeding `max_wire_frame_bytes` is rejected at the encode callback (never enters pending storage) and tears down with a structured report: actual size, cap, frame type, active encoder properties (as read back), and recent AU-size stats. A future agent adjusts one profile constant from evidence.
- Keyframes: no periodic IDRs (`MaxKeyFrameInterval = 3600`, `MaxKeyFrameIntervalDuration = 60`, results read back); a forced, **verified** RA AU starts every session (`AwaitingRA`); static screens replay `lastReplayableCapture` as the session-first frame after reconnect.

**Encoder session order (kept from v6):** create VT session → set **and read back** every load-bearing property (`RealTime`, `AllowFrameReordering=false`, `ProfileLevel`, bitrate, keyframe intervals, low-latency RC if supported for HEVC — logged either way) → `PrepareToEncodeFrames` → only then is the host willing to enter `Connecting`. Any failure is fatal-with-diagnostics (no silent fallback; a manual `--safe-mode` H.264/SW flag may exist, but its activation is always logged with the reason the profile path failed). The force-keyframe flag is pump-confined (the old data race dies with the old encoder thread).

## 6. Decoder contract (session-local, simplified from v6 §6)

- **Identity = `frameOrdinal`** (u64 from 1, session-local — fits ffmpeg's signed PTS domain trivially). Set packet `pts = frameOrdinal`; recover identity from `AVFrame.pts` (fallback `best_effort_timestamp`). The v6 cross-generation token machinery is deleted — unit mixing (review-6 defect 1) is resolved by having exactly one scope: the session.
- **No-reorder invariant, proven not assumed:** encoder `AllowFrameReordering = false` read back + the A0.0 experiment on the selected backend must show FIFO/pts-faithful output. "Proven-skip" inference is deleted; a missing output is caught by the §5 age watchdog, not arithmetic.
- Output with missing/unknown/duplicate pts ⇒ **discarded, never presented**, counted; >3 per session ⇒ teardown (`IDENTITY` code).
- EAGAIN/EOF/error are distinct classified outcomes (`DecodeOutcome` retained); `send_packet` EAGAIN is not an error; `error_concealment` off; the gray-frame heuristic and decoder recovery state machine are deleted (TCP: first frame is validated RA; every later frame arrives in order exactly once; any decode error ⇒ teardown).
- **RA verification** (kept, both ends): host parses encoder output — HEVC RA iff NAL 19/20 (IDR_W_RADL/IDR_N_LP; CRA rejected), session-first AU must carry VPS(32)+SPS(33)+PPS(34); header keyframe-claim must agree with the parse (the current `NotSync`-attachment default-true derivation is replaced); a forced output that is not RA ⇒ one re-force, then session failure. Client independently validates the session-first record the same way.

## 7. Input and cursor channels

- **Control TCP (reliable, ordered):** keyboard (`KeyEvent` — client send finally wired; host HID map exists), **buttons** (`ButtonEvent`), **scroll** (`ScrollEvent` — moved off UDP entirely; dedup semantics deleted with it), grab/release.
- **UDP latest-wins only for continuous state:** mouse-move (client→host, 22 B: `52 45 53 43` | `ver:u8=3` | `type:u8=2` | `epoch:u32` (= low 32 of run id) | `seq:u32` | `x:i32` | `y:i32`) and cursor (host→client, 39 B: same 10-B prefix, `type=1`, + the existing 29-B CursorUpdate body). Receivers validate magic/version/type/epoch and source IP == profile peer. One shared comparator, defined mathematically: `newer(a,b) ⇔ d ≠ 0 ∧ d < 2³¹ where d = (a − b) mod 2³²`; per-session `lastSeq` reset (fixes the verified cursor-restart rejection bug). The client-internal packed cursor snapshot (AtomicU64: `x:i16|y:i16|shape:u8|seq:u24`, u24 rule `d≠0 ∧ d<2²³`) is unchanged; wire stays u32.
- **`releaseAll()` wiring (verified missing today — `onReleaseInput` has no call site):** invoked, with released key/button counts logged, on all six triggers: control disconnect · session replacement · input watchdog expiry (no input-channel traffic for 10 s *(init)* while buttons/keys are down) · fatal protocol error · injector teardown · process shutdown (signal handler).

## 8. Clock & trace contract (diagnostics-mode; unchanged semantics from v6, restated normatively)

Four-timestamp sync over `ClockPing/Pong`: offset `= ((t2−t1)+(t3−t4))/2` (responder−requester), delay `= (t4−t1)−(t3−t2)`; all cross-machine arithmetic in **i128 intermediates**, offset stored signed i64 µs + u32 µs uncertainty; reject `delay < 0` or `≥ 5 ms` *(init)*; keep the min-delay sample; resample every 10 s, on reconnect, and after wake (wake invalidates). Host latency clock: continuous-monotonic epoch; CoreMedia host time bridged via a **bracketed** calibration (`t_c1 → t_a → t_c2`, require `t_c2−t_c1 < 50 µs`, anchor midpoint, uncertainty half-width); SCK PTS mapped with `CMSyncConvertTime` to **`CMClockGetHostTimeClock()`**; `SCStream.synchronizationClock == nil` ⇒ callback-time fallback, labeled. Software present-return is not photon time; the optical (high-speed camera) check remains part of the final gate.

## 9. Diagnostics & upgradeability (first-class deliverable — new, per review 6)

1. **Persistent structured logs**: bounded JSONL at `~/Library/Logs/RESC/host.jsonl` and `~/.local/state/resc/client.jsonl`. Every record: monotonic ts, wall ts, component, run id, profile hash, state before/after, frame ordinal where applicable, result, native error domain + numeric code + text, expected-vs-actual. Lifecycle transitions and failures only (no per-packet spam) + a 10–30 s aggregate + final summary.
2. **Startup environment record** both ends: commit/dirty, protocol version, full effective profile, OS release/build + arch, peers/ports, display/mode/codec/bitrate/cap, macOS SCK+VT availability; Ubuntu kernel, FFmpeg/libavcodec, SDL, NVIDIA/CUDA versions, selected decoder backend. (Motivation on record: this Mac already runs an unlisted build `26A5388g`; runtime evidence beats a growing allowlist — the allowlist becomes advisory logging only.)
3. **Native failure evidence**: every load-bearing call checks and logs its result — `CGVirtualDisplay` class/selector presence and creation, SCK permission/config/format/clock/start, every `VTSessionSetProperty` (requested vs read-back), `PrepareToEncodeFrames`, FFmpeg decoder discovery/creation/hw-device, `send_packet`/`receive_frame` via `av_strerror`, SDL init/renderer/texture/update, socket ops with errno. **No silent fallback anywhere.**
4. **Doctor commands** (replace the shallow `smoke_test.swift`): `remote-display-host --doctor` — create/destroy the profile virtual display, create the profile encoder, set/read-back all properties, encode a bundled test frame, verify its RA NALs. `remote-display-client --doctor` — open the selected decoder, decode a bundled RA sample, verify pts/FIFO behavior, create the required SDL texture format, report input capability. `--diagnose-peer` — exchange version/profile/build, one frame + one ACK, correlate logs; **input injection disabled**. Human summary + machine-readable report path on failure.
5. **Reproducible dependencies**: commit `ubuntu-client/Cargo.lock` (currently absent); keep `Package.resolved` committed; pin `ffmpeg-next`/`ffmpeg-sys-next` to exact tested versions (currently loose `"7"`); doctor records runtime system-library versions.

## 10. Phases

**A0.0 — GO now.** Generated protobuf (Swift `Protocol` target at the script's output path, toolchain version-verified and CI-regen-checked) + typed dispatch for the new/clock messages; §9.1–9.3 logging + environment record; improved doctors (§9.4); dependency pinning (§9.5); clock bridges (§8); trace joining via v1 `frameID` + host map (the v1 wire is not modified); **decoder token/FIFO experiment on CUVID and SW** — its outcome selects the profile's decoder backend and is a T1 entry gate.

**A0 — GO after A0.0 demonstrates trace joining + clock uncertainty.** Behaviorally unchanged baseline on the *actual profile*: HEVC AU-size histogram (fills `max_wire_frame_bytes`), e2e + cursor latency (software + one optical spot-check), encode/decode timings, capture-fps at 1/60, disjoint drop counters, and a **stop-and-wait throughput trial** (loopback/synthetic is acceptable) deciding `flow_window_frames` = 1 or 2. Output: the three profile constants committed.

**T1 — fixed-profile TCP core** (entry gates below): PersonalProfile + hash handshake; control pv3 messages incl. wired keyboard/buttons/scroll and `releaseAll` triggers; video socket + records; host pump (serial actor: pending/replayable capture, one-in-flight encode, window, age watchdog, encoder-before-ready order); client decode path with ordinal identity, ACK-after-drain, RA validation; lifecycle + retry budget; UDP epoch prefixes for move/cursor; **delete**: UDP video (sender chunking, assembler, jitter-buffer crate, gray detector, recovery machinery), mDNS, negotiation, `--client` (profile supplies peers; flag becomes debug override), FramePacer *stays* until T3.
*T1 entry gates:* profile constants + hash frozen from A0 · generated wire fixtures pass in Swift and Rust · doctor passes on both machines · A0 window decision made · token/FIFO gate concluded.

**T2 — client pipeline:** decode/render thread split with owned-frame handoff (one render actor owns the single SDL context, event pump, window, canvas, textures — today SDL is initialized twice; texture destroyed before canvas, replacing the `mem::forget` raw-pointer scheme); low-delay decoder flags per the A0.0 outcome; upload-once from planes+strides (both intermediate copies deleted); texture-resident cursor redraws (zero uploads on cursor-only presents); coherent packed cursor snapshot; `SDL_UpdateNVTexture` where available, I420 fallback.

**T3 — host tuning + FramePacer removal:** A/B `minimumFrameInterval` 1/60 vs 1/120 and `queueDepth` 3/5/8 against capture-age; `PrioritizeEncodingSpeedOverQuality=true`, `MaxFrameDelayCount=0` (read back); remove FramePacer only after the final-static gate (window closed → one last capture → window reopens → that exact `captureSeq` displays with no new SCK callback) and idle/reconnect tests pass.

**T4 — polish:** VirtualDisplay resilience (HiDPI re-assert, mirror-set detach, arrangement memory — ported from opendisplay's patterns), sleep/wake behavior, Night Shift warm filter off the CPU path if A0 data justifies, long soak.

## 11. Validation gates (complete, pruned to this architecture)

| Gate | Result required | Phase |
|---|---|---|
| Trace joining + clock uncertainty | joined capture→present traces; offset with bounded uncertainty; negative-delta/underflow tests pass | A0.0 |
| Token/FIFO | selected backend proves pts-faithful (or FIFO under `threads=1`) with induced delay | A0.0 |
| Doctor | both doctors pass; injected native failures produce actionable reports | A0.0/T1 |
| Baseline | profile constants derived from measured histogram/latency/throughput | A0 |
| Window sufficiency | window 1 sustains 60 Hz, or fixed 2 justified by measurement | A0 |
| Profile hash | mismatched profile/build refuses before capture or injection | T1 |
| Legacy rejection | any pv1 message/packet ⇒ protocol error, never acted on | T1 |
| ACK-after-drain | drain `Error` ⇒ teardown with no ACK; ACK ordinal always matches the outstanding frame | T1 |
| Age watchdog | stalled decoder/socket ⇒ teardown within the bound; no unbounded catch-up | T1 |
| Cap violation | oversize AU ⇒ structured fatal report; never enters pending storage; no retry loop | T1 |
| Retry budget | crash-loop (incl. stream-one-frame-then-fail patterns) halts at the rolling/process caps with a fatal report | T1 |
| RA/session-first | header claim, parsed NAL type, parameter sets agree; non-RA forced output ⇒ one re-force then fail; client rejects invalid first record | T1 |
| Stuck input | drop/delay/kill injection at any point leaves zero pressed keys/buttons (all six `releaseAll` triggers exercised, counts logged) | T1 |
| Stale UDP | old-epoch move/cursor packets discarded; comparator edge cases (equal/forward/wrap/stale) exact | T1 |
| Ordinal identity | induced decoder delay preserves ordinal mapping; unknown-pts output never presented; 4th ⇒ teardown | T1/T2 |
| Cursor residency | cursor-only presents perform zero texture uploads; coherence race shows no mixed-sequence tuple | T2 |
| Render ownership | single SDL owner; sanitizer-clean soak; texture-before-canvas destruction | T2 |
| Final static | last capture before an idle period displays with FramePacer off | T3 |
| Latency acceptance | measured software e2e + optical check against A0 baseline; pointer bounded by current-op + present (no queued-video age) | T3 |

## 12. Review-6 defect dispositions

| Review-6 finding | Disposition |
|---|---|
| 1. Token/ordinal unit mixing; "proven-skip" unsound without ordering proof | Fixed: single session scope, `frameOrdinal` identity, watchdog-only missing-output handling (§5, §6) |
| 2. Retry budget loophole | Fixed: rolling 5/60 s + 8/process, 30 s-stability decay (§2) |
| 3. Oversize/config ambiguity | Dissolved: ladder deleted; fixed cap, fatal-with-evidence (§5) |
| 4. Cap/window validation | Dissolved: compile-time equal constants + hash check (§1) |
| 5. `releaseAll` unwired; `ButtonEvent.seq` pointless | Fixed: six wired triggers with logging (§7); seq removed (§3) |
| 6. Freeze not standalone | Fixed: this document is fully self-contained; prior plans demoted to history |
| 7. A1 hybrid unimplementable; one-byte tag bug; interim UDP throwaway | Fixed: interim-UDP recovery work deleted; typed codegen is a hard prerequisite; UDP survives only through A0 measurement, untouched |
| Diagnostics/upgradeability missing | Added as §9, a first-class deliverable with its own gates |
| PersonalProfile | Adopted as §1 (values verified from launchers) |
| Scroll simplification | Adopted: scroll → control TCP; UDP keeps only move+cursor (§7) |

## 13. Effort (initial; revisit after A0)

A0.0: 2–3 d (codegen+dispatch 0.5–1 · logging/doctor 1 · clocks+experiments 0.5–1) · A0: 0.5–1 d + soak wall-time · T1: 3–5 d (protocol, pump, lifecycle, input, deletions, fault tests) · T2: 2–3 d · T3: 0.5–1 d · T4: open. Materially smaller than v6's path — the deleted machinery (credit ledger, generations, negotiation, interim-UDP recovery) was the bulk of B0–B2.

## 14. Go/no-go statement

- **A0.0: go today.** Nothing in it is speculative; it also front-loads the two real empirical risks (decoder pts/FIFO behavior, clock bridging).
- **A0: go** when A0.0's trace/clock gate passes.
- **T1: go** once A0 commits the three profile constants and the T1 entry gates hold. **No further paper iteration is required** — this document is the personal-profile amendment review 6 asked for; remaining risk is empirical and resolved by A0.0/A0 measurement, not by review.
- The v6 reusable-protocol spec remains on file if this project ever genuinely needs multi-device generality; it is not the implementation target.
