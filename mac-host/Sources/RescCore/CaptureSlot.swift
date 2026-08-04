import Foundation
import os.lock

/// Timestamp provenance label for `CapturedFrame.captureTsUs`
/// (A00_REMEDIATION_PLAN.md §4 item 1). The two sources carry different
/// uncertainty and must never be silently mixed — plan §4's "Nil SCK sync
/// clock" rule requires a fallback sample to be labeled and to carry its
/// own (larger, conservative) uncertainty rather than pass as a true
/// SCK-PTS sample.
public enum CaptureTsSource: Equatable {
    /// `captureTsUs` was derived from ScreenCaptureKit's own sample-buffer
    /// presentation timestamp, bridged into the host continuous-monotonic
    /// domain via `RescClockBridge`.
    case sckPts
    /// SCK gave no usable PTS (invalid/zero), or no host-time↔continuous
    /// calibration has ever succeeded — `captureTsUs` falls back to
    /// `RescClockBridge.continuousNowUs()` read at callback time, with a
    /// conservative fixed uncertainty.
    case callbackFallback
}

/// Immutable identity+payload record created exactly ONCE, inside the
/// ScreenCaptureKit callback that produced it (A00_REMEDIATION_PLAN.md §4
/// item 1; remediation item R2b). `Pixels` is generic so the generation/
/// store/take state machine below (`GenerationalFrameSlot`) is testable as
/// `GenerationalFrameSlot<Int>` in FixtureCheck — no `CVPixelBuffer` or
/// ScreenCaptureKit dependency needed. The host wires it up as
/// `CapturedFrame<CVPixelBuffer>` (DisplayCapturer.swift).
public struct CapturedFrame<Pixels> {
    /// The captured pixel payload (a `CVPixelBuffer` at the host).
    public let pixels: Pixels
    /// The capture run this frame belongs to — the token returned by the
    /// `GenerationalFrameSlot.beginGeneration()` call that started this
    /// run. Bound once, at callback creation; never reassigned.
    public let generation: UInt64
    /// Per-run capture sequence. Starts at 1 for the first frame of a run.
    public let captureSeq: UInt64
    /// Host continuous-monotonic microseconds
    /// (`RescClockBridge.continuousNowUs()` domain) — never wall-clock,
    /// never the raw SCK host-time-domain value.
    public let captureTsUs: UInt64
    /// Where `captureTsUs` came from — see `CaptureTsSource`.
    public let tsSource: CaptureTsSource
    /// Half-width clock uncertainty in microseconds for `captureTsUs`.
    public let uncertaintyUs: UInt64

    public init(pixels: Pixels, generation: UInt64, captureSeq: UInt64, captureTsUs: UInt64,
                tsSource: CaptureTsSource, uncertaintyUs: UInt64) {
        self.pixels = pixels
        self.generation = generation
        self.captureSeq = captureSeq
        self.captureTsUs = captureTsUs
        self.tsSource = tsSource
        self.uncertaintyUs = uncertaintyUs
    }
}

/// Outcome of `GenerationalFrameSlot.store(_:)`.
public enum StoreOutcome: Equatable {
    /// The slot was empty; `frame` is now held.
    case stored
    /// The slot already held an unconsumed same-generation frame; it was
    /// replaced (and counted in `dropCount`) — latest-wins.
    case storedReplacingDropped
    /// `frame.generation` is not the slot's current generation (either an
    /// older, torn-down run, or no run is currently active). The slot was
    /// left untouched and the attempt was counted in `staleRejectCount`.
    /// This is the CONTRACT_ERRATA.md "Implementation proofs required" ›
    /// late-capture-callbacks proof firing: a callback bound to a
    /// torn-down run must not populate a newer run's slot.
    case rejectedStale
}

/// Generation-checked latest-wins slot (A00_REMEDIATION_PLAN.md §4 items
/// 2–3; remediation item R2b). Single producer (the active run's capture
/// callback), single consumer (the encode loop). The pixel payload is
/// never stored separately from its identity — the whole `CapturedFrame` is
/// stored/consumed atomically under one lock, replacing the bare
/// `CVPixelBuffer` that the pre-R2b `LatestFrameSlot` held.
///
/// Every capture run gets its own generation token from `beginGeneration()`.
/// `store(_:)` only accepts a frame whose `generation` equals the slot's
/// CURRENT token — a callback bound to an earlier (torn-down) run's token
/// is rejected instead of overwriting whatever the live run has produced.
/// This is the fix for the live bug in `DisplayCapturer`'s -3805
/// auto-restart path: the restart reuses the same `DisplayCapturer`
/// instance, so a late callback from the old `SCStream` could previously
/// pollute the new run's slot.
public final class GenerationalFrameSlot<Pixels>: @unchecked Sendable {

