import RescProto

/// `Result`'s `Failure` type must conform to `Swift.Error`; the generated
/// `Resc_V3_FatalCode` (a plain `SwiftProtobuf.Enum`) does not by default.
/// This is the minimal conformance needed for `Result<_, Resc_V3_FatalCode>`
/// below to typecheck as specified — no behavior attached.
extension Resc_V3_FatalCode: Error {}

/// RESC protocol v3 two-layer inbound dispatch (remediation item R5).
///
/// Normative sources: `docs/WIRE.md` §1 "Control framing" (length-prefix
/// gate, per-field caps, oneof semantics, direction/state table);
/// `CONTRACT_ERRATA.md` ERR-01 (cross-TCP activation barrier — the reason
/// host-bound input/heartbeats require `.active` specifically, not merely
/// "post-video-Ack").
///
/// Two independent, pure layers:
///
/// - Layer 1 (`frameBodyLen`): the 4-byte length-prefix gate. No I/O, no
///   allocation — a pure function of 4 bytes. The caller allocates a body
///   buffer only after this returns `.success`.
/// - Layer 2 (`validateInbound`, `noteOutbound`, `noteVideoAck`): the typed
///   phase/direction validator and router. No sockets, no logging, no side
///   effects — a pure function of (role, phase, envelope).
///
/// This module is INACTIVE: nothing under `Sources/RemoteDisplayHost`
/// wires it up yet. The Rust twin lives at
/// `ubuntu-client/crates/protocol/src/v3dispatch.rs`; both are graded
/// against the same oracle-generated cases in
/// `proto/fixtures/dispatch_cases.json` (see
/// `tools/gen_dispatch_fixtures.py`, which encodes the tables below exactly
/// once as the ground truth).
public enum V3Dispatch {

    /// The receiving endpoint for `validateInbound` — i.e. "who is
    /// validating this inbound envelope", not who sent it.
    public enum Role: Equatable {
        case host
        case client
    }

    /// Session phase shared by both roles (docs/WIRE.md §1;
    /// CONTRACT_ERRATA.md ERR-01 for the `.videoAckAccepted` -> `.active`
    /// activation-barrier step). Canonical vector strings (used by the
    /// shared JSON fixtures) are noted per case.
    public enum Phase: Equatable {
        /// "bootstrap" — control TCP connected, announce not yet exchanged.
        case bootstrap
        /// "announced" — announce sent (host) / received (client).
        case announced
        /// "profile_accepted"
        case profileAccepted
        /// "profile_rejected" — terminal; only FatalReport legal inbound.
        case profileRejected
        /// "video_ack_accepted" — video handshake done.
        case videoAckAccepted
        /// "active" — ERR-01 activation barrier passed.
        case active
    }

    /// Result of a successful `validateInbound` call.
    public struct Accepted: Equatable {
        public let next: Phase
        /// Set only when `expectedRunId` was `nil` and the envelope's id
        /// became the candidate (docs/WIRE.md §1: legal only for the two
        /// client-bootstrap cases where no run id is known yet).
        public let learnedRunId: UInt64?

        public init(next: Phase, learnedRunId: UInt64?) {
            self.next = next
            self.learnedRunId = learnedRunId
        }
    }

    /// Payload kinds an endpoint may *send*, for `noteOutbound`. Mirrors
    /// `Resc_V3_Envelope.OneOf_Payload` but distinguishes the two
    /// `ProfileResult` outcomes, since the sender always knows which one
    /// it is sending (unlike `validateInbound`, which inspects an
    /// already-received envelope).
    public enum OutboundKind: Equatable {
        case hostProfileAnnounce
        case profileResultAccepted
        case profileResultRejected
        case frameAck
        case keyEvent
        case buttonEvent
        case scrollEvent
        case releaseInput
        case heartbeat
        case clockPing
        case clockPong
        case displaySettings
        case fatalReport
    }

    // MARK: - Layer 1 — framing-length gate (docs/WIRE.md §1 "Bounds")

