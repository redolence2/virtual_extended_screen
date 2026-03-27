import Foundation
import Network

/// TCP control channel with length-prefixed protobuf framing.
/// Framing: u32_le length + protobuf Envelope bytes.
/// TLS is added in Phase 7 (pairing); Phase 3 uses plaintext TCP.
final class ControlChannel {

    typealias MessageHandler = (Data) -> Void

    // MARK: - Properties

    private var listener: NWListener?
    private var connection: NWConnection?
    private let queue = DispatchQueue(label: "com.resc.control", qos: .userInitiated)
    private var onMessage: MessageHandler?
    private var onClientConnected: ((String) -> Void)?
    private let port: UInt16

    // MARK: - Init

    init(port: UInt16) {
        self.port = port
    }

    // MARK: - Server (Host side)

    /// Start listening for a client connection.
    func startServer(onMessage: @escaping MessageHandler,
                     onClientConnected: @escaping (String) -> Void) throws {
        self.onMessage = onMessage
        self.onClientConnected = onClientConnected

        let params = NWParameters.tcp
        params.serviceClass = .responsiveData
        // Allow immediate rebind after previous process dies (skip TIME_WAIT)
        params.allowLocalEndpointReuse = true

        let listener = try NWListener(using: params, on: NWEndpoint.Port(rawValue: port)!)
        listener.stateUpdateHandler = { state in
            switch state {
            case .ready:
                print("[RESC] Control channel listening on port \(self.port)")
            case .failed(let err):
                print("[RESC] Control listener failed: \(err)")
            default:
                break
            }
        }
        listener.newConnectionHandler = { [weak self] conn in
            self?.handleNewConnection(conn)
        }
        listener.start(queue: queue)
        self.listener = listener
    }

    private func handleNewConnection(_ conn: NWConnection) {
        // Accept only one client at a time
        if let existing = connection {
            existing.cancel()
        }
        connection = conn
        conn.stateUpdateHandler = { [weak self] state in
            switch state {
            case .ready:
                let endpoint = conn.endpoint.debugDescription
                print("[RESC] Control client connected: \(endpoint)")
                self?.onClientConnected?(endpoint)
                self?.startReceiving()
            case .failed(let err):
                print("[RESC] Control connection failed: \(err)")
                self?.connection = nil
            case .cancelled:
                print("[RESC] Control connection closed")
                self?.connection = nil
            default:
                break
            }
        }
        conn.start(queue: queue)
    }

    // MARK: - Send

    /// Send a protobuf-encoded message with u32_le length prefix.
    func send(data: Data) {
        guard let connection = connection else { return }

        // Frame: u32_le length + payload
        var frame = Data(capacity: 4 + data.count)
        var length = UInt32(data.count).littleEndian
        frame.append(Data(bytes: &length, count: 4))
        frame.append(data)

        connection.send(content: frame, completion: .contentProcessed { error in
            if let error = error {
                print("[RESC] Control send error: \(error)")
            }
        })
    }

    // MARK: - Receive

    private func startReceiving() {
        readLengthPrefix()
    }

    private func readLengthPrefix() {
        guard let connection = connection else { return }

        connection.receive(minimumIncompleteLength: 4, maximumLength: 4) { [weak self] data, _, isComplete, error in
            guard let self = self else { return }

            if let error = error {
                print("[RESC] Control recv error: \(error)")
                return
            }

            guard let data = data, data.count == 4 else {
                if isComplete {
                    print("[RESC] Control connection closed by peer")
                }
                return
            }

            let length = data.withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            guard length > 0, length < 1_000_000 else {
                print("[RESC] Control: invalid message length \(length)")
                return
            }

            self.readPayload(length: Int(length))
        }
    }

    private func readPayload(length: Int) {
        guard let connection = connection else { return }

        connection.receive(minimumIncompleteLength: length, maximumLength: length) { [weak self] data, _, isComplete, error in
            guard let self = self else { return }

            if let error = error {
                print("[RESC] Control recv error: \(error)")
                return
            }

            if let data = data, data.count == length {
                self.onMessage?(data)
            }

            // Continue reading
            if !isComplete {
                self.readLengthPrefix()
            }
        }
    }

    // MARK: - Stop

    func stop() {
        connection?.cancel()
        connection = nil
        listener?.cancel()
        listener = nil
    }

    var isConnected: Bool {
        connection != nil
    }
}
