# CONTRACT_ERRATA — normative corrections to IMPLEMENTATION_PLAN_V11.md

Governance: per V11 §14, findings against the contract are recorded here (dated) and reflected in
`proto/control.proto` / `docs/WIRE.md` / fixtures / code — never in a new plan. Each entry below is
normative and overrides the corresponding V11 text. Source: `IMPLEMENTATION_PLAN_review_v11.md`.

---

## 2026-07-30 · ERR-01 — Cross-TCP activation barrier (blocks Stage-1 freeze and T1)

`VideoHelloAck(OK)` travels on the video TCP connection; input/heartbeats travel on control. Without a
barrier, a fast client's first input can reach the host before the host processes the Ack and be killed
as `PROTOCOL_VIOLATION`. Normative fix (no new protobuf message):

1. After writing `VideoHelloAck(OK)`, the client emits **no** input and **no** client heartbeats.
2. After the host receives and accepts that Ack, it immediately sends one `Heartbeat` on control.
3. Receipt of that host heartbeat is the client's **activation signal**: only then does it arm input
   and start its normal heartbeats.
4. Capture may start when the host accepts the Ack (unchanged).

Freeze in `docs/WIRE.md`. Required test: delayed/reordered scheduling of the two TCP handlers proves no
client control payload is written before activation and the first post-barrier input is accepted.

## 2026-07-30 · ERR-02 — Backend IDs fully frozen; `TBD-A00` scope (blocks Stage-1 freeze)

`docs/WIRE.md` §Backend must give, for **each** of `cuvid-lowdelay` and `sw1-lowdelay`: FFmpeg decoder
name + hardware-device setup; pixel-format selection; codec flags/options; threading mode and exact
thread count; frame-reordering/low-delay settings; exact surface-pool / `extra_hw_frames` value;
permitted output format; and the statement that fallback is forbidden.

`"decoder_backend":"TBD-A00"` is accepted **only** by the placeholder canonicalization fixture and
A0.0 measurement tooling. A normal handshake or final-profile doctor rejects it. During A0.0 the
decoder doctor takes one explicit candidate from the two closed IDs and logs it. After Stage 2, normal
doctor mode opens exactly the final profile's backend and accepts no override.

## 2026-07-30 · ERR-03 — Ordinal-faithful decoder timestamps required (blocks backend selection)

The FIFO alternative in V11 §13 ("Token/FIFO" gate) is **removed**. A candidate backend passes only if
induced-delay tests prove every emitted frame preserves the submitted `frameOrdinal` in `AVFrame.pts`
or `best_effort_timestamp` — including across EAGAIN and packets that emit zero or multiple frames.
If neither candidate passes, an accepted-ordinal FIFO mapping would need its own exact contract and
tests before use; it must not be silently inferred.

## 2026-07-30 · ERR-04 — Exact scroll injection (blocks scroll fixture/WIRE freeze)

The point-conversion sentence in V11 §5.4 is **removed**. Normative rule: rotate the signed SDL steps
(axis swap per §5.4), multiply each by the 10-pixel quantum using widened checked arithmetic, saturate
to `i32`, and pass the result directly to a CoreGraphics **pixel-unit** scroll event. No point
conversion, no fractional rounding path. Fixtures must cover positive, negative, rotated, and
overflow/saturation cases.

## 2026-07-30 · ERR-05 — UDP hygiene gate wording (clerical)

The move and cursor UDP layouts contain no reserved field. The word "reserved" is removed from the
"UDP hygiene" gate only. Reserved-zero validation stays for the video handshake and frame-record
formats, which do contain reserved bytes. Neither UDP layout changes.

---

## 2026-08-04 · ERR-06 — Normative Stage-1 schema path (resolves A00 report review, finding 4)

Until T1's cutover deletes the legacy v1 wire, the normative Stage-1 protocol-v3 schema is
housed at **`proto/control_v3.proto`** (package `resc.v3`); `proto/control.proto` remains the
legacy `resc.control` v1 schema (plus the additive A0.0 trace-mode ClockPing/Pong fields) so
the running A0 baseline keeps compiling. Every freeze/regen/check reference to "control.proto
(v3)" reads on `control_v3.proto` until T1, whose entry checklist includes the file swap
(v3 content → `control.proto`, v1 retired under an explicitly legacy filename or deleted).
This entry formalizes what the A0.0 report had recorded as deviation §12.3.

---

## Implementation proofs required (recorded here; not plan changes)

- **Late capture callbacks:** each ScreenCaptureKit callback is bound at creation to its run; a
  callback from a torn-down capture session must not populate a newer run's pending slot. Test:
  teardown → new run → late callback.
- **Cursor `timestamp_us`:** sender-local diagnostic time in the host continuous-monotonic domain.
  Sequence number — never the timestamp — governs ordering, liveness, and presentation.
- **Profile hash bytes:** hash the JSON payload only — a file-writing helper must not include a
  trailing newline in the hashed bytes. Pinned placeholder values (verified by review 11):
  SHA-256 `0cc22496628805973f8d52292e7f838b95ec023faf658d71dd862f3fbf4ed6ff`, 8-byte prefix
  `0cc2249662880597`.
