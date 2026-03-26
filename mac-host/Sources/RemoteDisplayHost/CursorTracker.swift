import Foundation
import CoreGraphics

/// Tracks Mac cursor position and shape when over the virtual display.
/// Sends CursorUpdate packets over UDP to the Ubuntu client.
/// Active only in LocalControl mode (host-driven cursor).
final class CursorTracker {

    // MARK: - Cursor Shapes
    enum CursorShape: UInt8 {
        case arrow = 0, ibeam = 1, crosshair = 2, openHand = 3, closedHand = 4
        case pointingHand = 5, resizeN = 6, resizeS = 7, resizeE = 8, resizeW = 9
        case resizeNS = 10, resizeEW = 11, resizeNESW = 12, resizeNWSE = 13
        case notAllowed = 14, wait = 15
    }

    // MARK: - Properties
    private let displayID: CGDirectDisplayID
    private let streamWidth: Int
    private let streamHeight: Int
    private var fd: Int32 = -1
    private var destAddr: sockaddr_in?
    private var timer: DispatchSourceTimer?
    private var seq: UInt32 = 0
    private var lastX: Int32 = -1
    private var lastY: Int32 = -1
    private var lastShape: UInt8 = 0
    private var active = false
    private let queue = DispatchQueue(label: "com.resc.cursor", qos: .userInteractive)

    init(displayID: CGDirectDisplayID, streamWidth: Int, streamHeight: Int) {
        self.displayID = displayID
        self.streamWidth = streamWidth
        self.streamHeight = streamHeight
    }

    // MARK: - Start / Stop

    func start(host: String, port: UInt16) {
        fd = socket(AF_INET, SOCK_DGRAM, 0)
        guard fd >= 0 else { print("[RESC] Cursor UDP socket failed"); return }

        var addr = sockaddr_in()
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = port.bigEndian
        inet_pton(AF_INET, host, &addr.sin_addr)
        destAddr = addr

        // 120Hz timer for cursor position polling
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now(), repeating: .milliseconds(8)) // ~120Hz
        timer.setEventHandler { [weak self] in self?.poll() }
        timer.resume()
        self.timer = timer
        active = true

        print("[RESC] CursorTracker started → \(host):\(port) (120Hz)")
    }

    func stop() {
        active = false
        timer?.cancel()
        timer = nil
        if fd >= 0 { close(fd); fd = -1 }
        print("[RESC] CursorTracker stopped (seq=\(seq))")
    }

    // MARK: - Poll

    private var wasInBounds = false
    private var lastSendTime: CFAbsoluteTime = 0

    private func poll() {
        guard active else { return }

        let mousePos = CGEvent(source: nil)?.location ?? .zero
        let bounds = CGDisplayBounds(displayID)

        if bounds.contains(mousePos) {
            wasInBounds = true
            let localX = Int32((mousePos.x - bounds.origin.x) / bounds.width * Double(streamWidth))
            let localY = Int32((mousePos.y - bounds.origin.y) / bounds.height * Double(streamHeight))
            let x = max(0, min(Int32(streamWidth - 1), localX))
            let y = max(0, min(Int32(streamHeight - 1), localY))

            let posChanged = x != lastX || y != lastY
            let now = CFAbsoluteTimeGetCurrent()
            // Send if position changed OR every 50ms as heartbeat (keeps cursor visible)
            let heartbeat = now - lastSendTime > 0.05

            if posChanged || heartbeat {
                lastX = x
                lastY = y
                lastSendTime = now
                sendUpdate(x: x, y: y, shape: CursorShape.arrow.rawValue)
            }
        } else if wasInBounds {
            wasInBounds = false
            lastX = -1
            lastY = -1
            sendUpdate(x: -1, y: -1, shape: CursorShape.arrow.rawValue)
        }
    }

    // MARK: - Send

    private func sendUpdate(x: Int32, y: Int32, shape: UInt8) {
        guard fd >= 0, var addr = destAddr else { return }

        seq &+= 1
        let timestampUs = UInt64(CFAbsoluteTimeGetCurrent() * 1_000_000)

        // Build packet: PacketPrefix(6) + CursorUpdate(29) = 35 bytes
        var packet = Data(capacity: 35)

        // PacketPrefix
        packet.append(contentsOf: ProtocolConstants.magic)
        packet.append(ProtocolConstants.protocolVersion)
        packet.append(ProtocolConstants.packetTypeCursorUpdate)

        // CursorUpdate (29 bytes, exact field order from spec)
        appendLE(&packet, seq)                    // seq: u32
        appendLE(&packet, timestampUs)            // timestamp_us: u64
        appendLEi32(&packet, x)                   // x_px: i32
        appendLEi32(&packet, y)                   // y_px: i32
        packet.append(shape)                      // shape_id: u8
        appendLE16(&packet, 0)                    // hotspot_x_px: u16 (0 for arrow tip)
        appendLE16(&packet, 0)                    // hotspot_y_px: u16
        appendLEf32(&packet, 1.0)                 // cursor_scale: f32

        // Send
        let _ = packet.withUnsafeBytes { bufPtr in
            withUnsafePointer(to: &addr) { addrPtr in
                addrPtr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                    sendto(fd, bufPtr.baseAddress, packet.count, 0, sa, socklen_t(MemoryLayout<sockaddr_in>.size))
                }
            }
        }
    }

    // MARK: - Helpers
    private func appendLE(_ d: inout Data, _ v: UInt32) { var x = v.littleEndian; d.append(Data(bytes: &x, count: 4)) }
    private func appendLE(_ d: inout Data, _ v: UInt64) { var x = v.littleEndian; d.append(Data(bytes: &x, count: 8)) }
    private func appendLEi32(_ d: inout Data, _ v: Int32) { var x = v.littleEndian; d.append(Data(bytes: &x, count: 4)) }
    private func appendLE16(_ d: inout Data, _ v: UInt16) { var x = v.littleEndian; d.append(Data(bytes: &x, count: 2)) }
    private func appendLEf32(_ d: inout Data, _ v: Float) { var x = v.bitPattern.littleEndian; d.append(Data(bytes: &x, count: 4)) }
}
