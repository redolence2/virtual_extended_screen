# Review of `SESSION_2026-08-12_SUMMARY_AND_NEXT.md`

**Review date:** 2026-08-13  
**Scope:** current local source and history through `18ef10c`, committed
`evidence/zero_copy/` traces and metrics, the prior latency review, and targeted local
tests.

## Verdict

**ACCEPT THE SHIPPED BUILD FOR THE PERSONAL DEMO; AMEND THE REPORT BEFORE USING IT AS
THE FINAL ENGINEERING RECORD.**

The important result is real: cursor coalescing removes roughly **20–21 ms** from the
measured decode-to-application-present-return segment, repeatedly, and the owner accepts
the cursor feel. Keep it enabled by default. This is sufficient to stop latency work for
now.

The report is also appropriately honest that the final E2E number has not been measured.
Restore Screen Recording permission, take **one** clean 60-second measurement of the
shipped default, and then close this optimization branch. Do not begin F4, E3, F1, or a
compositor investigation unless the owner later states a concrete unmet target.

The document should nevertheless correct several claims:

- `decode→screen` is really **decode→return from `canvas.present()`**; scanout and photons
  were not measured;
- the stage values are cumulative **means**, not p50s, so the budget is consistent to
  about 0.2 ms but is not a closed p50 decomposition;
- the average presentation-rate explanation says approximately 54 presents/s exceeded
  a 60 Hz retire rate, which is arithmetically impossible; bursts/phase collisions or a
  lower effective retire rate remain plausible, but were not separately measured;
- the black-screen timeline conflates the original 2 MB ceiling with the first attempted
  fix's 2.083 MB `20 × average` bound;
- “six paired runs” overstates the design: the evidence contains two exploratory
  baseline/candidate comparisons and two interleaved A/B pairs;
- the post-fix oversize regression was not reproduced with a retained 2.4–2.7 MB raw
  keyframe; normal post-fix evidence stayed below the old 2 MB bound;
- F4's 17–19 ms and the 40–45 ms future outcome remain hypotheses, not measured removable
  latency or a defensible projection.

These corrections do **not** invalidate the cursor-coalescing ship decision.

## 1. What is established

### 1.1 Cursor coalescing is implemented correctly at the main decision point

Commit `a91da3b` makes coalescing the default. Current code enables it unless the literal
`RESC_NO_CURSOR_COALESCE=1` rollback is present
(`ubuntu-client/src/main.rs:897–907`). Cursor changes are normally drawn with a new video
frame, with a 50 ms no-video fallback (`main.rs:1059–1065`). This matches the stated
product behavior and retains a simple rollback.

The committed metrics reproduce the material client-side change:

| comparison | decode→present-return p50 |
|---|---:|
| c1 baseline → p1 coalesced | 23.847 → 2.583 ms |
| c3 baseline → p2 coalesced | 23.919 → 2.769 ms |
| interleaved pair 1 | 22.991 → 2.964 ms |
| interleaved pair 2 | 22.560 → 2.913 ms |

All four interleaved trace bundles have clean join summaries with no identity failure.
This is a large and repeatable application-side result even though host load invalidates
the corresponding E2E subtraction.

### 1.2 The attribution instrumentation exists in the right places

The current receiver measures:

- mailbox publication-to-pickup time and newest-wins sheds
  (`ubuntu-client/src/main.rs:529–574`, `925–934`);
- upload time, draw time, `canvas.present()` call time, and video/cursor presentation
  counts (`ubuntu-client/crates/renderer/src/lib.rs:229–300`, `327–368`);
- live SDL video-driver and renderer identity (`renderer/src/lib.rs:171–180`).

This instrumentation was sufficient to find the presentation-contention problem. The
per-frame trace still ends after `canvas.present()` returns and therefore says nothing
about physical display completion.

### 1.3 Host instability really invalidated the E2E A/B

The committed metrics directly show capture-to-encode p50 degrading from **26.559 ms**
in c1 to **91.408 ms** in p2 and remaining around 90–96 ms throughout the interleaved
runs. It is correct not to quote those E2E differences as the cursor win.

The named processes and load percentages were observed operationally but were not
retained as machine-readable evidence. Phrase the cause as “observed host/background
load consistent with saturation,” while treating the measured capture-to-encode drift
as the proof that the runs were E2E-invalid.

### 1.4 The final shipped E2E remains unmeasured

This is correctly disclosed. The committed `f1-shipped` bundle contains clock samples
but zero frames and fails the join gate. It proves one unusable post-ship attempt. A
second failed attempt and TCC diagnosis may have happened during the session, but their
raw artifacts are not committed.

Do not publish 60–65 ms as the current result. It is only rough subtraction across
different samples and environments.

## 2. Evidence wording that must be corrected

### 2.1 The budget is consistent, not “closed to ±0.2 ms”

