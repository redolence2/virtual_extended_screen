# Review of `LATENCY_CODE_AUDIT.md`

**Review date:** 2026-08-08  
**Reviewed against:** the current macOS host and Ubuntu receiver source, the retained
`evidence/zero_copy/` traces and metrics, and the currently available receiver logs.

## Verdict

**GO WITH MAJOR AMENDMENTS.**

The audit reaches one useful high-level conclusion: **do not start a broad
multithreading, async, or transport rewrite for the demo.** The current pipeline is
already substantially event-driven, bounded, and drop-aware. No measured evidence
justifies a large architectural change.

However, the document is not yet a closed latency decomposition, and F1/F2 should not
be implemented exactly as written. Several claims are either too absolute or assign
unmeasured residual time to a particular cause. In particular:

- the frame path does contain timers, bounded queues, timed waits, and a serial sender
  queue;
- the decoder's one-AU delay is observed, but the claimed 17.5 ms allocation is not;
- the 2–4 ms plane-copy cost has not been timed independently;
- the 13.4 ms renderer timer mixes texture blit, software cursor work, and
  `canvas.present()`; it does not prove 8–11 ms of Mutter delay;
- cursor-only redraws call the same full presentation path, so skipping the cursor path
  from this audit was unsafe;
- the proposed F1 "clone-owned" frame would perform another deep copy in the current
  FFmpeg Rust binding.

For this personal demo, the right response is small and empirical: make one diagnostic
build that separates the remaining stages, run two short presentation A/B tests, and
only implement an optimization that produces a repeatable material win.

## Decision summary

| Audit item | Review decision |
|---|---|
| No broad async/thread rewrite | **Accept.** This is the important architectural conclusion. |
| "No restructuring win is available" | **Reject as unproven.** There are unmeasured queues and contention points; park them rather than close them. |
| F1 AVFrame pass-through | **Conditional go, with a different ownership design and a measurement gate.** |
| F2 compositor work | **Go as a diagnostic, not as a confirmed 8–11 ms fix.** Inspect the live window first. |
| F3/F5 | **Park for the demo; do not close permanently.** |
| F4 explicit EOP feed | **Defer production work.** Keep a small pinned-FFmpeg proof as a later owner-approved option. |
| 70/65/48–55 ms projections | **Do not use as commitments.** They are optimistic scenarios, not measured outcomes. |

## 1. Architecture conclusion: directionally right, wording too strong

The source supports the narrower conclusion that all important frame handoffs wake on
new work and that the design deliberately sheds stale work. The 1 ms decoder wait and
2 ms renderer wait normally wake immediately; they are not fixed 1 ms and 2 ms taxes
on every frame. This is why a general async rewrite is unlikely to help.

The following statements in the audit should nevertheless be removed:

- "no sleeping poll, no timer tick, and no standing queue anywhere";
- "7 dedicated threads";
- "there is no restructuring win available."

Current counterexamples include:

- ScreenCaptureKit uses `queueDepth = 3` and a serial capture queue
  (`DisplayCapturer.swift`).
- `FramePacer` runs a repeating timer and is enabled by the host
  (`FramePacer.swift`, `main.swift`).
- the completed-frame handoff is `sync_channel(2)` and is explicitly described in the
  source as a possible latency reservoir (`ubuntu-client/src/main.rs`).
- the jitter assembler has four frame slots and a 100 ms expiry deadline.
- decode uses `recv_timeout(1 ms)`, render uses a 2 ms condition-variable timeout, and
  UDP receive has a 100 ms timeout.
- VideoToolbox callbacks enter a serial `StreamingState.queue.sync` block for the whole
  UDP burst after the two-frame encoder gate has already been released.

These are not evidence that a rewrite will win. They mean only that the correct verdict
is:

> No measured, low-complexity async or threading redesign is justified for the demo.
> Keep the current architecture and measure any suspected queue before changing it.

Also call the seven entries **logical execution contexts**, not seven dedicated OS
threads. Several are GCD queues, framework callbacks, or runtime-owned tasks.

### Small correctness issue found during the audit

`GenerationalFrameSlot.store()` signals the semaphore on every replacement even though
the slot holds only one frame. A burst of replacements can therefore accumulate signal
tokens; the consumer may then wake repeatedly and receive `nil`. `tryTake()` can also
remove the frame without consuming a signal.

This should eventually be changed to signal only on an empty-to-occupied transition,
with a small burst test. It is a simple correctness/CPU-hygiene fix, **not a claimed
latency optimization**, and it should not delay the demo.

## 2. The arithmetic is not closed

The retained headline metrics are reproducible, but their causal decomposition is not.
Segment medians cannot in general be added because they may describe different frames,
and the metrics script uses a smaller presented-frame survivor set for
decode-to-present and E2E.

Examples:

- a1 E2E p50 is **81.204 ms**, while its segment p50s sum to **78.156 ms**;
- a2 E2E p50 is **79.565 ms**, while its segment p50s sum to **82.993 ms**;
- the gap-one trigger-frame receipt medians are about **19.202 ms** in a1 and
  **19.188 ms** in a2, not a measured 17.5 ms parser component;
