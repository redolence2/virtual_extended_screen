import Foundation

/// mDNS service advertisement for host discovery.
/// Advertises `_remotedisplay._tcp.` so the Ubuntu client can auto-discover the Mac host.
final class Discovery: NSObject {

    private var netService: NetService?
    private let controlPort: UInt16
    private let displayName: String

    init(controlPort: UInt16, displayName: String = "Remote Extended Screen") {
        self.controlPort = controlPort
        self.displayName = displayName
        super.init()
    }

    func advertise() {
        let service = NetService(
            domain: ProtocolConstants.mdnsDomain,
            type: ProtocolConstants.mdnsServiceType,
            name: displayName,
            port: Int32(controlPort)
        )
        service.delegate = self
        service.publish()
        self.netService = service
        print("[RESC] mDNS: advertising \(ProtocolConstants.mdnsServiceType) on port \(controlPort)")
    }

    func stop() {
        netService?.stop()
        netService = nil
    }
}

extension Discovery: NetServiceDelegate {
    func netServiceDidPublish(_ sender: NetService) {
        print("[RESC] mDNS: published as '\(sender.name)' on port \(sender.port)")
    }

    func netService(_ sender: NetService, didNotPublish errorDict: [String: NSNumber]) {
        print("[RESC] mDNS: publish failed: \(errorDict)")
    }
}
