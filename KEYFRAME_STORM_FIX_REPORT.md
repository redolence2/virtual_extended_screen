# Keyframe-Storm Fix — Completion Report for Review

**Date**: 2026-08-06 · **Commit**: `cad416d` (host-only, 21+/24−) · **Origin**: `LOCAL_NETWORK_DILEMMA_REPORT_review.md` finding 6
**Status**: implemented, built, committed; running pair predates it — **live verification pending next relaunch** (§8)
**Scope**: `mac-host/Sources/RemoteDisplayHost/HostSession.swift` only. Wire, client, encoder untouched.

---

## 1. The defect (as shipped since the demo era)

`HostSession.handleStreamingMessage` detected "client requests a keyframe" like this
(verbatim, now deleted):

```swift
// Tag = (31 << 3) | 2 = 250.
// This is a minimal check; full protobuf parsing comes in Milestone C (Item 2).
if data.contains(where: { _ in true }) {
    // Try to find RequestIDR field tag (varint 250 = 0xFA)
    for i in 0..<data.count {
        if data[i] == 0xFA && i + 1 < data.count {
            // Likely RequestIDR message — force a keyframe
            ...rate-limit 250ms... onForceKeyframe?()
```

It scanned **every byte of every streaming-phase control payload** for the value `0xFA`
(250 — the protobuf tag of `Envelope.request_idr`, field 31, wire type 2). The comment
records it as a placeholder that "Milestone C" would replace; Milestone C shipped the
typed decode for *clock* messages (the intercept above it) but this scan survived below.

## 2. Root-cause mechanics — where stray `0xFA` bytes actually come from

Client→host control traffic during streaming (all in
`ubuntu-client/crates/net-transport/src/control_channel.rs`, all
`resc_control::Envelope { session_id, protocol_version, payload }`):

| message | cadence | fields that can contain a raw `0xFA` byte |
|---|---|---|
| `Stats` (`send_stats`, line 136) | **every 100 ms** (`main.rs:416` interval; sent whenever any counter moved, `main.rs:476-504`) | `packet_loss_rate`/`frame_drop_rate` are `f32` fixed32 — 8 arbitrary IEEE-754 bytes per message; **plus the envelope's `session_id`** |
| `RequestIDR` (`send_request_idr`, line 173) | on decoder corruption (`main.rs:458-463`) | the intended `0xFA` tag itself |
| `ClockPing` | 10 s, **trace mode only**; already intercepted by the typed path before the scan (a prior fix — evidence this trap was already known once) | n/a |

Two distinct misfire modes follow:

- **Per-session lottery (the storm).** `session_id` is a random u64 encoded as a varint
  in *every* envelope. A u64 varint has up to 9 continuation bytes; each has ~1/128
  probability of being exactly `0xFA` → **roughly 7% of sessions** get a session ID whose
  encoding contains `0xFA`. For such a session, *every* control message — including the
  10 Hz Stats stream — "is" an IDR request. The 250 ms limiter then admits one forced
  keyframe per ≥250 ms, which on a 100 ms message grid locks to one per 300 ms ≈
  **3.33/s for the entire session**.
- **Per-message noise (sporadic).** In lucky sessions, occasional Stats messages whose
  float bytes contain `0xFA` force isolated spurious keyframes.