- after the trigger frame is received, the remaining a1/a2 medians are about
  **5.885/10.514 ms**, but that residual still combines queueing, parser/decode work,
  hardware transfer, and CPU copies;
- no retained per-frame timestamp separately measures encoder queue wait, plane copy,
  mailbox pickup wait, or physical scanout.

The current `ts_present_us` is recorded after `canvas.present()` returns. It is valuable
for application latency, but it does not measure compositor completion, scanout, or
photons.

There is also an unexplained result that should be recorded before making another
projection: recomputing the clean `v1-stages-joined.jsonl` trace gives **68.869 ms E2E
p50 and 83.662 ms p90**. That run has no retained metrics file and its content rate and
launch configuration differ from the earlier baselines, so it is not proof of a 12 ms
improvement. It does show that "today is 80–85 ms" is not a universal controlled
baseline.

The audit should replace "closed arithmetic" with **working attribution hypotheses**.
Future A/B runs must use the same scripted content and record capture rate, payload
rate, delivery, launch environment, binary/source identity, and stage distributions.

## 3. A missing high-value suspect: cursor-only presentation contention

The audit says cursor code can be skipped because it does not carry video frames. That
is only half true. Remote or local cursor motion can trigger
`present_with_cursor()` even when there is no new video frame. That call performs the
full render/present path and can occupy the render thread while a newly decoded video
frame waits in the one-slot mailbox.

The available receiver logs reinforce this concern:

- one v1 log reports 3,000 uploads but 3,519 presents;
- a longer log reports 49,200 uploads but 60,288 presents.

The difference is consistent with substantial cursor-only presentations. These logs
are currently temporary runtime artifacts rather than sealed evidence, and the timers
are cumulative averages, but the counts are sufficient to require a controlled test.

Add separate counters and timings for:

- video-triggered presents;
- cursor-only presents;
- texture upload;
- texture/cursor drawing;
- the `canvas.present()` call itself;
- decoded-frame mailbox publication-to-pickup wait.

Then run a short **video-only-present** diagnostic in which cursor updates are drawn on
the next video presentation instead of initiating a presentation. If it saves at least
about 3 ms E2E without making the cursor unacceptable, use simple coalescing for the
demo. Otherwise restore current behavior and close this branch. Do not build a complex
cursor subsystem first.

## 4. F1: feasible, but not with the proposed clone

The current CUDA path creates a fresh software destination frame for
`av_hwframe_transfer_data`, then copies its two NV12 planes into new `Vec`s. Removing
those post-transfer CPU copies is technically feasible.

The safe simple design is:

1. move the fresh owning `ffmpeg_next::frame::Video` into `DecodedFrame`;
2. move that owner through the channel and newest-wins mailbox;
3. borrow its plane slices only for the synchronous SDL texture upload;
4. drop the owner promptly after upload or mailbox replacement.

Do **not** use `Video::clone()`: in `ffmpeg-next 7.1.0` it calls `av_frame_copy`, which
deep-copies the pixels and defeats F1. The underlying `Frame` already implements
`Send + Sync`, so a new unsafe Send wrapper should not be needed.

This removes only the post-transfer CPU plane copies. It does not remove the NVDEC
GPU-to-CPU transfer or SDL texture upload. The software-decoder fallback also needs
separate handling because its reused decoder frame has different ownership.

### F1 gate

Instrument hardware transfer and the two `to_vec()` copies separately first. Implement
F1 only if the plane-copy portion is at least **1.5–2 ms p50** under the controlled 4K
workload. Accept it only if repeated runs show a corresponding receive-to-decode or E2E
gain, no p90/delivery regression, correct frame identity and recovery, and stable RSS
for a 10-minute smoke run.

Run F1 separately from F2. Combining them would make attribution impossible.

## 5. F2: run a controlled probe, not "enforcement"

F2 is still the best cheap experiment, but the current evidence does not establish
Mutter as an 8–11 ms cause.