The 7.2 ms mailbox, 2.8 ms upload, 0.0 ms draw, and 14.0 ms present values are cumulative
averages. The code stores sums and counts and divides them at log time; the mailbox log
even labels its value `avg`. In contrast, 23.847 ms is a segment p50 over presented
survivors.

Therefore this comparison:

> 7.2 + 2.8 + 0.0 + 14.0 = 24.0 ms versus segment p50 23.8 ms

is useful evidence that the attribution is approximately complete, but not a p50
identity and not proof to ±0.2 ms. Use this wording instead:

> Mean stage accounting is consistent with the measured decode-to-present-return p50;
> the controlled A/B establishes that cursor-triggered presentation contention was the
> dominant removable application-side cost.

The raw c1/p1 stdout containing the new separated stage values is also not committed;
the values survive in prose. Preserve the raw client log in the closure run.

### 2.2 Correct the queue mechanism statement

The report says roughly 40 video plus 14 cursor presents per second exceeded a 60 Hz
retire rate. **54 is below 60.** The data still show that eliminating cursor-only
presents collapses both `present()` blocking and mailbox wait, so contention is well
supported. The precise mechanism may involve burst timing, phase collisions with
vblank, or an effective retire rate below the nominal display rate.

Use the narrower causal statement:

> Extra asynchronous cursor presents caused or exposed swap/presentation contention;
> coalescing them reduced presentation demand and removed the measured blocking.

No additional mechanism experiment is needed for the demo.

### 2.3 Correct “six paired runs”

The retained design is:

- two exploratory comparisons: c1→p1 and c3→p2;
- two interleaved pairs: base1→cand1 and base2→cand2.

Say **“four comparisons across eight runs”**, not six paired runs. The interleaved
driver is A/B/A/B rather than a fully counterbalanced A/B/B/A design; it reduces drift
but does not cancel it. That limitation does not weaken the repeatable downstream
segment result because both candidate runs show the same roughly 20 ms improvement.

## 3. Oversize-keyframe fix: right direction, incomplete closure claim

### 3.1 Correct the bound history

At 50 Mbps and 60 fps:

- `20 × average frame bytes` is about **2,083,333 bytes**;
- the original `min(20 × average, 2,000,000)` therefore bound frames at **2,000,000
  bytes**, not 2.08 MB;
- the first attempted change to `min(20 × average, 8,000,000)` then bound frames at
  about **2.083 MB**;
- the final `max(..., 8,000,000)` correctly creates the intended **8 MB floor**.

The final 8 MB/6144 setting accepts the observed 2.4–2.7 MB condition in principle and
is reasonable for this fixed personal 4K60/50 Mbps profile.

### 3.2 The byte/chunk pair is not a general invariant yet

At 1,358 payload bytes, 6,144 chunks can carry **8,343,552 bytes**, safely above the
current 8,000,000-byte profile limit. However, `maxFrameBytes()` can advertise up to
16 MB while `max_total_chunks_per_frame` remains hard-coded at 6,144
(`ProtocolConstants.swift:64–80`, `HostSession.swift:331–341`). The sender also does not
enforce the advertised byte limit before transmitting (`VideoSender.swift:65–96`).

For this one-user application, do not add a more elaborate adaptive policy. Simplify
the invariant:

- hard-code the supported byte budget for the known profile; and
- derive the advertised chunk count from that byte budget and payload size, or at least
  assert at startup that `max_frame_bytes <= max_chunks × payload_bytes`.

Add a boundary test covering the observed 2,697,156-byte keyframe and the advertised
maximum. This is more useful than another destructive high-entropy live run and gives a
future agent a direct failure if the constants drift apart.

### 3.3 Oversize rejection still has no recovery signal

The assembler drops an oversize keyframe and logs it
(`ubuntu-client/crates/jitter-buffer/src/lib.rs:152–175`). Only decoder-originated errors
currently feed the existing `RequestIDR` channel (`ubuntu-client/src/main.rs:777–785`,
`818–827`). If the startup keyframe is dropped, the decoder remains `WaitingForIDR` and
discards non-keyframes until a usable later IDR (`video-decode/src/lib.rs:287–301`).

The raised cap fixes the **observed** cause for the canonical profile, so this is not a
demo release blocker. It does mean the broader statement “the client cannot black-screen
this way again” is too strong.

If this path is hardened, do **not** request an IDR for every oversize event. Repeated
IDRs under unchanged content can create a keyframe storm and reproduce the same failure.
Use the existing control path for at most one request per recovery episode; if the next
keyframe is also oversize, fail/restart loudly with the measured size and both limits.
For a personal tool, an explicit failure is preferable to an indefinite black window.

## 4. One small cursor follow-up, not a reason to revert

Keep the 50 ms fallback unchanged. The owner has already accepted its feel, and there is
no evidence supporting a different number.

