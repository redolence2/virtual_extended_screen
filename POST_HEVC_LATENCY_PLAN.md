# Post-HEVC Latency Analysis and Optional Optimization Plan — for review

**Date**: 2026-08-07 · **Baseline**: Candidate A shipped (HEVC 50 Mbps; gate runs
`a1`/`a2`: E2E p50 **81.2 / 79.6 ms**) · **Owner question**: "if I want to further
optimize latency, do the shelved proposals help, or does HEVC reset the analysis?"
**Status**: analysis only — no code touched. The demo's own gate (10-minute owner
smoke on the shipped launcher) is SEPARATE, still pending, and not blocked by this
document. Everything here is optional post-demo work, each item re-entering review.

---

## 1. Measured budget under HEVC (a1/a2, evidence/zero_copy/, ±3.5 ms clock uncertainty)

| segment | a1 p50 | a2 p50 | decomposition (inference marked) |
|---|---:|---:|---|
| capture → encode out | 23.47 | 23.78 | HEVC encode + depth-2 in-flight queue (+ GPU load from the second ASUS display attached to the Mac) |
| encode out → receive | 6.46 | 6.55 | chunking + pacing + WiFi — near floor |
| receive → decode done | 25.16 | 29.45 | **~17.5 ms = one frame period of structural wait (gap-1)** + ~8 ms real decode *(inferred: gap mode is exactly 1; 25.2 − 17.5 ≈ 7.7 ms matches the M1-era 4K cuvid decode measurement of ~9.5 ms)* |
| decode done → present | 23.06 | 23.21 | copy chain (download + 3 CPU copies + upload + rotate blit, ~8–12 ms est.) + vsync wait (~8 ms avg) + mailbox poll (≤2 ms) |
| **E2E** | **81.20** | **79.57** | p90: 87.0 / 94.9 |

Ordinal gap collapsed 6 → **1** (3,156/3,157 and 2,950/2,951): the DPB wait is gone;
what remains at the decode stage is cuvid's one-AU parser latency — NVIDIA documents
one-frame parser delay for NAL-stream codecs when end-of-picture is not explicitly
signaled, and FFmpeg's cuviddec does not signal it.

## 2. Disposition of the shelved proposals under HEVC

| shelved item | status under HEVC | reason |
|---|---|---|
| SPS VUI surgery (Candidate B) | **DEAD** | H.264-specific; HEVC declares reorder limits properly. Returns only if H.264 ever returns. |
| Zero-copy CUDA/GL render | **VALID, re-ranked UP** | Its territory was always decode→present (codec-agnostic; the copy chain still runs today). Was ~8 ms of 162 (5%); now ~10–15 ms of 80 (~15%). The reviewed contracts (`ZERO_COPY_PLAN_review.md` §C–F) carry verbatim; W0b (real-frame interop gate) — mooted for the demo — would be **required again** before that build. |
| Apple `EnableLowLatencyRateControl` probe | **TRANSFERS, unproven** | Now aimed at the 23.5 ms encode segment; HEVC support on this OS/hardware must be verified first; known interactions (infinite GOP / temporal layers) must be checked against the 10 s-KF + IDR-request design. |

So: not a whole new story — same rig, same segment map, same review loop. But the
**ranking inverted**: the top lever is now something that was noise before (gap-1),
and the biggest shelved item (zero-copy) moved from "refuted as THE fix" to "largest
single remaining lever," which is a different claim than the one W0a killed.

## 3. Ranked experiments (each gated, cheapest first)

### E1 — AUD-append probe: kill gap-1 (~1 h incl. measurement; potential −15 ms)

Hypothesis: cuvid holds picture N because it cannot know the access unit ended until
the next NAL arrives. Our wire delivers complete AUs, so the client can append an
**HEVC access-unit delimiter** after each fed AU; per H.265, an AUD begins the *next*
AU, so the parser should close and emit picture N immediately.

- Bytes appended (Annex-B): `00 00 00 01 46 01 50` — NAL header type 35 (AUD_NUT),
  layer 0, temporal_id_plus1 1; payload `pic_type=2` (B/P/I allowed) + stop bit.
