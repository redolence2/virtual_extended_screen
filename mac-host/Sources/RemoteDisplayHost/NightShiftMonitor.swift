import Foundation

/// Monitors macOS Night Shift state via CBBlueLightClient (CoreBrightness private framework).
/// Polls every 2 seconds and calls the onChange callback when strength changes.
final class NightShiftMonitor {

    private var timer: DispatchSourceTimer?
    private var lastStrength: Float = -1
    private var blueLightClient: AnyObject?
    private var getStatusSel: Selector?

    /// Called when Night Shift strength changes. Strength is 0.0 (off) to 1.0 (max warm).
    var onChange: ((Float) -> Void)?

    init() {
        // Load CoreBrightness framework at runtime
        if let bundle = Bundle(path: "/System/Library/PrivateFrameworks/CoreBrightness.framework") {
            bundle.load()
        }
        // Create CBBlueLightClient instance
        if let cls = NSClassFromString("CBBlueLightClient") as? NSObject.Type {
            blueLightClient = cls.init()
            getStatusSel = NSSelectorFromString("getBlueLightStatus:")
        }
    }

    func start() {
        guard blueLightClient != nil else {
            print("[RESC] Night Shift monitoring unavailable (CBBlueLightClient not found)")
            return
        }

        let timer = DispatchSource.makeTimerSource(queue: .global(qos: .utility))
        timer.schedule(deadline: .now(), repeating: 2.0)
        timer.setEventHandler { [weak self] in
            self?.poll()
        }
        timer.resume()
        self.timer = timer
        print("[RESC] Night Shift monitor started (polling every 2s)")
    }

    func stop() {
        timer?.cancel()
        timer = nil
    }

    /// Read Night Shift state. Returns strength 0.0-1.0, or 0 if off/unavailable.
    func currentStrength() -> Float {
        guard let client = blueLightClient else { return 0 }

        // CBBlueLightClient.getBlueLightStatus: fills a struct with enabled + strength
        // The struct layout (from reverse engineering):
        //   offset 0: bool enabled
        //   offset 4: float strength (0.0 to 1.0)
        //   ... other fields
        var statusData = [UInt8](repeating: 0, count: 512)

        // Call [client getBlueLightStatus:&statusData]
        let method = unsafeBitCast(
            (client as! NSObject).method(for: NSSelectorFromString("getBlueLightStatus:")),
            to: (@convention(c) (AnyObject, Selector, UnsafeMutableRawPointer) -> Bool).self
        )
        let success = statusData.withUnsafeMutableBufferPointer { buf in
            method(client as! NSObject, NSSelectorFromString("getBlueLightStatus:"), buf.baseAddress!)
        }

        if success {
            // Parse: enabled at offset 0 (as i32), strength at offset 8 (as Float)
            let enabled = statusData.withUnsafeBufferPointer { buf in
                buf.baseAddress!.withMemoryRebound(to: Int32.self, capacity: 1) { $0.pointee }
            }
            if enabled != 0 {
                let strength: Float = statusData.withUnsafeBufferPointer { buf in
                    (buf.baseAddress! + 8).withMemoryRebound(to: Float.self, capacity: 1) { $0.pointee }
                }
                return max(0, min(1, strength))
            }
        }

        return 0
    }

    private func poll() {
        let strength = currentStrength()
        if strength != lastStrength {
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
