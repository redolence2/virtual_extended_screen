import Foundation
import ObjectiveC

/// Monitors macOS Night Shift via CBBlueLightClient (CoreBrightness private framework).
/// Uses both notification callback and polling for reliability.
final class NightShiftMonitor {

    private var timer: DispatchSourceTimer?
    private var lastStrength: Float = -1
    private var client: NSObject?

    /// Called when Night Shift strength changes. 0.0 = off, up to 1.0 = max warm.
    var onChange: ((Float) -> Void)?

    init() {
        // Load CoreBrightness framework
        if let bundle = Bundle(path: "/System/Library/PrivateFrameworks/CoreBrightness.framework") {
            bundle.load()
        }
        // Create CBBlueLightClient
        if let cls = NSClassFromString("CBBlueLightClient") as? NSObject.Type {
            client = cls.init()
            print("[RESC] CBBlueLightClient created")
        } else {
            print("[RESC] CBBlueLightClient not available")
        }
    }

    func start() {
        guard client != nil else {
            print("[RESC] Night Shift monitoring unavailable")
            return
        }

        // Set notification callback for real-time updates
        setupNotificationCallback()

        // Also poll every 2s as fallback
        let timer = DispatchSource.makeTimerSource(queue: .global(qos: .utility))
        timer.schedule(deadline: .now() + 1.0, repeating: 2.0)
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

    private func setupNotificationCallback() {
        guard let client = client else { return }
        let sel = NSSelectorFromString("setStatusNotificationBlock:")
        guard client.responds(to: sel) else {
            print("[RESC] CBBlueLightClient doesn't respond to setStatusNotificationBlock:")
            return
        }
        let block: @convention(block) (AnyObject?) -> Void = { [weak self] _ in
            self?.poll()
        }
        client.perform(sel, with: block)
        print("[RESC] Night Shift notification callback registered")
    }

    /// Read current Night Shift strength via getBlueLightStatus:
    func currentStrength() -> Float {
        guard let client = client else { return 0 }
        let sel = NSSelectorFromString("getBlueLightStatus:")
        guard client.responds(to: sel) else { return 0 }

        // Allocate buffer for status struct (oversized for safety)
        var buf = [UInt8](repeating: 0, count: 128)
        let success: Bool = buf.withUnsafeMutableBufferPointer { ptr in
            // Call [client getBlueLightStatus:ptr]
            let imp = unsafeBitCast(
                client.method(for: sel),
                to: (@convention(c) (AnyObject, Selector, UnsafeMutableRawPointer) -> Bool).self
            )
            return imp(client, sel, ptr.baseAddress!)
        }

        guard success else { return 0 }

        // Parse status struct — try multiple known layouts
        // Layout A (macOS 14+): enabled at byte 0 (i32), active at byte 4 (i32), strength at byte 8 (f32)
        // Layout B (macOS 13):  enabled at byte 0 (bool), strength at byte 4 (f32)
        let enabled_i32 = buf.withUnsafeBufferPointer { p in
            p.baseAddress!.withMemoryRebound(to: Int32.self, capacity: 1) { $0.pointee }
        }

        if enabled_i32 == 0 {
            // Check if maybe it's active at offset 4
            let active_i32: Int32 = buf.withUnsafeBufferPointer { p in
                (p.baseAddress! + 4).withMemoryRebound(to: Int32.self, capacity: 1) { $0.pointee }
            }
            if active_i32 == 0 { return 0 }
        }

        // Try reading strength at offsets 4, 8, 12 (check which one is a valid float 0.0-1.0)
        for offset in [4, 8, 12] {
            let val: Float = buf.withUnsafeBufferPointer { p in
                (p.baseAddress! + offset).withMemoryRebound(to: Float.self, capacity: 1) { $0.pointee }
            }
            if val > 0.0 && val <= 1.0 {
                return val
            }
        }

        // Enabled but couldn't find strength — assume moderate
        if enabled_i32 != 0 { return 0.5 }

        return 0
    }

    private func poll() {
        let strength = currentStrength()
        if abs(strength - lastStrength) > 0.01 || (lastStrength < 0) {
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