The renderer currently uses `fullscreen_desktop()`. On the deployed SDL 2.0.20 X11
path, SDL already defaults
[`SDL_VIDEO_X11_NET_WM_BYPASS_COMPOSITOR`](https://wiki.libsdl.org/SDL2/SDL_HINT_VIDEO_X11_NET_WM_BYPASS_COMPOSITOR)
to enabled and normally writes `_NET_WM_BYPASS_COMPOSITOR=1` when it creates the
window. Setting the environment variable to `1` is therefore likely a no-op. The
property is also only a request; seeing `1` in `xprop` does not prove Mutter actually
unredirected the window.

The reported 13.4 ms value spans blit, software-cursor drawing, and
`canvas.present()`. It is not a compositor-only timer. The v0 no-vsync result also
changes decode-to-present p50 by about 2.2 ms relative to R2, but those runs lack a
retained paired configuration proving vsync was the only changed variable.

### Safest minimal F2 order

1. On an unchanged run, record the live SDL video driver and renderer information,
   session type, Mutter version, monitor topology, output mode/rotation, window ID,
   `_NET_WM_BYPASS_COMPOSITOR`, `_NET_WM_STATE`, window type, and geometry.
2. If this is not an X11 window, stop using the X11 hint as the hypothesis.
3. If X11 already shows bypass value `1`, skip the environment-variable test. Run a
   reversible same-workload `1` versus `2` property A/B while keeping application
   vsync enabled. Value `2` requests that compositing not be bypassed; this tests
   whether the Mutter decision path changes the measurements.
4. If that is inconclusive, compare the current desktop fullscreen with one
   native-resolution SDL real-fullscreen restart. Do not change any other variable.
5. Keep F2 only if two paired repetitions produce at least about **5 ms improvement**
   in both application presentation and E2E p50, with stable p90, delivery, image
   warmth, rotation, input, and no tearing.

Use an attended test with SSH/TTY recovery because real fullscreen can reconfigure or
blank the portrait output. Do not begin by globally disabling Mutter, and do not
combine compositor testing with `RESC_NO_VSYNC`.

## 6. F3 and F5: park, do not permanently close

These are reasonable non-priorities for the demo, but the code and evidence cannot
support permanent closure or a precise `≤0.5 ms` bound:

- sender callbacks serialize on `StreamingState.queue.sync` for the full UDP burst;
- the sender allocates and sends each chunk separately and deliberately sleeps after
  large chunk groups;
- keyframes can block the receiver when `sync_channel(2)` is full;
- data is still copied through protocol parsing, assembler storage/compaction, and an
  FFmpeg packet;
- `sendmmsg` is not a macOS sender API, so that particular host proposal is not
  directly applicable;
- no retained A/B benchmark measures F3 or F5.

Network and sender time are currently small enough to deprioritize. Revisit only if a
new trace shows sender-queue wait, receiver backpressure/kernel drops, or CPU saturation.
The right status is **parked**, not **proved zero**.

## 7. F4: defer production work, keep the proof option precise

The repeated ordinal gap is good evidence that the FFmpeg/CUVID path waits for a later
access unit before emitting the prior frame. The failed AUD workaround does **not**
emulate `CUVID_PKT_ENDOFPICTURE`, so it does not refute the explicit-EOP hypothesis.

A direct Video Codec SDK decoder is project-scale and should remain out of the demo.
A throwaway patch to the pinned FFmpeg/CUVID feed is a narrower proof and may be worth
an owner decision later if F1 and F2 fail their gates or a sub-65 ms target remains
important.

For that proof, require:

- ordinal trigger gap changes from one frame to zero;
- at least 10 ms repeated improvement in both receive-to-decode and E2E p50;
- no frame-identity, recovery, IDR, delivery, or p90 regression;
- a repeat run and a 10-minute smoke before considering productionization.

Do not claim the whole roughly 19.2 ms observed gap is removable until this experiment
measures it.

## 8. Recommended convergent execution plan

This is sufficient for the demo and intentionally avoids another broad redesign cycle:

1. **Preserve the current baseline.** Commit or otherwise seal the dirty source,
   launch configuration, existing traces, and the temporary v1 stage log.
2. **Make one attribution build.** Add the stage, cursor/video-present, queue-wait,
   trace-overhead, and SDL/window-state observations described above.
3. **Run one paired controlled baseline** using fixed scripted content. Also compare
   tracing on versus off so synchronous client trace writing is not mistaken for
   pipeline latency.
4. **Run two short presentation probes:** cursor-only-present suppression and the
   revised F2 window/compositor A/B. Keep only a repeatable gated win.
5. **Run F1 only if its copy timer clears the 1.5–2 ms gate.** Move the owner; do not
   clone it.
6. **Keep E3 off** until its capture-drop anomaly is explained.
7. **Defer F4 to a fresh owner decision.** Leave F3/F5 parked.

If neither presentation probe nor the measured F1 copy clears its gate, stop optimizing
this branch and ship the demo at the verified baseline. That outcome is preferable to
another speculative architecture revision.

## Answers to the audit's five questions

1. **Threading/async:** I concur that no broad rewrite is justified. I do not concur
   that no restructuring win can exist; some waits remain unmeasured.
2. **F1 ownership:** Not acceptable as written. Use a moved owning transfer frame, not
   `clone()`, and do not add an unnecessary unsafe Send wrapper. Proceed only after the
   copy-cost gate is measured.
3. **F2 order:** inspect/log the live window first; on X11, use a controlled bypass
   `1`/`2` A/B; then try native-resolution real fullscreen only if needed. Skip the
   `=1` environment test when the property is already `1`. Do not globally change
   Mutter first.
4. **F3/F5:** Object to permanent closure. Park them with explicit evidence-based
   reopen conditions.
5. **F4:** Agree that production implementation remains unscheduled. Preserve the
   option of a narrowly scoped pinned-FFmpeg proof after the small experiments.

## Final recommendation

**Approve the audit only after the above wording and execution changes.** Its most
valuable conclusion is to avoid a broad rewrite. Its two immediate optimization claims
are hypotheses, not established savings. For the fastest path to a working personal
demo, perform one attribution pass, test cursor presentation and F2 independently,
conditionally test F1, and then stop unless a result crosses its gate.
