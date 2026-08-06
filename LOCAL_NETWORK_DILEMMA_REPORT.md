# Local-Network Dilemma Report — Dock-Icon Launcher Blocked by OS Permission State

**Date**: 2026-08-06 · **Author**: session agent (owner-directed) · **Track**: demo/product line (outside formal A0.0)
**Status**: streaming WORKS via workaround; one-click launcher independence BLOCKED · **Requested output**: reviewer verdict on the decision in §8

---

## 1. One-paragraph summary

RESC native-4K (Mac host → Ubuntu box, WiFi, 2160×3840 Retina virtual display, H.264/UDP)
is healthy and sharp: the pipeline itself has no open defect. The dilemma is purely an
**operating-system permission problem**: on the Mac's beta OS (build 26A5388g), a
per-application "Local Network" privacy filter silently denies outbound LAN UDP for every
launch identity the owner can click — Terminal-launched and sshd-launched hosts both die
with `sendto errno 65 (No route to host)` — while the same binary streams flawlessly when
launched from the Claude-session shell, the one identity the filter still allows. The OS
provides no prompt and no UI entry to grant the denied identities (the Local Network pane
lists only XQuartz and has no "+" button). Result: the one-click Dock icon the owner wants
("click it, screen lights up") cannot work today; the screen currently requires asking the
session agent to launch the pair. This report documents the evidence, the eliminated
hypotheses, the two engineering escape routes, and a recommendation, for adversarial review.

## 2. System context (for a cold reviewer)

- **Host**: MacBook, macOS 26 beta build **26A5388g** (unchanged across working and broken
  eras — verified in host logs both sides of the incident; no OS update involved).
- **Client**: Ubuntu box `192.168.50.47` (RTX GPU, X11, cuvid decode), Mac is `.125`, both
  on 5 GHz WiFi (–51 dBm, 1.73 Gb/s link, excellent), same /24.
- **Wire**: UDP video 9871 + cursor 9873 (Mac→box), UDP input 9872 (box→Mac), TCP control
  9870 (box connects INBOUND to Mac).
