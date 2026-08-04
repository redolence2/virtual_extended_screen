# Response to the A0.0 Implementation Report Review

| | |
|---|---|
| **Date** | 2026-08-04 |
| **Responds to** | `A00_IMPLEMENTATION_REPORT_review.md` (verdict: GO to continue · NO-GO on "A0.0 complete"/Stage-1 freeze/A0 entry) |
| **Position** | **The verdict is accepted in full.** The report overclaimed completion; the review's blocking findings are correct, and every checkable factual claim in it was independently re-verified before this response (results below). No architecture change, no plan v12 — remediation proceeds under the existing v11 §14 governance. |
| **Already done with this response** | (a) ffmpeg pins corrected to true exact `=7.1.0` / `=7.1.3` in the three new crates, lockfile resolution verified unchanged; (b) **ERR-06** added to `CONTRACT_ERRATA.md` naming `proto/control_v3.proto` the normative Stage-1 artifact until T1's swap (the review's finding-4 option b); (c) `A00_IMPLEMENTATION_REPORT.md` re-labeled a **progress/candidate report** with a status-correction banner listing its exact-count errata. |

> **Amendment (2026-08-04, per `A00_IMPLEMENTATION_REPORT_response_review.md`):** the disposition
> language below overstated closure — the same process error this response acknowledges. Status
> claims now follow the five-state evidence ladder (accepted → patched in working tree → locally
> verified → committed at clean checkpoint → gate closed after retained evidence and independent
> re-review); the authoritative current-state table is `A00_REMEDIATION_PLAN.md` §1. Specifically:
> **F8** was only *partially patched* here (the root `ubuntu-client/Cargo.toml` was still caret and
> `video-decode` still loose `"7"`; both exact-pinned in remediation step R1 with a lock-hash
> evidence chain), and **F10** was *banner-patched only* (body reconciliation completed in R1).
> Inline `CLOSED` wording below is retained as the historical record and superseded by this note.

---

## 1. Independent re-verification of the review's claims

Before accepting, every claim I could check was checked. All held — including two that sharpen the review's own wording:

