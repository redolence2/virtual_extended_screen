# R5 evidence summary — locally verified (ladder state 3)

Date: 2026-08-04 · Executor: protocol-implementer worker (2 rounds), design + review + independent
re-verification by root reviewer · Base: candidate checkpoint `ce2d693` + R1.
Scope: `A00_REMEDIATION_PLAN.md` §5 R5 / §3 D1 (plan-review amendment 1).

## Deliverables

- **Layer 1 — framing-length gate**: `frame_body_len([u8;4])` (Rust) / `V3Dispatch.frameBodyLen`
  (Swift): pure function of the 4-byte LE prefix; ≤ 65536 → Ok(length), > 65536 →
  MALFORMED_FRAMING; no allocation, no reads. Counting-reader test proves zero body reads and
  zero body-sized allocation after an oversized prefix (Rust).
- **Layer 2 — typed validator/router** (pure, no side effects):
  `validate_inbound(role, phase, &Envelope, expected_run_id) -> Accepted{next, learned_run_id} | FatalCode`
  with the frozen 6-step order (version → run-id → payload-present → caps → direction/phase →
  semantics), plus `note_outbound(role, phase, kind)` and `note_video_ack(phase)`. Six-phase
  model (bootstrap/announced/profile_accepted/profile_rejected/video_ack_accepted/active); the
  ERR-01 barrier is encoded in it (host reaches `active` by sending the activation heartbeat,
  client by receiving it; input legal at host only in `active`).
  Files: `ubuntu-client/crates/protocol/src/v3dispatch.rs` (895),
  `mac-host/Sources/RescCore/V3Dispatch.swift` (356), `lib.rs` +5, `Package.swift` RescCore +
  RescProto/SwiftProtobuf deps. Inactive: no live-runtime file references it.
- **Shared vectors** (single python oracle → both languages graded against identical rows):
  `tools/gen_dispatch_fixtures.py` (694, idempotent) → `proto/fixtures/dispatch_cases.json`
  (113,622 B): framing 6 · state 164 (144 full matrix + 20 specials) · raw 3 (+2 `.bin`
  fixtures: unknown-only-oneof 15 B, empty envelope 0 B) · outbound 156 (13 kinds × 6 phases ×
  2 roles) · video_ack 6 = **335 rows**. README dispatch section added.

## Verification (worker-run, then independently re-run by root reviewer — identical results)

- Generator idempotent (second run: only `unchanged`, all 4 artifacts).
- Rust `cargo test -p protocol`: **66 passed / 0 failed** (49 pre-existing + 17 new).
- Swift `resc-fixture-check`: **471 ok / 0 FAIL / exit 0** (307 → 471 with outbound coverage).
- Cross-language agreement: all 335 vector rows produce identical verdicts in both languages;
  the outbound tables agreed across three independent transcriptions (Rust, Swift, python
  oracle) on first run.

## Review notes (root reviewer)

- Oracle tables inspected against the frozen design line-by-line: match. Worker improvement
  accepted: `FatalReport.native_domain` capped at 256 B (in the proto; my spec's field list
  missed it; WIRE §1's blanket "strings ≤ 256 B" covers it).
- Worker deviations 1–3, 5 accepted. Deviation 1 is a **spec erratum on my side**: the brief
  predicted PROTOCOL_VIOLATION for the empty envelope, but proto3 decodes `protocol_version=0`
  and the frozen order checks version first → VERSION_MISMATCH is the only verdict the real
  function can produce. Worker correctly followed the frozen order over the prose.
- Deviation 4 (no Swift outbound coverage) rejected → closed by the round-2 addendum above.
- `extension Resc_V3_FatalCode: Error {}` accepted as a compile-necessity for
  `Result<_, Resc_V3_FatalCode>` (no behavior attached).

## Ladder state

D1 typed dispatch (framing gate + validator/router + shared vectors, both languages) →
**locally verified (state 3)**. Commit at R7. The R2a barrier proof builds on this phase model.
