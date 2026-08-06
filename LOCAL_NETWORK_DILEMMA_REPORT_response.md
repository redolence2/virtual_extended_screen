# Response to the Local-Network Dilemma Review

**Date**: 2026-08-06 · **Responds to**: `LOCAL_NETWORK_DILEMMA_REPORT_review.md`
**Verdict on the verdict**: **ACCEPTED in full.** Option S adopted as Step 1; Options W/P
demoted exactly as the review orders; Option T deferred behind a failed Step 1–3. The
review also caught one outright factual error in the report (Finding 4) — owned below,
with the report's claim retracted. The reviewed report file is left byte-identical
(its SHA-256 is pinned in the review); all corrections live here.

## 1. Option S verified against Apple's documentation

Fetched TN3179 (2026-08-06, live) and confirmed the review's citation verbatim:

- `AllowedWiFiLocalNetworkAddresses` / `AllowedEthernetLocalNetworkAddresses`, domain
  `com.apple.network.local-network`, array of CIDR strings, **debuted macOS 15.5**,
  **restart required**: "the system treats every address on that network as if it were
  not a local network address. Every program can access that address, regardless of its
  Local Network privilege state."
- TN3179 also states macOS **automatically allows** local network access for "command-line
  tools run from Terminal or over SSH, including any child processes they spawn" (and any
  launchd daemon, and any root process). The observed Terminal and sshd denials therefore
  violate documented behavior — upgrading the diagnosis from "ordinary per-app denial"
  (report §6's overreach) to **beta regression of documented auto-allow behavior**, as the
  review argued. The reviewer's own inability to ssh to the box from their session is a
  third consistent data point (their non-Terminal app context is not in TN3179's
  auto-allow list either).

Step 1 (read-before-write, `/32` scope, restart, rollback discipline) is being handed to
the owner verbatim — privileged system-setting changes are owner actions by policy.

## 2. Finding 4 accepted — the "initial burst passes" claim is RETRACTED

The review is correct and the report was wrong, twice over:

- `VideoSender.swift` logs send errors **only while `totalPacketsSent == 0`**; a log with
  35,790 errno-65 lines therefore proves **zero packets ever succeeded** in that run —
  the exact opposite of the report's reading.
- "First keyframe sent to client (NNN KB)" prints **before** any `sendto` is attempted;
  it is a queued-frame message, not a delivery receipt.

Consequently the report's §4 claims that denied runs "pass an initial 300–900 packets"
and the derived "async policy resolution" inference are retracted. The denial is total
from the first packet. (The allowed-identity runs remain proven by receiver-side counters:
+300 K box-kernel datagrams, 0 drops.) The logging-semantics trap that produced this error
is precisely Finding 4's list; those logging corrections are adopted wholesale.

## 3. Findings 3, 5, 6 accepted as real defects — hardening pass queued

- **StreamingReady race** (client sends ready before binding video/cursor sockets; bind
  failure dies inside a thread while control stays healthy): fix = bind both receivers
  first, then send ready; bind failure exits nonzero with an explicit log line.
- **Launcher false positive** (waits only for `Host session started`, prints "up"
  unconditionally): fix = gate on receiver-side progress (frame/packet counters advancing
  across two samples via the existing ssh path), plus a `RESC_NETWORK_UNHEALTHY` marker
  path; the disproven "sshd is immune" comment is removed.
- **`RequestIDR` 0xFA byte-scan** (4,246 false IDR requests / 4,247 keyframes in ~83 K
  frames on the current allowed run — a keyframe storm masking real recovery): fix =
  decode the control envelope properly. The report's "pipeline has no open defect"
  sentence is withdrawn; this is an open defect, now tracked.
- Sender-side: independent success/failure counters, errno captured at failure site,
  first-failure + 1 Hz aggregate logging, cursor-send failures included.

Tracked as task #29 ("post-review hardening pass"), scheduled immediately after the
Step-2 acceptance test so the reboot doesn't interrupt cross-machine rebuilds.

## 4. Finding 7 accepted — launcher reproducibility

After Step 2 passes: single canonical bundle, source + install script checked into the
repo, `NSLocalNetworkUsageDescription` added to Info.plist, ssh-localhost hop removed
(obsolete under Option S; TN3179 auto-allow makes it doubly redundant once the beta is
fixed), release-build host launched directly. Scheduled within task #29.

## 5. Finding 8 noted for the (deferred) fallback

If Option T is ever needed: separate **Ubuntu-initiated** TCP video stream; control kept
separate (1 MB record caps + head-of-line risk make control-channel video a non-starter);
v3 harness material is scaffolding, not a drop-in — concurred, recorded in the fallback
spec so no future session reaches for the wrong direction.

## 6. Small corrections adopted

- OS wording: "macOS ProductVersion 27.0, build 26A5388g (beta)" — report said "macOS 26".
- TCC.db is not the Local Network store (Apple DTS); the report's TCC.db probe was only
  ever meaningful for Screen Recording identity, not the network question.

## 7. Convergence status

| step | state |
|---|---|
| 1 — owner applies `/32` Wi-Fi exemption + restart | handed to owner (this session) |
| 2 — controlled Dock acceptance test (launch/relaunch/reboot-repeat, counters + visual) | pending Step 1 |
| 3 — hardening pass (binds, logging, IDR decode, launcher gate, canonical bundle) | task #29, after Step 2 |
| 4 — fallback diagnostics + Ubuntu-initiated TCP | only if 1–3 fail |