    /// `u32` length-prefix domain accepted before frame allocation: `> 64
    /// KiB` is rejected pre-allocation (docs/WIRE.md §1 "Bounds"). Length 0
    /// is allowed through this gate — an empty body then fails at the
    /// absent-payload check in `validateInbound`, not here.
    private static let maxFrameBodyLen: UInt32 = 65536

    /// Layer 1: gate the 4-byte little-endian length prefix that precedes
    /// every `Envelope` on the wire (docs/WIRE.md §1 "Control framing":
    /// `u32_le` length + protobuf `Envelope`). Pure function of 4 bytes —
    /// no I/O, no allocation; the caller allocates a body buffer of the
    /// returned length only after this returns `.success`. Takes a 4-tuple
    /// (rather than `Data`) so it is impossible to pass a body alongside
    /// the prefix.
    public static func frameBodyLen(_ prefix: (UInt8, UInt8, UInt8, UInt8)) -> Result<Int, Resc_V3_FatalCode> {
        let len = UInt32(prefix.0)
            | (UInt32(prefix.1) << 8)
            | (UInt32(prefix.2) << 16)
            | (UInt32(prefix.3) << 24)
        if len > maxFrameBodyLen {
            return .failure(.malformedFraming)
        }
        return .success(Int(len))
    }

    // MARK: - Layer 2 — typed validator/router (docs/WIRE.md §1
    // direction/state table; CONTRACT_ERRATA.md ERR-01)

    /// Validate one inbound `Envelope` against the receiver's current
    /// `(role, phase)`, per the fixed 6-step order below (vectors in
    /// `proto/fixtures/dispatch_cases.json` depend on this exact order):
    ///
    /// 1. `protocolVersion != 3` -> `.versionMismatch`.
    /// 2. Run id: `expectedRunId` mismatch -> `.protocolViolation`; if
    ///    `expectedRunId` is `nil`, the envelope's id is learned instead.
    /// 3. Absent `payload` (unknown-only oneof decodes as absent — the
    ///    generated decoder's unknown-field ignoring and last-one-wins
    ///    oneof resolution are accepted as-is, no raw scanner) ->
    ///    `.protocolViolation`.
    /// 4. Per-field caps -> `.recordCapViolation`.
    /// 5. Direction/phase legality (docs/WIRE.md §1 table, ERR-01
    ///    refinement) -> `.protocolViolation`.
    /// 6. Semantic ranges -> `.protocolViolation`.
    ///
    /// The payload the caller needs is already in `env`; this returns only
    /// the accept/reject verdict, never a copy of the payload.
    public static func validateInbound(
        role: Role,
        phase: Phase,
        env: Resc_V3_Envelope,
        expectedRunId: UInt64?
    ) -> Result<Accepted, Resc_V3_FatalCode> {
        // 1. protocol version.
        guard env.protocolVersion == 3 else { return .failure(.versionMismatch) }

        // 2. run id.
        var learnedRunId: UInt64?
        if let expected = expectedRunId {
            guard env.sessionRunID == expected else { return .failure(.protocolViolation) }
        } else {
            learnedRunId = env.sessionRunID
        }

        // 3. payload presence.
        guard let payload = env.payload else { return .failure(.protocolViolation) }

        // 4. per-field caps.
        if let capError = checkCaps(payload) { return .failure(capError) }

        // 5. direction/phase legality.
        let transition = (role == .client) ? clientTransition : hostTransition
        guard let next = transition(phase, payload) else { return .failure(.protocolViolation) }

        // 6. semantic ranges.
        if let semanticError = checkSemantic(payload) { return .failure(semanticError) }

        return .success(Accepted(next: next, learnedRunId: learnedRunId))
    }