There is one static logic concern: `local_moved` compares the current local mouse
coordinates with `cursor_renderer`, even when that renderer currently holds the remote
Mac cursor (`ubuntu-client/src/main.rs:1043–1064`). If the stationary local pointer and
remote cursor differ, the condition can remain true and cause fallback presentations
every 50 ms during a no-video interval. This has not been reproduced and is not a demo
blocker.

In the closure run, retain video/cursor presentation counts. If unexpected cursor-only
presents persist on a static screen, compute one selected **desired cursor state**
(remote, grabbed, or local fallback) and compare that state with the last rendered state.
Also update the stale comment at `main.rs:1055`, which still names the obsolete
`RESC_COALESCE_CURSOR=1` probe variable.

## 5. Forward plan: stop by default

The document's explicit stop option should become the default decision. The owner says
latency is hardly perceivable, and a pinned custom FFmpeg would add maintenance to a
single-user tool that already meets its usability bar.

### Closure sequence

1. Correct the report wording above.
2. Restore Screen Recording permission.
3. Take one 60-second run of the **shipped default**, not another A/B:
   - one virtual-display lifecycle;
   - fixed source-text workload;
   - Mac held awake;
   - local source/binary identity and launch environment retained;
   - raw host/client logs plus a CPU/load snapshot retained;
   - clean trace footers and join;
   - capture-to-encode p50 at or below roughly 30 ms;
   - zero oversize, decoder, renderer, and identity failures.
4. Report application E2E p50/p90 and decode-to-present-return p50/p90. Do not label
   them physical screen or photon latency.
5. Stop optimization and use the software.

Write a short repository measurement protocol containing these rules. The current
runner neither invokes `caffeinate` nor captures host load, and the A/B driver creates
four separate host/virtual-display lifecycles. The protocol is valuable maintainability
for the next agent and avoids repeating the operational damage documented here.

### Only if the owner later sets a new target

Reopen in this order:

1. explain/instrument the E3 capture-drop anomaly and run one controlled E3 A/B;
2. separately time the F1 post-transfer plane copies and implement the AVFrame move only
   if the 1.5–2 ms gate is met;
3. consider F4 last as a throwaway pinned-FFmpeg proof, not a production commitment.

F4 must change ordinal gap one to zero and repeatedly improve receive-to-decode and E2E
by at least 10 ms before productionization is discussed. The observed one-frame wait is
not proof that explicit EOP will remove all 17–19 ms. Remove the current 40–45 ms
projection; it is not supported by composable measurements.

## 6. Answers to the six questions

1. **Cursor default and 50 ms fallback:** Keep both as-is. Correct the desired-cursor
   comparison only if the closure log shows recurring false fallback presents.
2. **8 MB/6144 versus encoder-side keyframe cap:** Keep the generous receiver budget for
   the fixed profile. Do not add IDR QP/rate shaping now; it adds latency/quality risk.
   Derive or assert the byte/chunk invariant and add a boundary regression test.
3. **Request IDR after an oversize keyframe:** Not unconditionally. If hardened, make one
   bounded request per recovery episode and fail loudly on a repeated oversize IDR; never
   create an IDR loop.
4. **F4:** Decline it now. Preserve the proof plan only for a future explicit numeric
   target.
5. **Cancel F2:** Yes, cancel it for demo/application-present latency. Post-coalescing
   `canvas.present()` is already sub-millisecond on average, so F2 cannot clear its old
   application gate. Do not claim that compositor/scanout latency was measured or proved
   irrelevant.
6. **Measurement protocol:** Yes. Keep it short and operational: permission preflight,
   minimal display lifecycles, fixed workload, provenance, awake/idle host, CPU capture,
   invalidation threshold, clean footer/join, and precise latency endpoint naming.

## Verification performed for this review

- Local worktree was clean at `18ef10c`; local `origin/main` matched. The Ubuntu box's
  deployed SHA was not independently queried in this review.
- All committed c/p/AB metrics were recomputed from their joined traces and matched the
  stored files.
- `tools/join_trace.py --selftest` passed.
- `cargo test -p jitter-buffer` passed 7/7, including existing oversize-rejection tests.
- `cargo test -p protocol` passed 74 tests across unit and integration suites.
- A full Ubuntu-client workspace build could not be validated on this Mac because it
  lacks the target FFmpeg development libraries and includes Linux-specific socket
  types. Swift tests were also blocked by the locally selected Command Line Tools/SDK
  mismatch. These are review-environment limitations, not evidence of a new product
  regression.

## Final recommendation

**Ship/keep the current cursor-coalesced build, make the report corrections, take one
clean closure measurement, and stop.** Treat the byte/chunk invariant and bounded
oversize recovery as small robustness work, not another latency project. Do not accept
new latency optimization complexity until the owner supplies a concrete target that the
current product fails to meet.
