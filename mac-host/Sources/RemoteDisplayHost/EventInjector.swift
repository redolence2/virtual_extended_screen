import Foundation
import CoreGraphics

/// Injects mouse and keyboard events into macOS from remote input.
/// Uses CGEvent posting with Accessibility permission.
final class EventInjector {

    let coordinateMapper: CoordinateMapper
    let pressedKeyState: PressedKeyState
    private let source: CGEventSource?
    private var lastMoveTime: CFAbsoluteTime = 0
    private let minMoveInterval: CFAbsoluteTime = 1.0 / 240.0 // 240Hz cap
    private var injectedEvents: UInt64 = 0

    // HID Usage → macOS CGKeyCode mapping (US layout, ~120 common keys)
    static let hidToKeyCode: [UInt16: CGKeyCode] = {
        var map: [UInt16: CGKeyCode] = [:]
        // Letters (HID 0x04-0x1D → CGKeyCode)
        let letters: [(UInt16, CGKeyCode)] = [
            (0x04, 0), (0x05, 11), (0x06, 8), (0x07, 2), (0x08, 14), (0x09, 3),
            (0x0A, 5), (0x0B, 4), (0x0C, 34), (0x0D, 38), (0x0E, 40), (0x0F, 37),
            (0x10, 46), (0x11, 45), (0x12, 31), (0x13, 35), (0x14, 12), (0x15, 15),
            (0x16, 1), (0x17, 17), (0x18, 32), (0x19, 9), (0x1A, 13), (0x1B, 7),
            (0x1C, 16), (0x1D, 6),
        ]
        // Numbers (HID 0x1E-0x27)
        let numbers: [(UInt16, CGKeyCode)] = [
            (0x1E, 18), (0x1F, 19), (0x20, 20), (0x21, 21), (0x22, 23),
            (0x23, 22), (0x24, 26), (0x25, 28), (0x26, 25), (0x27, 29),
        ]
        // Special keys
        let special: [(UInt16, CGKeyCode)] = [
            (0x28, 36),  // Return
            (0x29, 53),  // Escape
            (0x2A, 51),  // Backspace
            (0x2B, 48),  // Tab
            (0x2C, 49),  // Space
            (0x2D, 27),  // Minus
            (0x2E, 24),  // Equals
            (0x2F, 33),  // Left Bracket
            (0x30, 30),  // Right Bracket
            (0x31, 42),  // Backslash
            (0x33, 41),  // Semicolon
            (0x34, 39),  // Quote
            (0x35, 50),  // Grave
            (0x36, 43),  // Comma
            (0x37, 47),  // Period
            (0x38, 44),  // Slash
            (0x39, 57),  // Caps Lock
            // F1-F12
            (0x3A, 122), (0x3B, 120), (0x3C, 99), (0x3D, 118),
            (0x3E, 96), (0x3F, 97), (0x40, 98), (0x41, 100),
            (0x42, 101), (0x43, 109), (0x44, 103), (0x45, 111),
            // Navigation
            (0x49, 114), // Insert (Help on Mac)
            (0x4A, 115), // Home
            (0x4B, 116), // Page Up
            (0x4C, 117), // Delete Forward
            (0x4D, 119), // End
            (0x4E, 121), // Page Down
            (0x4F, 124), // Right Arrow
            (0x50, 123), // Left Arrow
            (0x51, 125), // Down Arrow
            (0x52, 126), // Up Arrow
            // Modifiers
            (0xE0, 59),  // Left Control
            (0xE1, 56),  // Left Shift
            (0xE2, 58),  // Left Alt/Option
            (0xE3, 55),  // Left GUI/Command
            (0xE4, 62),  // Right Control
            (0xE5, 60),  // Right Shift
            (0xE6, 61),  // Right Alt/Option
            (0xE7, 54),  // Right GUI/Command
        ]
        for (hid, kc) in letters + numbers + special { map[hid] = kc }
        return map
    }()

    init(coordinateMapper: CoordinateMapper) {
        self.coordinateMapper = coordinateMapper
        self.pressedKeyState = PressedKeyState(coordinateMapper: coordinateMapper)
        self.source = CGEventSource(stateID: .hidSystemState)
    }

