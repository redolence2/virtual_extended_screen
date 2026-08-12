# 2026-08-12 — Progress Summary and Forward Optimization Plan (for review)

**Executes**: `LATENCY_CODE_AUDIT_review.md` §8 (attribution → controlled baseline →
presentation probes → gated F1) · **Commits**: `1a84805` … `2059e9a` (+ the ship commit)
· **Both machines synced, trees clean.**
**Headline**: one user-visible win shipped (cursor coalescing: decode→screen
**23.8 → 2.9 ms**, owner verdict *"latency is hardly perceivable"*), one latent
**black-screen defect found and fixed**, and the decode→present budget closed to ±0.2 ms.
**Not established**: the post-fix end-to-end number (measurement blocked — §7).

---

## 1. What shipped

| change | commit | evidence |
|---|---|---|
| **Cursor coalescing, default ON** (`RESC_NO_CURSOR_COALESCE=1` reverts) | ship commit | 6 paired runs: `present()` 14.0 → 0.3 ms, mailbox wait 7.2 → 0.0 ms, decode→present 23.8 → 2.9 ms |
| **Oversize-keyframe black-screen fix** (8 MB floor + 6144 chunks, matched pair) | 2 commits | reproduced twice pre-fix (`decode_emitted=0`), zero oversize drops post-fix |
| **Loud oversize-drop logging** (always for keyframes) | same | the failure mode was previously silent |
| **Attribution instrumentation** (mailbox wait/sheds, video-vs-cursor presents, draw vs `present()`, SDL identity) | `1a84805` | produced §4 |
| **Scripted workload tool** (`tools/scripted_workload.sh`) | same | replaces owner-dragging; reproducible |
| **Interleaved A/B driver** + evidence records | `2059e9a` | `evidence/zero_copy/` |

### 1.1 The shipped win, mechanically

Cursor movement used to trigger its **own** screen present. Video (~40/s) plus cursor
(~14/s) issued presents **faster than the vsync-limited swap chain could retire them**,
so *every* present paid a queue wait (14.0 ms measured), and a freshly decoded frame sat
in the one-slot mailbox behind cursor-only presents (7.2 ms measured). 23–29 % of all
presents carried no new video. Now the cursor is drawn with the next video frame (50 ms
no-video fallback so it cannot freeze). Present rate drops below the retire rate and both
costs vanish. Trade accepted by the owner: cursor redraws at video rate (~40–50/s).

## 2. Defect found and fixed: permanent black screen on large keyframes

The first scripted run produced a **2,697,156-byte** HEVC keyframe. The host advertised
`max_frame_bytes = min(20 × avg, 2 MB)` — binding term **2.08 MB** at 50 Mbps/60 fps — so
the client assembler dropped it as oversize; since every later keyframe was also oversize,
the `WaitingForIDR`-gated decoder never recovered: `decode_emitted=0`, `presents=0`,
ledger cap-overflow cascade, aborted trace, **black screen for the entire run**. Reproduced
(second run: 42 s black, recovery only when content briefly compressed under the limit).

- **Why it was latent in daily use**: ordinary desktop content compresses under the cap;
  measured owner-content keyframes were 242 KB–1.3 MB. Dense 4K text pushed 2.4–2.7 MB.
- **Fix, second attempt** — the first raised only the 2 MB ceiling and missed that
  `20 × avg` was the *binding* term. A per-frame limit must bound worst-case **intra**
  size, not average-bitrate arithmetic: **8 MB floor**, `max_total_chunks_per_frame`
  2048 → **6144** (matched pair — raising one alone silently reintroduces the bug).
- **Severity**: this could have black-screened the owner's daily screen with no recovery
  path, and only a `frame_drops` counter ticking by one as the symptom.

## 3. Second-order finding: the workload itself was adversarial

The first scripted workload scrolled random base64 — pathologically high-entropy and the
direct cause of the 2.7 MB keyframe. Replaced with ordinary source text (representative,
still reproducible). Recorded because it shaped the defect discovery: an unrealistic
workload found a real bug, then had to be corrected before latency numbers meant anything.

## 4. The decode→present budget, closed

Baseline `c1-traced` (joiner PASS, 3,168 joined):

| stage | p50 |
|---|---:|
| mailbox pickup wait | 7.2 ms |
| texture upload | 2.8 ms |
| draw / blit | **0.0 ms** |
| `canvas.present()` | **14.0 ms** |
| **sum** | **24.0 ms** vs measured segment **23.8 ms** ✔ |

The arithmetic closing to 0.2 ms is what made the shipped fix findable, and it retired two
of my earlier hypotheses as *unnecessary* (§5).

## 5. Corrections to my own prior claims (kept explicit)

