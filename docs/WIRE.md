# RESC Wire Protocol

| | |
|---|---|
| **Date** | 2026-07-30 |
| **Status** | **Stage-1 structural freeze (A0.0).** |
| **Governance** | Normative. Any change to a fact in this document requires a dated entry in `CONTRACT_ERRATA.md` first; this file is then updated to match. Do not hand-edit a structural fact here without a corresponding errata entry. |
| **Normative sources** | `IMPLEMENTATION_PLAN_V11.md` §1, §4, §5, §7, §9 + `CONTRACT_ERRATA.md` (all entries). Where the two disagree, `CONTRACT_ERRATA.md` is later and wins — it exists specifically to record corrections against this plan. |
| **Companions** | `proto/control_v3.proto` (protobuf schema) · `proto/fixtures/*` (binary fixtures) · `tools/gen_fixtures.py` (fixture generator) |

## Global rules

These apply to every binary record defined below unless a section says otherwise:

1. **Little-endian everywhere in non-protobuf records.** Every fixed-width integer and float in `VideoHello`/`VideoHelloAck`, the frame record header, and the UDP records is little-endian. (The protobuf `Envelope` uses protobuf's own wire encoding; only its outer `u32_le` length prefix is a raw little-endian integer.)
2. **Magics are literal bytes.** Every magic value shown in this document is the exact byte sequence to write and compare — not a numeric constant to encode some other way.
3. **Reserved fields are zero on send and validated on receive.** A sender writes zero into every reserved field; a receiver rejects a record whose reserved field is nonzero.
4. **Every UDP datagram is exactly its declared length.** No padding, no truncation; a datagram of the wrong length for its record type is rejected.

---

## 1. Control framing

Source: `IMPLEMENTATION_PLAN_V11.md` §4.

Framing: `u32_le` length + protobuf `Envelope` (package `resc.v3`; schema in `proto/control_v3.proto`).

**Bounds:** reject `length > 64 KiB` **before frame allocation** — the length prefix is inspected first; the receiver must not allocate a buffer sized to an over-length claim.

**Per-field caps**, validated **immediately post-decode**, before any state mutation, logging of contents, injection, or copying:

- strings ≤ 256 B, except `FatalReport.summary` ≤ 2048 B
- `profile_canonical` ≤ 4096 B
- hashes (`profile_hash`) exactly 8 B

**Oneof semantics:** the generated protobuf decoder's unknown-field ignoring and last-one-wins `oneof` resolution are accepted as-is. An **absent** `payload` ⇒ `PROTOCOL_VIOLATION`. No raw/manual scanner second-guesses the generated decoder.

### Direction/state table

A violation of any row below ⇒ `PROTOCOL_VIOLATION`. Pre-Ack input is never injected, regardless of what arrives on the wire.

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

---

## 2. Video handshake — `VideoHello` / `VideoHelloAck`

Source: `IMPLEMENTATION_PLAN_V11.md` §5.1.

### `VideoHello` (32 B, host→client)

| Offset | Size | Field | Value |
|---|---|---|---|
| 0 | 4 B | magic | `52 53 43 56` |
| 4 | 1 B | `ver` | `u8` = 3 |
| 5 | 1 B | `res` | `u8` = 0 (reserved) |
| 6 | 2 B | `len` | `u16` LE = 32 |
| 8 | 8 B | `session_run_id` | `u64` LE |
| 16 | 8 B | `profile_hash` | 8 raw bytes |
| 24 | 8 B | `res` | `u64` LE = 0 (reserved) |

Total: 32 B.

### `VideoHelloAck` (16 B, client→host)

| Offset | Size | Field | Value |
|---|---|---|---|
| 0 | 4 B | magic | `52 53 43 41` |
| 4 | 1 B | `ver` | `u8` = 3 |
| 5 | 1 B | `status` | `u8`, 0–3 (frozen; see below) |
| 6 | 2 B | `len` | `u16` LE = 16 |
| 8 | 8 B | `session_run_id` | `u64` LE |

Total: 16 B.

### Ack status bytes (frozen)

| Status | Name | Meaning | Failure class |
|---|---|---|---|
| 0 | OK | accepted; video/input may open | — |
| 1 | MISMATCH | structurally valid Hello, wrong active run or profile | deterministic |
| 2 | BUSY | a video socket is already active for this run | transient |
| 3 | INTERNAL | local preparation failed | transient |
| any other byte | — | — | ⇒ `PROTOCOL_VIOLATION` |

### Client OK-checklist

The client returns `OK` only if **all** of the following hold:

- peer IP == Mac IP (§1 trust model)
- magic/version/length exact
- reserved zero
- `session_run_id == activeRun`
- `profile_hash == activeProfileHash`
- no other active video socket for this run

Any of magic/version/length/reserved being wrong makes the Hello malformed ⇒ `PROTOCOL_VIOLATION`.

### Host Ack-checklist

The host accepts the Ack only if:

- magic/version/length exact
- status is a known value (0–3)
- Ack run == dialed run

Only status `OK` opens capture/input.

### Stale-dial run tagging

Every asynchronous video connect/Hello/Ack operation captures its run ID at initiation. The host requires `ack.run == dialedRun == activeRun` before mutating state; the client requires `hello.run == activeRun`. A late socket/response for an old run is closed and logged **without failing the newer active run**. After answering a wrong-run Hello with `MISMATCH`, the client keeps awaiting the correct current-run Hello until the handshake deadline.

---

## 3. ERR-01 — Cross-TCP activation barrier

Source: `CONTRACT_ERRATA.md` ERR-01 (blocks Stage-1 freeze and T1; normative, overrides any conflicting reading of the plan). `VideoHelloAck(OK)` travels on the video TCP connection; input/heartbeats travel on control. Without a barrier, a fast client's first input could reach the host before the host processes the Ack and be killed as `PROTOCOL_VIOLATION`. Normative fix, verbatim:

1. After writing `VideoHelloAck(OK)`, the client emits **no** input and **no** client heartbeats.
2. After the host receives and accepts that Ack, it immediately sends one `Heartbeat` on control.
3. Receipt of that host heartbeat is the client's **activation signal**: only then does it arm input and start its normal heartbeats.
4. Capture may start when the host accepts the Ack (unchanged).

Required test (normative): delayed/reordered scheduling of the two TCP handlers proves no client control payload is written before activation, and the first post-barrier input is accepted.

---

## 4. Frame records

Source: `IMPLEMENTATION_PLAN_V11.md` §5.2.

### Header (32 B)

| Offset | Size | Field | Notes |
|---|---|---|---|
| 0 | 2 B | magic | `56 46` |
| 2 | 1 B | `headerLen` | `u8`; must equal 32 |
| 3 | 1 B | `flags` | `u8`; bit0 = keyframe-claim; any unknown bit set ⇒ `PROTOCOL_VIOLATION` |
| 4 | 8 B | `frameOrdinal` | `u64` LE; domain `1..i64::MAX`; session-local; +1 per AU |
| 12 | 4 B | `captureSeq` | `u32` LE |
| 16 | 8 B | `contentCaptureTs_us` | `u64` LE; host continuous-monotonic µs epoch |
| 24 | 4 B | `res` | `u32` LE = 0 (reserved) |
| 28 | 4 B | `payloadLen` | `u32` LE |

Total header: 32 B. The payload (`payloadLen` bytes) follows immediately and is **one Annex-B HEVC AU**.

### Cap validation

Validate `u64(headerLen) + u64(payloadLen)` using checked, widened arithmetic — both operands are promoted to `u64` before the add, so the add itself cannot overflow — **before allocating space for the payload**. If the sum exceeds the active profile's `max_record_bytes` ⇒ `RECORD_CAP_VIOLATION`.

---

## 5. UDP records

Source: `IMPLEMENTATION_PLAN_V11.md` §5.3, §9; `CONTRACT_ERRATA.md` ERR-05.

### Prefix (14 B, all UDP records)

| Offset | Size | Field | Value |
|---|---|---|---|
| 0 | 4 B | magic | `52 45 53 43` |
| 4 | 1 B | `ver` | `u8` = 3 |
| 5 | 1 B | `type` | `u8`; 1 = cursor, 2 = move |
| 6 | 8 B | `session_run_id` | `u64` LE |

### Move (26 B, client→host)

| Offset | Size | Field |
|---|---|---|
| 0 | 14 B | prefix (above) |
| 14 | 4 B | `seq` — `u32` LE |
| 18 | 4 B | `x` — `i32` LE |
| 22 | 4 B | `y` — `i32` LE |

Total: 26 B.

### Cursor (43 B, host→client)

Body fields at absolute offsets 14–42, per `IMPLEMENTATION_PLAN_V11.md` §5.3:

| Offset | Size | Field |
|---|---|---|
| 0 | 14 B | prefix (above) |
| 14 | 4 B | `seq` — `u32` LE |
| 18 | 8 B | `timestamp_us` — `u64` LE |
| 26 | 4 B | `x_px` — `i32` LE |
| 30 | 4 B | `y_px` — `i32` LE |
| 34 | 1 B | `shape_id` — `u8` (0–15) |
| 35 | 2 B | `hotspot_x_px` — `u16` LE |
| 37 | 2 B | `hotspot_y_px` — `u16` LE |
| 39 | 4 B | `cursor_scale` — `f32` LE |

Total: 43 B (the record ends at offset 42, i.e. bytes 39–42 inclusive hold `cursor_scale`).

### Validation

Every receiver rejects a UDP record with the wrong magic, length, run, or source (the T1 "UDP hygiene" gate, wording corrected by ERR-05 — see note below):

- **magic** — must equal the literal prefix bytes above.
- **length** — the datagram must be exactly 26 B (move) or 43 B (cursor); any other length is rejected.
- **run** — `session_run_id` must equal the receiver's `activeRun`.
- **source** — accepted only from the profile peer IP (§1 trust model).

**Note (ERR-05):** neither UDP layout contains a reserved field. The word "reserved" was removed from the "UDP hygiene" gate wording as a clerical fix; reserved-zero validation still applies to the video handshake (§2 above) and frame-record (§4 above) formats, which do contain reserved bytes. Neither UDP layout changed.

### Sequence comparator (ordering/liveness)

Source: `IMPLEMENTATION_PLAN_V11.md` §9.

```
newer(a, b)  ⇔  d ≠ 0 ∧ d < 2^31,   where d = (a − b) mod 2^32
```

`lastSeq` resets per session. This comparator governs `seq` wraparound comparisons for both move and cursor records.

### Cursor `timestamp_us` semantics

Source: `CONTRACT_ERRATA.md`, "Cursor `timestamp_us`" implementation proof. `timestamp_us` is **sender-local diagnostic time in the host continuous-monotonic domain** — it is informational only. The **sequence number**, never the timestamp, governs ordering, liveness, and presentation.

---

## 6. Input & cursor semantics

Source: `IMPLEMENTATION_PLAN_V11.md` §5.4; `CONTRACT_ERRATA.md` ERR-04.

### Coordinates

Portrait StreamSpace pixels, domain `0…1079 × 0…1919`. The client applies the inverse −90° render transform (including letterbox scale/offset) before sending; the host clamps to profile bounds and maps via the live `CGDisplayBounds`.

### Scroll — ERR-04 (normative; supersedes the point-conversion sentence in V11 §5.4)

`CONTRACT_ERRATA.md` ERR-04, verbatim:

> The point-conversion sentence in V11 §5.4 is **removed**. Normative rule: rotate the signed SDL steps (axis swap per §5.4), multiply each by the 10-pixel quantum using widened checked arithmetic, saturate to `i32`, and pass the result directly to a CoreGraphics **pixel-unit** scroll event. No point conversion, no fractional rounding path. Fixtures must cover positive, negative, rotated, and overflow/saturation cases.

The surrounding V11 §5.4 facts that ERR-04 leaves intact, and that the rule above depends on:

- One wire unit = **one raw SDL wheel step**.
- The pixel-scroll quantum is **N = 10 pixels** (this constant).
- Rotation (axis swap): the client swaps axes before sending — `dx' = −dy, dy' = dx`.
- Sign: the client forwards SDL's sign unchanged (natural scrolling as SDL reports it); the host applies it as-is.
- The sign/axis convention is pinned by reference fixtures.

### Cursor shapes (0–15)

| ID | Shape |
|---|---|
| 0 | arrow |
| 1 | ibeam |
| 2 | crosshair |
| 3 | openHand |
| 4 | closedHand |
| 5 | pointingHand |
| 6 | resizeN |
| 7 | resizeS |
| 8 | resizeE |
| 9 | resizeW |
| 10 | resizeNS |
| 11 | resizeEW |
| 12 | resizeNESW |
| 13 | resizeNWSE |
| 14 | notAllowed |
| 15 | wait |

### Hidden convention

The cursor is hidden ⇔ `x == −1 ∧ y == −1` (both fields together, not either alone).

### Hotspot/scale constants

`hotspot_x_px`/`hotspot_y_px`/`cursor_scale` are per-shape constants **in this profile**, and the constant table is uniform: **hotspot (0, 0) and scale 1.0 for every one of the 16 shapes** (matching the current sender, which only ever emits those values). The 43-B wire layout stays frozen — the fields remain on the wire — but the client validates received `hotspot`/`cursor_scale` values against these constants and, on mismatch, counts the mismatch and substitutes the constants rather than trusting the wire value. The packed T2 snapshot claims coherence for `(x, y, shape, seq)` only — not for hotspot/scale.

### Grab state machine (client-local; the host does not track it)

`released`/local → `Ctrl+Alt+G` consumed locally ⇒ `grabbed` (relative mouse) → only `grabbed` emits key/button/scroll/move → `Ctrl+Alt+Esc` consumed locally ⇒ sends a reliable `ReleaseInput`, returns to `released`; quit sends `ReleaseInput` when possible. Accepted looseness: one delayed pre-release UDP move may move the pointer once more — harmless.

---

## 7. Backend

Source: `CONTRACT_ERRATA.md` ERR-02 (blocks Stage-1 freeze); cross-referenced by `IMPLEMENTATION_PLAN_V11.md` §2, which calls `decoder_backend` "a closed configuration ID." `decoder_backend` is one of exactly two closed configuration IDs. Both are fully specified below; Stage 2 selects one of the two — it may not invent a third or new hashed keys.

| Config ID | Decoder | HW device | Pixel format | Codec flags | Threading | Surface pool | Permitted output | Fallback |
|---|---|---|---|---|---|---|---|---|
| `cuvid-lowdelay` | `hevc_cuvid` | CUDA, created via `av_hwdevice_ctx_create` defaults | decoder-default NV12 output after GPU→CPU `av_hwframe_transfer_data` | `AV_CODEC_FLAG_LOW_DELAY` set before open | n/a (hardware) | `extra_hw_frames = 8` (explicit) | NV12 (post-transfer) | forbidden |
| `sw1-lowdelay` | `hevc` (native software) | none | yuv420p | `AV_CODEC_FLAG_LOW_DELAY` | `thread_count = 1`, `thread_type` none — no frame-threading | n/a (software) | yuv420p | forbidden |

Init failure on the selected backend ⇒ `REQUIRED_NATIVE_API`. There is no silent fallback path between the two IDs, or to any third option.

### `TBD-A00` scope

`"decoder_backend":"TBD-A00"` (the §9 canonical-profile placeholder value) is accepted **only** by the placeholder canonicalization fixture and A0.0 measurement tooling. A normal handshake or final-profile doctor **rejects** it. During A0.0, the decoder doctor takes one explicit candidate from the two closed IDs above and logs it. After Stage 2, normal doctor mode opens exactly the final profile's backend and accepts no override.

---

## 8. ERR-03 — Ordinal-faithful decoder timestamps

Source: `CONTRACT_ERRATA.md` ERR-03 (blocks backend selection). The "Token/FIFO" alternative in `IMPLEMENTATION_PLAN_V11.md` §13 ("Token/FIFO" gate) is **removed**. A candidate backend (§7 above) passes selection only if induced-delay tests prove that **every emitted frame preserves the submitted `frameOrdinal`** in `AVFrame.pts` or `best_effort_timestamp` — including across `EAGAIN` and packets that emit zero or multiple frames. If neither candidate passes, an accepted-ordinal FIFO mapping would need its own exact contract and tests before use; it must not be silently inferred.

---

## 9. Canonical profile

Source: `IMPLEMENTATION_PLAN_V11.md` §2; `CONTRACT_ERRATA.md`, "Profile hash bytes" implementation proof.

### Canonicalization rules

- UTF-8 JSON
- keys sorted lexicographically
- no whitespace (minified)
- base-10 integers
- NFC-normalized strings

### Placeholder bytes (exact, one line)

```json
{"bitrate_bps":20000000,"client_ip":"192.168.50.47","codec":"hevc-main-8bit","control_port":9870,"cursor_udp_port":9873,"decoder_backend":"TBD-A00","decoder_lag_bound_frames":4,"display_index":0,"flow_window_frames":1,"frame_reordering":false,"height_px":1920,"host_ip":"192.168.50.125","max_record_bytes":2097184,"move_udp_port":9872,"output_deadline_ms":200,"profile_id":"moyunfei-desk-1","protocol_version":3,"refresh_hz":60,"rotation":"portrait-neg90-render","video_port":9871,"width_px":1080}
```

`profile_hash` = first 8 bytes of SHA-256 over exactly those bytes.

- SHA-256: `0cc22496628805973f8d52292e7f838b95ec023faf658d71dd862f3fbf4ed6ff`
- 8-byte prefix (`profile_hash`): `0cc2249662880597`

### No-trailing-LF warning

Source: `CONTRACT_ERRATA.md` implementation proof, "Profile hash bytes." Hash the JSON payload **only** — a file-writing helper must not include a trailing newline in the hashed bytes. A text editor or an `echo` without `-n` that appends `\n` when saving/printing the canonical bytes will silently produce the wrong hash. The pinned values above are verified against the exact bytes shown, with no trailing LF.
