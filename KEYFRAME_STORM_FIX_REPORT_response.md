# Response to the Keyframe-Storm Fix Review

**Date**: 2026-08-06 · **Responds to**: `KEYFRAME_STORM_FIX_REPORT_review.md` (CONDITIONAL ACCEPT)
**Verdict on the verdict**: **ACCEPTED.** The post-fix relaunch and 9-minute-45-second
soak passed. A genuine recovery request was **not** exercised; the reviewer accepts
closure of the raw-byte storm independently and leaves recovery reliability as separate
scope. (Opening corrected per `KEYFRAME_STORM_FIX_REPORT_response_review.md` §1 — the
original sentence claimed the recovery exercise was executed, contradicting §4's own
table. Full correction list: §6 Amendments.) The two
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

## 6. Amendments (ordered by `KEYFRAME_STORM_FIX_REPORT_response_review.md`, all accepted)

The reviewed version of this file is preserved at commit `e9b8f78`; the corrections
below supersede the corresponding statements above.

1. **Opening contradiction** — fixed in place (§ header): the genuine-recovery exercise
   was NOT executed; closure rests on the storm evidence + static argument.
2. **Cause of client death: UDP receive EINTR, not a control-read error.** The retained
   client log shows `UDP recv error: Interrupted system call (os error 4)` → video
   receiver stop → frame-channel disconnect → decoder shutdown
   (`video_receiver.rs:111-118` breaks on `Interrupted`; `main.rs:633-637` shuts down on
   the disconnected channel). There is no `Control recv error` in the retained log —
   §4.5's finding 1 is corrected accordingly. Minimal future fix: retry
   `ErrorKind::Interrupted` in the receive loop (recorded, not scheduled).
3. **Reconnect diagnosis retracted.** §4.5's finding 2 ("host logged no disconnect,
   stayed `streaming`, ate the replacement `ModeRequest`") is NOT supported by the
   retained host log, which shows `Control connection closed by peer` (line 290) and a
   replacement connection followed by `Session: streaming → negotiating` (lines
   381-383), then no further handshake events. The supported statement is only:
   **the replacement connection did not complete negotiation** (no second ModeConfirm /
   StreamingReady / Streaming started). The reviewer's candidate cause — old-connection
   cancellation in `ControlChannel.swift:57-75` asynchronously clearing the shared
   `connection` after the new one is assigned — is an untested inference, recorded for
   any future reconnect work. Operational rule stands: restart both ends together.
4. **Evidence wording**: file is `evidence/keyframe_fix/client.log` (not
   `client-tail.log`); the "post-incident fresh session (12 KF / 6,600)" metric is
   withdrawn (not in the retained log — it came from the live log after the evidence
   copy); zero invalid-IDR lines do NOT demonstrate the guard accepts valid requests
   (no valid request reached the guard); "no silent drops" → invalid requests are
   observable at most once per second and never force a keyframe.
5. **Telemetry provenance**: "55 KF / 31,800 frames" is process-lifetime encoder
   telemetry (the host pre-encoded ~2,100 frames / 4 KF before the client connected).
   Primary runtime statement: the client's 9 m 45 s live interval (30,450 decoded
   frames) produced zero `IDR requested` lines. "Zero sendto errors" is narrowed to
   zero **logged** initial sendto errors (`VideoSender` logs only while
   `totalPacketsSent == 0`).
6. **Race-fix guarantee precision**: the flag is consumed before submission; a
   synchronous submission failure does not restore it. Guarantee: the next admitted,
   successful submission normally carries the force-keyframe property.
7. **GOP-bound wording**: "stalls at most ~10 s" → the host normally attempts a
   scheduled keyframe around the 10 s GOP bound; a lost scheduled keyframe can extend
   recovery beyond it.
