# RESC latency measurement protocol

Short and operational. Every rule below was paid for with a wasted or misleading run;
sources: `SESSION_2026-08-12_SUMMARY_AND_NEXT{_review}.md`,
`evidence/zero_copy/attribution-day-record.md`.

## Before any run

1. **Permission preflight.** Launch the host for ~8 s and require `Capture started` in
   its log. The beta re-rolls the agent identity's Screen Recording grant without
   warning (4 occurrences); a permission-dead host still negotiates and streams
   nothing, producing an empty trace that *looks* like a harness bug. The Dock-icon
   (sshd) identity has its own grant and is unaffected.
2. **Instance lock.** Stop any running pair first (`pkill -TERM remote-display-host`,
   box `pkill -TERM remote-display`) — the daily-use pair holds the profile lock and
   the gate host dies on it.
3. **Hold the Mac awake**: `caffeinate -disu -t <runsecs+120> &`. An idle Mac's own
   background work (`mediaanalysisd`, aerial wallpaper, WindowServer) drove
   capture→encode from 26.6 ms to ~90 ms and invalidated an entire afternoon of E2E
   comparisons.
4. **Record environment**: `uptime` load averages + top-3 CPU processes, alongside the
   run tag. Retain with the metrics.

## Workload

5. **Never measure with manual dragging** (irreproducible; owner fatigue corrupts the
   design). Use `tools/scripted_workload.sh <secs>` — a Terminal window placed on the
   virtual display scrolling ordinary source text.
6. **Never scroll random/high-entropy content** — it produces adversarial keyframes
   (2.7 MB measured) unrepresentative of real use.
7. **Close workload windows after every run** (`osascript` close-non-tail). Seven
   leaked windows once stacked into the load average.

## Run hygiene

8. **Minimize virtual-display lifecycles.** ~15 create/destroy cycles in one day
   degraded WindowServer (3 h CPU) until the whole Mac was laggy and only a reboot
   cleared it. Batch questions into as few runs as possible; prefer one run that
   answers three questions over three runs.
9. **No display plug/unplug during a run** — topology changes renumber the virtual
   display mid-measurement.
10. **One variable per comparison**; interleave A/B/A/B when the host may drift, and
    keep tracing state identical across compared runs.

## Validity gates (a run failing any of these is discarded, not argued with)

11. Clean trace footers, joiner PASS, zero identity failures/ambiguities.
12. `capture→encode` p50 ≤ ~30 ms — above that the host was saturated and every
    downstream/E2E number is host-bound noise.
13. Zero oversize drops (`OVERSIZE` in client log), zero decode/render failures.
14. Content-volume sanity: compare host `KB sent` across compared runs (±30 %).

## Reporting

15. Name endpoints precisely: the trace measures **capture → return from
    `canvas.present()`**. Never call it screen/photon latency.
16. Stage timers are cumulative **means**; joined segments are **p50/p90 over
    survivors**. Do not mix them into one arithmetic identity, and always report
    delivery ratios (joined/host, presented/joined) next to latency.
17. Segment p50s do not sum to E2E p50 (survivorship, covariance). Quote both, derive
    neither from the other.