1. **"vsync owns decode→present"** — WRONG. Owner challenged it on arithmetic
   (23 ms > 16.67 ms period). The `RESC_NO_VSYNC=1` run bought ~2 ms, not ~18.
2. **"Mutter/compositor owns 8–11 ms"** — UNNECESSARY. The queue-saturation mechanism
   explains `present()` fully; **F2 (compositor A/B) is therefore moot and cancelled**,
   including its attended-testing requirement.
3. **"Copies are the latency lever"** (the whole zero-copy project) — refuted twice:
   W0a probe (8.4 ms), then the upload timer (2.5–3.1 ms *including* the SDL upload).
   **CUDA/GL interop is dead for latency purposes** on measured evidence.
4. **"No queues / no restructuring win"** (audit §1) — the reviewer rejected this as
   overreach and was right: the win shipped today *was* a queueing fix.
5. **AUD-append as an EOP proxy** — negative result; does not refute the explicit
   end-of-picture hypothesis (§8, F4).

## 6. Operational hazards discovered (all cost real time today)

1. **Virtual-display create/destroy is expensive.** ~15 create/destroy cycles across the
   day left `WindowServer` pinned near 100 % (3 h 07 m CPU in 9 h 53 m uptime) and made
   the **owner's whole Mac laggy even after the remote screen was closed**; only a reboot
   cleared it. **This was self-inflicted by my measurement pattern.** Rule adopted: batch
   measurements into as few display lifecycles as possible; keep run counts low; never
   leave a long series unattended.
2. **Idle-Mac background work saturates the host.** With the owner away, `mediaanalysisd`,
   `WallpaperAerialsExtension`, `WindowServer` and `loginwindow` drove `capture→encode`
   from 26.6 ms to ~90 ms, making every E2E comparison useless. Rule: `caffeinate` for the
   run, record load + top-CPU alongside metrics, and treat `capture→encode` p50 > ~30 ms
   as host-bound ⇒ E2E-invalid.
3. **TCC permission roulette (4th occurrence).** The reboot re-rolled the agent identity's
   Screen Recording grant *mid-session* (worked 14:29, gone by 14:38), which is what
   blocks §7. The Dock-icon path (sshd identity) is unaffected.

## 7. What is NOT established

- **The post-fix E2E number.** Two attempts produced empty traces (§6.3). The honest
  arithmetic estimate is ~60–65 ms (last clean pre-fix baseline 84.9 ms minus the ~21 ms
  segment win) — **an estimate, not a measurement**, and this project's record shows such
  estimates get corrected by data. Unblocking needs one owner action: re-enable **Claude**
  under Privacy & Security → Screen & System Audio Recording, then one 60 s run.
- **The E2E delta attributable to cursor coalescing.** The interleaved A/B (4 runs, all
  PASS) confirmed the client-side effect 4× but showed only −5.0/−2.9 ms E2E *because the
  host was saturated at ~90 ms encode*; that comparison must be redone on an idle,
  owner-present Mac.
- **Whether sustained 4K60 encode degrades the Mac in ordinary hours-long use** (§6.1/6.2
  suggest a mechanism; unmeasured).

## 8. Forward optimization plan (ranked, each gated)

