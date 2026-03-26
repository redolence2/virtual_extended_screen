import Foundation
import CoreGraphics

/// Tracks currently pressed keys and mouse buttons.
/// On disconnect, releases all pressed keys/buttons to prevent stuck input.
final class PressedKeyState {

    private var pressedKeys: Set<CGKeyCode> = []
    private var pressedButtons: Set<CGMouseButton> = []
    private let coordinateMapper: CoordinateMapper

    init(coordinateMapper: CoordinateMapper) {
        self.coordinateMapper = coordinateMapper
    }

    func keyDown(_ keyCode: CGKeyCode) {
        pressedKeys.insert(keyCode)
    }

    func keyUp(_ keyCode: CGKeyCode) {
        pressedKeys.remove(keyCode)
    }

    func buttonDown(_ button: CGMouseButton) {
        pressedButtons.insert(button)
    }

    func buttonUp(_ button: CGMouseButton) {
        pressedButtons.remove(button)
    }

    /// Release all pressed keys and buttons. Called on disconnect or reconnect.
    func releaseAll() {
        let source = CGEventSource(stateID: .hidSystemState)

        // Release all keys
        for keyCode in pressedKeys {
            if let event = CGEvent(keyboardEventSource: source, virtualKey: keyCode, keyDown: false) {
                event.post(tap: .cghidEventTap)
            }
        }

        // Release all mouse buttons
        let currentPos = CGEvent(source: nil)?.location ?? .zero
        for button in pressedButtons {
            let eventType: CGEventType
            switch button {
            case .left: eventType = .leftMouseUp
            case .right: eventType = .rightMouseUp
            default: eventType = .otherMouseUp
            }
            if let event = CGEvent(mouseEventSource: source, mouseType: eventType,
                                   mouseCursorPosition: currentPos, mouseButton: button) {
                event.post(tap: .cghidEventTap)
            }
        }

        let keyCount = pressedKeys.count
        let btnCount = pressedButtons.count
        pressedKeys.removeAll()
        pressedButtons.removeAll()

        if keyCount > 0 || btnCount > 0 {
            print("[RESC] Released \(keyCount) keys + \(btnCount) buttons")
        }
    }
}
