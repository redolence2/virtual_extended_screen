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

## 2026-08-04 · ERR-07 — ASCII-only profile strings (replaces the NFC canonicalization rule)

V11 §2 and `docs/WIRE.md` §9 list "NFC-normalized strings" among the canonicalization rules. For
this fixed personal profile the value vocabulary is closed and entirely ASCII, so Unicode
normalization machinery is unreachable complexity — and worse, the two languages' JSON serializers
may disagree on non-ASCII emission (raw UTF-8 vs escape sequences), so a non-ASCII value could
produce divergent canonical-form verdicts. Normative rule, replacing NFC: **every profile key and
every string value MUST consist only of ASCII bytes.** Both validators check this on the parsed
document (covering raw-UTF-8 and \uXXXX-escaped encodings alike) **before** any canonical-bytes
comparison, so both languages return the same verdict on the same bytes. No Unicode normalization
machinery anywhere. Fixtures: `proto/fixtures/profile.canonical.json` (valid, all-ASCII);
`profile_nonascii_raw.json` and `profile_nonascii_escaped.json` (sorted/minified, real backend id,
one non-ASCII `profile_id` value in raw and escaped encodings — rejected by both validators with
the ASCII verdict, attributing rejection to this rule alone). Generator:
`tools/gen_profile_fixtures.py` (idempotent).

## 2026-08-04 · Governance note — `docs/WIRE.md` status line during remediation

Recorded before the corresponding WIRE.md edit, per that file's own governance header. The status
line previously read "Stage-1 structural freeze (A0.0)"; the freeze has **not** occurred — the
A0.0 completion claim was withdrawn (see the `A00_IMPLEMENTATION_REPORT.md` status-correction
banner and `A00_REMEDIATION_PLAN.md`). The line now reads "**Stage-1 candidate; freeze pending
A0.0 gates and clean checkpoint**" until the R7 clean-checkpoint evidence passes independent
re-review. No structural fact in WIRE.md changes under this note; ERR-07 (above) is the only
concurrent structural change and carries its own entry.

---

## 2026-08-04 · ERR-08 — Clock-sample acceptance: best-sample selection replaces the 5 ms cutoff

V11 §10's clock-sync acceptance gate ("reject delay ≥ 5 ms") assumed a wired sub-millisecond
LAN. The deployment link measures ~7 ms RTT (A0 harness p50 7.09 ms; two R4 live-gate runs —
30 s and 90 s, ~12 ping cycles total — accepted **zero** samples), so the gate structurally
empties the joined trace artifact's offset column on the only link this personal deployment
uses. Normative replacement: accept every sample with non-negative delay below a 100 ms sanity
ceiling; per-sample `uncertainty_us = delay_us / 2` (the standard NTP bound on offset error —
larger on this link, and carried honestly rather than hidden by a reject gate); the existing
minimum-delay best-sample selection continues to pick the authoritative offset; the joined
artifact records each sample's offset ± uncertainty explicitly. The A0 optical spot-check
remains the end-to-end validation of transferred offsets. Discovered by, and first verified
against, the R4 live joined-artifact gate.

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
