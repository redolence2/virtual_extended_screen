# C2 evidence summary — write-level ERR-01 barrier proof (locally verified)

Date: 2026-08-04 · Executor: root reviewer (inline).
Scope: corrective item C2 (`A00_COMPLETION_REPORT_AMENDED_review.md` finding 3, as amended:
deterministic scheduler + writer-attempt spy around the shared outbound gate; named
delayed/reordered cross-TCP schedules; retained attempt traces; no production-network
integration — the V3 cutover stays T1).

## Mechanism

A modeled client control writer (`ClientGate`) whose ONLY write path consults `note_outbound`
first; a spy records every send **attempt** as (kind, written?). The retained attempt traces are
asserted **exactly** (full vectors, not counts), proving the gate — not scheduling luck —
refused every pre-activation write.

## Named schedules (identical in both languages)

- **S1 correct**: input pressure at every stage of ack-note → activation; retained trace shows
  3 refusals, then FrameAck written pre-activation (by design — acks race the activation
  signal), then the first post-barrier input and heartbeat written.
- **S2 control-first (reordered)**: the activation heartbeat arrives BEFORE the video thread's
  ack-note — rejected by the dispatcher (surfaced race); input attempts stay unwritten; only
  after ack-note + re-delivered activation does the first input write.
- **S3 delayed-activation sweep**: activation inserted at every position 0..5 of a fixed
  input-attempt stream; writes == post-activation attempts at every insertion point; the
  refused prefix is exactly the pre-activation attempts.
- **S4 host side**: the activation SEND attempted before the ack-note is refused (no write);
  exactly one activation write after; the first post-barrier inbound input is accepted.

## Artifacts + verification

- Rust: `ubuntu-client/crates/protocol/tests/err01_writer_spy.rs` — 4 tests, all green.
- Swift: FixtureCheck section (h2) — 14 checks; `resc-fixture-check` **624 ok / 0 FAIL**
  (610 post-C1 + 14).

## Ladder state

Write-level barrier proof → locally verified. Commit at checkpoint C (C6).
