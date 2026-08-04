# A0.0 Remediation Plan — amended v2

| | |
|---|---|
| **Date** | 2026-08-04 (v2, same day) |
| **Responds to** | `A00_IMPLEMENTATION_REPORT_response_review.md` (conditional accept as disposition) and `A00_REMEDIATION_PLAN_review.md` (direction accepted; NO-GO as written; eight required amendments). **All eight amendments are incorporated in this revision** — §2 maps each to where it landed. |
| **What this document is** | The execution specification for closing A0.0 under `IMPLEMENTATION_PLAN_V11.md` + `CONTRACT_ERRATA.md`. Not a contract document; no V12. Contract-touching items (ERR-07, the WIRE status note, ERR-08 if triggered) enter `CONTRACT_ERRATA.md` as dated entries per V11 §14. |
| **Standing decision** | Per the plan review: remediation implementation is **GO once these amendments are incorporated** (this revision). **A0.0 completion, Stage-1 freeze, A0 entry, and T1 entry remain NO-GO** until the clean-checkpoint evidence passes independent re-review. |

---

## 1. Evidence state ladder (governs all status language)

1. **accepted** · 2. **patched in working tree** · 3. **locally verified** · 4. **committed at clean checkpoint** · 5. **gate closed** (retained evidence + independent re-review). Nothing is "closed" below state 5; "done" is banned below state 3.

Current true states (unchanged from v1, re-verified): F1/F2/F3/F5/F6/F7 accepted-open; F4 ERR-06 at state 3 with typed-dispatch and clean-commit halves open; F8 partially patched (root `ubuntu-client/Cargo.toml:38–39` caret, `video-decode` loose `"7"`, lockfiles uncommitted); F9 accepted-in-principle; F10 banner patched, body unreconciled.

## 2. Where each required amendment landed

| Review amendment | Landed in |
|---|---|
| 1. Framing-length gate split from typed validator/router; protobuf rules; RescCore deps; barrier reuses phase model | §3 D1, work items R5 + R2a |
| 2. Immutable trace identity chain with explicit carrier | §4 (frozen design), work items R2b + R4 |
| 3. ERR-03 bounded characterization; no forcing guarantees; backend fixed for A0 | §3 D2, work item R6 |
| 4. Host/client doctor split; R3a/R3b split around R6; receiver predicate additions; cursor-clock test correction | work items R3a, R3b, R2b |
| 5. R1 gains full ASCII implementation + fixtures + tests; corrected Cargo evidence protocol; WIRE status governance note | work item R1 |
| 6. Explicit dependency order, file ownership, role-based labels | §5 ordering + §6 |
| 7. Named evidence layout, manifest, platform matrix, "applicable A0.0 gates" wording, re-checkpoint rule | §7 |
| 8. Corrected phase exit sequence (T1 does not unlock from the A0.0 review) | §8 |

## 3. Decisions (amended)

**D1 — Typed dispatch in A0.0 as two explicit layers, both languages.** The v1 single-API design could not prove V11's 64 KiB rule, since it received an already-materialized buffer. Amended:

- **Layer 1 — framing-length gate.** Consumes only the 4-byte length prefix; rejects length > 64 KiB *before any body allocation or read*; returns the permitted exact body length. Its test proves an oversized prefix causes zero body reads and zero body-sized allocation.
- **Layer 2 — typed validator/router (pure).** Consumes an already-bounded, generated-decoded V3 `Envelope`; takes endpoint role, message direction, current protocol phase, active/candidate run context, and the minimal external facts semantic checks need; validates field caps and semantic ranges before producing a transition; returns *(typed message, next phase)* or the exact `FatalCode`. No socket, logging, injection, or other transport side effects. Kept small — a pure validator/router with role-and-phase legality checks, not a second lifecycle actor.
- **Protobuf rules, stated exactly:** unknown fields remain ignored by the generated decoder; an unknown-only oneof decodes as an *absent payload* and is rejected as `PROTOCOL_VIOLATION`. No raw unknown-field scanner is added.
- **Phase model is singular:** the ERR-01 activation barrier (R2a) reuses this same phase model; no second, drifting state machine.
- **Swift wiring:** `RescCore` gains `RescProto` + `SwiftProtobuf` dependencies (smallest current-tree change; `HarnessSender` inherits them transitively — accepted).
- Not wired into the live A0 runtime; T1's cutover consumes it. No cutover evidence is claimed.

**D2 — ERR-03: test double AND real-backend evidence via a *bounded characterization protocol*.** The v1 claims ("deterministic by the second packet", "bounded by the surface pool") are withdrawn — they are not established API guarantees, and the fixed sample's bframes=0 means zero-output first packets and multi-output drains are not automatic. Amended protocol (full spec in R6):

