# C4 evidence summary — real zero-output forced on the selected backend (ERR-09 not needed)

Date: 2026-08-04 · Executor: decoder/harness-implementer worker + root-reviewer verification and
the acceptance ruling. Scope: corrective item C4 (`A00_COMPLETION_REPORT_AMENDED_review.md`
finding 5, as amended: selected `sw1-lowdelay` first; dated ERR-09 only if unforced).

## Result

**Strategy A forced it on both backends, every run.** A parameter-set-only packet (VPS/SPS/PPS,
NAL types 32/33/34, extracted from the real sample's first AU) submitted as ordinal 0 through
the normal retry path was **accepted by the real, unmodified decoder and emitted nothing** —
a genuine zero-output packet. The remaining 480 VCL AUs then ran to EOF/tail with exact ordinal
coverage, exactly-once acceptance, and ordinal 0 never appearing in any emission (the
zero-output packet produced nothing and corrupted nothing).

- sw1-lowdelay (gating): zero_output_packets 1 · 480/480 submitted/emitted · all invariants
  true · pass · **two runs byte-identical** (determinism re-verified by root reviewer via
  on-box diff).
- cuvid-lowdelay (supplementary): pass, and it caught one *incidental* real zero-output drain
  at its second submission — consistent with its R6 `max_lag=2` pipeline depth.

## Acceptance ruling (root reviewer)

The reserved question was whether a crafted packetization counts as "real" evidence. Ruling:
**yes** — ERR-03's requirement concerns the decoder-loop state machine's behavior when the real
decoder accepts a packet and emits nothing; that is exactly what was exercised, on the exact
selected configuration, through the normal submit/drain path, with the whole-sample invariants
proving no downstream corruption. The packet's *content* is crafted; the decoder's *behavior*
is genuine. `strategy_used: "param_set_packet"` records the provenance honestly. The ERR-09
escape hatch was implemented (would report `not_forced`) but never exercised — **no equivalence
erratum enters the contract.**

## Worker deviations — accepted

Deriving `zero_output_packets` structurally from the detail list (a real counter/list divergence
surfaced on cuvid and was eliminated by construction); additive report fields in sibling-mode
style; retained per-run determinism copies on the box.

## Verification

Box workspace suite 21 suites / 0 failed (incl. 4 new annexb splitter tests); clean scoped
diffs (backend-construct untouched — its existing API sufficed); reports retained at
`c4-{sw1,cuvid}-zero-output.json`.

ERR-03's zero-output branch: **closed with forced real evidence on the selected backend.**
