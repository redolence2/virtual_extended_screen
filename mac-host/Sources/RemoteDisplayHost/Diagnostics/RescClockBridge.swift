import Foundation
import os.lock

/// Host-side clock bridging for the A0/A0.0 measurement mode
/// (IMPLEMENTATION_PLAN_V11.md §10, §12 A0.0).
///
/// macOS exposes two "monotonic" clocks with different sleep behavior:
///
/// - `mach_absolute_time()` (the "host time" / absolute domain) is what
///   CoreMedia timestamps are expressed in (SCK sample buffer PTS via
///   `CMSyncConvertTime`, VideoToolbox host time). It **halts while the
///   system sleeps**.
/// - `mach_continuous_time()` (the continuous domain) keeps advancing
///   through sleep. Cross-frame latency arithmetic (capture → encode →
///   send) wants this domain, so a sleep/wake cycle straddling two
///   timestamps can never manufacture a negative or wildly inflated
///   latency sample.
///
/// `bracketedHostTimeCalibration()` + `hostTimeToContinuousUs(_:calibration:)`
/// translate one domain into the other via a single anchor sample.
enum RescClockBridge {

    /// An anchor mapping the mach absolute-time domain to the mach
    /// continuous-time domain at one instant: absolute time `absUs`
    /// corresponds to continuous time `contMidUs`, accurate to within
    /// ±`uncertaintyUs` — the half-width of the bracket the anchor was read
    /// under (see `bracketedHostTimeCalibration()`).
    typealias Calibration = (contMidUs: UInt64, absUs: UInt64, uncertaintyUs: UInt64)

    // MARK: - Timebase (cached)

    private static let timebase: mach_timebase_info_data_t = {
        var info = mach_timebase_info_data_t()
        mach_timebase_info(&info)
        return info
    }()

    /// Converts raw mach ticks (from either `mach_absolute_time()` or
    /// `mach_continuous_time()` — both share this timebase) to
    /// microseconds. Splits the multiply to avoid overflowing `UInt64`:
    /// `ticks / denom` stays large but `ticks % denom` is bounded by
    /// `denom`, which is small, so `(ticks % denom) * numer` cannot
    /// overflow.
    private static func ticksToUs(_ ticks: UInt64) -> UInt64 {
        let numer = UInt64(timebase.numer)
        let denom = UInt64(timebase.denom)
        guard denom != 0 else { return ticks / 1000 } // defensive; never 0 in practice
        let ns = (ticks / denom) * numer + (ticks % denom) * numer / denom
        return ns / 1000
    }

    // MARK: - Continuous-domain "now"

    /// Monotonic microseconds since boot, in the continuous-time domain
    /// (keeps advancing through sleep). Use this for all cross-frame
    /// latency arithmetic and trace timestamps.
    static func continuousNowUs() -> UInt64 {
        ticksToUs(mach_continuous_time())
    }

    // MARK: - Absolute ↔ continuous bridging

    private static let calibrationLogLock = OSAllocatedUnfairLock()
    private static var didLogCalibration = false

    /// Brackets one `mach_absolute_time()` read between two
    /// `continuousNowUs()` reads to anchor the absolute (host-time) domain
    /// to the continuous domain — see the type-level doc comment for why
    /// both domains exist. Requires the bracket width `(c2 - c1)` to be
    /// under 50 µs (guards against the thread being preempted between the
    /// three raw reads, which would make the midpoint anchor inaccurate);
    /// retries up to 5 times, returning `nil` — after logging a
    /// `native_call` failure — if no attempt met that bound.
    ///
    /// Logs one `"clock_calibration"` event the first time this succeeds.
    static func bracketedHostTimeCalibration() -> Calibration? {
        for _ in 0..<5 {
            let c1 = continuousNowUs()
            let absUs = ticksToUs(mach_absolute_time())
            let c2 = continuousNowUs()
            let bracketUs = c2 - c1
            guard bracketUs < 50 else { continue }

            let contMidUs = c1 + bracketUs / 2
            let uncertaintyUs = bracketUs / 2
            logCalibrationOnce(contMidUs: contMidUs, absUs: absUs, uncertaintyUs: uncertaintyUs)
            return (contMidUs: contMidUs, absUs: absUs, uncertaintyUs: uncertaintyUs)
        }
        nativeCheck("clock_bridge", "bracketed_host_time_calibration", ok: false,
                    detail: "no bracket under 50us within 5 attempts")
        return nil
    }

    /// Translates a timestamp from the absolute (host-time) domain into the
    /// continuous domain using a previously-captured `calibration` anchor.
    static func hostTimeToContinuousUs(_ hostTimeUs: UInt64, calibration: Calibration) -> UInt64 {
        let deltaUs = Int64(hostTimeUs) - Int64(calibration.absUs)
        let resultUs = Int64(calibration.contMidUs) + deltaUs
        return UInt64(max(resultUs, 0))
    }

    private static func logCalibrationOnce(contMidUs: UInt64, absUs: UInt64, uncertaintyUs: UInt64) {
        let shouldLog = calibrationLogLock.withLock { () -> Bool in
            guard !didLogCalibration else { return false }
            didLogCalibration = true
            return true
        }
        guard shouldLog else { return }
        RescLog.shared.event("clock_calibration", component: "clock_bridge", fields: [
            "cont_mid_us": contMidUs,
            "abs_us": absUs,
            "uncertainty_us": uncertaintyUs,
        ])
    }
}
