import Foundation

/// Receives mouse/scroll input over UDP from the Ubuntu client.
/// Parses binary InputEvent packets and forwards to EventInjector.
final class InputReceiver {

    private var fd: Int32 = -1
    private let port: UInt16
    private let injector: EventInjector
    private var thread: Thread?
    private var running = false
    private var packetsReceived: UInt64 = 0

    init(port: UInt16, injector: EventInjector) {
        self.port = port
        self.injector = injector
    }

    func start() {
        fd = socket(AF_INET, SOCK_DGRAM, 0)
        guard fd >= 0 else { print("[RESC] Input UDP socket failed"); return }

        var addr = sockaddr_in()
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = port.bigEndian
        addr.sin_addr.s_addr = INADDR_ANY.bigEndian

        let bindResult = withUnsafePointer(to: &addr) { addrPtr in
            addrPtr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                bind(fd, sa, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard bindResult == 0 else {
            print("[RESC] Input UDP bind failed on port \(port): \(errno)")
            close(fd); fd = -1; return
        }

        // Set 100ms read timeout
        var tv = timeval(tv_sec: 0, tv_usec: 100_000)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))

        running = true
        let t = Thread { [weak self] in self?.recvLoop() }
        t.name = "com.resc.input-recv"
        t.qualityOfService = QualityOfService.userInteractive
        t.start()
        self.thread = t

        print("[RESC] Input receiver listening on UDP port \(port)")
    }

    func stop() {
        running = false
        if fd >= 0 { close(fd); fd = -1 }
        print("[RESC] Input receiver: \(packetsReceived) packets")
    }

    private func recvLoop() {
        var buf = [UInt8](repeating: 0, count: 128)
        var lastSeq: UInt32 = 0

        while running {
            let n = recv(fd, &buf, buf.count, 0)
            guard n > 0 else { continue }
            guard n >= ProtocolConstants.packetPrefixBytes + ProtocolConstants.inputEventBytes else { continue }

            // Validate prefix
            guard buf[0] == 0x52, buf[1] == 0x45, buf[2] == 0x53, buf[3] == 0x43 else { continue } // "RESC"
            guard buf[4] == ProtocolConstants.protocolVersion else { continue }
            guard buf[5] == ProtocolConstants.packetTypeInputEvent else { continue }

            // Parse InputEvent (22 bytes after prefix)
            let off = ProtocolConstants.packetPrefixBytes
            let seq = readU32LE(buf, off)
            let eventType = buf[off + 4]
            let x = readI32LE(buf, off + 5)
            let y = readI32LE(buf, off + 9)
            let button = buf[off + 13]
            let scrollDx = readI16LE(buf, off + 14)
            let scrollDy = readI16LE(buf, off + 16)
            // modifiers at off + 18 (4 bytes) — unused in MVP

            // Latest-seq-wins for mouse moves
            if eventType == 0 { // mouse move
                if seq <= lastSeq && !(lastSeq > 0xFFFF0000 && seq < 0x0000FFFF) {
                    continue // stale move, skip
                }
            }
            lastSeq = seq
            packetsReceived += 1

            // Dispatch to injector
            switch eventType {
            case 0: injector.mouseMove(x: x, y: y)
            case 1: injector.mouseDown(x: x, y: y, button: button)
            case 2: injector.mouseUp(x: x, y: y, button: button)
            case 3: injector.scroll(dx: scrollDx, dy: scrollDy)
            default: break
            }
        }
    }

    // MARK: - Binary helpers
    private func readU32LE(_ buf: [UInt8], _ off: Int) -> UInt32 {
        UInt32(buf[off]) | UInt32(buf[off+1]) << 8 | UInt32(buf[off+2]) << 16 | UInt32(buf[off+3]) << 24
    }
    private func readI32LE(_ buf: [UInt8], _ off: Int) -> Int32 {
        Int32(bitPattern: readU32LE(buf, off))
    }
    private func readI16LE(_ buf: [UInt8], _ off: Int) -> Int16 {
        Int16(bitPattern: UInt16(buf[off]) | UInt16(buf[off+1]) << 8)
    }
}