- Explicit maximum attempted packets and wall-clock timeout — no unbounded experiments.
- Record every attempted ordinal, accepted ordinal, EAGAIN result, drain start/stop, outputs-per-drain, and recovered PTS.
- On EAGAIN: retain and resubmit the *same* packet after draining; prove exactly-once acceptance, never skipped or double-counted.
- Prove every real-backend EOF/tail drain ends with emitted == submitted and exact ordinal coverage.
- Run both `hevc` and `hevc_cuvid` under recorded machine/driver/FFmpeg versions.
- The deterministic test double covers all *hypothetical* state-machine branches; it is never claimed as proof of real decoder timestamp behavior.
- If a stated case cannot be forced within the bounded protocol on a real candidate: record that outcome, then add **ERR-08** defining equivalent acceptable evidence *before* substitution. Never silent.
- **End state:** one backend selected and **fixed for A0**, together with frozen `decoder_lag_bound` and `output_deadline_ms`. The final canonical profile artifact still freezes at Stage 2, but A0 runs on a decided backend, not a "candidate".

**D3 — ASCII canonicalization: erratum + full implementation inside R1.** Definition, exactly: *every profile key and string value must consist only of ASCII bytes*, after which the existing schema/value validation applies. No Unicode normalization machinery. The v1 plan scheduled only writing ERR-07; R1 now carries the validators, fixtures, and cross-language tests (list in R1).

**Phase clarifications (unchanged from v1, review-confirmed):** the formal optical spot-check is A0 work — an early optical sanity check is permitted but appears in no A0.0 pass predicate. The client doctor validates the candidate/selected backend's required texture path; the other candidate's format is informational.

## 4. Frozen A0.0 trace identity design (the R4 contract)

The carrier is explicit — an implementation that recreates latest-value joining under another name fails review:

1. The SCK callback creates one immutable **`CapturedFrame`**: pixel buffer · capture generation/run tag · capture sequence · actual SCK presentation time converted to host continuous-monotonic microseconds · conversion/fallback label · clock uncertainty.
2. **`LatestFrameSlot`** stores and consumes the entire `CapturedFrame` atomically. The pixel buffer is never stored separately from its identity.
3. Intentional latest-wins replacement increments a drop counter. A dropped identity is never reused or relabeled.
4. Encoder submission carries the capture sequence through `presentationTime` (or an equally exact per-submit context); the asynchronous output callback recovers the identity of *that exact submitted frame*.
5. The encoded output is bound to the actual V1 wire `frameID`; the host trace records the frameID → capture-identity mapping.
6. The client timestamps receipt when the complete encoded frame is assembled, *before* any drop-capable queue.
7. The decoder packet PTS is set to `frameID`. Every emitted decoded frame recovers its *own* PTS; delayed and multi-output emissions never inherit the current decode call's ID.
8. The render trace records the recovered frameID immediately adjacent to the successful presentation call — not when upload is merely scheduled.
9. The joiner accepts only exact identities and records clock offset, delay, uncertainty, fallback status, and every rejected sample's reason.

Pass predicate: the retained live artifact shows **zero identity ambiguities**. Intentional capture/render drops are allowed and counted; identity substitution is not. **Nil SCK sync clock:** use the labeled callback-time fallback V11 allows, carry its larger uncertainty, and never mix it silently with true SCK-PTS samples.

## 5. Work breakdown — dependency order

Labels are stable (v1 + review names). Order is normative; §6 gives ownership. Estimates yield to correctness — R4 in particular is the highest-integration-risk item and may exceed its figure.

**R1 — Paper, pins, canonicalization (~0.5 d).**
(a) Root `ubuntu-client/Cargo.toml` and `crates/video-decode/Cargo.toml` → `=7.1.0`/`=7.1.3`. Evidence protocol, corrected: SHA-256 of `Cargo.lock` before the edits → exact manifest edits → `cargo metadata --locked --offline` showing the `=` requirements resolving 7.1.0/7.1.3 → `cmp`/SHA-256 proving `Cargo.lock` bytes unchanged → lockfile committed at R7. (The v1 "metadata diff empty" sentence was wrong and is withdrawn — metadata embeds the requirement strings.)
(b) **ERR-07** (dated) in `CONTRACT_ERRATA.md`: ASCII-only rule replacing NFC, per §3 D3. `docs/WIRE.md` canonicalization text replaced accordingly.
(c) A dated **governance status note** in `CONTRACT_ERRATA.md` recording the WIRE status transition, *then* `docs/WIRE.md:6` → "**Stage-1 candidate; freeze pending A0.0 gates and clean checkpoint**".
(d) ASCII validation implemented both ends: schema-specific check in Swift `validateRuntimeProfile` and Rust `validate_runtime_profile`; a shared valid-ASCII fixture; shared non-ASCII rejection fixtures *including a canonically-encoded non-ASCII string that would pass sort/minify checks*; tests proving both languages return the same verdicts.
(e) Report body sweep (ERR-01…06 at line 7; "(new, frozen)" 25; "structural freeze" 92; 480→451 at 120; ~140→126 at 168/241; 22-entry→23 at 229; plus a retained grep sweep for `frozen|freeze|committed|~140|480|22-entry|four`). Amend `A00_IMPLEMENTATION_REPORT_response.md` dispositions to §1's states and reference this plan.
*Evidence:* sweep output + Cargo hash chain retained. *Reaches state 3.*

