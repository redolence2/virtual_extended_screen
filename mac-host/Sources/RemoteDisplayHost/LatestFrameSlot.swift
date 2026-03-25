import Foundation
import CoreVideo
import os.lock

/// Lock-free single-producer latest-frame slot.
/// Capture callback writes the newest CVPixelBuffer; encoder thread reads it.
/// Old frames are automatically dropped (latest-wins).
final class LatestFrameSlot: @unchecked Sendable {

    private var buffer: CVPixelBuffer?
    private let lock = OSAllocatedUnfairLock()
    private let semaphore = DispatchSemaphore(value: 0)
    private var _frameCount: UInt64 = 0
    private var _dropCount: UInt64 = 0

    /// Called by capture callback. Must return immediately.
    /// Overwrites any unread frame (previous frame is dropped).
    func store(_ pixelBuffer: CVPixelBuffer) {
        lock.withLock {
            let wasOccupied = buffer != nil
            buffer = pixelBuffer
            _frameCount += 1
            if wasOccupied {
                _dropCount += 1
            }
        }
        semaphore.signal()
    }

    /// Called by encoder thread. Blocks until a frame is available.
    /// Returns the latest frame, consuming it from the slot.
    func waitAndTake() -> CVPixelBuffer? {
        semaphore.wait()
        return lock.withLock {
            let pb = buffer
            buffer = nil
            return pb
        }
    }

    /// Non-blocking attempt to take the latest frame.
    func tryTake() -> CVPixelBuffer? {
        lock.withLock {
            let pb = buffer
            buffer = nil
            return pb
        }
    }

    /// Stats for monitoring.
    var frameCount: UInt64 {
        lock.withLock { _frameCount }
    }

    var dropCount: UInt64 {
        lock.withLock { _dropCount }
    }
}
