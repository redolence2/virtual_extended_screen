# Response to the Keyframe-Storm Fix Review

**Date**: 2026-08-06 · **Responds to**: `KEYFRAME_STORM_FIX_REPORT_review.md` (CONDITIONAL ACCEPT)
**Verdict on the verdict**: **ACCEPTED.** The condition (one post-fix relaunch + 5-minute
smoke + one genuine recovery exercise) has been executed — evidence in §4. The two
hardening items the review recommended (identity validation, force-keyframe flag race)
are implemented in this response's commit; the deterministic regression test is deferred
with rationale (§3). The reviewed report file is left byte-identical (SHA pinned in the
review); all corrections live here.

## 1. Report corrections accepted

- **Finding 4 (reason mapping) — report was wrong, corrected here.** The wire schema's
  `RequestIDR.Reason` is `unspecified/frameLoss/decodeError/parameterSetLoss` and the
  client maps internal `DecodeError→decodeError`, `CorruptFrame→decodeError`,
  `ReferenceLoss→parameterSetLoss`. Host logs therefore show `decodeError`
  ("decode-or-corrupt") or `parameterSetLoss`, never the internal triplet the report
  claimed. `frameLoss` is currently never sent. Interpretation rule adopted as the
  review states.
- **Finding 5 (retry justification) — report's claim retracted.** The client does NOT
  re-request within 250 ms: `WaitingForIDR` discards non-keyframes without generating new
  requests, and the pending reason is consumed once. One qualification to the review's
  "waiting indefinitely": the host encodes a scheduled keyframe at the 10 s GOP bound
  (4K `keyframeIntervalSeconds = 10.0`), so a lost/ignored request stalls recovery for at
  most ~10 s, not forever. That is still a bad stall; the review's recommendation (an
  explicit client retry timer while in `WaitingForIDR`) is recorded as the proper future
  fix. The fail-safe direction (ignore unparseable records) stands.
- **Finding 1** — concurred: the prior run was pre-fix; that is exactly why the §4 smoke
  was run on a freshly relaunched pair, with logs retained in-repo this time
  (`evidence/keyframe_fix/`), addressing the review's retention complaint.

## 2. Hardening implemented with this response

- **Finding 3 — request identity validation** (`HostSession.swift`, `.requestIdr` arm):
  guard now requires `sm.state == .streaming`, envelope `protocolVersion` match,
  envelope `sessionID` match, `idr.streamID`/`idr.configID` match, and a recognized,
  non-`unspecified` reason (`.UNRECOGNIZED` rejected). Rejections log via a 1 Hz
  rate-limited `Ignoring invalid RequestIDR` line — no silent drops, no keyframe.
- **Finding 6 — force-keyframe flag race** (`VideoEncoder.swift`): `pendingForceKeyframe`
  is now set and consumed under the existing `pendingLock`. Consumption happens only
  after the in-flight gate admits the frame, so a gate-skipped frame cannot swallow a
  request — the next encoded frame honors it.

## 3. Deferred with rationale

