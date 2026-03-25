import Foundation
import CoreGraphics
import VirtualDisplayBridge

/// Manages the virtual display lifecycle and display ID rebinding.
/// Wraps CGVirtualDisplayBridge and adds layered display resolution.
final class VirtualDisplayManager {

    // MARK: - Display Handle

    struct DisplayHandle {
        let creationToken: UUID
        var lastKnownDisplayID: CGDirectDisplayID
        let expectedWidth: Int
        let expectedHeight: Int
        let expectedRefreshRate: Double
        let vendorID: UInt32
        let productID: UInt32
        let serialNum: UInt32
        let creationTime: CFAbsoluteTime
    }

    // MARK: - Properties

    private let bridge = CGVirtualDisplayBridge()
    private(set) var handle: DisplayHandle?
    private var reconfigCallback: DisplayReconfigCallback?

    /// Kill switch — set to false to disable virtual display creation.
    var virtualDisplayEnabled: Bool = true

    // MARK: - OS Version Gating

    /// Known-good macOS builds. Unknown builds get a warning but proceed.
    private static let allowedBuilds: Set<String> = [
        // macOS 14.x (Sonoma)
        "23A344", "23B92", "23C71", "23D60", "23E224", "23F79", "23G93",
        // macOS 15.x (Sequoia)
        "24A335", "24B83", "24C101",
        // macOS 26.x (Tahoe)
        "25E241",
    ]

    private static let deniedBuilds: Set<String> = [
        // Add known-broken builds here
    ]

    enum OSGateResult {
        case allowed
        case denied(String)
        case unknown(String)
    }

    static func checkOSVersion() -> OSGateResult {
        let build = CGVirtualDisplayBridge.osBuildVersion()
        if deniedBuilds.contains(build) {
            return .denied(build)
        }
        if allowedBuilds.contains(build) {
            return .allowed
        }
        return .unknown(build)
    }

    // MARK: - Create / Destroy

    func create(width: Int, height: Int, refreshRate: Int = 60) throws -> DisplayHandle {
        guard virtualDisplayEnabled else {
            throw DisplayError.killSwitchActive
        }

        guard CGVirtualDisplayBridge.isAPIAvailable() else {
            throw DisplayError.apiUnavailable
        }

        // Check OS version
        switch Self.checkOSVersion() {
        case .allowed:
            break
        case .denied(let build):
            throw DisplayError.osDenied(build)
        case .unknown(let build):
            print("[RESC] WARNING: Unknown macOS build \(build) — attempting virtual display creation")
        }

        do {
            try bridge.create(withWidth: UInt(width), height: UInt(height), refreshRate: UInt(refreshRate))
        } catch {
            throw DisplayError.creationFailed(error.localizedDescription)
        }

        let displayID = bridge.displayID
        guard displayID != kCGNullDirectDisplay else {
            bridge.destroy()
            throw DisplayError.noDisplayID
        }

        let h = DisplayHandle(
            creationToken: UUID(),
            lastKnownDisplayID: displayID,
            expectedWidth: width,
            expectedHeight: height,
            expectedRefreshRate: Double(refreshRate),
            vendorID: bridge.vendorID,
            productID: bridge.productID,
            serialNum: bridge.serialNumber,
            creationTime: CFAbsoluteTimeGetCurrent()
        )
        self.handle = h

        // Register for display reconfiguration
        registerReconfigCallback()

        logDisplayIdentity(h, context: "creation")
        return h
    }

    func destroy() {
        unregisterReconfigCallback()
        bridge.destroy()
        handle = nil
        print("[RESC] Virtual display destroyed")
    }

    // MARK: - Display ID Resolution (Layered Fallback)

