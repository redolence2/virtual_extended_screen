# Attribution block + cursor-coalescing probe — run record (2026-08-12)

Executes `LATENCY_CODE_AUDIT_review.md` §8 steps 1–4. All runs: HEVC 50 Mbps,
2160×3840@60, `r4_live_gate.sh` 60 s, **scripted workload** (`tools/scripted_workload.sh`
— a Terminal window on the virtual display scrolling this repo's `main.rs`), owner absent
(no manual drag). Client/host at the commits noted per section; both machines synced.

## 0. Two defects found before any measurement was possible

**(a) Permanent black screen on large keyframes (FIXED).** The first scripted run
produced a 2,697,156-byte HEVC keyframe. The host advertised
`max_frame_bytes = min(20 × avg, 2MB)` — binding term **2.08 MB** at 50 Mbps/60fps — so
the client assembler dropped it as oversize, and since every later keyframe was also
oversize the `WaitingForIDR`-gated decoder never recovered: `decode_emitted=0`,
`presents=0`, ledger cap-overflow cascade, aborted trace, black screen for the entire
run. A second run reproduced it (42 s black; recovery only when content briefly
compressed under the limit). Fix (two commits — the first raised only the ceiling and
missed the binding 20×avg term): **8 MB floor** for `maxFrameBytes`,
`max_total_chunks_per_frame` 2048 → **6144** (matched pair), and the client now **logs
every oversize drop loudly** — always for keyframes, the unrecoverable case. This was
latent in daily use: ordinary desktop content simply compresses under the old cap.

**(b) Measurement hygiene.** The first workload scrolled random base64 — pathologically
high-entropy, and the direct cause of the 2.7 MB keyframe. Replaced with ordinary source
text (representative, still reproducible).

## 1. Attribution build (commit `1a84805`)

Added, per review §8.2: mailbox publication→pickup wait + shed counter, video-vs-cursor
present split, `draw` and `canvas.present()` timed separately, SDL driver/renderer
startup log.

## 2. Closed attribution of decode→present (baseline `c1-traced`, joiner PASS 3,168 joined)

| stage | p50 |
|---|---:|
| mailbox pickup wait | 7.2 ms |
| texture upload | 2.8 ms |
| draw / blit | **0.0 ms** |
| `canvas.present()` | **14.0 ms** |
| **sum** | **24.0 ms** vs measured segment **23.8 ms** ✔ |

Presents: 2,699 video / 825 cursor-only (**23% carried no new video frame**) — the
review's §3 suspect, confirmed. The arithmetic closing to 0.2 ms retires the earlier
"vsync owns this segment" and "compositor owns 8–11 ms" hypotheses as *unnecessary*:
`present()` blocking is explained below without invoking Mutter.

## 3. Cursor-coalescing probe (`RESC_COALESCE_CURSOR=1`, default OFF)

Cursor-only presents suppressed; cursor drawn on the next video present; 50 ms no-video
fallback so the cursor cannot freeze on a static stream.

| metric | baseline (c1/c3) | coalesced (p1/p2) |
|---|---:|---:|
| `present()` | 14.0 ms | **0.4 / 0.6 ms** |
| mailbox pickup wait | 7.2 ms | **0.0 / 0.0 ms** |
| cursor-only presents | 825 | 35 / 520 |
| **decode→present p50** | **23.8 / 23.9 ms** | **2.58 / 2.77 ms** |

**Mechanism (now measured, not hypothesized):** presents were being issued *faster than
the vsync-limited swap chain could retire them* (video + cursor ≈ 54/s against a 60 Hz
pipe), so every present paid a queue wait, and freshly decoded frames sat in the
one-slot mailbox behind a cursor-only present. Removing the cursor presents drops the
present rate below the retire rate; the wait disappears. This also explains why
`RESC_NO_VSYNC=1` only bought ~2 ms earlier: disabling app vsync does not stop the
queue from saturating.

**Gate (review §3: "≥3 ms E2E without making the cursor unacceptable"): the segment
gain is ~21 ms, reproduced twice.** Cursor acceptability is **UNJUDGED** — requires the
owner's eyes (cursor now updates at video rate, ~40–50/s, instead of ~54/s).

## 4. E2E numbers are CONFOUNDED this session — do not quote them

| run | capture→encode p50 | E2E p50 |
|---|---:|---:|
| c1 baseline | 26.6 | 84.9 |
| p1 coalesce | 33.0 | 72.9 |
| c2 baseline | 39.2 | 109.0 |
| c3 baseline | 38.1 | 101.2 |
| p2 coalesce | 91.4 | 173.6 |

`capture→encode` drifted **26.6 → 91.4 ms across the session** while the client-side
segment stayed rock-steady (23.8/23.9 baseline, 2.6/2.8 coalesced). The Mac had been
encoding 4K60 HEVC continuously for ~45 min plus repeated cargo/swift builds; load
average peaked ~15 and the 15-min average was still 14.2 at the end. **Host-side
thermal/sustained-load degradation is the working explanation.** Consequence: the
client-side win is established, the E2E delta is not. A trustworthy E2E A/B needs a
cooled, idle Mac and an interleaved (baseline, coalesce, baseline, coalesce) design.

Secondary observation worth its own check: sustained 4K60 HEVC encode may degrade this
Mac over tens of minutes in ORDINARY use too — the owner's daily sessions run for hours.
Not measured; recorded.

## 4b. Interleaved A/B (`ab1-*`, all four PASS) — client win confirmed 4×, E2E still host-bound

| run | joined | cap→enc p50 | **decode→present p50** | E2E p50 |
|---|---:|---:|---:|---:|
| ab1-base1 | 1294 | 90.1 | **22.99** | 203.9 |
| ab1-cand1 | 1118 | 95.8 | **2.96** | 198.9 |
| ab1-base2 | 1134 | 90.1 | **22.56** | 195.0 |
| ab1-cand2 | 1090 | 90.6 | **2.91** | 192.1 |

Interleaving was the right design: the client-side effect is identical in both pairs
(~23 → ~2.9 ms, now **reproduced across 4 paired runs plus p1/p2** = 6 total), while
E2E moved only −5.0 / −2.9 ms because `capture→encode` sat at ~90 ms — the host was
saturated, so frames queued upstream and the downstream win could not propagate.

**Environment root cause (found, not guessed).** With the owner away the Mac went idle
and its own background work took over: `WindowServer` 79.4%, `mediaanalysisd` (photo
library analysis) 47.6%, `WallpaperAerialsExtension` (animated aerial desktop wallpaper)
33.8%, `loginwindow` 36.4%; load average peaked ~34. That starves ScreenCaptureKit and
the encoder — capture→encode 26.6 ms (owner present, morning) → ~90 ms (idle-Mac
afternoon). `caffeinate -disu` suppresses idle-triggered work but the aerial wallpaper
animates whenever the desktop is visible; it is an owner setting and was NOT changed.

**Measurement rules this establishes** (for every future run): hold the Mac awake with
`caffeinate` for the run's duration; record top-CPU processes and load average alongside
the metrics; treat any run whose `capture→encode` p50 exceeds ~30 ms as host-bound and
therefore useless for E2E comparison; close scripted-workload windows between runs.

**Product note (unmeasured, recorded):** the same idle-time macOS work will degrade the
owner's screen if the Mac is left idle while the remote display stays up.

## 5. State / next

- Committed: attribution build, black-screen fix (+ oversize logging), coalescing probe,
  workload tool. Both machines synced. Probe defaults OFF — daily use unchanged.
- **Owner-gated next**: (a) judge cursor feel with `RESC_COALESCE_CURSOR=1`;
  (b) attended F2 window/compositor A/B (can blank the portrait output);
  (c) cooled interleaved E2E A/B to quantify the end-to-end gain.
- F1 (AVFrame move) unchanged in status: gate on a measured ≥1.5–2 ms copy cost; the
  upload timer (2.5–3.1 ms, includes the SDL upload itself) has not isolated the copy.

## 6. CLOSURE (2026-08-13): the campaign's final measured number

`closure1` — one 60 s run of the shipped default per the session review §5 sequence:
single display lifecycle, scripted source-text workload, `caffeinate` held, environment
recorded (load 2.03→3.61, no saturating processes), host via the roulette-proof sshd
path (`HOST_VIA_SSHD=1`; the agent identity's Screen Recording grant was re-revoked by
the beta and survived toggle+app-restart — the sshd manual grant has survived every
re-roll). Joiner **PASS**, 3,367 joined / 3,255 presented, 0 identity failures, gap 1,
capture→encode p50 **24.2 ms** (< 30 ms validity threshold).

| application-latency endpoint | p50 | p90 |
|---|---:|---:|
| capture → encode out | 24.2 | 32.8 |
| encode out → receive | 4.0 | 11.8 |
| receive → decode done | 25.6 | 41.1 |
| decode done → present-return | **4.2** | 8.6 |
| **E2E capture → present-return** | **63.3** | **79.0** |

Campaign totals (all application-endpoint, same rig): **162.0 → 63.3 ms p50**
(**2.56×**), p90 177.9 → 79.0, with cable-level sharpness, working input, and a
one-click launcher. Optimization branch **CLOSED** per review §5: no further latency
work without a new concrete owner target (reopen order: E3 anomaly → F1 gate → F4
throwaway proof).
