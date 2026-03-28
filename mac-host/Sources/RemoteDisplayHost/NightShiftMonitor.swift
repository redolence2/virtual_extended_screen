import Foundation
import VirtualDisplayBridge

/// Monitors macOS Night Shift by polling CBBlueLightClient via safe ObjC helper.
final class NightShiftMonitor {

    private var timer: DispatchSourceTimer?
    private(set) var lastStrength: Float = 0

    /// Called when Night Shift strength changes. 0.0 = off, up to 1.0 = max warm.
    var onChange: ((Float) -> Void)?

    func start() {
        let timer = DispatchSource.makeTimerSource(queue: .global(qos: .utility))
        // Initial read on the utility queue (safe for CBBlueLightClient)
        lastStrength = RESCGetNightShiftStrength()

        timer.schedule(deadline: .now() + 0.5, repeating: 2.0)
        timer.setEventHandler { [weak self] in
            self?.poll()
        }
        timer.resume()
        self.timer = timer
        print("[RESC] Night Shift monitor started")
    }

    func stop() {
        timer?.cancel()
        timer = nil
    }

    private func poll() {
        let strength = RESCGetNightShiftStrength()
        if abs(strength - lastStrength) > 0.01 || lastStrength < 0 {
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
