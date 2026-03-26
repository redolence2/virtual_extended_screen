import Foundation
import CoreGraphics

/// Maps StreamSpace pixel coordinates to global macOS coordinates.
/// Queries CGDisplayBounds live at injection time (never cached).
final class CoordinateMapper {

    let displayID: CGDirectDisplayID
    let streamWidth: Int
    let streamHeight: Int

    init(displayID: CGDirectDisplayID, streamWidth: Int, streamHeight: Int) {
        self.displayID = displayID
        self.streamWidth = streamWidth
        self.streamHeight = streamHeight
    }

    /// Convert StreamSpace (x_px, y_px) → global macOS coordinates.
    func toGlobal(x: Int32, y: Int32) -> CGPoint {
        let bounds = CGDisplayBounds(displayID)
        let globalX = bounds.origin.x + (Double(x) / Double(streamWidth)) * bounds.width
        let globalY = bounds.origin.y + (Double(y) / Double(streamHeight)) * bounds.height
        return CGPoint(x: globalX, y: globalY)
    }

    /// Clamp coordinates to StreamSpace bounds.
    func clamp(x: Int32, y: Int32) -> (Int32, Int32) {
        let cx = max(0, min(Int32(streamWidth - 1), x))
        let cy = max(0, min(Int32(streamHeight - 1), y))
        return (cx, cy)
    }
}
