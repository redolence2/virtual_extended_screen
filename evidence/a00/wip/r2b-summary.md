# R2b evidence summary — locally verified (ladder state 3)

Date: 2026-08-04 · Executor: capture/trace-implementer worker; design + review + independent
re-verification by root reviewer · Base: `ce2d693` + R1 + R5 + R2a.
Scope: `A00_REMEDIATION_PLAN.md` §5 R2b — §4 items 1–3 + the two CONTRACT_ERRATA implementation
proofs (late capture callbacks; cursor `timestamp_us` clock domain).

## Deliverables

- **`RescCore/CaptureSlot.swift`** (214, new): `CapturedFrame<Pixels>` (immutable: pixels ·
  generation tag · per-run captureSeq · captureTsUs in host continuous-monotonic µs ·
  `CaptureTsSource` label · uncertaintyUs) and `GenerationalFrameSlot<Pixels>`
  (begin/endGeneration token lifecycle, store → `.stored | .storedReplacingDropped |
  .rejectedStale`, latest-wins dropCount, staleRejectCount, frameCount excludes rejections,
  semaphore signaled on successful store only; whole-frame atomicity — pixels never separated
  from identity). Generic so FixtureCheck tests it with `Int` payloads — no SCK/CVPixelBuffer
  needed. Replaces the deleted bare-CVPixelBuffer `LatestFrameSlot.swift`.
- **`DisplayCapturer.swift`**: no longer conforms to SCStreamOutput; each `start()` mints a
  generation and creates a per-run `CaptureRunOutput` bound to it at creation. The callback
  builds the full `CapturedFrame`: SCK sample-buffer PTS (already host-clock domain on macOS —
  accepted worker deviation, documented in-code, replacing the sketched `CMSyncConvertTime`)
  bridged via a per-run cached `bracketedHostTimeCalibration()` anchor (refreshed when a fresh
  bracket succeeds), `tsSource = .sckPts`, uncertainty from the calibration; labeled
  callback-time fallback (`continuousNowUs()`, 16,667 µs conservative bound) when PTS is
  invalid/zero or no calibration ever succeeded — never silently mixed. `.rejectedStale` logs
  once per run — live evidence when the late-callback path fires. The -3805 auto-restart path
  is now safe by construction and commented as such. Trace hook keeps its signature but passes
  the frame's converted captureTsUs (full trace repair is R4).
- **`CursorTracker.swift`**: injectable `nowUs` clock defaulting to
  `RescClockBridge.continuousNowUs` (CFAbsoluteTimeGetCurrent removed from the packet path);
  packet built by new pure **`RescCore/CursorPacket.swift`** (51) — the running v1 35-byte
  layout, byte-identical.
- **`main.swift`**: slot type swap + `captured.pixels` in the encode loop; identity fields
  explicitly noted unused until R4.

## Proof checks (FixtureCheck section (i) — 33 new)

- **Late-callback proof** (errata): teardown → new generation → store bound to the old token ⇒
  `.rejectedStale`, staleRejectCount+1, slot untouched; current-generation stores unaffected;
  no-current-token window also rejects; teardown discards a held frame into dropCount.
- **Latest-wins identity**: replacement counted; only the latest identity ever consumed; the
  dropped identity never reappears.
- **Cursor proofs** (errata, as two separate properties per the plan-review correction):
  35-byte round-trip against the prefix constants; strictly-increasing timestamps under an
  injected increasing clock; and under a deliberately NON-monotonic injected clock, seq still
  strictly increments while timestamps reflect the clock as fed — ordering authority is seq,
  never the timestamp.

## Verification (independent re-run by root reviewer)

- Full `swift build`: clean (remote-display-host builds against the new slot).
- `resc-fixture-check`: **539 ok / 0 FAIL / exit 0** (506 + 20 slot + 13 cursor).
- Touched files exactly the seven specified (git status verified).
- Process note: the worker's final report was lost to an awkward background-build stop; the
  review proceeded from the tree itself (all deliverables present and green).

## Ladder state

Late-capture-callback proof + cursor clock migration + §4 items 1–3 → **locally verified
(state 3)**. Runtime capture behavior is exercised live in R3b/R7 (doctor/harness runs).
