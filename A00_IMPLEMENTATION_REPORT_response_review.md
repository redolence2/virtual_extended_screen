# A0.0 Implementation Report Response Review — Conditional Acceptance as Disposition, Not Closure

Reviewed: A00_IMPLEMENTATION_REPORT_response.md

Response SHA-256: 1798fddf4fc42f658dc3842fe7173af7ad6a114c6a2daacdde30fe396889ee3d

Review date: 2026-08-04

## Executive verdict

**CONDITIONAL ACCEPT as a remediation disposition.**

**NOT ACCEPTED as final closure, A0.0 completion, Stage-1 freeze, A0 entry, or T1 entry.**

The response gets the central decision right: the original implementation report overclaimed completion, all material blockers remain binding, remediation should continue under V11 and its errata, and neither a V12 plan nor an architecture rewrite is needed. ERR-06 is a reasonable narrow resolution of the temporary schema-path problem.

The response is not yet suitable as the authoritative closure record because it repeats the same kind of process error it acknowledges: several dirty-tree edits are called “CLOSED” or “done” before their full scope and reproducibility evidence are complete. It also silently defers one A0.0 obligation to T1 and describes an ERR-03 test strategy that is insufficient by itself.

| Decision | Result |
|---|---|
| Accept withdrawal of the original completion/freeze claim | **YES** |
| Accept the overall remediation direction | **YES** |
| Accept ERR-06 | **YES** |
| Accept F1–F7 and F9 dispositions as open work | **YES** |
| Accept F8 as closed | **NO** |
| Accept F10 as closed | **NO** |
| Accept A0.0 completion or Stage-1 freeze | **NO** |
| Start A0 | **NO** |
| Create V12 or redesign the architecture | **NO** |

## What the response gets right

- It explicitly withdraws the premature A0.0 completion and Stage-1 freeze claim.
- It accepts every substantive runtime, contract, trace, doctor, harness, and reproducibility finding rather than attempting to explain the blockers away.
- It preserves the correct phase boundary: all A0.0 work and a clean-checkpoint rerun must pass before A0 begins.
- It correctly retains the existing architecture and V11 governance.
- It correctly treats the 594/594 harness result and the two decoder trials as encouraging evidence rather than formal gates.
- It correctly recognizes that recorded deviations are not closed requirements.
- It correctly identifies the intermittent host doctor-completion logging race. The retained host log contains doctor_complete for two runs while another reported run lacks it, which is consistent with the claimed exit/flush race.
- It gives a useful, compact remediation order.

## Accepted immediate change

### ERR-06 is a valid schema-path correction

CONTRACT_ERRATA.md now explicitly names proto/control_v3.proto as the normative Stage-1 V3 schema while the A0 baseline continues compiling the legacy proto/control.proto. This is a narrow governance correction, not an architecture change.

It is consistent with:

- docs/WIRE.md already naming proto/control_v3.proto as its V3 companion;
- Rust code generation compiling both schemas;
- Swift generation including the V3 schema;
- V11 governance allowing dated contract errata instead of a new plan.

The remaining report references should be updated from ERR-01…05 to ERR-01…06, but that clerical cleanup does not invalidate ERR-06 itself.

## Required corrections before final acceptance

### 1. F8 and remediation step 9a are only partially closed

The response correctly changed the three new crates to true exact requirements:

- backend-construct;
- decoder-experiment;
- harness-receiver.

However:

- the active root ubuntu-client/Cargo.toml still contains ffmpeg-next = “7.1.0” and ffmpeg-sys-next = “7.1.3”, which Cargo interprets as caret requirements;
- the root client directly uses ffmpeg-next in its doctor;
- video-decode remains a workspace member and a direct root dependency with loose version “7” requirements;
- Cargo.lock and Package.resolved remain uncommitted;
- because the lockfiles are untracked and have no committed predecessor, “lock resolution verified unchanged” cannot be independently established from this working tree.

The current Cargo.lock does resolve ffmpeg-next 7.1.0 and ffmpeg-sys-next 7.1.3. That proves the current resolution, not that project-wide exact pinning and reproducibility are closed.

Required disposition:

1. Change F8 and step 9a from **CLOSED/done** to **partially closed; dirty-tree patch and clean-commit verification pending**.
2. Exact-pin the active root dependencies.
3. Until video-decode is deleted, either exact-pin it too or remove it from the active dependency graph under an explicit, testable quarantine. A prose promise that it dies at T1 is not operational quarantine.
4. Commit both lockfiles with the final A0.0 artifacts and rerun from that clean commit.

For this personal fixed-pair project, exact-pinning all remaining FFmpeg manifest entries until the legacy crate is deleted is the simplest solution.

### 2. F10 and status-language cleanup are only partially closed

The new top-level report banner is good: it clearly relabels the report as progress/candidate work and lists the corrected counts.

The repository remains internally inconsistent:

- docs/WIRE.md still declares “Stage-1 structural freeze (A0.0)” even though the response correctly says the freeze has not happened;
- the report scope still references ERR-01…05 rather than ERR-01…06;
- the report body still contains the stale 480-line Doctor count, approximately 140 fixture checks, a 22-entry table description, and four-proto wording;
- the report still contains headings and sentences such as “new, frozen” and “Stage-1 structural freeze” that conflict with the correction banner;
- “committed generated sources” is inaccurate while those sources remain untracked.

A top-level erratum makes the intended status understandable, but it does not justify calling F10 fully closed.

Required disposition:

1. Change F10 from **CLOSED** to **partially closed** until the body is reconciled.
2. Change the WIRE status to **Stage-1 candidate; freeze pending A0.0 gates and clean checkpoint**.
3. Update the report scope to ERR-01…06.
4. Correct the stale body values and remove premature freeze/committed wording.

### 3. Typed dispatch cannot be silently deferred to T1

The response says generated V3 types, tested parsers, and the clock intercept exist, while the stateful V3 dispatcher is T1 work.

V11 explicitly assigns “codegen + typed dispatch” to A0.0. Generated types and standalone parsing helpers are not, by themselves, a stateful typed dispatcher enforcing direction, state, caps, and semantic validation.

Keeping the active A0 runtime on V1 is compatible with this requirement. The simplest closure is to implement and test the V3 dispatcher as an inactive/shared component during A0.0 without cutting the live runtime over to it.

Choose one:

- implement a tested, inactive V3 typed dispatcher in A0.0; or
- add another narrow erratum explicitly moving that obligation to T1 and explaining what A0.0 evidence replaces it.

The response alone cannot move the requirement.

### 4. An ERR-03 test double is necessary but not sufficient

A deterministic decoder test double is appropriate for proving the packet-retention, drain, retry, exact-once acceptance, ordinal-bookkeeping, and tail-flush state machine.

ERR-03 also requires proof about the selected real backend: emitted frames must preserve submitted frameOrdinal through pts or best_effort_timestamp, including the problematic EAGAIN and zero/multiple-output behavior. The retained real-backend runs observed eagain_retries = 0, so that evidence does not yet exist.

Required closure:

1. Use the test double for deterministic transport/decoder-loop state-machine coverage.
2. Obtain corresponding behavior evidence from the actual hevc and hevc_cuvid candidates under conditions that exercise the required paths.
3. If the real APIs cannot be made to exercise a stated case deterministically, add a narrowly justified erratum defining equivalent acceptable evidence; do not silently substitute simulation.
4. Then run the clean stall-free measurements, select one backend, and freeze decoder_lag_bound and output_deadline_ms.

### 5. Correct two phase/simplicity interpretations

#### Optical validation

V11 places the formal optical spot-check in A0. A0.0 must first produce valid trace joining and clock-uncertainty evidence so A0 may start.

The earlier review contained an overly broad sentence suggesting the optical comparison should happen before A0; its final phase summary correctly placed it inside A0. This review clarifies the intended rule:

- A0.0: validate trace identity, timestamps, joining, calibration, and uncertainty;
- A0: run the formal software latency baseline plus one optical spot-check.

An early optical sanity check is allowed, but it is optional and must not become a new A0.0 gate.

#### SDL texture requirements

For the fixed personal profile, the client doctor should fail unless the texture format required by the candidate/final backend works:

- sw1-lowdelay requires its specified software output/upload path;
- cuvid-lowdelay requires its specified NV12 transfer/upload path.

Testing the other format may remain informational. Requiring both formats unconditionally would add an unnecessary compatibility gate for a single selected backend.

### 6. ASCII-only canonicalization requires a narrow erratum

V11 and WIRE currently specify NFC-normalized strings. The response proposes the simpler rule of restricting profile strings to the fixed ASCII vocabulary. That is sensible for this personal deployment, but it changes the normative canonicalization contract.

Record the ASCII-only restriction in a dated erratum and update WIRE and both validators. Do not silently replace the NFC rule only in implementation.

## Verification of claimed immediate closures

| Claim | Review result |
|---|---|
| Three new FFmpeg-using crates changed to =7.1.0/=7.1.3 | **Verified** |
| Active root client exact-pinned | **False** |
| Legacy video-decode operationally quarantined | **False** |
| Current Cargo lock resolves 7.1.0/7.1.3 | **Verified** |
| Lock resolution “unchanged” from a committed predecessor | **Not independently provable** |
| ERR-06 exists and matches current V3 schema references | **Verified** |
| Report has a progress/candidate correction banner | **Verified** |
| Banner’s 126/23/five/451 corrections | **Verified** |
| Report body fully reconciled | **False** |
| Repository-wide Stage-1 status withdrawn | **False; WIRE still says frozen** |

## Corrected disposition language

The response should use these states:

| Finding | Correct state |
|---|---|
| F1 trace/clock | **Accepted; open** |
| F2 ERR-03/backend/bounds | **Accepted; open** |
| F3 mandatory behavioral proofs | **Accepted; open** |
| F4 schema path | **ERR-06 recorded; dirty-tree/typed-dispatch/freeze work still open** |
| F5 doctors | **Accepted; open** |
| F6 harness | **Accepted; open** |
| F7 synchronous evidence/locks | **Accepted; open** |
| F8 dependencies | **Partially patched; not closed** |
| F9 canonicalization | **Accepted in principle; erratum and implementation pending** |
| F10 report precision | **Banner corrected; body cleanup pending** |

Avoid calling a finding closed merely because its first edit exists in a dirty tree. Use:

1. accepted;
2. patched in working tree;
3. locally verified;
4. committed at clean checkpoint;
5. gate closed after retained evidence and independent re-review.

## Final recommendation

**Accept the response's acknowledgment and overall remediation plan after the disposition wording above is corrected. Do not accept it as a final closure record in its current form.**

No new architecture plan is needed. Amend the response and status documents, continue the listed remediation under V11 plus dated errata, and preserve the explicit decision:

**Remediation GO; A0.0 completion, Stage-1 freeze, A0 entry, and T1 entry remain NO-GO until the clean-checkpoint evidence passes independent re-review.**