- Two variants to test: (a) appended to the same `send_packet` buffer;
  (b) fed as a separate packet immediately after. Env-gated `RESC_APPEND_AUD=1`
  (value-parsed, not presence-parsed — per the W0a-probe lesson), one warn log.
- Gate (same B0/A1 protocol, one 60 s run): receive→decode p50 ≤ ~12 ms and E2E
  p50 ≈ 65 ms. If the gap-1 does not move, the wait is not AU-boundary detection —
  STOP this line and record the negative result.
- Risks for review: decoder/parser state confusion from trailing AUDs (malformed-AU
  perception, POC effects); recovery/IDR-path interaction; HEVC conformance of
  `pic_type=2` for our stream.

### E2 — zero-copy render revival (7–9 h; ~−10–15 ms; only worthwhile after E1)

Unchanged from the reviewed plan + amendments; W0b gate reinstated as the entry
condition; acceptance numbers to be re-derived from the then-current baseline. Not
started without a fresh owner GO — this is the expensive item and the owner already
declined gold-plating once.

### E3 — encoder-side probes (cheap, independent; ~−3–6 ms combined)

1. **ASUS-display tax measurement** (zero code): one measured run with the Mac's
   second display unplugged. Quantifies the compositor/GPU share of the encode
   segment. If material, the "fix" is a usage habit, not code.
2. **`EnableLowLatencyRateControl` with HEVC** (env-gated host probe): create-time
   specification; dump + inspect the stream first (per the prior review's pattern);
   measure only if the stream/behavior changes promisingly; explicitly verify forced
   keyframes still work (Apple documents infinite-GOP behavior in this mode — our
   10 s GOP + client IDR requests must survive).
3. **Depth-2 → 1**: rejected on existing evidence — HEVC encode p50 23.5 ms exceeds
   the 16.7 ms frame period, so depth 1 would re-halve throughput (the sealed
   depth-1 finding). Listed to show it was considered.

### E4 — present-path notes (no separate work)

Vsync interval stays 1 (prior review ruling; tear-free). The vsync-average ~8 ms is
architectural at 60 Hz; zero-copy (E2) restructures the rest of the present path.
No independent E4 work item.

## 4. Projected outcomes (hypotheses, gates decide)

| state | E2E p50 (projected) |
|---|---:|
| shipped today | **~80 ms (measured)** |
| + E1 (gap-1 killed) | ~65 ms |
| + E1 + E3 | ~60 ms |
| + E1 + E2 (+E3) | **~50–55 ms** |
| architecture floor (all levers, this design) | ~45–55 ms |

Below that floor means custom NVDEC decode paths / non-vsync presentation — different
architecture, different project, not proposed.

## 5. Honest caveats

- The 17.5 ms gap-1 attribution is inferred (gap ≡ 1 × frame period + decode-time
  cross-check), not separately timestamped; E1 is precisely its falsification test.
- decode→present decomposition (copy chain vs vsync) is estimated from payload
  accounting and vsync statistics, not per-stage timestamps; E2's own gates would
  measure it.
- a1/a2 are two runs on one day with owner-drag workloads; segment p50s vary run-to-run
  by a few ms (see a1 vs a2 receive→decode: 25.2 vs 29.5).
- The owner's stated bar was "lower than perceivable"; at ~80 ms the marginal value of
  each further −10 ms is an owner judgment, not an engineering one.

## 6. Questions for the reviewer

1. Is the AUD-append mechanism sound per H.265 AU semantics (AUD as next-AU opener
   forcing emission), and which feed variant (same-buffer vs separate-packet) is
   preferable for cuvid's parser?
2. Any conformance/stability objection to `pic_type=2` trailing AUDs on this stream,
   or a safer NAL choice for the same purpose?
3. Does the zero-copy revival correctly inherit the existing reviewed contracts with
   W0b reinstated, or does the changed baseline require any contract update beyond
   re-derived acceptance numbers?
4. Known constraints for `EnableLowLatencyRateControl` + HEVC on macOS 26/Apple
   Silicon that would kill E3.2 before the probe?
5. Approve the E1 → E3 → (owner decision) → E2 ordering?
6. Is the 45–55 ms architecture-floor claim sound?