    /// Step 5 for `role == .client` (docs/WIRE.md §1 table).
    /// `hostProfileAnnounce` is legal only as the first bootstrap message;
    /// `fatalReport` is legal in every phase (the run id becomes known
    /// there via the `expectedRunId == nil` learn path in `.bootstrap`).
    /// Every other payload kind is wrong-direction at the client and never
    /// legal inbound.
    private static func clientTransition(_ phase: Phase, _ payload: Resc_V3_Envelope.OneOf_Payload) -> Phase? {
        switch payload {
        case .hostProfileAnnounce:
            return phase == .bootstrap ? .announced : nil
        case .displaySettings:
            return (phase == .videoAckAccepted || phase == .active) ? phase : nil
        case .heartbeat:
            // ERR-01: receipt of the host's activation Heartbeat is the
            // client's activation signal (.videoAckAccepted -> .active).
            switch phase {
            case .videoAckAccepted: return .active
            case .active: return .active
            default: return nil
            }
        case .clockPing, .clockPong:
            return (phase == .profileAccepted || phase == .videoAckAccepted || phase == .active) ? phase : nil
        case .fatalReport:
            return phase
        // profileResult, frameAck, keyEvent, buttonEvent, scrollEvent,
        // releaseInput: client->host only, never legal inbound at client.
        default:
            return nil
        }
    }

    /// Step 5 for `role == .host` (docs/WIRE.md §1 table). `expectedRunId`
    /// is always non-nil at the host, so `fatalReport` excludes
    /// `.bootstrap` (no client can legitimately know the host's run id
    /// before the announce). ERR-01: `keyEvent`/`buttonEvent`/
    /// `scrollEvent`/`releaseInput`/`heartbeat` require `.active`
    /// specifically — the host has sent its activation Heartbeat (which is
    /// what moved it to `.active`) before any input is legal; pre-Ack
    /// input is never injected.
    private static func hostTransition(_ phase: Phase, _ payload: Resc_V3_Envelope.OneOf_Payload) -> Phase? {
        switch payload {
        case .profileResult(let p):
            guard phase == .announced else { return nil }
            return p.accepted ? .profileAccepted : .profileRejected
        case .frameAck:
            return (phase == .videoAckAccepted || phase == .active) ? phase : nil
        case .keyEvent, .buttonEvent, .scrollEvent, .releaseInput:
            return phase == .active ? .active : nil
        case .heartbeat:
            return phase == .active ? .active : nil
        case .clockPing, .clockPong:
            return (phase == .profileAccepted || phase == .videoAckAccepted || phase == .active) ? phase : nil
        case .fatalReport:
            return phase == .bootstrap ? nil : phase
        // hostProfileAnnounce, displaySettings: host->client only, never
        // legal inbound at host.
        default:
            return nil
        }
    }

    /// Step 4: per-field caps (docs/WIRE.md §1 "Per-field caps"). All
    /// string fields <= 256 B except `FatalReport.summary` <= 2048 B;
    /// `profileCanonical` <= 4096 B; `profileHash` exactly 8 B. Only the
    /// three message kinds below carry variable-length fields.
    private static func checkCaps(_ payload: Resc_V3_Envelope.OneOf_Payload) -> Resc_V3_FatalCode? {
        switch payload {
        case .hostProfileAnnounce(let p):
            if p.buildCommit.utf8.count > 256 { return .recordCapViolation }
            if p.profileCanonical.count > 4096 { return .recordCapViolation }
            if p.profileHash.count != 8 { return .recordCapViolation }
        case .profileResult(let p):
            if p.buildCommit.utf8.count > 256 { return .recordCapViolation }
            if p.profileCanonical.count > 4096 { return .recordCapViolation }
            if p.profileHash.count != 8 { return .recordCapViolation }
        case .fatalReport(let p):
            if p.component.utf8.count > 256 { return .recordCapViolation }
            if p.nativeDomain.utf8.count > 256 { return .recordCapViolation }
            if p.summary.utf8.count > 2048 { return .recordCapViolation }
        default:
            break
        }
        return nil
    }