    private var held: CapturedFrame<Pixels>?
    /// The current run's token, or `nil` when no run is active (before the
    /// first `beginGeneration()`, or after `endGeneration()` and before the
    /// next `beginGeneration()` — the "no-current-token window").
    private var current: UInt64?
    private var generationCounter: UInt64 = 0

    private let lock = OSAllocatedUnfairLock()
    private let semaphore = DispatchSemaphore(value: 0)
    private var _frameCount: UInt64 = 0
    private var _dropCount: UInt64 = 0
    private var _staleRejectCount: UInt64 = 0

    public init() {}

    // MARK: - Generation lifecycle

    /// Starts a new capture run: mints a new generation token, makes it the
    /// current one (atomically invalidating every earlier token — any
    /// callback still in flight from an earlier run carries an older token
    /// and will be rejected by `store(_:)`), and returns the new token for
    /// the caller to bind into every `CapturedFrame` the new run produces.
    ///
    /// If a frame from a previous run is still sitting unconsumed in the
    /// slot (e.g. a restart path that calls `beginGeneration()` again
    /// without an intervening `endGeneration()`), it is discarded here too
    /// — see `endGeneration()`.
    public func beginGeneration() -> UInt64 {
        lock.withLock {
            discardHeldLocked()
            generationCounter += 1
            current = generationCounter
            return generationCounter
        }
    }

    /// Tears down the current run: invalidates its token (a `store(_:)`
    /// bound to it now rejects as `.rejectedStale`, including during the
    /// no-current-token window before the next `beginGeneration()`) and
    /// discards any frame still held — it was never consumed, so it counts
    /// in `dropCount`, not `staleRejectCount`.
    public func endGeneration() {
        lock.withLock {
            discardHeldLocked()
            current = nil
        }
    }

    /// Must be called with `lock` already held.
    private func discardHeldLocked() {
        if held != nil {
            held = nil
            _dropCount += 1
        }
    }

    // MARK: - Store / take

    /// Called by the active run's capture callback. Must return quickly —
    /// no heavy work happens under `lock`.
    ///
    /// Compares `frame.generation` against the slot's CURRENT token only:
    /// equal → store (replacing any held same-generation frame, counted in
    /// `dropCount` — a dropped identity is never reused or relabeled); not
    /// equal → `.rejectedStale`, slot left untouched. Signals the consumer
    /// semaphore on a successful store only, never on a stale rejection.
    @discardableResult
    public func store(_ frame: CapturedFrame<Pixels>) -> StoreOutcome {
        let outcome: StoreOutcome = lock.withLock {
            guard current == frame.generation else {
                _staleRejectCount += 1
                return .rejectedStale
            }
            let wasOccupied = held != nil
            held = frame
            _frameCount += 1
            if wasOccupied {
                _dropCount += 1
                return .storedReplacingDropped
            }
            return .stored
        }
        if outcome != .rejectedStale {
            semaphore.signal()
        }
        return outcome
    }

    /// Called by the encoder thread. Blocks until a frame is available,
    /// then returns and consumes it. May occasionally return `nil` right
    /// after waking (e.g. a generation boundary discarded the very frame
    /// that signaled this wake) — callers already loop on `nil`, exactly
    /// as they did against the pre-R2b `LatestFrameSlot`.
    public func waitAndTake() -> CapturedFrame<Pixels>? {
        semaphore.wait()
        return lock.withLock {
            let f = held
            held = nil
            return f
        }
    }

    /// Non-blocking attempt to take the latest frame.
    public func tryTake() -> CapturedFrame<Pixels>? {
        lock.withLock {
            let f = held
            held = nil
            return f
        }
    }

    // MARK: - Stats

    /// Successful stores only (`.stored` + `.storedReplacingDropped`) —
    /// stale rejections are excluded.
    public var frameCount: UInt64 { lock.withLock { _frameCount } }
    /// Frames discarded without ever being consumed: latest-wins
    /// replacements within a generation, plus whatever a generation
    /// boundary (`beginGeneration`/`endGeneration`) found still held.
    public var dropCount: UInt64 { lock.withLock { _dropCount } }
    /// `store(_:)` attempts bound to a generation that was not current —
    /// the late-capture-callback proof counter.
    public var staleRejectCount: UInt64 { lock.withLock { _staleRejectCount } }
}