Current measured budget (pre-coalescing baseline segments, owner-present conditions):
`capture→encode ~22–26 ms` · `network ~5–6 ms` · `receive→decode ~25–29 ms`
(of which **~17–19 ms is cuvid's one-AU parser wait**) · `decode→present **2.9 ms**`.

| # | lever | est. gain | cost / risk | status |
|---|---|---:|---|---|
| **F4** | **Explicit end-of-picture feed to NVDEC** (`CUVID_PKT_ENDOFPICTURE`): the decoder currently holds frame N until frame N+1 arrives — a full frame period, every frame | **~17–19 ms** | needs a **patched, pinned ffmpeg** on the box (build + maintenance weight for a daily tool); reviewer approved only as a *narrow throwaway proof* first | **owner-gated** |
| **E3** | Apple `EnableLowLatencyRateControl` (already built, `RESC_LOWLAT=1`) | ~5–10 ms encode | unexplained capture-drop anomaly (841/4196 in one run) must be explained first | parked |
| **F1** | AVFrame **move** (not clone — ffmpeg's `clone()` deep-copies) to drop post-transfer plane copies | 2–4 ms | ownership across threads; gate: measured copy cost ≥1.5–2 ms, which the current upload timer has not isolated | gated |
| F3/F5 | sender allocations, `recvmmsg`, channel tuning | ≤0.5 ms | — | parked (not closed) |
| ~~F2~~ | ~~compositor unredirection~~ | — | — | **cancelled** (§5.2) |
| ~~E2~~ | ~~CUDA/GL zero-copy~~ | — | — | **dead** (§5.3) |

**If F4 + E3 both landed: ~40–45 ms E2E** — the first time "cable-ish" would be defensible.
**Recommended order**: (0) restore the permission and *measure* today's shipped state —
no further work should be planned against an estimate; (1) explain E3's capture-drop
anomaly, since it is cheap and already built; (2) F4 as a throwaway ffmpeg patch *proof*
before any productionization decision; (3) F1 only if its gate measures.

**Explicit stop option**: the owner's stated bar — "hardly perceivable unless you go
looking for it" — is already met. F4's maintenance weight (a pinned custom ffmpeg on the
receiver) is the kind of cost that outlives the benefit for a personal tool; stopping here
with everything documented is a legitimate outcome, not a failure.

## 9. Amendments (per `SESSION_2026-08-12_SUMMARY_AND_NEXT_review.md`, all accepted)

The reviewed version is preserved at `18ef10c`; corrections below supersede the text above.

1. **Endpoint naming**: every "decode→screen" above means **decode→return from
   `canvas.present()`**. Scanout/photons were never measured.
2. **§4 "closed to ±0.2 ms"** overstated: the stage values are cumulative **means**, the
   23.8 ms a segment **p50** over presented survivors. Correct claim: *mean stage
   accounting is consistent with the measured p50; the controlled A/B establishes
   cursor-triggered presentation contention as the dominant removable application-side
   cost.*
3. **§1.1 mechanism**: "54 presents/s exceeded a 60 Hz retire rate" is arithmetically
   impossible (54 < 60). Supported statement: *extra asynchronous cursor presents caused
   or exposed swap/presentation contention — bursts, vblank phase collisions, or an
   effective retire rate below nominal; coalescing removed the measured blocking.* No
   further mechanism experiment needed.
4. **"Six paired runs"** → **four comparisons across eight runs** (two exploratory
   baseline/candidate comparisons + two interleaved A/B pairs; A/B/A/B, not
   counterbalanced).
5. **§2 bound history** conflated two states: the ORIGINAL formula bound at its
   **2,000,000-byte ceiling** (20×avg = 2,083,333 exceeded it); the FIRST fix bound at
   **2.083 MB** via the 20×avg term. Code comment corrected likewise.
6. **§2 severity wording**: the fix addresses the *observed* cause for this fixed
   profile; "cannot black-screen this way again" was too strong — oversize rejection
   still has no recovery signal (one bounded IDR request + loud fail is the recorded
   hardening design, deliberately NOT an unconditional per-drop request, which could
   loop into a keyframe storm).
7. **§6.2 process attribution** ("mediaanalysisd 47.6%…"): observed operationally, not
   retained as machine-readable evidence. Proof of E2E-invalidity is the measured
   capture→encode drift (26.6 → ~90 ms).
8. **§8 F4 numbers**: 17–19 ms is the observed one-frame wait, **not** proven removable
   by explicit EOP; the 40–45 ms projection is withdrawn. F4 is **declined by default**
   (reviewer + owner bar both met); its proof plan survives only for a future explicit
   numeric target, gated on gap 1→0 plus ≥10 ms repeated E2E improvement.
9. **Hardening landed with these amendments** (commit after `18ef10c`): the byte/chunk
   pair is now a single-source invariant (`ProtocolConstants.maxTotalChunksPerFrame` +
   startup precondition `bytes ≤ chunks × payload`), with a boundary regression test
   accepting the observed 2,697,156-byte keyframe and rejecting advertised-max+1
   (`jitter-buffer` 8/8 pass). The stale probe comment at the coalescing site now
   documents the known `local_moved` imprecision to watch in the closure log.
10. **Closure plan adopted** (review §5): one 60-second run of the shipped default under
    `docs/MEASUREMENT_PROTOCOL.md`, then **stop by default** — no F4/E3/F1 work without
    a concrete unmet owner target.

## 10. Questions for the reviewer

1. Is the shipped cursor-coalescing default acceptable as-is, or should the 50 ms
   no-video fallback be tightened/loosened (cursor now redraws at video rate)?
2. The oversize fix uses an **8 MB floor** and 6144 chunks. Right shape (bound worst-case
   intra), or should the host instead *cap keyframe size at encode time* (e.g. QP/rate
   control on IDR) so the wire limit stays small?
3. Should the client **request an IDR** when it drops an oversize keyframe, given it
   cannot recover on its own? (Today it just logs loudly.)
4. F4: approve a throwaway pinned-ffmpeg EOP proof, or decline the maintenance risk for a
   personal tool and stop at the shipped state?
5. Any objection to cancelling F2 outright on the §4 arithmetic?
6. Does the §6 hazard set warrant a written measurement protocol in the repo (rather than
   living in evidence records)?