    /// Check if Accessibility permission is granted (required for CGEvent injection).
    static func checkAccessibility() -> Bool {
        let trusted = CGPreflightPostEventAccess()
        if !trusted {
            print("[RESC] Accessibility permission NOT granted.")
            print("[RESC]   System Settings → Privacy & Security → Accessibility")
            print("[RESC]   Enable 'remote-display-host' (or Terminal)")
            CGRequestPostEventAccess()
        }
        return trusted
    }

    // MARK: - Mouse Events

    /// Inject mouse move to StreamSpace coordinates.
    func mouseMove(x: Int32, y: Int32) {
        // Rate limit to 240Hz
        let now = CFAbsoluteTimeGetCurrent()
        guard now - lastMoveTime >= minMoveInterval else { return }
        lastMoveTime = now

        let (cx, cy) = coordinateMapper.clamp(x: x, y: y)
        let global = coordinateMapper.toGlobal(x: cx, y: cy)

        if let event = CGEvent(mouseEventSource: source, mouseType: .mouseMoved,
                               mouseCursorPosition: global, mouseButton: .left) {
            event.post(tap: CGEventTapLocation.cghidEventTap)
            injectedEvents += 1
        }
    }

    /// Inject mouse button down.
    func mouseDown(x: Int32, y: Int32, button: UInt8) {
        let (cx, cy) = coordinateMapper.clamp(x: x, y: y)
        let global = coordinateMapper.toGlobal(x: cx, y: cy)
        let (eventType, cgButton) = mouseEventParams(button: button, isDown: true)

        if let event = CGEvent(mouseEventSource: source, mouseType: eventType,
                               mouseCursorPosition: global, mouseButton: cgButton) {
            event.post(tap: CGEventTapLocation.cghidEventTap)
            pressedKeyState.buttonDown(cgButton)
            injectedEvents += 1
        }
    }

    /// Inject mouse button up.
    func mouseUp(x: Int32, y: Int32, button: UInt8) {
        let (cx, cy) = coordinateMapper.clamp(x: x, y: y)
        let global = coordinateMapper.toGlobal(x: cx, y: cy)
        let (eventType, cgButton) = mouseEventParams(button: button, isDown: false)

        if let event = CGEvent(mouseEventSource: source, mouseType: eventType,
                               mouseCursorPosition: global, mouseButton: cgButton) {
            event.post(tap: CGEventTapLocation.cghidEventTap)
            pressedKeyState.buttonUp(cgButton)
            injectedEvents += 1
        }
    }

    /// Inject scroll event.
    func scroll(dx: Int16, dy: Int16) {
        if let event = CGEvent(scrollWheelEvent2Source: source, units: .pixel,
                               wheelCount: 2, wheel1: Int32(dy), wheel2: Int32(dx), wheel3: 0) {
            event.post(tap: CGEventTapLocation.cghidEventTap)
            injectedEvents += 1
        }
    }

    // MARK: - Keyboard Events

    /// Inject key event from HID usage code.
    func keyEvent(hidUsage: UInt16, isDown: Bool) {
        guard let keyCode = Self.hidToKeyCode[hidUsage] else {
            // Unknown HID usage — log but don't inject
            return
        }

        if let event = CGEvent(keyboardEventSource: source, virtualKey: keyCode, keyDown: isDown) {
            event.post(tap: CGEventTapLocation.cghidEventTap)
            if isDown {
                pressedKeyState.keyDown(keyCode)
            } else {
                pressedKeyState.keyUp(keyCode)
            }
            injectedEvents += 1
        }
    }

    var totalInjected: UInt64 { injectedEvents }

    // MARK: - Helpers

    private func mouseEventParams(button: UInt8, isDown: Bool) -> (CGEventType, CGMouseButton) {
        switch button {
        case 0: return (isDown ? .leftMouseDown : .leftMouseUp, .left)
        case 1: return (isDown ? .rightMouseDown : .rightMouseUp, .right)
        default: return (isDown ? .otherMouseDown : .otherMouseUp, CGMouseButton(rawValue: UInt32(button))!)
        }
    }
}
