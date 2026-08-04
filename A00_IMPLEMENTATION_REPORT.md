# A0.0 Implementation Report — for cold-start review

| | |
|---|---|
| **Date** | 2026-08-03 (§15 addendum same day; status corrected 2026-08-04) |
| **STATUS CORRECTION (2026-08-04)** | Per `A00_IMPLEMENTATION_REPORT_review.md`: this is an **A0.0 progress/candidate report — the completion/freeze claim is withdrawn**. A0.0 is complete only after the review's blocking gates close (trace identity/clock path, clean decoder bounds + backend selection, the three errata behavioral proofs, fail-closed doctors/harness, clean-commit freeze). Dispositions and closure plan: `A00_IMPLEMENTATION_REPORT_response.md`. Known precision errata in this document (exact values, per the review): fixture-check = **126** assertions (not ~140); `fatal_code_classes.json` = **23** entries incl. code 0; **five** proto inputs; `Doctor.swift` = **451** lines post-refactor; ffmpeg pins were caret until corrected to `=7.1.0`/`=7.1.3` on 2026-08-04; "typed dispatch" exists as generated types + parsers + the clock intercept, while the live host otherwise remains on the legacy v1 path by design. |
| **Scope** | Phase A0.0 of `IMPLEMENTATION_PLAN_V11.md` §12 as amended by `CONTRACT_ERRATA.md` (ERR-01…07 + three implementation proofs). Stage-1 **candidate** artifacts (freeze pending the remediation gates) + all A0.0 tooling and experiments. |
| **Base commit** | `12b87d1` (branch `main`). The work this report describes was committed 2026-08-04 as candidate checkpoint `ce2d693`; remediation (`A00_REMEDIATION_PLAN.md` R1–R7) continues on top of it. |
| **Machines** | Mac host `192.168.50.125` (Darwin 27.0 / build `26A5388g`, Apple Silicon, CommandLineTools only — **no Xcode, no XCTest**); Ubuntu client box `wan@192.168.50.47` (Linux 6.8.0-65, x86_64, RTX 4090 / driver 570.169, key-auth SSH from the Mac). |
| **Authorship** | Orchestrated by Fable 5; implementation chunks by Sonnet workers (W1–W8), every diff reviewed by the orchestrator; foundations (proto schema, canonical bytes, codegen script, package wiring, RescCore profile module) written inline. |
| **How to review** | Every claim below is reproducible via §14's runbook. Normative context: `IMPLEMENTATION_PLAN_V11.md`, `CONTRACT_ERRATA.md`, `docs/WIRE.md`. |

---

## 1. Environment provisioning (recorded because the doctor/environment contract §11 makes it evidence)

**Mac:** Rust installed via Homebrew (`/opt/homebrew/bin/cargo`, 1.97.1) — needed for lockfile generation and pure-crate tests; ffmpeg/SDL crates are *never* built on the Mac. Pinned `protoc 27.3` + `protoc-gen-swift 1.36.1` installed by `tools/generate_proto.sh` into `tools/bin/` (gitignored path).

**Ubuntu box:** (a) `rustup` existed with **no default toolchain**; the first `rustup default stable` wedged holding rustup's lock — diagnosed via `pgrep`, the wedged pid killed, then a clean `rustup toolchain install stable` → **rustc/cargo 1.97.1 healthy** (verified `rustc --version` + full test run). (b) System FFmpeg is 4.4 (libavcodec 58) — incompatible with the workspace's `ffmpeg-next 7.x`; installed **BtbN FFmpeg n7.1.5 gpl-shared** (headers + `.so`, `hevc_cuvid` + 9 other CUVID decoders confirmed) side-by-side at `~/ffmpeg7`; builds use `FFMPEG_DIR=$HOME/ffmpeg7`, runs use `LD_LIBRARY_PATH=$HOME/ffmpeg7/lib`; zero system contamination (removable with `rm -rf ~/ffmpeg7`). (c) Repo synced to `~/resc/remote_extended_screen` via rsync (excludes: `target`, `mac-host/.build`, `tools/bin`). (d) Test sample generated on-box by `tools/gen_sample_hevc.sh`: `~/resc/sample_1080x1920.h265` — 480 AUs (8 s × 60), keyint 60, **bframes 0**, 8,554,446 bytes, portrait profile geometry.

Incident disclosure: two early remote/background failures were briefly mis-read as passes because `… | tail` masked exit codes; both were caught, corrected in-conversation, and all subsequent remote verification uses `set -o pipefail`.

## 2. Protocol & code generation

