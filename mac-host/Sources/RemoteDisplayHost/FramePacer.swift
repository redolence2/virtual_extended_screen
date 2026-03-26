import Foundation
import CoreGraphics
import AppKit

/// Forces the macOS compositor to deliver frames at a steady rate.
/// Creates a tiny 1x1 transparent window on the virtual display that
/// toggles its alpha slightly every frame. This tricks ScreenCaptureKit
/// into delivering continuous frames even on an idle desktop.
final class FramePacer {
    private var window: NSWindow?
    private var timer: DispatchSourceTimer?
    private var toggle = false

    func start(displayID: CGDirectDisplayID, fps: Double = 60.0) {
        let bounds = CGDisplayBounds(displayID)

        // Create a tiny window in the corner of the virtual display
        let rect = NSRect(
            x: bounds.origin.x + bounds.width - 2,
            y: bounds.origin.y + bounds.height - 2,
            width: 1, height: 1
        )

        DispatchQueue.main.async { [weak self] in
            guard let self = self else { return }

            let win = NSWindow(
                contentRect: rect,
                styleMask: .borderless,
                backing: .buffered,
                defer: false
            )
            win.level = .screenSaver
            win.isOpaque = false
            win.backgroundColor = NSColor.clear
            win.ignoresMouseEvents = true
            win.collectionBehavior = [.canJoinAllSpaces, .stationary]
            win.orderFront(nil)

            // Create a tiny view that toggles pixel color
            let view = NSView(frame: NSRect(x: 0, y: 0, width: 1, height: 1))
            view.wantsLayer = true
            view.layer?.backgroundColor = NSColor(white: 0.01, alpha: 0.01).cgColor
            win.contentView = view

            self.window = win

            // Timer to toggle the pixel at target FPS
            let interval = 1.0 / fps
            let timer = DispatchSource.makeTimerSource(queue: .main)
            timer.schedule(deadline: .now(), repeating: interval)
            timer.setEventHandler { [weak self] in
                guard let self = self, let view = self.window?.contentView else { return }
                self.toggle.toggle()
                let alpha: CGFloat = self.toggle ? 0.01 : 0.02
                view.layer?.backgroundColor = NSColor(white: 0.01, alpha: alpha).cgColor
            }
            timer.resume()
            self.timer = timer

            print("[RESC] FramePacer started: forcing \(Int(fps))fps compositor updates on display \(displayID)")
        }
    }

    func stop() {
        timer?.cancel()
        timer = nil
        DispatchQueue.main.async { [weak self] in
            self?.window?.close()
            self?.window = nil
        }
    }
}
