# Shipped-launcher smoke — provenance record (review §5.2)

**Date**: 2026-08-08 · **Config under test**: Dock icon → `resc4k.sh` → host
`2160 3840 60 --client 192.168.50.47 --hevc --bitrate 50` via ssh-localhost, box client
plain (render path).

## Source / binary provenance

- Mac repo HEAD: `c7270bb` (from push log; direct git verification blocked this morning —
  the beta's overnight permission re-roll broke the agent shell's git/dir access again,
  same class as 2026-08-06; file reads/writes still work).
- Host binary `mac-host/.build/debug/remote-display-host`
  SHA-256[0:16] `eae37306d5450164` (built at the Candidate-A-era source; host code
  unchanged since `862f5cd`+`e9b8f78` hardening — flags come from the launcher).
- Launcher `/Applications/RESC 4K.app/Contents/Resources/resc4k.sh`
  SHA-256[0:16] `97d65695c1065232` (HEVC 50 Mbps line; both installed copies synced
  byte-identical at edit time).
- Box checkout: `66fe9e8` ("Ship Candidate A") — the last client-code commit; the Mac's
  `c7270bb` on top is docs/evidence only, so the box binary is current. Client binary
  SHA-256[0:16] `391c8794e1865b73`.
- Box driver: NVIDIA 570.169; monitor `Screen 0 current 2160 x 3840` (portrait-left).

## Same-morning permission-roulette note (affects agent tooling, NOT the smoke)

The agent shell's identity lost Screen Recording overnight (host under it logs
"Screen Recording permission needed"; a 15 s HEVC dump attempt yielded 0 bytes) and git
directory access. The icon path is unaffected: it launches the host under the sshd
identity (`sshd-keygen-wrapper`), whose Screen Recording grant was added manually via
the pane's + button and has held. Consequence: the HEVC parameter-set/AUD sample
(review §5.2 + E1 pre-check) is DEFERRED until the agent identity is re-granted; it
requires a dump-flagged client and must not interrupt the smoke (client restart against
a live host trips the known reconnect defect).

## Smoke results (2026-08-08, ~14 minutes of normal use — exceeds the 10-min gate)

- Host: `Codec: HEVC, bitrate: 50Mbps`; capture 58.2 fps sustained (47,933 frames,
  1 dropped); encode 38,400 frames / 81 KF (ordinary ~8–10 s cadence, no storms);
  **0 sendto errors; 0 IDR requests**.
- Client: `HEVC CUVID decoder initialized (NVDEC hardware)`; **0 WARN/ERROR lines**;
  38,760 decoded @ 45.9 fps avg, 6.0 ms avg decode; 170,900 packets / **0 kernel
  drops** / 38,793 assembled / 12 frame_drops (0.03%).
- **Owner verdict (verbatim)**: "This version of the demo is usable. Its latency is so
  low that there is no need to endure it. If there is no further optimization, I could
  even use it on a daily basis. the sharpness is still cable-level." — immediately
  clarified: "but the latency is still Perceivable, and it's far from cable level."
- **Gate: PASS — demo released at HEVC 50 Mbps, ~80 ms E2E p50** (usable daily,
  sharpness cable-level). Latency is accepted but NOT settled: owner wants further
  reduction. The reviewed optimization ladder (POST_HEVC_LATENCY_PLAN + review:
  ASUS A/B → E1 AUD probe → renderer cleanup → Apple low-latency probe → owner
  decision on CUDA/GL) is ACTIVE, not shelved.
