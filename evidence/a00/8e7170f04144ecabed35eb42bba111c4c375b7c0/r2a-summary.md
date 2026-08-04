# R2a evidence summary — locally verified (ladder state 3)

Date: 2026-08-04 · Executor: root reviewer (inline) · Base: `ce2d693` + R1 + R5.
Scope: `A00_REMEDIATION_PLAN.md` §5 R2a — the ERR-01 activation-barrier scheduling proof,
expressed against the R5 phase model (plan-review amendment 1: one state machine, no drift).

## What is proven

Section (g)'s vectors grade every single (role, phase, payload) cell; R2a adds the **sequence**
dimension — walking the two TCP handlers' events in explicit schedules:

1. **Correct schedule, step-asserted** (client): ProfileAccepted (input+client-heartbeat+FrameAck
   sends all illegal) → note_video_ack → VideoAckAccepted (input/heartbeat still fully disarmed —
   the barrier window; FrameAck legal by design, racing the activation signal) → inbound
   DisplaySettings (accepted; does NOT arm input) → inbound activation Heartbeat → Active →
   first post-barrier input and client heartbeat accepted; liveness heartbeat keeps Active.
2. **Reordered handlers surfaced, not silently armed** (client): an activation heartbeat (or
   DisplaySettings) arriving while the model still shows ProfileAccepted — i.e. the control
   handler scheduled before the video thread noted the Ack — is PROTOCOL_VIOLATION. An
   implementation that writes the Ack before noting it has a *provable* race.
3. **Prefix sweep** over the legal schedule [A=note_video_ack, D=DisplaySettings, H=activation]:
   after every prefix, input is armed **iff** H was processed.
4. **Host side**: full walk Bootstrap → announce → ProfileResult(accepted) → note_video_ack;
   pre-activation rogue KeyEvent and client Heartbeat are violations, FrameAck racing the
   activation is accepted; the activation *send* moves the host to Active; the first
   post-barrier input, heartbeat, and FrameAck are all accepted. Also: no host heartbeat send
   exists before the video handshake (ProfileAccepted → Err).

## Artifacts

- Rust: `ubuntu-client/crates/protocol/tests/err01_barrier.rs` (4 integration tests).
- Swift: `mac-host/Sources/FixtureCheck/main.swift` section (h) (35 checks, same traces).
- Hygiene fix en route: `v3wire.rs`'s top-level `use crate::resc_v3;` moved into its test module
  (it was test-only, warning as unused on the lib target). One misstep recorded honestly: I first
  deleted the import outright after a short-circuited grep misread — the test build failed,
  the import was restored, then moved correctly. Final state: warning-free, all green.

## Verification

- `cargo test -p protocol`: **66 passed** (lib+vectors) + **4 passed** (err01_barrier), 0 failed,
  no warnings.
- `resc-fixture-check`: **506 ok / 0 FAIL / exit 0** (471 + 35 ERR-01 checks).

## Ladder state

ERR-01 behavioral proof → **locally verified (state 3)**. Commit at R7.
