# Review of the Local-Network Dilemma Report

Reviewed: `LOCAL_NETWORK_DILEMMA_REPORT.md`

Report SHA-256: `bfa66371958ec30feb6717b76880a960754e201cbca1c4cfd9901c879c1078b1`

Review date: 2026-08-06

## Executive verdict

**Yes, the current problem is fixable, and I do not recommend waiting for the beta or rewriting the video transport yet.**

The report misses an Apple-supported system setting introduced in macOS 15.5. It lets an administrator mark a specific Wi-Fi CIDR as exempt from Local Network privacy classification. For this single-user, fixed-address setup, exempting only the Ubuntu box is the simplest and most directly targeted remedy. First inspect the privileged value:

```sh
sudo defaults read com.apple.network.local-network \
  AllowedWiFiLocalNetworkAddresses
```

If the key does not exist, set it with:

```sh
sudo defaults write com.apple.network.local-network \
  AllowedWiFiLocalNetworkAddresses -array "192.168.50.47/32"
```

If it already contains other CIDRs, preserve them and append this one instead:

```sh
sudo defaults write com.apple.network.local-network \
  AllowedWiFiLocalNetworkAddresses -array-add "192.168.50.47/32"
```

Then restart the Mac. Apple states that an address covered by this setting is treated as non-local for privacy purposes, so programs may access it regardless of their Local Network privilege. Apple specifically presents this mechanism for machines where the administrator has deep control over the Mac. See [TN3179: Understanding local network privacy](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy) and the [Apple DTS homelab discussion](https://developer.apple.com/forums/thread/787032).

The unprivileged user domain currently has no such setting. I could not verify the privileged value because `sudo` requires the owner's password, so the read-before-write step is important. A `/32` is preferable to the report's entire `/24`: it exempts only the already hard-coded Ubuntu address. A restart is required. Applying the privileged setting and restarting must be an explicit owner action; I have not done either during this review.

If the exemption restores the Dock launch, keep the existing UDP video path and ship the demo. Do not spend time on Option P or Option T.

There is, however, a second issue: the current launcher and handshake cannot reliably tell whether the receiver is ready or whether a frame was delivered. I recommend one small code-hardening pass after, or in parallel with, the system-setting test:

1. bind the Ubuntu video and cursor UDP sockets before sending `StreamingReady`;
2. make a receiver bind failure fatal and visible;
3. report real send successes and rate-limited failures on the Mac;
4. make the launcher wait for receiver-side packet/frame progress before saying the display is live;
5. correctly decode `RequestIDR` rather than scanning arbitrary protobuf bytes for `0xFA`.

These are bounded fixes. They do not require a new architecture.

## Decision

| Option | Review decision |
|---|---|
| **S: Apple-supported `/32` system exemption** | **Do first. Best fit for this fixed personal setup.** |
| **W: wait for a beta update** | Keep only as a temporary fallback until the next convenient restart. It is not the primary fix. |
| **P: UDP hole punch** | Skip. It still asks a denied process to send outbound UDP and does not address address-policy classification. |
| **T: TCP video** | Defer. Use only if the supported exemption fails after the readiness defects are removed. |
| **Native signed app** | Optional later cleanup, not the fastest remedy. No Apple-issued signing identity is currently installed. |

My revised recommendation is therefore:

> **Apply Option S, restart, run a controlled Dock test, and keep UDP if it passes. Fix the receiver-ready and health-reporting defects regardless. Only design TCP after those steps fail.**

## Findings

### 1. The report omits the smallest supported fix

Apple's current macOS guidance documents two administrator preferences:

- `AllowedWiFiLocalNetworkAddresses`
- `AllowedEthernetLocalNetworkAddresses`

Each accepts CIDR strings. Traffic to an address in the configured range is not treated as local-network traffic for privacy enforcement. This directly targets the report's suspected failure without deleting Network Extension state, changing Cisco configuration, spoofing another app identity, or touching the video protocol.

This deployment already assumes fixed addresses (`192.168.50.125` and `192.168.50.47`), so using `192.168.50.47/32` is consistent with the rest of the software. The privacy tradeoff is also narrow: every process on the Mac may contact that one IP without Local Network approval. For this owner-controlled setup, that is acceptable and simpler than per-build identity management.

If the key was absent before this change, roll it back with:

```sh
sudo defaults delete com.apple.network.local-network \
  AllowedWiFiLocalNetworkAddresses
```

Restart after rollback as well.

If the key already contained other CIDRs, restore that original array instead of deleting it.

### 2. The broad OS diagnosis is plausible, but not yet proved as narrowly as the report claims

The launch-context substitution is strong evidence for a macOS networking-policy or attribution problem. Apple's own operation table also matches the asymmetry:

- outgoing UDP unicast requires Local Network access;
- accepting an incoming TCP connection does not;
- receiving incoming UDP unicast does not.

That makes Local Network/NECP policy the leading category. However, the narrower claim "ordinary per-app Local Network denial" is not established:

- [TN3179](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy) says command-line tools launched from Terminal or over SSH, including child processes, are automatically allowed. The reported Terminal and sshd failures contradict normal documented behavior and point to a beta regression, attribution bug, or other policy/filter state.
- Local Network privacy is not managed by TCC. In an [Apple DTS explanation](https://developer.apple.com/forums/thread/766270), Apple confirms that `TCC.db` and `tccutil reset` are not the mechanism. The report's inability to read `TCC.db` is therefore irrelevant.
- The report does not show a controlled identity-only A/B test that keeps the same already-bound Ubuntu receiver alive while changing only the Mac process identity.

The `/32` exemption is both a remedy and a useful diagnostic. If it fixes the Dock path, that strongly implicates Local Network address classification. If it does not, the next investigation should include NECP/content-filter state and the receiver readiness defect below.

### 3. `StreamingReady` currently means the opposite of what its name promises

The Ubuntu client sends `StreamingReady` before binding either receive socket:

- `ubuntu-client/src/main.rs:314-350` sends ready and only afterward spawns the video receiver;
- `ubuntu-client/crates/net-transport/src/video_receiver.rs:50-77` performs the actual video bind;
- the cursor bind occurs even later at `ubuntu-client/src/main.rs:360-369`;
- the Mac begins sending immediately when ready arrives at `mac-host/Sources/RemoteDisplayHost/HostSession.swift:212-242`.

A video bind failure occurs inside the spawned thread with `expect`, while the main control connection can remain alive. The launcher can therefore see a healthy TCP session even when UDP port 9871 was never ready. This is a real startup race and a confounder for the report's diagnosis.

The minimal correction is to construct and bind both UDP receivers first, then send `StreamingReady`, then move the already-bound sockets into their worker threads. If either bind fails, the client must exit nonzero and leave an explicit error in `/tmp/resc4k-client.log`.

### 4. The failed log contradicts the claimed successful initial burst

`/tmp/resc4k-host.log` contains 35,790 `UDP sendto error: 65` records. The source logs a send error only while `totalPacketsSent == 0`, and increments that counter on every successful `sendto`:

- `mac-host/Sources/RemoteDisplayHost/VideoSender.swift:104-120`

Consequently, this sender instance recorded no successful socket write. If even one packet had succeeded, all later failures would have become silent under the current logging condition.

The message `First keyframe sent to client` is printed before `sender.sendFrame(...)` attempts any writes:

- `mac-host/Sources/RemoteDisplayHost/StreamingState.swift:51-62`

It is a queued-frame message, not a delivery receipt. The report's statements that the 412 KB keyframe burst passed and that every denied run passed an initial 300-900 packets are not supported by this log and, for this particular run, conflict with the sender counter behavior.

Required logging correction:

- rename the message to `First keyframe queued`;
- count successes and failures independently;
- capture `errno` immediately after a failed call;
- log the first failure and then one aggregate update per second;
- emit a stable marker such as `RESC_NETWORK_UNHEALTHY errno=65 failures=N last_success_ms=M`;
- include cursor-send failures, which `CursorTracker.swift:136-143` currently ignores.

### 5. The launcher health check is a guaranteed false positive for display readiness

The installed launcher waits only for `Host session started`, before it even launches the Ubuntu client:

- `/Applications/RESC 4K.app/Contents/Resources/resc4k.sh:33-55`

It then unconditionally prints `RESC 4K is up`. This proves only that the Mac process started. It does not prove client bind, a successful video send, packet reception, frame assembly, decode, or presentation.

Grep-for-errno alone is also insufficient because the current sender can become silent after its first successful packet. The launcher should require a fresh receiver-side milestone, ideally `first frame presented`, or at minimum a packet/assembled-frame counter that increases across two samples. With a fixed personal machine, polling `/tmp/resc4k-client.log` over the already-used SSH connection is simple and adequate.

The script comment saying sshd is immune to the beta problem is disproved by the report and must be removed.

### 6. The allowed run is useful, but it is not evidence that the pipeline has no open defect

The current allowed-session log contains 4,246 `IDR requested by client` entries and ends near 82,800 encoded frames / 4,247 keyframes. A normal ten-second GOP would produce far fewer keyframes.

Moreover, `mac-host/Sources/RemoteDisplayHost/HostSession.swift:184-210` does not decode a protobuf `RequestIDR`. It scans every control payload for byte `0xFA` and treats any occurrence as a request, rate-limited to 250 ms. Ordinary payload bytes can therefore trigger false keyframes.

This does not negate the visible, sharp stream or the reported packet/decode metrics. It does mean the sentence "the pipeline itself has no open defect" is too strong. Proper envelope decoding is a small, worthwhile correction because the current keyframe storm consumes bandwidth and can mask genuine recovery behavior.

### 7. The deployed launcher is not reproducible from the repository

Current deployment state:

- two installed copies exist: `/Applications/RESC 4K.app` and `~/Applications/RESC 4K.app`;
- both are ad-hoc signed with bundle ID `com.resc.launcher4k`;
- neither `Info.plist` contains `NSLocalNetworkUsageDescription`;
- the bundle executable is a shell script that opens Terminal, which then launches the host through `ssh localhost`;
- it runs `mac-host/.build/debug/remote-display-host`, not a release artifact;
- the installed 4K script is not checked into the repository;
- the checked-in `mac-host/RESC-Host.app` is an older, different 1080p launcher.

Apple notes that multiple copies of the same app can produce confusing Local Network identity/UI behavior. After the `/32` test succeeds, keep one canonical installed bundle, check its source and install script into the repository, remove the unnecessary self-SSH hop, and launch the known release host directly. This is maintainability work, not a reason to delay the demo.

### 8. Option T is possible, but the existing v3 material is not a production TCP fallback

The shipping paths are still protocol-v1 UDP for video and cursor. The v3 wire records, parsers, fixtures, harness sender, and harness receiver are scaffolding and test infrastructure; production `main.swift` and `main.rs` do not use them for video.

The existing v3 plan/harness has the Mac initiate a connection to an Ubuntu listener. That is the wrong direction for this incident because Mac-originated local-network operations are the suspected blocked class. Putting video on the current control channel is also not a drop-in change: both sides cap control records near 1 MB, while observed 4K keyframes can exceed 1 MB, and large video records would head-of-line block control and recovery messages.

If TCP is eventually necessary, use a separate connection initiated by Ubuntu and accepted by a Mac listener. Keep control separate, carry cursor on control or the new stream, and retain inbound UDP input. That design is feasible, but it is hours of production transport work and should follow a failed `/32` test—not precede it.

## Current code and verification status

- Repository HEAD: `862f5cdf7ce7df743ddf3ee10c7f59428b8388bb` (`Retina enforcement loop`).
- HEAD is two commits ahead of `origin/main`.
- Before this review file was added, the only worktree item was the untracked `LOCAL_NETWORK_DILEMMA_REPORT.md`.
- The current machine reports `ProductVersion 27.0`, build `26A5388g`, arm64. The report should preserve the build number but correct its `macOS 26` wording.
- The Mac host builds successfully with Swift Package Manager.
- The repository's `resc-fixture-check` executable completed with all checks passing.
- Targeted Rust suites for `protocol`, `diagnostics`, and `jitter-buffer` passed: 101 tests passed, zero failed, one ignored.
- A full Rust workspace build was not validated on this Mac: the Ubuntu-oriented network code has a Darwin `sockaddr_in.sin_family` type mismatch, and local FFmpeg development libraries are absent. This does not show that the intended Ubuntu build is broken.
- Live Ubuntu verification could not be repeated from this review session because SSH to `192.168.50.47` returned `No route to host`. The Ubuntu runtime status is therefore based on the retained logs and source inspection.

## Minimal convergence plan

### Step 1 — owner-approved system fix

Inspect the privileged value, preserve any existing CIDRs, apply the `/32` Wi-Fi exemption, and restart the Mac.

After restart, confirm:

```sh
sudo defaults read com.apple.network.local-network \
  AllowedWiFiLocalNetworkAddresses
```

Expected entry: `192.168.50.47/32`.

### Step 2 — controlled acceptance test

From the Dock icon, not the agent shell:

1. launch once and require increasing Ubuntu packet/frame counters plus visible output for at least ten seconds;
2. verify cursor and input;
3. stop and relaunch twice;
4. retain both fresh host and client logs;
5. repeat once after another ordinary restart because reboot-sensitive state triggered this incident.

If this passes, declare the network dilemma closed for the personal demo and keep UDP.

### Step 3 — bounded code hardening

Implement receiver-bind-before-ready, fatal bind reporting, rate-limited network health, receiver-side launcher gating, proper `RequestIDR` decoding, and a checked-in canonical launcher. These changes improve diagnosis and one-click reliability without changing transport.

### Step 4 — fallback only if Steps 1-3 fail

Run one controlled identity-only A/B with the same already-bound Ubuntu receiver. If failure still follows launch identity despite the `/32` setting, collect Network framework/NECP path diagnostics and build a separate Ubuntu-initiated TCP video stream. Skip UDP hole punching.

## Final answer to the owner's question

**Yes. I can fix the code-side readiness, logging, IDR parsing, and launcher defects. The most likely immediate unblock is not a code rewrite: it is the Apple-supported `192.168.50.47/32` Wi-Fi exemption plus a Mac restart.**

I would not accept Option W as the main recommendation, and I would not authorize Option T yet. Try the supported system fix first, make the small reliability corrections, and get the demo out on the existing sharp UDP pipeline.