### 2.1 `proto/control_v3.proto` (new; Stage-1 candidate — normative per ERR-06, freeze pending)
Verbatim transcription of plan v11 §4: `resc.v3` package; `Envelope{session_run_id=1, protocol_version=2 (runtime ==3), oneof payload}` with fields 32/40/60–66/68–70, `reserved 67` (removed StatsSummary slot); messages `DisplaySettings`, `KeyEvent`, `HostProfileAnnounce` (`reserved 5` = removed psk), `ProfileResult`, `FrameAck`, `ButtonEvent`, `ScrollEvent`, `ClockPing`, `ClockPong`, `FatalReport`, `ReleaseInput`, `Heartbeat`; `FatalCode` enum values 0–22 with classification comments. Validates under `protoc 3.20.3` (the reviewer's own version) and pinned 27.3.

### 2.2 `proto/control.proto` (v1 — additive edit only)
Appended `ClockPing clock_ping = 65; ClockPong clock_pong = 66;` to the v1 Envelope oneof + the two message definitions. Wire-compatible: legacy peers ignore unknown fields; numbers 65/66 deliberately match v3 so semantics carry over at T1. **No other v1 change** — the running wire is untouched (A0 baseline requirement).

### 2.3 `tools/generate_proto.sh` (rewritten)
- `SWIFT_PROTOBUF_VERSION` pinned **1.36.1** matching the resolved runtime (was 1.28.1 — review-7 M4).
- `ensure_swift_plugin()` now **verifies the plugin's `--version` equals the pin** and rebuilds on mismatch (previously any preinstalled binary was trusted).
- Clone uses `--recurse-submodules --shallow-submodules` (the 1.36.x manifest references submodule sources; without it SwiftPM fails with "target 'protoc' … is empty" — hit and fixed in-session).
- `ensure_protoc()` verifies the exact pinned protoc version, reinstalling otherwise.
- New **`--check` mode**: regenerates into a temp dir and diffs against committed `mac-host/Sources/Protocol/` — the CI regen-clean gate. Verified passing.
- `validate_protos()` runs a descriptor-set compile of all `.proto`s.
- The stray `generate_rust()` that wrote a bogus `mod.rs` into the crate is removed (Rust generation is `prost-build` in `build.rs`).

### 2.4 Rust protocol crate
`crates/protocol/build.rs`: `control_v3.proto` added to the compile list. `crates/protocol/src/lib.rs`: new `pub mod resc_v3 { include!(concat!(env!("OUT_DIR"), "/resc.v3.rs")); }`.

### 2.5 `mac-host/Package.swift`
- swift-protobuf dependency changed `from: "1.28.0"` → **`exact: "1.36.1"`** (generator/runtime lockstep, comment says bump both together).
- New targets/products: **`RescProto`** (path `Sources/Protocol`, the 5 generated `.pb.swift` files — named RescProto because a module literally named `Protocol` collides with the ObjC runtime type); **`RescCore`** (pure/shared logic — see §3, §10); **`FixtureCheck`** → product `resc-fixture-check`; **`HarnessSender`** → product `resc-harness-sender` (deps RescCore; CoreMedia/CoreVideo linker settings). `RemoteDisplayHost` gains deps `RescProto`, `RescCore`.

## 3. Canonical profile (Stage-1 fixture + mechanism, both languages)

### 3.1 Fixture
`proto/fixtures/profile.canonical.json` — the **exact 497 bytes** from plan v11 §2 (sorted keys, minified, `TBD-A00` backend placeholder, **no trailing LF**). SHA-256 `0cc22496628805973f8d52292e7f838b95ec023faf658d71dd862f3fbf4ed6ff`, 8-byte prefix `0c c2 24 96 62 88 05 97` — byte-identical to review 11's independent computation.

### 3.2 Rust — `crates/diagnostics/src/profile.rs`
Functions: `canonicalize(&serde_json::Value) -> Vec<u8>` (recursive BTreeMap re-sort + minified `to_string`); `hash8(&[u8]) -> [u8;8]` (sha2); `is_placeholder_backend(&Value) -> bool`; `validate_runtime_profile(&[u8]) -> Result<Value, String>` (parse → canonical-bytes round-trip equality → **`TBD-A00` rejected with ERR-02 wording** → backend id must be `cuvid-lowdelay` | `sw1-lowdelay`). Constants: `PROFILE_ID`, `PLACEHOLDER_BACKEND`, `BACKEND_CUVID`, `BACKEND_SW1`. Tests (4): pinned-prefix hash + no-trailing-LF assert; fixture-is-canonical round-trip; placeholder rejection; sort/minify unit.

### 3.3 Swift — `Sources/RescCore/CanonicalProfile.swift`
Mirror API: `canonicalize(_ object: Any) throws -> Data` (JSONSerialization `.sortedKeys`, minified); `hash8(_ : Data) -> Data` (CryptoKit SHA256 prefix 8); `isPlaceholderBackend`, `validateRuntimeProfile(_ : Data) throws -> [String: Any]` (same rejection ladder via `ProfileError` enum: `.parse/.notCanonical/.placeholderBackend/.unknownBackend`).

### 3.4 Swift test vehicle — `Sources/FixtureCheck/main.swift` (product `resc-fixture-check`)
Because this Mac has **no XCTest** (CommandLineTools only), the Swift fixture tests are an executable running the same five assertions as the Rust tests (no-LF, pinned prefix, canonical round-trip, placeholder rejection, sort/minify), exit 0/1. **Result: 5/5 pass.** Rust twin: **6/6 pass locally and on the box** (`cargo test -p diagnostics` — includes W2's 2 jsonl tests).

## 4. Diagnostics cores (plan §11.1–11.3, §3 instance lock)

### 4.1 Swift — `Sources/RemoteDisplayHost/Diagnostics/`
- **`RescLog.swift`** (170): singleton `RescLog.shared`; `event(_:component:fields:)` and `fatal(code:component:nativeDomain:nativeCode:summary:fields:)`; every record auto-carries `ts_mono_us` (`RescClock.monoUs()` from `DispatchTime`), ISO-8601 `ts_wall`, component, optional global `session_run_id`/`profile_hash` via `setContext` (serial-queue-confined — no data race). File `~/Library/Logs/RESC/host.jsonl`, dirs+file created `0600` (reasserted post-rotation), rotation at 10 MiB → `.1….4` (5 files). Buffered (flush at 32 records / 1 s timer); `fatal` = `queue.sync` + `synchronizeFile()`. JSON-encode-failure writes a minimal fallback line (no silent loss).
- **`EnvironmentRecord.swift`** (96): `emit()` logs `startup_environment` — build commit (`RESC_BUILD_COMMIT` env → `git rev-parse HEAD` at the `#filePath`-derived repo root → `"unknown"`), `build_dirty` (`git status --porcelain`, JSON `null` if git absent), `protocol_version` 1 (commented: running wire is v1), macOS version + `sysctlbyname("kern.osversion")` build, `utsname` arch.
- **`InstanceLock.swift`** (49): `acquire(profileId:) -> Bool` — `open(O_CREAT|O_RDWR, 0o600)` at `~/Library/Application Support/RESC/<id>.lock` + `flock(LOCK_EX|LOCK_NB)`; fd retained in a static for process lifetime (kernel releases on exit/crash); errno captured before `close` on the failure path; logs `instance_lock {acquired, profile_id, reason?, errno?}`.
- **`NativeEvidence.swift`** (33): `@discardableResult nativeCheck(_ component,_ call, status:expected:extra:) -> Bool` + `ok:detail:` overload — logs `native_call` with expected-vs-observed; the §11.3 building block.
- **`main.swift`**: the pgrep/`SIGTERM`/`SIGKILL` stale-process sweep (**deleted**) replaced by logger bootstrap + `EnvironmentRecord.emit()` + lock guard → message + `exit(20)` (`INSTANCE_LOCK_HELD`).

### 4.2 Rust — `crates/diagnostics/` (new crate; deps log/libc/serde_json/sha2 only — no chrono)
- **`jsonl.rs`** (350): `RescLog` — `event(event, component, fields: Value)` / `fatal(code, component, native_domain, native_code, summary, fields)`; `ts_mono_us` from a `OnceLock<Instant>` anchor (`mono_us()` now `pub`); hand-rolled ISO-8601 (`civil_from_days`, verified against Python datetime incl. leap-day cases); `set_context(session_run_id, profile_hash)`; `~/.local/state/resc/client.jsonl` (base overridable via `RESC_LOG_DIR`), `0600` via `OpenOptionsExt::mode`, same 5×10 MiB rotation; `BufWriter` behind `Mutex`, flush per 32 / on `fatal` (+`sync_all`); global via `OnceLock` with a `/dev/null` fallback (a logging failure can't crash the client); **`flush()` added later by W8** — see §8.2 bug. Tests: ISO-8601 known-instants, rotation order, `#[ignore]` tempdir fs test.
- **`environment.rs`** (111): `emit(&RescLog)` — commit/dirty (env → `git -C <workspace-root>` fallback), `protocol_version: 1`, kernel via `libc::uname` (`kernel_info()` now `pub`), `"pending_doctor"` placeholders for ffmpeg/SDL/NVIDIA (doctor fills real values).
- **`instance_lock.rs`** (89): `acquire(profile_id) -> bool` — same flock pattern; `File` kept in a static `OnceLock`; never kills anything.
- **`src/main.rs`**: pgrep/kill block (**deleted**) → logger + env emit + lock guard → `exit(20)`.
- **Verified on the real box**: `cargo test -p diagnostics` → **12 passed / 0 failed / 1 ignored** (after §5 additions).

## 5. Clock bridges & trace joining (plan §10; v1 wire untouched)

### 5.1 Swift
- **`Diagnostics/RescClockBridge.swift`** (115): `continuousNowUs()` — `mach_continuous_time()` → µs via cached `mach_timebase_info` (overflow-safe split multiply); `bracketedHostTimeCalibration() -> Calibration?` — `c1 → mach_absolute → c2`, ≤5 retries requiring bracket `< 50 µs`, returns `(contMidUs, absUs, uncertaintyUs)`, logs one `clock_calibration` event, `nativeCheck(ok:false)` after 5 failures; `hostTimeToContinuousUs(_:calibration:)` signed-delta translation.
- **`Diagnostics/RescTrace.swift`** (163): enabled iff `RESC_TRACE=1`; separate `host-trace.jsonl` (same rotation/0600; per-frame records explicitly allowed here — the A0-measurement exemption, doc-commented); `captureMeta(captureSeq:contentCaptureTsUs:)` latest-wins slot under `OSAllocatedUnfairLock`; `frameSent(frameID:bytes:isKeyframe:encodeOutTsUs:)` joins the latest capture meta into one `{"t":"frame",…}` record (doc-comments the ±1-frame join caveat under drops); `clockSample(seq:t1:t2:t3:)`. Never touches the filesystem when disabled.
- **`DisplayCapturer.swift`** (+12): after `frameSlot.store`, when tracing: `traceCaptureSeq += 1` + `captureMeta(...)`.
- **`VideoSender.swift`** (+6): end of `sendFrame`, when tracing: `frameSent(...)`.
- **`HostSession.swift`** (+50, `import RescProto`): `handleMessage` captures `t2 = continuousNowUs()` at entry, `try? Resc_Control_Envelope(serializedBytes:)`; `.clockPing` → `handleClockPing(ping, sessionID:, t2:)` (echo t1/seq, t2 from entry, `t3 = continuousNowUs()` immediately pre-serialize, reply via the same `controlChannel.send(data:)` path ModeConfirm uses) then **return** — clock traffic never reaches the legacy `0xFA` scan; `.clockPong` → ignore (host is responder-only); anything else / decode failure → **fall through to the byte-identical legacy dispatch**.

### 5.2 Rust
- **`crates/diagnostics/src/clocksync.rs`** (172): `ClockSync::on_pong(t1,t2,t3,t4,seq) -> Option<Sample>` — `offset = ((t2−t1)+(t3−t4))/2`, `delay = (t4−t1)−(t3−t2)`, **all in i128**; rejects `delay < 0` or `≥ 5000 µs`; retains the min-delay sample; `best() -> Option<Sample{offset_us: i64, uncertainty_us: u32 = delay/2, seq}>`. **6 unit tests**: far-ahead/far-behind responder, negative-delay rejection, ≥5 ms rejection, min-delay retention, near-`u64::MAX` no-overflow.
- **`crates/diagnostics/src/trace.rs`** (141): `ClientTrace` — `RESC_TRACE=1`-gated `client-trace.jsonl`, `frame(fields)` + `clock(offset_us, delay_us, uncertainty_us, seq)`.
- **`crates/net-transport/src/control_channel.rs`**: `send_clock_ping(&mut self, t1_mono_us, seq)` (same envelope style as `send_stats`).
- **`src/main.rs`** (trace-gated, additive): 10 s ClockPing tokio interval; ClockPong arm in the existing control-recv `select!` (t4 = mono now → `ClockSync` → `ClientTrace.clock`); decode-render loop logs per-assembled-frame `{frame_id, ts_recv_us, ts_decode_done_us, presented}`.

## 6. `docs/WIRE.md` + golden/malformed fixtures (Stage-1 candidate; freeze pending)

**`docs/WIRE.md`** (336 lines, 9 sections + front matter): global LE/magic/reserved/exact-UDP-length rules; control framing + 64 KiB pre-allocation cap + per-field caps + direction/state table; VideoHello (32 B)/Ack (16 B) offset tables with **status bytes 0–3**, OK-checklists, stale-dial run-tagging; **ERR-01 activation barrier verbatim**; frame-record table (32 B, ordinal domain, `contentCaptureTs_us` = host continuous-monotonic µs); UDP prefix/move/cursor tables (cursor body at absolute offsets 14–42) + comparator formula + cursor-timestamp-is-diagnostic rule (**no "reserved" in the UDP list — ERR-05**); input/cursor semantics (StreamSpace + inverse −90°, **ERR-04 scroll rule verbatim** with N=10, shapes 0–15, hidden (−1,−1), hotspot/scale constants, grab state machine); **§Backend** — the two closed rows fully specified (`cuvid-lowdelay`: `hevc_cuvid`, CUDA `av_hwdevice_ctx_create` defaults, `AV_CODEC_FLAG_LOW_DELAY` pre-open, `extra_hw_frames = 8`, NV12 post-`av_hwframe_transfer_data`, fallback forbidden; `sw1-lowdelay`: `hevc`, no hw, LOW_DELAY, `thread_count = 1`/no frame-threading, yuv420p, fallback forbidden) + `TBD-A00` scoping; ERR-03 statement; canonical-profile rules + pinned hash.

**`tools/gen_fixtures.py`** (402, stdlib-only, idempotent — re-run diffs byte-identical): writes 16 fixtures + `proto/fixtures/README.md` manifest (name/size/expected classification). Golden: `videohello.bin` (32), 4 × `videohelloack_*.bin` (16), `frame_header_min.bin` (32), `move.bin` (26), `cursor.bin` (43). Malformed: bad magic (first byte bit-inverted), bad length, nonzero reserved, unknown status 9, unknown flag 0x03, `payloadLen=0xFFFFFFFF` overflow, short move (25), long cursor (44). All hand-audited against the offset tables (incl. `1.0f = 00 00 80 3F`, run id `0x1122334455667788` LE). Classification judgment call (recorded): UDP length mismatches labeled `PROTOCOL_VIOLATION` (v11 §5.1 precedent), `RECORD_CAP_VIOLATION` only for the overflow frame.

## 7. Backend construction, ERR-03 experiment, harness receiver (Rust, runs on the box)

### 7.1 `crates/backend-construct/` (extracted shared lib, 248 lines)
API: `open_decoder(backend_id: &str) -> Result<DecoderHandle>` — implements **exactly** the WIRE §Backend rows via raw `avcodec_find_decoder_by_name` / `av_hwdevice_ctx_create` / pre-open flag+`extra_hw_frames` writes (cuvid) and safe `find_by_name` + `set_threading(None,1)` (sw1); `TBD-A00`/unknown ⇒ `BackendOpenError{failing_call, detail}` with ERR-02 wording; **no fallback**. Helpers: `receive_one -> ReceiveOutcome{Frame|Again|Eof|Error}` (classified via `ffmpeg::Error::Other{errno: EAGAIN}`), `transfer_hw_frame` (GPU→CPU), `recovered_ordinal` (`pts()` → `timestamp()` fallback).

### 7.2 `crates/decoder-experiment/` (binary, 502 lines post-refactor)
CLI `--input --backend --frames --stall-every --stall-ms --json-out`. Feeds demuxed AUs with `pts = ordinal`; send-side EAGAIN = retain→drain→resubmit (bounded 64); drains to classified `Again` after every accept; `--stall-*` injects the induced delay ERR-03 requires. Pass criteria: emitted ordinals must be exactly `1..=N` in order — `unknown_pts` / `duplicates` / `reorders` / silent-skip counters all zero, plus a **tail-drop check** (`frames_emitted != frames_submitted` after flush ⇒ fail; a worker-added strengthening, since gap-proof can't catch trailing loss). JSON report per the plan schema; exit 0 iff pass.

**ERR-03 results (real 4090, induced 50 ms stalls every 30 frames, 240 frames each):**
```json
{"backend":"sw1-lowdelay","pass":true,"frames_submitted":240,"frames_emitted":240,"unknown_pts":0,
 "duplicates":0,"reorders":0,"max_lag_frames":1,"output_delay_ms":{"p50":3.60,"p95":6.62,"max":53.69},"eagain_retries":0}
{"backend":"cuvid-lowdelay","pass":true,"frames_submitted":240,"frames_emitted":240,"unknown_pts":0,
 "duplicates":0,"reorders":0,"max_lag_frames":2,"output_delay_ms":{"p50":1.88,"p95":51.93,"max":52.40},"eagain_retries":0}
```
**Both candidates pass ERR-03** (cuvid's ffmpeg note "passing timestamps as-is" independently confirms raw pts passthrough). Backend selection is therefore an A0 performance decision, not a correctness one. Post-refactor re-run (120 frames, sw1) reproduced `pass:true` — extraction is behavior-preserving.

### 7.3 `crates/harness-receiver/` (binary, 552 lines)
One TCP connection; parses the 32-B v3 frame header inline (magic `56 46`, `headerLen==32`, unknown-flag rejection, reserved-zero, checked widened length, 8 MiB sanity cap) — deliberately **not** using the protocol crate (disposable rig; doc-commented). Decodes via `backend-construct`; ACK **only after exact-once accept + drain-to-`Again`** as a private 12-byte record `41 4B` + u16 reserved + u64 ordinal LE (doc-commented as differing from the real protobuf `FrameAck`). Reports receive→ack p50/p95/max + decode lag. Smoke-tested by its worker with 20 real AUs over loopback before the live run.

## 8. Doctors (plan §11.4/§11.5)

### 8.1 Host — `Sources/RemoteDisplayHost/Doctor.swift` (451) + `--doctor` in `main.swift`
`HostDoctor.run() -> Int32`, checks: (a) environment (OS/build/arch, `auth_mode: trusted_lan_none`); (b) `CGVirtualDisplayBridge.isAPIAvailable` (required → 3); (c) create+destroy the **profile display** 1080×1920@60; (d) create the **profile encoder** (HEVC, 20 Mbps) with `VTSessionCopyProperty` read-backs (RealTime/AllowFrameReordering/AverageBitRate/ProfileLevel; enabled by a one-line `vtSession` accessor added to `VideoEncoder` — the only change in that file); (e) encode one synthetic NV12 frame (semaphore, 2 s timeout); (f) **RA verification** — Annex-B split, HEVC NAL type `(b>>1)&0x3F`, require VPS 32 + SPS 33 + PPS 34 + IDR 19/20, CRA ⇒ fail; (g) CoreBrightness/Night Shift probe (class lookup + instantiate + strength read; non-required feature — recorded, never changes exit code); (h) report `doctor_report_v:1` → stdout + `~/Library/Logs/RESC/doctor_host.json` (0600). Exit map: 0 pass / 2 environment (`kern.osversion` read failure only) / 3 native-API.

**Live run on this Mac (unlisted build `26A5388g`): exit 0, all checks ok** — `encode_bundled_frame {bytes:1019, is_keyframe:true}`, `ra_verification {nal_types_found:[20,32,33,34,39], has_vps/sps/pps/idr:true, has_cra:false}`, `corebrightness_nightshift {observed_strength:0.5}`. This is the doctor-over-allowlist policy (§11.5) functioning on an OS build the old allowlist does not contain.

### 8.2 Client — `src/doctor.rs` (578) + `--doctor/--doctor-backend/--sample` in `main.rs`
Checks: (a) environment (kernel via `diagnostics::environment::kernel_info`, ffmpeg avcodec runtime version, `sdl2::version`, `nvidia-smi` name/driver); (b) `backend_open` via `backend_construct::open_decoder` with the **explicit candidate logged** (ERR-02) — `TBD-A00`/unknown ⇒ check fails, exit 3; (c) `decode_sample` — first 60 AUs of `--sample` with `pts = ordinal`, ERR-03 conditions must hold (missing sample ⇒ exit 2 with the gen-script hint); (d) `sdl_texture` — hidden window + renderer + IYUV **and** NV12 1080×1920 streaming textures (SDL init failure ⇒ exit 2 with the error text); (e) `input_capability` (informational). Report → stdout + `~/.local/state/resc/doctor_client.json`.

**Live runs on the box (`DISPLAY=:0`): exit 0 for BOTH backends** — kernel 6.8.0-65, avcodec 61.19.101, SDL 2.0.20, RTX 4090/570.169; `decode_sample` 60/60 zero anomalies on sw1 **and** cuvid (`is_hw:true` — proves the GPU→CPU transfer path); both texture formats created. **ERR-02 negative tests**: `--doctor-backend TBD-A00` and `bogus-id` both exit 3 with the ERR-02 wording.

**Bug found & fixed during verification (W8):** doctor evidence records never reached disk — `RescLog`'s 32-record buffer + `std::process::exit` (which skips `Drop`) lost them. Fix: `RescLog::flush()` / `LogWriter::flush_now()` + a flush at the end of `doctor::run()`. Verified: `client.jsonl` now contains `startup_environment`, `instance_lock`, `doctor_backend_candidate`, `doctor_complete`.

## 9. A0 measurement harness — sender + live LAN smoke

### 9.1 `Sources/HarnessSender/main.swift` (548, product `resc-harness-sender`)
CLI `--connect --port --window 1|2 --seconds --bitrate --json-out`. Synthetic 60 Hz NV12 source (1080×1920, phase-shifting bar pattern); real `VideoEncoder` (HEVC, profile size, forced first keyframe); POSIX TCP + `TCP_NODELAY`; 32-B WIRE §4 frame records (implemented inline; comment marks the future RescCore hoist); stop-and-wait window with **one latest-wins pending encoded record** (`pending_replaced` counter — the plan §6 pump semantic); ACK-reader thread parsing the rig's 12-B `AK` records, validating **oldest-outstanding ordinal** (violation ⇒ counted + stop). Worker-caught correctness fix during self-review: socket `write()`s serialized under the pump lock so the encoder-callback and ACK threads cannot interleave wire bytes. Report `harness_report_v:1` with rtt/encode/bytes percentiles + `sustained_60hz`. Local mach-continuous helper duplicated (~10 lines) because `RescClockBridge` lives in the app target (recorded deviation; hoist candidate).

### 9.2 Live smoke — real Mac encoder → real LAN → real box decoder (sw1), window 1, 10 s
```
frames_sent = 594   frames_acked = 594   ack_order_violation = 0
rtt_ms   p50 7.09   p95 10.10   max 65.83
encode_ms p50 10.48  p95 12.24  max 21.46
pending_replaced = 6            sustained_60hz = TRUE (59.4 fps incl. startup)
```
**Window = 1 sustains 60 Hz on the real pair** with the full encode→wire→decode→drain→ack loop at ~7 ms median — versus the ~200 ms symptom that started this project. (Caveat: synthetic bar content ⇒ tiny AUs (p50 371 B); the A0 real-capture histogram still owns `max_record_bytes`.)

## 10. File moves & cross-cutting deltas

- `VideoEncoder.swift` and `NALUPackager.swift` moved (git mv) `Sources/RemoteDisplayHost/` → **`Sources/RescCore/`** with mechanical `public` annotations + a `public init` for `VideoEncoder.Config` + the `vtSession` accessor; zero logic change (diff-verified). `import RescCore` added to exactly: `main.swift`, `BitrateAdapter.swift`, `Doctor.swift`.
- `.gitignore`: `Package.resolved` and `Cargo.lock` ignore rules **removed** (both files now trackable; `ubuntu-client/Cargo.lock` generated — resolution incl. `ffmpeg-next 7.1.0` / `ffmpeg-sys-next 7.1.3`; lockfiles are platform-independent by design). `decoder-experiment`/`harness-receiver`/`backend-construct` pin ffmpeg crates **exactly** (`= 7.1.0` / `= 7.1.3`); the legacy `video-decode` crate still carries loose `"7"` (dies at T1 — see §13).
- Workspace members added: `diagnostics`, `decoder-experiment`, `harness-receiver`, `backend-construct`; root binary deps: `diagnostics`, `backend-construct`, `serde_json`, ffmpeg pins, `libc`.

## 11. Verification matrix (all green unless noted)

| Check | Where | Result |
|---|---|---|
| `protoc` validation of all 4 protos | Mac, protoc 3.20.3 + 27.3 | OK |
| `tools/generate_proto.sh --check` (regen-clean) | Mac | PASS |
| `swift build` (all targets) incl. full clean rebuild | Mac | Build complete; only 3 pre-existing warnings in untouched files |
| `resc-fixture-check` | Mac | 5/5 |
| `cargo test -p diagnostics` | Mac and box | 6/6 → **12/12** after clock additions (1 ignored fs test passes with `--include-ignored`) |
| `cargo check --workspace` | box (FFMPEG_DIR) | clean; pre-existing warnings only |
| `gen_fixtures.py` idempotency | Mac | byte-identical re-run |
| ERR-03 experiment ×2 backends | box | both PASS (§7.2) |
| Host doctor | Mac (unlisted OS build) | exit 0 (§8.1) |
| Client doctor ×2 backends + ×2 negative | box | exit 0 / 0 / 3 / 3 (§8.2) |
| Live harness smoke | Mac↔box LAN | sustained 60 Hz @ window 1 (§9.2) |
| Contract test suite (§15): `cargo test -p protocol` | Mac and box | **49/49** (41 new v3wire + 8 pre-existing) |
| Contract test suite (§15): `resc-fixture-check` | Mac | **126 checks, all ok** (profile group + 6 wire groups; count as of this report — remediation adds more) |
| Doctor re-run after RA-hoist refactor | Mac | exit 0; NAL set identical `[20,32,33,34,39]` — behavior-preserving |
| Envelope fixtures idempotency + cross-encoder byte-equality | Mac | hand-encoded = prost = SwiftProtobuf, byte-identical |

## 12. Consolidated deviations & judgment calls (each recorded at decision time)

1. Swift protobuf module named `RescProto`, not `Protocol` (ObjC runtime collision); generated dir stays `Sources/Protocol`.
2. Swift fixture tests are an executable (`resc-fixture-check`) — **no XCTest exists on this machine**; same assertions as the Rust tests.
3. v3 physically lives at `proto/control_v3.proto` until T1 swaps it into `control.proto` (the running v1 wire must survive A0); Stage-1 "freeze control.proto" is satisfied by content, housed under the v3 filename — swap is a T1 entry item. *(Since formalized as ERR-06.)*
4. v1 proto gained ClockPing/Pong additively (numbers matching v3) — required for baseline trace/clock on the untouched wire.
5. Harness ACK framing is rig-private (12-B `AK`), documented as ≠ the real `FrameAck`; harness sender/receiver implement the 32-B header inline by design (disposable tooling).
6. `ack_order_violation` is tracked/printed but not in the frozen `harness_report_v1` JSON field list (worker declined to widen a specified schema unilaterally — correct call).
7. UDP length-mismatch fixtures classified `PROTOCOL_VIOLATION` (v11 §5.1 precedent); one-line README edit if `MALFORMED_FRAMING` is preferred.
8. `vtSession` accessor is `public` (cross-module doctor need; spec said "internal" pre-move).
9. `RescClockBridge` technique duplicated (~10 lines) in HarnessSender; hoist candidate noted.
10. Backend row constants (`extra_hw_frames = 8`, etc.) were orchestrator design decisions transcribed into WIRE.md (the errata only listed *what* to freeze).
11. `RescLog.flush()` added beyond spec to fix the real exit-before-flush evidence loss (§8.2).
12. Instance-lock/doctor/diagnostics run under profile id `moyunfei-desk-1` throughout.
13. Two `tail`-masked false-positive readings during ops were disclosed and corrected in-session; later verifications use `pipefail`.

## 13. Known gaps / not yet done (honest list for the reviewer)

1. ~~Malformed/golden `.bin` fixtures have no consuming parser yet~~ — **CLOSED by §15**: the v3 wire parsers now exist in both languages with fixture-sweep tests consuming all 16 record fixtures plus 4 envelope fixtures; the Stage-1 "consumed by Swift and Rust tests" gate is fully satisfied.
2. **A0 not run**: real-capture AU histogram (`max_record_bytes`), formal window trial protocol, trace-joined latency baseline + optical spot-check, `bitrate_bps` confirmation — the harness smoke is a preview, not the trial.
3. **Backend selection open** (both pass ERR-03): decide at Stage-2 with A0 numbers. Orchestrator's inclination: `sw1-lowdelay` (lag bound 1 vs 2, no GPU round-trip, p50 3.6 ms is ample at this resolution) — **not yet decided**.
4. `decoder_lag_bound`/`output_deadline_ms` need **clean-run** (stall-free) numbers for the profile; current experiment p95s include injected stalls.
5. Legacy paths untouched by design until T1: v1 UDP video/jitter machinery, the 0xFA scan (now shielded from clock traffic only), `video-decode` crate, mDNS, `--client` flag.
6. ~~Everything is **uncommitted**~~ *(resolved 2026-08-04: committed as candidate checkpoint `ce2d693`; the R7 clean checkpoint follows remediation)*; `RESC_BUILD_COMMIT`/dirty-tagging become meaningful post-commit.
7. Stray empty legacy dirs under `mac-host/Sources/` (pre-existing, unwired) left as-is.

## 14. Cold-start verification runbook

```bash
# Mac — repo root
tools/generate_proto.sh --check                      # regen-clean gate
cd mac-host && swift build && .build/debug/resc-fixture-check
.build/debug/remote-display-host --doctor            # brief display flash; expect exit 0
cd .. && /opt/homebrew/bin/cargo test -p diagnostics --manifest-path ubuntu-client/Cargo.toml

# Ubuntu box (wan@192.168.50.47; repo at ~/resc/remote_extended_screen)
export PATH=$HOME/.cargo/bin:$PATH FFMPEG_DIR=$HOME/ffmpeg7 LD_LIBRARY_PATH=$HOME/ffmpeg7/lib
cd ~/resc/remote_extended_screen/ubuntu-client
cargo check --workspace && cargo test -p diagnostics
bash ../tools/gen_sample_hevc.sh                     # if sample absent
cargo run -p decoder-experiment --release -- --input ~/resc/sample_1080x1920.h265 \
  --backend sw1-lowdelay --frames 240 --stall-every 30 --stall-ms 50   # expect pass:true (repeat: cuvid-lowdelay)
DISPLAY=:0 cargo run --release -- --doctor           # expect exit 0 (and --doctor-backend cuvid-lowdelay)

# Live harness (receiver on box, then sender on Mac)
cargo run -p harness-receiver --release -- --listen 0.0.0.0:9871 --backend sw1-lowdelay
# Mac: mac-host/.build/debug/resc-harness-sender --connect 192.168.50.47 --window 1 --seconds 10

# Contract test suite (§15)
# Mac:  PROTOC=$(which protoc) /opt/homebrew/bin/cargo test -p protocol --manifest-path ubuntu-client/Cargo.toml
#       mac-host/.build/debug/resc-fixture-check
# Box:  cargo test -p protocol -p diagnostics        # expect 49/49 + 12/12
python3 tools/gen_envelope_fixtures.py                # idempotent; regenerates envelopes/
```

## 15. Contract test suite (addendum — the six mandated case groups)

Added after the initial report at the user's direction; closes §13.1. Shared truth fixtures created first (orchestrator-inline, so both languages test against identical bytes): **`proto/fixtures/fatal_code_classes.json`** (23-entry code→class table — 22 nonzero codes + code 0 `unspecified` — from `control_v3.proto`); **`proto/fixtures/scroll_cases.json`** (12 ERR-04 cases incl. the `-INT_MIN`-under-rotation edge that forces fully-widened arithmetic); **`proto/fixtures/envelopes/*.bin` + `envelopes_manifest.json`** via new **`tools/gen_envelope_fixtures.py`** (4 hand-encoded v3 Envelopes — heartbeat, clock_ping, frame_ack, fatal_report — hex-audited, idempotent).

**Rust — `ubuntu-client/crates/protocol/src/v3wire.rs`** (1020 lines incl. inline tests; module exported from `lib.rs`; `serde_json` added as dev-dependency only):
- `WireError{ProtocolViolation(&'static str), RecordCapViolation{total, cap}}`; `parse_video_hello`, `parse_video_hello_ack` (status strictly 0..=3 → `AckStatus`), `parse_frame_header(bytes, max_record_bytes)` (unknown flag bits, ordinal domain 1..=i64::MAX, widened checked length vs cap), `parse_move` (26 B exact), `parse_cursor` (43 B exact; shape 0..=15; scale finite>0; **no reserved check — ERR-05**).
- `classify(code) -> Option<FailureClass{Deterministic|Transient|Terminal}>` — 1–9/21 det, 10–18/22 transient, 19–20 terminal, 0/unknown ⇒ None (review-10 rule).
- `newer_u32` / `newer_u24` (wrap-safe; masked 24-bit variant).
- `scroll_transform(dx, dy, rotated)` — swap **and** multiply in i64, single clamp (the fixture's `-(INT_MIN)` case is why).
- `scan_annexb` (3- and 4-byte start codes, HEVC type `(b>>1)&0x3F`), `validate_session_first` (VPS+SPS+PPS+IDR 19|20 required; **CRA ⇒ Err even alongside IDR**), `keyframe_claim_matches`.
- Tests: 41 new — golden-fixture value sweep, malformed-classification sweep (README manifest as oracle), envelope decode-assert-reencode-byte-equal, classification-vs-JSON (incl. every `FatalCode` enum value present), comparator edges (incl. `d == 0x8000_0000` boundary), 12 scroll cases, 8 RA scenarios.

**Swift — `mac-host/Sources/RescCore/`**: `WireRecords.swift` (259), `Comparators.swift` (23), `ScrollTransform.swift` (15), `FatalCodeClass.swift` (28), `RAVerification.swift` (83 — **hoisted from `Doctor.swift`**, which now delegates; a scoped `extension String: @retroactive Error` was required by the spec'd `Result<Void, String>` signature). `FixtureCheck/main.swift` extended 74 → 391 lines with sections (a)–(f) mirroring the Rust tests; `Package.swift`: FixtureCheck gains `RescProto` + SwiftProtobuf deps.

**Verification:** Rust 49/49 on the Mac **and** on the box; Swift 126/126 via `resc-fixture-check` (worker-run and orchestrator-rerun); host doctor re-run post-hoist: exit 0 with the identical NAL set (behavior-preserving proof); envelope re-encodes are **byte-identical across three encoders** (hand-python, prost, SwiftProtobuf).

**Process note:** the Rust worker was terminated by a session usage limit immediately after writing the code and before its build step; the orchestrator completed verification (first build passed 49/49 unmodified), performed the spec-fidelity review, and ran the remote proof. All six groups therefore carry the full evidence chain despite the interruption.
