import Foundation

/// Session lifecycle state machine.
/// Idle → Discovered → Paired → Negotiating → Streaming → Disconnected
///                                                              ↓
///                                               grace period → reconnect or → Idle
final class SessionStateMachine {

    enum State: String, CustomStringConvertible {
        case idle
        case waitingForClient
        case negotiating
        case streaming
        case disconnected  // in grace period, display still alive
        var description: String { rawValue }
    }

    private(set) var state: State = .idle
    private var gracePeriodTimer: DispatchSourceTimer?
    private let gracePeriodSec: Double
    private var onGracePeriodExpired: (() -> Void)?
    private var onReconnect: (() -> Void)?

    init(gracePeriodSec: Double = 30.0) {
        self.gracePeriodSec = gracePeriodSec
    }

    func transition(to newState: State) {
        let old = state
        state = newState
        print("[RESC] Session: \(old) → \(newState)")

        // Cancel grace period if transitioning out of disconnected
        if old == .disconnected && newState != .disconnected {
            gracePeriodTimer?.cancel()
            gracePeriodTimer = nil
        }
    }

    /// Enter disconnected state with grace period.
    func enterDisconnected(onExpired: @escaping () -> Void, onReconnect: @escaping () -> Void) {
        transition(to: .disconnected)
        self.onGracePeriodExpired = onExpired
        self.onReconnect = onReconnect

        let timer = DispatchSource.makeTimerSource(queue: .main)
        timer.schedule(deadline: .now() + gracePeriodSec)
        timer.setEventHandler { [weak self] in
            guard let self = self, self.state == .disconnected else { return }
            print("[RESC] Grace period expired (\(self.gracePeriodSec)s)")
            self.onGracePeriodExpired?()
            self.transition(to: .idle)
        }
        timer.resume()
        gracePeriodTimer = timer

        print("[RESC] Grace period: \(gracePeriodSec)s (display stays alive)")
    }

    /// Called when client reconnects during grace period.
    func handleReconnect() {
        if state == .disconnected {
            gracePeriodTimer?.cancel()
            gracePeriodTimer = nil
            onReconnect?()
            transition(to: .negotiating)
        }
    }
}