**R5 — Framing gate + typed validator/router (~0.5–1 d).**
Both layers per §3 D1 in Rust (`protocol::v3wire` additions or sibling module) and Swift (`RescCore`, with the RescProto/SwiftProtobuf dependency wiring). Shared state-machine fixture vectors under `proto/fixtures/` (valid sequences, wrong-direction, wrong-state, wrong-phase, oversized length prefix, unknown-only oneof, malformed payload, cap violations), consumed by `resc-fixture-check` and `cargo test`.
*Evidence:* identical verdict tables both ends over shared vectors; the layer-1 zero-allocation/zero-read test. *Reaches state 3.*

**R2a — ERR-01 activation-barrier proof (~0.25–0.5 d).** Delayed/reordered scheduling of the two TCP handlers proves no client control payload before activation and first post-barrier input accepted — expressed against the R5 phase model.
*Reaches state 3.*

**R2b — CapturedFrame + capture-generation binding + cursor clock (~0.5–1 d).**
Introduce §4 items 1–3 (immutable `CapturedFrame`, atomic `LatestFrameSlot`, drop counter) and bind every SCK callback to its run at creation; late-callback test: teardown → new run → late callback must not populate the newer run's slot (fixes the live bug). CursorTracker moves to the host continuous clock *directly*; tests cover injected-clock monotonicity and sequence-number ordering as **separate properties** (no calibration-refresh coupling — the v1 wording is withdrawn).
*Reaches state 3.*

**R4 — Trace identity propagation + joiner (~1.5–2.5 d; correctness over schedule).**
§4 items 4–9 end to end: submit-context identity recovery in the encoder callback, frameID binding and host-side mapping record, client receive stamp at assembly, decoder PTS=frameID with per-emission recovery, present-adjacent render stamp, exact-identity joiner with offset/delay/uncertainty/fallback/rejection records. Optional early optical sanity check — not a gate.
*Evidence:* joined live artifact, zero identity ambiguities, per-sample uncertainty. *Reaches state 3.*