    /// Step 6: semantic ranges. `ButtonEvent.button` in `{0,1,2}`;
    /// `DisplaySettings.warmStrength` finite and in `[0.0, 1.0]`;
    /// `ProfileResult.rejectCode == .fatalUnspecified` iff `accepted`,
    /// `videoListenerReady == true` iff `accepted`; `buildCommit` (both
    /// messages) exactly 40 lowercase hex chars.
    private static func checkSemantic(_ payload: Resc_V3_Envelope.OneOf_Payload) -> Resc_V3_FatalCode? {
        switch payload {
        case .buttonEvent(let p):
            if !(p.button == 0 || p.button == 1 || p.button == 2) { return .protocolViolation }
        case .displaySettings(let p):
            if !(p.warmStrength.isFinite && p.warmStrength >= 0.0 && p.warmStrength <= 1.0) {
                return .protocolViolation
            }
        case .profileResult(let p):
            let unspecified = p.rejectCode == .fatalUnspecified
            if unspecified != p.accepted { return .protocolViolation }
            if p.videoListenerReady != p.accepted { return .protocolViolation }
            if !isBuildCommitValid(p.buildCommit) { return .protocolViolation }
        case .hostProfileAnnounce(let p):
            if !isBuildCommitValid(p.buildCommit) { return .protocolViolation }
        default:
            break
        }
        return nil
    }

    /// Exactly 40 lowercase hex characters (a full git object id).
    private static func isBuildCommitValid(_ s: String) -> Bool {
        let bytes = Array(s.utf8)
        guard bytes.count == 40 else { return false }
        return bytes.allSatisfy { (0x30...0x39).contains($0) || (0x61...0x66).contains($0) }
    }

    /// Advance `phase` for an endpoint about to *send* `kind` (the mirror
    /// image of `validateInbound`'s direction/phase table, from the
    /// sender's side). ERR-01: `.heartbeat` sent by the host while
    /// `.videoAckAccepted` is the activation send (-> `.active`); client
    /// input/heartbeats are legal to send only once the client is already
    /// `.active`.
    public static func noteOutbound(role: Role, phase: Phase, kind: OutboundKind) -> Result<Phase, Resc_V3_FatalCode> {
        let next: Phase?
        switch role {
        case .host:
            switch kind {
            case .hostProfileAnnounce:
                next = phase == .bootstrap ? .announced : nil
            case .displaySettings:
                next = (phase == .videoAckAccepted || phase == .active) ? phase : nil
            case .heartbeat:
                switch phase {
                case .videoAckAccepted: next = .active
                case .active: next = .active
                default: next = nil
                }
            case .clockPing, .clockPong:
                next = (phase == .profileAccepted || phase == .videoAckAccepted || phase == .active) ? phase : nil
            case .fatalReport:
                next = phase
            case .profileResultAccepted, .profileResultRejected, .frameAck, .keyEvent, .buttonEvent,
                 .scrollEvent, .releaseInput:
                next = nil
            }
        case .client:
            switch kind {
            case .profileResultAccepted:
                next = phase == .announced ? .profileAccepted : nil
            case .profileResultRejected:
                next = phase == .announced ? .profileRejected : nil
            case .frameAck:
                next = (phase == .videoAckAccepted || phase == .active) ? phase : nil
            case .keyEvent, .buttonEvent, .scrollEvent, .releaseInput:
                next = phase == .active ? .active : nil
            case .heartbeat:
                // ERR-01: client heartbeats armed only post-activation.
                next = phase == .active ? .active : nil
            case .clockPing, .clockPong:
                next = (phase == .profileAccepted || phase == .videoAckAccepted || phase == .active) ? phase : nil
            case .fatalReport:
                next = phase == .bootstrap ? nil : phase
            case .hostProfileAnnounce, .displaySettings:
                next = nil
            }
        }
        guard let resolved = next else { return .failure(.protocolViolation) }
        return .success(resolved)
    }

    /// Video handshake completion: `.profileAccepted -> .videoAckAccepted`,
    /// the only legal transition (docs/WIRE.md §2/§3; host accepts the Ack
    /// / client sends it).
    public static func noteVideoAck(_ phase: Phase) -> Result<Phase, Resc_V3_FatalCode> {
        phase == .profileAccepted ? .success(.videoAckAccepted) : .failure(.protocolViolation)
    }
}