- **Healthy-state metrics** (when launched from the allowed identity, measured today):
  299,700 packets received / 0 kernel drops / 46.7 fps decode / 5.3 ms avg decode;
  E2E p50 157 ms (bottleneck is cuvid pipeline depth, tracked separately as task #28).
- Recent hardening relevant to trust in the pipeline: Retina 2× mode is now re-asserted
  every 2 s (commit `862f5cd`) after an observed WindowServer stale-mode revert mid-session.

## 3. Incident timeline (condensed)

1. **For weeks → 2026-08-05 evening**: Terminal-launched hosts stream at full rate daily
   (demo, M1, native-4K sessions). Terminal's identity is therefore *allowed* during this era.
2. **2026-08-05 night**: owner reboots the box, then clicks the new Dock icon → black
   screen. Box-side boot re-arms were found and fixed (ufw ON again → disabled; WiFi
   power_save ON again → persistently disabled; monitor mode reset → launcher self-heals).
   Still black.
3. **Same night, pre-Mac-reboot measurements**: host submits ~2,000 UDP pkt/s with
   `sendto()` returning success; box's kernel UDP counter rises ~2–5 pkt/s; Mac `en0`
   egress counter shows only ~8–25 pkt/s during streams. Single `nc -u` probes DO pass.
   TCP flawless. → Packets die on the Mac, **silently**, below the socket API.
4. **Cisco exoneration**: owner toggled Cisco Secure Client's Socket Filter extension OFF —
   flood still ~7/300 delivered. (Agent's Cisco theory was wrong; owner's skepticism right.)
5. **2026-08-06 morning, after Mac reboot**: failure mode changes from silent eating to
   explicit **`sendto errno 65 (No route to host)`** on every video packet, beginning right
   after the first keyframe burst. Meanwhile ARP entry for .47 is present and correct, route
   table clean, TCP/ssh to the box work from the same script.
6. **Proof by substitution**: identical binary + args launched from the Claude-session
   shell: zero errors, full-rate stream, box counters +300 K. Same Mac, same WiFi, same
   destination, different launching app → **per-application network permission denial**,
   with errno 65 as macOS's (documented-in-the-wild) denial symptom for LAN sends.
7. **ssh-localhost route built and tested** (owner-approved "option 2"): launcher rewired so
   the icon's script spawns the host via `ssh localhost` (identity: `sshd-keygen-wrapper`).
   - Run 1: capture DENIED (`Screen Recording permission needed`), no video frames, cursor
     idle → **zero UDP actually transmitted**; the absence of network errors was initially
     misread as "network allowed under sshd" — a **false negative** (lesson: absence of
     sendto errors proves nothing until packets actually flow).
   - Owner manually added `/usr/libexec/sshd-keygen-wrapper` in Screen & System Audio
     Recording via the pane's **+** button (that pane accepts manual adds; the grant stuck).
   - Run 2: capture ✓ (`Capture started: displayID=12`), first keyframe sent (412 KB burst
     passes), then **errno 65 spam** — sshd identity is network-denied too.
8. **Current state**: pair relaunched from the session shell; screen live and sharp.

## 4. Identity scoreboard (the core finding)

| launch identity | screen capture | outbound LAN UDP | evidence |
|---|---|---|---|
| Claude-session shell | ✓ | **✓** | 300 K pkts/0 err today; multi-day history |
| Terminal.app | ✓ (historical grants) | **✗** | errno 65 after first keyframe (1,270 KB) |
| sshd (`sshd-keygen-wrapper`) | ✓ (owner's manual + add) | **✗** | errno 65 after first keyframe (412 KB) |

Additional observations:
- In all denied cases the **initial burst passes** (~300–900 packets) before denials begin —
  consistent with an asynchronous policy resolution racing the first sends (inference, not
  proven).
- Apple platform binaries (`ssh` itself) sail through in every context: the box-side
  client/xrandr steps of the same script always work.
- Inbound-established flows are never affected: control TCP (box→Mac) and input UDP
  (box→Mac 9872) work in every identity, including during video denial.
- The **Local Network pane lists only `X11.bin`** (XQuartz) and, by macOS design, **has no
  "+" button** — denied identities cannot be added manually. No prompt ever fires (on a
  healthy macOS, first LAN access pops "X would like to find devices on your local
  network"). Registration of new entries appears broken on this build.
- The same beta corrupted unrelated permission state twice more in 24 h (session shell lost
  ~/Downloads directory access mid-uptime; the Mac reboot restored that while flipping the
  network state from silent-drop to explicit-deny). State **re-rolls at reboot**.

## 5. Eliminated hypotheses (with the killing evidence)

| hypothesis | killed by |
|---|---|
| box firewall (ufw) | disabled; no change (it WAS a real secondary issue re-armed at box boot) |
| box WiFi power save | disabled persistently; no change |
| monitor mode reset | self-healed by launcher; screen still black with correct mode |
| Cisco Socket Filter ext. | owner toggle test: flood unchanged with it off |
| MTU / fragmentation | 1,400 B DF pings pass |
| ARP / routing | entry correct (`b0:a4:60:eb:97:45`), route clean, TCP fine |
| WiFi quality | –51 dBm, 1.73 Gb/s, both ends confirmed |
| OS update between eras | build string identical (26A5388g) in working and broken logs |
| RESC regression | identical binary streams perfectly from the allowed identity |

## 6. What remains unproven (honesty section)

- The actual denial record has not been observed directly: TCC.db is unreadable from the
  session shell (needs Full Disk Access), and the Local Network backing store
  (`networkextension` preferences) was deliberately not poked — it also holds the owner's
  Cisco VPN configurations (deleting it is the internet-folklore "fix" and is judged too
  risky).
- WHY the Claude-session identity is allowed is unknown (old grant? never-registered
  default-allow? exemption?). Symmetrically, whether Terminal/sshd are explicitly denied or
  denied-by-unregistered-default is unknown.
- The "async policy resolution" explanation for the initial-burst-passes pattern is an
  inference from timing only.

## 7. Current workaround and its cost

The session agent launches/stops the pair on request (four words from the owner). The
rewired launcher (both app copies) is correct and harmless: if a future OS update or
reboot re-roll un-denies either identity, the icon starts working with zero further
changes. The owner's permanent gains from tonight regardless of path: sshd screen-capture
grant, self-ssh keys, Retina enforcement loop, and this documented diagnosis.

Known cosmetic gap: when the icon path fails, the script's health check passes (the host
*runs*; only its sends are denied) so the user sees a live-looking log with errno 65 lines
and a black screen. Proposed small amendment (not yet implemented): after streaming
starts, grep the log for `sendto error` for ~5 s and print a clear
"OS network permission still broken — use the session agent to launch" banner.

## 8. Decision space for review

- **Option W (recommended): wait out the beta.** Keep agent-as-launcher. Rationale: the
  blocker is OS state corruption on a beta build, already observed to re-roll at reboot;
  every future macOS update is a chance of silent full recovery; the icon infrastructure
  is finished and self-healing; both engineering routes below spend hours routing around a
  bug Apple will eventually fix. Cost: launcher depends on an open Claude session.
- **Option P: UDP hole-punch experiment (~1 h, uncertain).** Bind the host's video/cursor
  sender to a fixed local port; at StreamingReady the client sends one "punch" packet to
  it; hypothesis: the host's outbound then rides an inbound-initiated flow and bypasses
  the filter (inbound flows are provably unaffected). Cheap to falsify; genuine
  uncertainty whether macOS's filter classifies UDP flow direction this way.
- **Option T: video over box-initiated TCP (~hours, near-certain).** Move video (and
  cursor) onto the existing box→Mac control TCP or a second box-initiated TCP stream.
  Inbound-established TCP is proven to flow in every identity all week. Cost: protocol
  work on both ends + TCP head-of-line-blocking risk under WiFi loss (mitigable: clean
  LAN, 50 Mb/s vs 1.7 Gb/s link, jitter buffer already tolerates reordering; keyframe
  recovery logic exists). This permanently immunizes RESC against this entire class of
  OS filtering, at the price of abandoning UDP's loss-isolation on a link where loss is
  currently negligible.
- **Option X (rejected, listed for completeness): piggyback on XQuartz's identity** (the
  one registered Local Network entry). Launching the host as a descendant of XQuartz to
  inherit its attribution is fragile, unverified, and adds a GUI dependency; noted only
  because a reviewer may ask.

## 9. Questions for the reviewer

1. Does the evidence in §4–5 justify the per-app-filter diagnosis at the stated
   confidence, or is there an untested alternative that fits "initial burst passes, then
   errno 65, only for some identities, TCP unaffected"?
2. Is Option W's "beta will heal" bet acceptable for a personal-use tool with a working
   agent-launched path, or does launcher independence justify Option T's hours now?
3. If an engineering route is taken: P-then-T, or straight to T? (P is cheap but its
   failure teaches little; T is certain but heavier.)
4. Any objection to the §7 launcher amendment (errno-65 detection banner)?

## 10. Recommendation

**Option W now; adopt the §7 banner amendment; revisit after the next macOS beta update
or any reboot (re-test is one icon click). If the owner wants icon independence before
the beta heals, go straight to Option T (skip P unless an hour of curiosity is welcome).**