**Arithmetic check against the observed storm** (2026-08-06 morning run, counted by the
external reviewer from the live log): 4,246 `IDR requested` lines, 4,247 keyframes,
~82,800 encoded frames. Encoder runs from capture start; at 3.33/s the 4,246 requests
imply ~1,270 s of *streaming* within the ~1,660 s of *encoding* — consistent with the
observed several minutes of pre-client capture. The **current** session (different random
session ID) shows 20 keyframes in 10,200 frames — exactly the lucky/unlucky split the
lottery model predicts. (Model, not proof; §8's live protocol makes it falsifiable.)

Real corruption-driven requests (mode 2 of the table) are indistinguishable in old logs —
a storm could also mask genuine recovery traffic. That ambiguity is itself part of the
defect.

## 3. Impact when the storm fires

- A 2160×3840 keyframe measures **0.4–1.3 MB** (owner logs: 412 KB, 820 KB, 847 KB,
  978 KB, 1021 KB, 1270 KB) vs ~50–150 KB deltas.
- 3.33 KF/s × ~0.8 MB ≈ 2.7 MB/s ≈ **21 Mb/s of the 50 Mb/s encoder budget** burned on
  redundant intra frames → delta-frame quality starved (visible softening under motion).
- Each keyframe is ~500–900 UDP chunks; the burst pacer stretches sends by ~6–10 ms per
  keyframe → periodic latency ripple at 3.33 Hz.
- Decoder chews full intra frames instead of cheap deltas — extra GPU load on the box,
  feeding the exact contention that `ZERO_COPY_PLAN.md` Part B targets.
- Genuine `ReferenceLoss` recovery requests drown in the noise — corruption diagnosis was
  effectively blind.

## 4. The fix (commit `cad416d`, verbatim)

`handleMessage` already decodes every inbound record as `Resc_Control_Envelope` for the
clock intercept. The oneof switch gained one arm, placed before `default`:

```swift
case .requestIdr(let idr):
    // Typed decode — replaces the legacy 0xFA byte-scan, which
    // fired on any control payload containing that byte value.
    // reason is the client's IDRReason discriminant, logged so
    // keyframe storms are attributable (real corruption vs. bug).
    if sm.state == .streaming {
        forceKeyframeRateLimited(reason: idr.reason)
    }
    return
```

The byte-scan body of `handleStreamingMessage` is deleted (the function remains as the
silent consumer for Stats/unknown payloads), and the rate-limit logic moved intact into:

```swift
private func forceKeyframeRateLimited(reason: Resc_Control_RequestIDR.Reason) {
    let lastIDRTime = lastIDRRequestTime ?? Date.distantPast
    let elapsed = Date().timeIntervalSince(lastIDRTime)
    if elapsed >= 0.25 { // Rate limit: 250ms
        lastIDRRequestTime = Date()
        onForceKeyframe?()
        print("[RESC] IDR requested by client (reason=\(reason), rate-limited)")
    }
}
```

Same 250 ms limit, same `onForceKeyframe` callback, now gated on an actual decoded
`RequestIDR` — plus the client's `reason` (DecodeError / CorruptFrame / ReferenceLoss,
from `video-decode`'s `IDRReason`) in every log line.

## 5. Wire-compatibility argument

Both ends build/parse the same generated schema (`resc_control` prost types ↔
`Sources/Protocol/control.pb.swift`); the client's encoder
(`send_request_idr`, control_channel.rs:173-184) populates exactly the oneof arm the new
case matches. The typed decode path itself is production-proven — the clock-ping
intercept has run through it since Milestone C. No bytes on the wire change.

## 6. Behavioral deltas (enumerated for adversarial review)

1. **Unparseable payloads**: OLD — still byte-scanned (could force a keyframe);
   NEW — ignored. Fail-safe direction: on TCP, an unparseable record means a framing/
   schema bug, and a client that truly needs recovery re-requests within 250 ms anyway.
2. **`RequestIDR` arriving during negotiation**: OLD — consumed by
   `handleStreamingReady`'s any-message guard (could falsely complete the handshake);
   NEW — intercepted and dropped (`sm.state` gate). A hidden misdispatch removed.
   NOTE: `StreamingReady`'s own accept-any-message behavior is pre-existing and
   **unchanged** — out of this fix's scope, flagged for the record (also listed in
   `LOCAL_NETWORK_DILEMMA_REPORT_response.md` §3 as descoped).
3. **Log format**: `(rate-limited)` → `(reason=X, rate-limited)`. No tooling parses this
   line (the trace joiner consumes JSONL, not stdout); grep-counts on
   `IDR requested` still work.
4. **Stats**: still consumed silently (unchanged scope; typed Stats handling would be a
   separate, optional follow-up).

## 7. What this fix does NOT claim

It does not eliminate keyframe storms whose cause is real corruption (e.g. sustained
assembler frame_drops driving `ReferenceLoss` requests) — it makes them **attributable**.
If reason-tagged storms persist after relaunch, that is a true signal of the decode-side
contention pathology, and the zero-copy work (`ZERO_COPY_PLAN.md` Part B) is the recorded
attack on its cause.

## 8. Verification

**Done**: `swift build` clean (the compiler enforced the generated enum type on `reason`);
static analysis above; commit `cad416d`.

**Pending (requires the next icon relaunch — the running pair predates the fix)**:
1. Steady desktop, no motion, ≥5 min: expect **zero** `IDR requested` lines and keyframes
   only at the 10 s GOP cadence (`Encode: N frames, M KF` ratio ≈ 600:1).
2. Repeat across ≥3 sessions (re-rolls the session-ID lottery): no session shows the
   3.33/s storm signature.
3. Any `IDR requested` lines that do appear carry a `reason=` tag; forced-drop test
   (brief WiFi interruption) shows `ReferenceLoss`/`DecodeError` requests arriving and
   being honored.

## 9. Questions for the reviewer

1. Does §2's lottery model satisfactorily explain the observed 4,246 (vs. the
   alternative: sustained real corruption requests)? §8.2 discriminates them — should
   the seal WAIT for that live evidence, or is the static argument sufficient to close
   this item with §8 as a standing check?
2. Any objection to leaving Stats untyped-silent and `StreamingReady`'s any-message
   guard as-is (both flagged, both pre-existing)?
3. Is the fail-safe choice in §6.1 (ignore unparseable records rather than guess)
   acceptable on a TCP control channel?
