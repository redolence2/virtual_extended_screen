import Foundation
import CoreGraphics

/// Monitors macOS Night Shift by reading the display gamma table.
/// When Night Shift is active, the blue channel gamma is reduced.
/// Polls every 2 seconds and calls onChange when strength changes.
final class NightShiftMonitor {

    private var timer: DispatchSourceTimer?
    private var lastStrength: Float = -1

    /// Called when Night Shift strength changes. 0.0 = off, up to ~1.0 = max warm.
    var onChange: ((Float) -> Void)?

    func start() {
        let timer = DispatchSource.makeTimerSource(queue: .global(qos: .utility))
        timer.schedule(deadline: .now(), repeating: 2.0)
        timer.setEventHandler { [weak self] in
            self?.poll()
        }
        timer.resume()
        self.timer = timer
        print("[RESC] Night Shift monitor started (gamma-based, polling every 2s)")
    }

    func stop() {
        timer?.cancel()
        timer = nil
    }

    /// Detect Night Shift by comparing red vs blue gamma.
    /// Returns 0.0 when off, up to ~0.7 at maximum Night Shift.
    func currentStrength() -> Float {
        var redTable = [CGGammaValue](repeating: 0, count: 256)
        var greenTable = [CGGammaValue](repeating: 0, count: 256)
        var blueTable = [CGGammaValue](repeating: 0, count: 256)
        var sampleCount: UInt32 = 0

        let err = CGGetDisplayTransferByTable(
            CGMainDisplayID(), 256,
            &redTable, &greenTable, &blueTable,
            &sampleCount
        )
        guard err == .success, sampleCount > 0 else { return 0 }

        let idx = Int(sampleCount) - 1
        let redHigh = redTable[idx]
        let blueHigh = blueTable[idx]

        // Night Shift reduces blue relative to red.
        // Normal: red ≈ blue ≈ 1.0. Night Shift: blue < red.
        if redHigh > 0.01 && blueHigh < redHigh - 0.02 {
            let strength = Float(1.0 - Double(blueHigh) / Double(redHigh))
            return max(0, min(1, strength))
        }
        return 0
    }

    private func poll() {
        let strength = currentStrength()
        // Only notify on meaningful change (> 1% difference)
        if abs(strength - lastStrength) > 0.01 {
            lastStrength = strength
            if strength > 0 {
                print("[RESC] Night Shift: ON (strength \(String(format: "%.0f%%", strength * 100)))")
            } else {
                print("[RESC] Night Shift: OFF")
            }
            onChange?(strength)
        }
    }
}