| Review claim | My verification |
|---|---|
| ffmpeg pins are caret, not exact | Confirmed: `"7.1.0"`/`"7.1.3"` in all three new crates (now fixed to `=`); `video-decode` remains loose `"7"` — deliberate, that crate dies at T1 (quarantine note accepted) |
| `resc-fixture-check` ≈140 is wrong | Confirmed: exactly **126** `ok` lines |
| `fatal_code_classes.json` has 23 entries | Confirmed: 23 (22 nonzero codes + code 0) |
| Five proto inputs, `Doctor.swift` 451 lines | Confirmed: 5 files; 451 (the 480 figure was pre-refactor and stale) |
| `doctor_complete` absent from the retained host log | **Sharper than claimed**: my grep finds it in **2 of 3** doctor runs — the loss is *intermittent* (a race between the logger's 1-s flush timer and process exit), which is the worst kind of fail-open evidence bug. The reviewer's run lost it; two of mine kept it. Fully confirms finding 7's mechanism. |
| Trace-path gaps (SCK PTS unconverted; latest-wins join; channel-pull recv timestamp; no present timestamp; no joined artifact) | Confirmed by code inspection — all five sub-claims are accurate descriptions of what W4/W8 built |
| Experiment never exercised send-EAGAIN (`eagain_retries = 0` both runs) | Confirmed from the retained reports — the retry path is implemented but has zero runtime coverage |
| Doctor/harness pass predicates fail open (emitted==submitted not required; NV12 not in the exit condition; unchecked `VTSessionSetProperty` statuses; `frames_sent` pre-write; unconditional exit 0; `ack_order_violation` absent from JSON) | Confirmed by code inspection; several were already recorded as deviations (§12.6 of the report) but the review is right that *recorded* ≠ *closed* |

## 2. Dispositions, finding by finding

**F1 — Trace path (highest priority): ACCEPTED.** What exists was built as the v1-wire interim joiner and honestly documented its ±1-frame caveat — but the review is right that the A0-entry gate requires the full contract, and a 16.7 ms identity ambiguity is material to a latency baseline. Closure adopted verbatim: SCK PTS → `CMSyncConvertTime` → bracketed-calibration bridge (the calibration code exists and is unused by the capture path — that is the gap); exact per-frame identity threaded capture→encode→wire→decode→present (no latest-wins join); receive timestamp at the socket/assembly boundary; a present-call timestamp; a retained joined artifact with offset ± uncertainty per sample; optical cross-check before A0.

**F2 — ERR-03 completeness + backend selection: ACCEPTED.** The two real-hardware passes were selection *evidence*, not selection. Remaining, as specified: a deterministic decoder test double forcing send-EAGAIN and zero-/multi-output packets (proving retain-drain-resubmit and ordinal recovery incl. tail flush), stall-free runs on the fixed sample for clean `decoder_lag_bound`/`output_deadline_ms`, then **one** backend frozen. My recorded inclination (sw1-lowdelay: lag 1, no GPU round-trip, 3.6 ms p50 at this resolution) stands as inclination only until those runs exist.

**F3 — Three errata proofs missing: ACCEPTED without qualification.** ERR-01 activation-barrier scheduling test, capture-generation binding on SCK callbacks (today a late callback from a torn-down session can store into a newer run's slot — real bug, not just missing test), and CursorTracker's `CFAbsoluteTimeGetCurrent` → continuous-monotonic migration. All three were documented obligations that never became code; the review is right to block on them.

**F4 — Schema path + dirty tree: ACCEPTED; half closed now.** The path question is resolved by **ERR-06** (added with this response): `control_v3.proto` is formally the normative Stage-1 artifact until T1's swap, with all check references reading accordingly. The reproducibility half stands in full: **no freeze claim is valid until every artifact, lockfile, and piece of retained evidence exists at one clean commit** — the freeze re-run happens from that commit (closure step 10). The "typed dispatch" claim is narrowed as the review demands: generated types + tested parsers + the clock intercept exist; the live host's legacy path is otherwise intact by design, and the stateful v3 dispatcher is T1 work.

**F5 — Doctors fail open: ACCEPTED.** All five closure items adopted: every load-bearing invariant participates in the exit code (incl. NV12), exact submitted==emitted with tail drain, every VideoToolbox status checked with native domain/code logged (this also pulls the `VideoEncoder.start` status-checking forward from its T1 slot, since the doctor's claims rest on it), profile-true settings once selected (incl. the final keyframe interval), and synchronous evidence flush before exit.

**F6 — Harness fail open: ACCEPTED.** All seven hardening items adopted (write-confirmed `frames_sent`, fail-and-stop on write error, `sustained_60hz` requiring acked==sent ∧ zero outstanding ∧ zero order violations, `ack_order_violation` in the JSON, nonzero exit on any failure, receiver pass requiring accepted==acked ∧ emitted==submitted ∧ unknown_pts==0 ∧ nonzero frames, profile-true encoder settings). The 594/594 run keeps its stated status: encouraging preview, not a gate.

**F7 — Immediate-exit evidence loss: ACCEPTED**, and empirically confirmed as intermittent (2-of-3 above). Closure: a synchronous fatal/flush path covering instance-lock denial, doctor completion, and every deliberate immediate exit, on both ends (the Rust side already grew `RescLog::flush()` for the client doctor; the host and the lock-denial paths need the same), plus the mode-reassertion fix on pre-existing files and a two-process lock-contention test.

**F8 — ffmpeg pins: ACCEPTED and CLOSED** with this response (`=7.1.0`/`=7.1.3` in `backend-construct`, `decoder-experiment`, `harness-receiver`; lock resolution verified unchanged; `video-decode`'s loose `"7"` explicitly quarantined to its T1 deletion boundary).

**F9 — Non-ASCII canonicalization: ACCEPTED as scoped** — adopt the review's own simplest rule (restrict profile string values to the ASCII vocabulary; no Unicode machinery until a contract needs it). A validation line in `validate_runtime_profile` on both ends at remediation.

**F10 — Report precision: ACCEPTED and CLOSED** — the exact values now sit in the report's status-correction banner (126, 23, five, 451, the pin history, the narrowed dispatch claim). Point taken permanently: a cold-start evidence report quotes machine-checked numbers or none.

## 3. What is *not* contested

Nothing of substance. Two notes for the record, neither a disagreement: (a) several fail-open items were already listed as §12 deviations — the review's correct rebuttal is that a *recorded* gap is not a *closed* gate, and the completion claim should never have coexisted with them; (b) the interim trace joiner remains useful for relative debugging exactly as the review says, and is retained until the exact-identity path replaces it.

## 4. Remediation order (adopting the review's closure plan; no new plan document)

| # | Work | Notes / est. |
|---|---|---|
| 1 | Status language | banner patched at response time; body reconciled in R1 *(amended 2026-08-04)* |
| 2 | Schema path | ERR-06 recorded; locally verified |
| 9a | Exact pins | three new crates at response time; root + `video-decode` in R1 with hash-chain evidence *(amended 2026-08-04)* |
| 3 | Three behavioral proofs (barrier test, capture-generation binding, cursor clock) | ~0.5–1 d |
| 7/8-hardening | Immediate-exit flush both ends + lock-contention test; doctor + harness fail-closed predicates (F5+F6 lists) | ~1 d |
| 4 | Trace contract repair (PTS conversion, exact identity, true recv/present stamps, joined artifact) + optical validation | ~1–1.5 d |
| 5 | ERR-03 determinism (test double) + clean-run bounds + backend selection frozen | ~1 d |
| 9b | Commit lockfiles with artifacts | with step 10 |
| 10 | **Clean checkpoint commit → rerun every gate from it** → record commands/hashes/environments/exit codes → short amended A0.0 completion report → re-review | ~0.5 d |

Only after step 10 passes does A0.0 get re-declared complete and Stage 1 frozen; A0 (real-capture histogram, formal window trials, joined latency baseline + optical spot-check, bitrate confirmation) begins after that, exactly as the review sequences it.

## 5. Standing acknowledgment

The review's core criticism — *the completion claim ran ahead of the evidence* — is accepted as a process lesson, not just a checklist: measured-once preview rigs were allowed to read as gates, and "documented as deviation" was allowed to read as "closed." The governance that caught it (adversarial review against a frozen contract, every claim re-verified, evidence retained) is working as designed and continues unchanged.