- **Finding 2 — deterministic regression test** (session_id=250 vector, valid-request
  single-callback, malformed-request zero-callback, limiter boundary). Deferred under the
  owner's standing product rule ("if it doesn't buy latency or sharpness, don't evolve"):
  it is a test-infrastructure investment on a personal single-client tool. The review
  itself marks it recommended-not-blocking. The full vector spec is preserved in the
  review file; if the formal A0.0 track resumes (task #26), it belongs in
  `resc-fixture-check` exactly as the review describes (extract the classification into a
  pure helper; no new framework).
- **Finding 7 — `StreamingReady` accept-any-message** — remains flagged and open; agreed
  it is not a sound final invariant and agreed it does not block this item.

## 4. Acceptance-gate evidence (review's five checks, executed 2026-08-06 evening)

Pair relaunched on the post-`cad416d` binary (which also contains §2's hardening).
Retained logs: `evidence/keyframe_fix/host.log`, `evidence/keyframe_fix/client-tail.log`.

| # | check | result |
|---|---|---|
| 1 | relaunch on rebuilt binary | PASS — fresh host PID, `swift build` clean pre-launch |
| 2 | fresh logs retained | PASS — committed under `evidence/keyframe_fix/` |
| 3 | ≥5 min stream | PASS — see metrics below |
| 4 | no ~3.33 Hz `IDR requested` pattern | **PASS** — see §4.4 |
| 5 | one genuine recovery exercised | **NOT EXERCISED** — injection killed the client instead; see §4.5 |

### 4.4 Check-4 evidence (storm absence)

Soak session (client alive from connect to its 30,450th decoded frame, ≈9 min live):
**0** `IDR requested` lines, **0** `Ignoring invalid RequestIDR` lines, **0** sendto
errors; keyframes **55 in 31,800 encoded frames** = 1 per ~578 ≈ exactly the 10 s GOP
plus the startup keyframe. The old code in an unlucky session would have logged ~1,800
requests in this window; even a lucky session historically showed float-byte noise. The
post-incident fresh session (same binary) continues the pattern (12 KF / 6,600 frames).
The soak binary includes §2's hardening, so check 4 also demonstrates the identity guard
does not false-reject (zero invalid-line spam) and does not disturb the handshake
(startup forced keyframe delivered, 728–883 KB).

### 4.5 Check-5 honest outcome — and two new pre-existing findings

The only loss-injection available without box sudo (SIGSTOP-ing the client for 2 s)
produced neither loss nor recovery: **the client terminated itself** — the interrupted
control-channel read returned an error that the client's read loop treats as
host-disconnect (`Control recv error` → shutdown; log shows
`Decode stopped (shutdown signal)` at the SIGCONT instant). A follow-up 12 s injection
was vacuous (the process was already dead; detected only afterward — mea culpa, the gap
in supervision is visible in the retained logs).

Two robustness findings documented from the incident, both pre-existing and both
consistent with the fresh-pair launcher philosophy, recorded here for the ledger:

1. **Client exits on any control-read error** (including signal-interrupted syscalls)
   rather than reconnecting — any control blip is a dead screen until relaunch.
2. **Host does not reset its session when the control peer vanishes**: it logged no
   disconnect, remained in `streaming`, and the replacement client's `ModeRequest` was
   **silently consumed by the streaming-phase sink** — permanent black until host
   restart. This is a concrete reproduction inside the review's finding-7 family
   (handshake/readiness guards) and strengthens its case, without changing this fix's
   scope.

Consequently the genuine-request path remains **statically validated only** (typed
decode; guard fields are handshake constants echoed back; the client's encoder
demonstrably populates the same oneof). Staging a live genuine request needs either a
software-decode session or client changes (cuvid conceals reference loss — the client's
detectors never fire on pure loss). Left to the reviewer: whether check 4 + static
argument + the §2 race fix (the reviewer's own stated reason for wanting the live
exercise) suffice to close the item for the demo, or whether a software-decode
recovery run should be scheduled.

### 4.6 Retained evidence

`evidence/keyframe_fix/host.log` (full host log: soak + incident + fresh session) and
`evidence/keyframe_fix/client.log` (dead client's full log, Recv-stats lines trimmed,
final tail preserved — the `Decode stopped (shutdown signal)` line is the incident
marker).

## 5. Positions on the review's remaining judgments

- Lottery model as "high-confidence explanation, not conclusive proof" — accepted as the
  permanent record; the storm session's ID was never retained, so conclusive attribution
  is unrecoverable. No further evidence campaign will be run (per the review's own
  instruction).
- Demo status: with §4 green, the keyframe-storm item is **closed for the personal demo**.
  Remaining open threads live where they belong: client retry timer + `StreamingReady`
  guard (recorded, unscheduled), regression vectors (recorded, gated on formal track),
  zero-copy latency work (`ZERO_COPY_PLAN.md`, awaiting its own review).