**R3a — Fail-closed infrastructure, parameterized by candidate (~1 d).**
Exit-flush on every deliberate immediate exit both ends (host lock-denial + doctor paths gain what the client's `RescLog::flush()` has); 0600 reassertion on pre-existing files; two-process lock-contention test. **Host doctor** (host-side conditions only): every VideoToolbox property-set status checked, load-bearing read-backs, `PrepareToEncodeFrames` checked, profile-true keyframe interval and settings, bundled-frame encode with required RA NAL set, every required condition in the exit code, synchronous `doctor_complete` before exit. **Client doctor** (parameterized over the two candidates until R6 selects): opens the explicit candidate; decodes the complete bundled sample with EOF/tail drain; requires submitted == emitted, exact ordinal recovery, zero unknown/duplicate/reordered outputs; validates the candidate's actual decoded-frame→SDL *texture update* path (not creation only) — sw1-lowdelay → its software/IYUV path, cuvid-lowdelay → the transferred-NV12 path per WIRE; other candidate informational; synchronous `doctor_complete`. **Harness sender:** write-confirmed `frames_sent`, fail-stop on write error, `sustained_60hz` ⇐ acked==sent ∧ zero outstanding ∧ zero order violations, `ack_order_violation` in JSON, nonzero exit on any failure. **Receiver pass predicate:** accepted==acked ∧ emitted==submitted ∧ unknown_pts==0 ∧ nonzero frames ∧ zero duplicates ∧ zero reorders/skips ∧ zero ACK-order violations ∧ zero protocol/fatal decoder errors ∧ clean EOF/tail drain ∧ zero outstanding frames at exit. Failure-injection tests prove each predicate can actually fail.
*Reaches state 3.*

**R6 — Decoder characterization + backend selection (~1–1.5 d).**
Test double first (retain-drain-resubmit, exact-once accept, zero/multi-output branches, tail flush). Then the §3 D2 bounded characterization on real `hevc` and `hevc_cuvid` on the box — including **real-backend EOF/tail-flush ordinal evidence** — then clean stall-free runs → freeze `decoder_lag_bound`/`output_deadline_ms` → **select and fix the A0 backend**. ERR-08 only per the escape hatch.
*Evidence:* retained per-attempt records (ordinals, EAGAIN events, outputs-per-drain, recovered PTS), version identifiers, a recorded selection decision. *Reaches state 3.*

**R3b — Selected-profile doctor + harness evidence (~0.25–0.5 d).**
Repeated runs (≥3×) of both doctors on the selected backend/profile and the harness pair, `doctor_complete` present in every retained log, predicates green.
*Reaches state 3.*

**R7 — Clean checkpoint + applicable-gate matrix (~0.5 d).**
One commit containing all sources, generated code, fixtures, lockfiles, docs, errata, and the compact evidence per §7. From that commit, run the full **platform matrix** (§7) covering every *applicable A0.0 gate* — no claim is made about A0/T1 gates. Then the amended A0.0 completion report (each gate's ladder state) → independent re-review. Any source or contract correction after the checkpoint ⇒ new checkpoint ⇒ full matrix rerun before re-review.
*Reaches state 4; state 5 only via the re-review.*

Total: **~6–8 focused days.**

## 6. Ownership and parallelism

Role-based ownership (model routing may change without changing this plan):

| Role | Owns (exclusive while their item is open) |
|---|---|
| **Root reviewer** (orchestrator; also merges everything) | `Package.swift`, both root `Cargo.toml`s, `CONTRACT_ERRATA.md`, `docs/WIRE.md`, report/response docs, `proto/fixtures/` manifests, all R1 + R7 work |
| **Protocol implementer** | `ubuntu-client/crates/protocol/src/*` (v3 modules), Swift dispatch sources in `RescCore`, FixtureCheck registration for R5 vectors |
| **Capture/trace implementer** | `DisplayCapturer.swift` / `LatestFrameSlot` / new `CapturedFrame`, `VideoEncoder.swift` submit path, `HostSession`/`VideoSender` trace hooks, CursorTracker, client receive/present trace, joiner |
| **Decoder/harness implementer** | `backend-construct`, `decoder-experiment`, `harness-receiver`, client doctor decode path, `HarnessSender` predicate code |

Rules: the dependency order in §5 is serial except that **decoder/harness (Ubuntu-side) work may run concurrently with capture/trace (Mac-side) work** once R5 is merged — they share no files. No two workers ever concurrently modify `Package.swift`, `FixtureCheck`, `DisplayCapturer`/`LatestFrameSlot`, `VideoEncoder`/`Doctor.swift`, or `decoder-experiment`; contended files (`Package.swift`, FixtureCheck's `main.swift`) are edited only by the role whose item is open, with the root reviewer serializing merges. Every worker brief lists its exact owned paths; `set -o pipefail` on all remote verification (standing rule).

## 7. Evidence layout and platform matrix

All retained evidence lives at **`evidence/a00/<full-commit-sha>/`** with a `manifest.json` recording: exact commit + `dirty=false`; Mac and Ubuntu OS/build/architecture; toolchain, FFmpeg, SDL, NVIDIA-driver, protoc, SwiftProtobuf versions; for every run — command, working directory, start/end time, exit code, and the gate it serves; path + SHA-256 for each doctor/decoder/harness/fixture/joined-trace artifact; the selected backend and measured bounds; explicit PASS/FAIL per predicate. Compact JSON reports and a representative joined-trace sample are committed; large raw media is retained as hash + generation command + size + stable machine path.

| Platform | Runs |
|---|---|
| Mac | protobuf regen `--check`, Swift build + `resc-fixture-check`, host doctor, harness sender, capture/trace evidence |
| Ubuntu | locked workspace build/tests, client doctor, decoder experiments, harness receiver |
| Cross-machine | harness pair, clock/join trace, same-commit + environment checks |

## 8. Phase exit — exact sequence

1. R1–R6 locally verified with retained evidence (state 3).
2. R7 clean commit + complete applicable-A0.0 matrix pass (state 4).
3. Independent re-review returns GO (state 5).
4. **Then** A0.0 is complete, Stage 1 freezes, and **A0 may begin**.
5. A0 performs the real-pair histogram, the software latency baseline plus optical spot-check, the window trial, bitrate confirmation, and the Stage-2 final profile/fixture freeze.
6. **Only after** the separate V11 T1 entry checks and both final-profile doctors pass may **T1** begin.

T1 does not unlock from the A0.0 re-review. Until step 3, every status line in the repo reads *candidate* and the header's NO-GO stands.