    /// Resolves the current display ID using layered fallback.
    /// Called after sleep/wake or display reconfiguration.
    @discardableResult
    func resolveDisplayID() -> CGDirectDisplayID? {
        guard var h = handle else { return nil }

        // Layer 0: Check if last known ID is still active
        if CGDisplayIsActive(h.lastKnownDisplayID) != 0 {
            return h.lastKnownDisplayID
        }

        print("[RESC] Display ID \(h.lastKnownDisplayID) no longer active — resolving...")

        // Enumerate all online displays
        var displayCount: UInt32 = 0
        CGGetOnlineDisplayList(0, nil, &displayCount)
        var displays = [CGDirectDisplayID](repeating: 0, count: Int(displayCount))
        CGGetOnlineDisplayList(displayCount, &displays, &displayCount)

        // Layer 1: Match by vendor + product + serial
        for did in displays {
            let vendor = CGDisplayVendorNumber(did)
            let model = CGDisplayModelNumber(did)
            let serial = CGDisplaySerialNumber(did)
            if vendor == h.vendorID && model == h.productID && serial == h.serialNum {
                h.lastKnownDisplayID = did
                self.handle = h
                logDisplayIdentity(h, context: "rebind-layer1-vendor-match")
                return did
            }
        }

        // Layer 2: Match by pixel size (common for virtual displays returning 0/generic)
        var candidates: [CGDirectDisplayID] = []
        for did in displays {
            let w = CGDisplayPixelsWide(did)
            let hgt = CGDisplayPixelsHigh(did)
            if w == h.expectedWidth && hgt == h.expectedHeight {
                candidates.append(did)
            }
        }

        if candidates.count == 1 {
            h.lastKnownDisplayID = candidates[0]
            self.handle = h
            logDisplayIdentity(h, context: "rebind-layer2-size-match")
            print("[RESC] WARNING: Display rebinding used heuristic match — verify correct display targeted.")
            return candidates[0]
        }

        // Layer 3: Most recently appearing display matching our size
        // (heuristic: pick the highest display ID, which is often newest)
        if let newest = candidates.max() {
            h.lastKnownDisplayID = newest
            self.handle = h
            logDisplayIdentity(h, context: "rebind-layer3-newest-heuristic")
            print("[RESC] WARNING: Display rebinding used heuristic match — verify correct display targeted.")
            return newest
        }

        // Layer 4: No match found
        print("[RESC] ERROR: Could not resolve virtual display ID. Display may have been destroyed.")
        return nil
    }

    // MARK: - Display Reconfiguration Callback

    private func registerReconfigCallback() {
        let callback = DisplayReconfigCallback { [weak self] display, flags in
            guard let self = self else { return }
            if flags.contains(.addFlag) || flags.contains(.removeFlag) || flags.contains(.movedFlag) {
                print("[RESC] Display reconfiguration detected: display=\(display), flags=\(flags.rawValue)")
                self.resolveDisplayID()
            }
        }
        self.reconfigCallback = callback
        callback.register()
    }

    private func unregisterReconfigCallback() {
        reconfigCallback?.unregister()
        reconfigCallback = nil
    }

    // MARK: - Logging

    private func logDisplayIdentity(_ h: DisplayHandle, context: String) {
        let did = h.lastKnownDisplayID
        let vendor = CGDisplayVendorNumber(did)
        let model = CGDisplayModelNumber(did)
        let serial = CGDisplaySerialNumber(did)
        let w = CGDisplayPixelsWide(did)
        let hgt = CGDisplayPixelsHigh(did)
        print("[RESC] Display identity [\(context)]: displayID=\(did), vendor=\(vendor)/model=\(model)/serial=\(serial), size=\(w)x\(hgt)")
    }

    // MARK: - Errors

    enum DisplayError: Error, CustomStringConvertible {
        case killSwitchActive
        case apiUnavailable
        case osDenied(String)
        case creationFailed(String)
        case noDisplayID

        var description: String {
            switch self {
            case .killSwitchActive: return "Virtual display disabled by kill switch"
            case .apiUnavailable: return "CGVirtualDisplay API not available"
            case .osDenied(let build): return "macOS build \(build) is on deny list"
            case .creationFailed(let msg): return "Virtual display creation failed: \(msg)"
            case .noDisplayID: return "Virtual display created but no display ID assigned"
            }
        }
    }
}

// MARK: - Display Reconfiguration Helper

/// Wraps CGDisplayRegisterReconfigurationCallback in a class-based API.
private final class DisplayReconfigCallback {
    typealias Handler = (CGDirectDisplayID, CGDisplayChangeSummaryFlags) -> Void
    let handler: Handler

    init(handler: @escaping Handler) {
        self.handler = handler
    }

    func register() {
        let ctx = Unmanaged.passRetained(self).toOpaque()
        CGDisplayRegisterReconfigurationCallback({ display, flags, userInfo in
            guard let userInfo = userInfo else { return }
            let cb = Unmanaged<DisplayReconfigCallback>.fromOpaque(userInfo).takeUnretainedValue()
            cb.handler(display, flags)
        }, ctx)
    }

    func unregister() {
        let ctx = Unmanaged.passRetained(self).toOpaque()
        CGDisplayRemoveReconfigurationCallback({ display, flags, userInfo in
            guard let userInfo = userInfo else { return }
            let cb = Unmanaged<DisplayReconfigCallback>.fromOpaque(userInfo).takeUnretainedValue()
            cb.handler(display, flags)
        }, ctx)
        // Balance the passRetained from register
        Unmanaged.passUnretained(self).release()
    }
}
