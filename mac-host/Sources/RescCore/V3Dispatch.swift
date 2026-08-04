import RescProto

/// `Result`'s `Failure` type must conform to `Swift.Error`; the generated
/// `Resc_V3_FatalCode` (a plain `SwiftProtobuf.Enum`) does not by default.
/// This is the minimal conformance needed for `Result<_, Resc_V3_FatalCode>`
/// below to typecheck as specified — no behavior attached.
extension Resc_V3_FatalCode: Error {}

/// RESC protocol v3 two-layer inbound dispatch (remediation item R5; C1
/// extends this with `DispatchFacts` — corrective-cycle item closing review
/// finding F2).
///
/// Normative sources: `docs/WIRE.md` §1 "Control framing" (length-prefix
/// gate, per-field caps, oneof semantics, direction/state table);
/// `IMPLEMENTATION_PLAN_V11.md` §4 (nonzero run ids; `FatalReport` needs a
/// known candidate run and a known code; `FrameAck` names the oldest
/// outstanding ordinal; `ClockPing`/`ClockPong` are trace/doctor-only;
/// rejected `ProfileResult` needs a known deterministic `rejectCode`);
/// `CONTRACT_ERRATA.md` ERR-01 (cross-TCP activation barrier — the reason
/// host-bound input/heartbeats require `.active` specifically, not merely
/// "post-video-Ack"). `DispatchFacts` and the checks below implement these
/// EXISTING rules; they are not a contract change.
///
/// Two independent, pure layers:
///
/// - Layer 1 (`frameBodyLen`): the 4-byte length-prefix gate. No I/O, no
///   allocation — a pure function of 4 bytes. The caller allocates a body
///   buffer only after this returns `.success`.
/// - Layer 2 (`validateInbound`, `noteOutbound`, `noteVideoAck`): the typed
///   phase/direction validator and router, gated by `DispatchFacts` on both
///   the inbound and outbound paths. No sockets, no logging, no side
///   effects — a pure function of (role, phase, facts, envelope-or-kind).
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
    /// shared JSON fixtures) are noted per case. Remote-fatal dispositions
    /// (`FailureClass`, routed via `Dispatch.remoteFatal`) are lifecycle
    /// outcomes the session actor maps to `.failed`/`.backoff` — they are
    /// deliberately NOT phases here.
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

    /// Nonzero run-identity fact carried by `DispatchFacts.run`
    /// (IMPLEMENTATION_PLAN_V11.md §4 "run ids nonzero"). `.candidate`/
    /// `.active` ids are nonzero by construction — every constructor in
    /// this module and its callers is expected to hold that invariant —
    /// but `validateInbound` and `noteOutbound` re-check it defensively at
    /// the facts<->phase consistency boundary anyway (a zero id inside
    /// `.candidate`/`.active` is treated as inconsistent facts, not
    /// trusted).
    public enum RunFact: Equatable {
        /// No run id known yet — legal only at `(.client, .bootstrap)` (the
        /// host owns its id from process start, so it is never `.noRun`).
        case noRun
        /// A run id learned from an inbound HostProfileAnnounce but not yet
        /// confirmed by a ProfileResult — legal at `.announced`/
        /// `.profileRejected`.
        case candidate(UInt64)
        /// A run id confirmed by an accepted ProfileResult — legal at
        /// `.profileAccepted`/`.videoAckAccepted`/`.active`.
        case active(UInt64)
    }

    /// Diagnostics-mode fact gating `ClockPing`/`ClockPong` (docs/WIRE.md
    /// §1: "trace/doctor mode after profile acceptance").
    public enum DiagMode: Equatable {
        case normal
        case traceOrDoctor
    }

    /// Pure context consumed by BOTH `validateInbound` and `noteOutbound`
    /// (review finding F2: inbound-only enforcement would leave
    /// normal-mode clock sends and unknown-run fatal sends legal). Carries
    /// exactly the facts the direction/phase table cannot express on its
    /// own: which run id is current, whether trace/doctor diagnostics are
    /// active, and the oldest outstanding frame ordinal a FrameAck must
    /// name.
    public struct DispatchFacts: Equatable {
        public let run: RunFact
        public let diagnostics: DiagMode
        public let oldestOutstandingOrdinal: UInt64?

        public init(run: RunFact, diagnostics: DiagMode, oldestOutstandingOrdinal: UInt64?) {
            self.run = run
            self.diagnostics = diagnostics
            self.oldestOutstandingOrdinal = oldestOutstandingOrdinal
        }
    }

    /// Result of a `validateInbound` call that did not return a
    /// `Resc_V3_FatalCode`. A fully-valid FatalReport is `.remoteFatal`
    /// rather than an `.accepted` phase transition — see step 7 in
    /// `validateInbound`'s doc comment. `.failed`/`.backoff` are lifecycle
    /// dispositions the session actor derives from the carried
    /// `FailureClass`; they are not protocol phases.
    public enum Dispatch: Equatable {
        case accepted(next: Phase, learnedCandidate: UInt64?)
        case remoteFatal(FailureClass)
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
    // direction/state table; IMPLEMENTATION_PLAN_V11.md §4;
    // CONTRACT_ERRATA.md ERR-01)

    /// Step 0 (both `validateInbound` and `noteOutbound`): does
    /// `facts.run` match the RunFact variant required for `(role, phase)`,
    /// checked before any other field? Bootstrap is the only phase where a
    /// run id may be unknown, and only at the client.
    private static func factsConsistent(role: Role, phase: Phase, facts: DispatchFacts) -> Bool {
        switch phase {
        case .bootstrap:
            switch role {
            case .client:
                return facts.run == .noRun
            case .host:
                if case .candidate(let id) = facts.run { return id != 0 }
                return false
            }
        case .announced, .profileRejected:
            if case .candidate(let id) = facts.run { return id != 0 }
            return false
        case .profileAccepted, .videoAckAccepted, .active:
            if case .active(let id) = facts.run { return id != 0 }
            return false
        }
    }

    /// Validate one inbound `Envelope` against the receiver's current
    /// `(role, phase)` and `DispatchFacts`, per the fixed 8-step order
    /// below (vectors in `proto/fixtures/dispatch_cases.json` depend on
    /// this exact order):
    ///
    /// 0. Facts<->phase consistency (above) -> `.protocolViolation`.
    /// 1. `protocolVersion != 3` -> `.versionMismatch`.
    /// 2. Run id (IMPLEMENTATION_PLAN_V11.md §4 nonzero rule):
    ///    `env.sessionRunID == 0` -> `.protocolViolation` always; if
    ///    `facts.run` is `.candidate`/`.active`, the envelope's id must
    ///    equal it -> else `.protocolViolation`; if `.noRun`, no comparison
    ///    is made and the id is tentatively the learned candidate
    ///    (surfaced only if the payload reaches step 7 as an accepted
    ///    HostProfileAnnounce — every other payload dies at step 5 in
    ///    `.bootstrap`).
    /// 3. Absent `payload` (unknown-only oneof decodes as absent — the
    ///    generated decoder's unknown-field ignoring and last-one-wins
    ///    oneof resolution are accepted as-is, no raw scanner) ->
    ///    `.protocolViolation`.
    /// 4. Per-field caps -> `.recordCapViolation`.
    /// 5. Direction/phase legality (docs/WIRE.md §1 table, ERR-01
    ///    refinement), amended: `.clockPing`/`.clockPong` additionally
    ///    require `facts.diagnostics == .traceOrDoctor`; inbound
    ///    `.fatalReport` at the client additionally excludes `.bootstrap`
    ///    (WIRE permits FatalReport only once a candidate run id is known)
    ///    -> `.protocolViolation`.
    /// 6. Semantic ranges, amended: `FrameAck.frameOrdinal` must equal
    ///    `facts.oldestOutstandingOrdinal`; a rejected ProfileResult's
    ///    `rejectCode` must classify as `.deterministic`;
    ///    `FatalReport.code` must be a known `FatalCode` ->
    ///    `.protocolViolation`.
    /// 7. Routing: a fully-valid FatalReport returns `.remoteFatal` (phase
    ///    does not advance) instead of `.accepted`.
    ///
    /// The payload the caller needs is already in `env`; this returns only
    /// the accept/reject verdict, never a copy of the payload.
    public static func validateInbound(
        role: Role,
        phase: Phase,
        facts: DispatchFacts,
        env: Resc_V3_Envelope
    ) -> Result<Dispatch, Resc_V3_FatalCode> {
        // 0. facts<->phase consistency, before any envelope field.
        guard factsConsistent(role: role, phase: phase, facts: facts) else { return .failure(.protocolViolation) }

        // 1. protocol version.
        guard env.protocolVersion == 3 else { return .failure(.versionMismatch) }

        // 2. run id.
        guard env.sessionRunID != 0 else { return .failure(.protocolViolation) }
        let learnedCandidate: UInt64?
        switch facts.run {
        case .candidate(let id), .active(let id):
            guard env.sessionRunID == id else { return .failure(.protocolViolation) }
            learnedCandidate = nil
        case .noRun:
            learnedCandidate = env.sessionRunID
        }

        // 3. payload presence.
        guard let payload = env.payload else { return .failure(.protocolViolation) }

        // 4. per-field caps.
        if let capError = checkCaps(payload) { return .failure(capError) }

        // 5. direction/phase legality.
        let transition = (role == .client) ? clientTransition : hostTransition
        guard let next = transition(phase, payload, facts.diagnostics) else { return .failure(.protocolViolation) }

        // 6. semantic ranges.
        if let semanticError = checkSemantic(payload, facts.oldestOutstandingOrdinal) {
            return .failure(semanticError)
        }

        // 7. routing: a fully-valid FatalReport is a remote-fatal
        // disposition, not a phase transition.
        if case .fatalReport(let p) = payload, let cls = classify(Int32(p.code.rawValue)) {
            return .success(.remoteFatal(cls))
        }

        return .success(.accepted(next: next, learnedCandidate: learnedCandidate))
    }

    /// Step 5 for `role == .client` (docs/WIRE.md §1 table).
    /// `hostProfileAnnounce` is legal only as the first bootstrap message;
    /// `fatalReport` is legal once a candidate run id is known (every phase
    /// except `.bootstrap`, where `facts.run` is always `.noRun`);
    /// `.clockPing`/`.clockPong` additionally require `diagnostics ==
    /// .traceOrDoctor`. Every other payload kind is wrong-direction at the
    /// client and never legal inbound.
    private static func clientTransition(
        _ phase: Phase,
        _ payload: Resc_V3_Envelope.OneOf_Payload,
        _ diagnostics: DiagMode
    ) -> Phase? {
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
            guard diagnostics == .traceOrDoctor else { return nil }
            return (phase == .profileAccepted || phase == .videoAckAccepted || phase == .active) ? phase : nil
        case .fatalReport:
            return phase == .bootstrap ? nil : phase
        // profileResult, frameAck, keyEvent, buttonEvent, scrollEvent,
        // releaseInput: client->host only, never legal inbound at client.
        default:
            return nil
        }
    }

    /// Step 5 for `role == .host` (docs/WIRE.md §1 table). `facts.run` is
    /// never `.noRun` at the host (it owns its id from process start), so
    /// `fatalReport` excludes only `.bootstrap`, same as the client.
    /// ERR-01: `keyEvent`/`buttonEvent`/`scrollEvent`/`releaseInput`/
    /// `heartbeat` require `.active` specifically — the host has sent its
    /// activation Heartbeat (which is what moved it to `.active`) before
    /// any input is legal; pre-Ack input is never injected.
    /// `.clockPing`/`.clockPong` additionally require `diagnostics ==
    /// .traceOrDoctor`.
    private static func hostTransition(
        _ phase: Phase,
        _ payload: Resc_V3_Envelope.OneOf_Payload,
        _ diagnostics: DiagMode
    ) -> Phase? {
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
            guard diagnostics == .traceOrDoctor else { return nil }
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
    /// `FrameAck.frameOrdinal` must equal `oldestOutstandingOrdinal`
    /// (IMPLEMENTATION_PLAN_V11.md §4 / docs/WIRE.md §1: FrameAck names the
    /// oldest outstanding ordinal; nothing outstanding is always a
    /// violation); `ProfileResult.rejectCode == .fatalUnspecified` iff
    /// `accepted`, and a rejected result's `rejectCode` must additionally
    /// classify as `.deterministic`; `videoListenerReady == true` iff
    /// `accepted`; `buildCommit` (both messages) exactly 40 lowercase hex
    /// chars; `FatalReport.code` must be a known `FatalCode` (0 or an
    /// unknown numeric is a violation here, before step 7's routing ever
    /// runs `classify` again to pick the `FailureClass`).
    private static func checkSemantic(
        _ payload: Resc_V3_Envelope.OneOf_Payload,
        _ oldestOutstandingOrdinal: UInt64?
    ) -> Resc_V3_FatalCode? {
        switch payload {
        case .buttonEvent(let p):
            if !(p.button == 0 || p.button == 1 || p.button == 2) { return .protocolViolation }
        case .displaySettings(let p):
            if !(p.warmStrength.isFinite && p.warmStrength >= 0.0 && p.warmStrength <= 1.0) {
                return .protocolViolation
            }
        case .frameAck(let p):
            if oldestOutstandingOrdinal != p.frameOrdinal { return .protocolViolation }
        case .profileResult(let p):
            let unspecified = p.rejectCode == .fatalUnspecified
            if unspecified != p.accepted { return .protocolViolation }
            if !p.accepted && classify(Int32(p.rejectCode.rawValue)) != .deterministic {
                return .protocolViolation
            }
            if p.videoListenerReady != p.accepted { return .protocolViolation }
            if !isBuildCommitValid(p.buildCommit) { return .protocolViolation }
        case .hostProfileAnnounce(let p):
            if !isBuildCommitValid(p.buildCommit) { return .protocolViolation }
        case .fatalReport(let p):
            if classify(Int32(p.code.rawValue)) == nil { return .protocolViolation }
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

    /// Advance `phase` for an endpoint about to *send* `kind`, gated by
    /// `DispatchFacts` the same way `validateInbound` is (review finding
    /// F2: inbound-only enforcement would leave normal-mode clock sends
    /// and unknown-run fatal sends legal) — the mirror image of
    /// `validateInbound`'s direction/phase table, from the sender's side.
    /// ERR-01: `.heartbeat` sent by the host while `.videoAckAccepted` is
    /// the activation send (-> `.active`); client input/heartbeats are
    /// legal to send only once the client is already `.active`.
    /// `.clockPing`/`.clockPong` sends require `facts.diagnostics ==
    /// .traceOrDoctor`; a client `.fatalReport` send requires `facts.run !=
    /// .noRun` (client `.bootstrap` can't report — no candidate run yet);
    /// the host is unaffected (it is never `.noRun`).
    public static func noteOutbound(
        role: Role,
        phase: Phase,
        facts: DispatchFacts,
        kind: OutboundKind
    ) -> Result<Phase, Resc_V3_FatalCode> {
        // 0. facts<->phase consistency (same rule as validateInbound).
        guard factsConsistent(role: role, phase: phase, facts: facts) else { return .failure(.protocolViolation) }

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
                if facts.diagnostics != .traceOrDoctor {
                    next = nil
                } else {
                    next = (phase == .profileAccepted || phase == .videoAckAccepted || phase == .active) ? phase : nil
                }
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
                if facts.diagnostics != .traceOrDoctor {
                    next = nil
                } else {
                    next = (phase == .profileAccepted || phase == .videoAckAccepted || phase == .active) ? phase : nil
                }
            case .fatalReport:
                // facts.run != .noRun -- client bootstrap (the only no_run
                // cell) can't report; no candidate to report against yet.
                next = (facts.run != .noRun) ? phase : nil
            case .hostProfileAnnounce, .displaySettings:
                next = nil
            }
        }
        guard let resolved = next else { return .failure(.protocolViolation) }
        return .success(resolved)
    }

    /// Video handshake completion: `.profileAccepted -> .videoAckAccepted`,
    /// the only legal transition (docs/WIRE.md §2/§3; host accepts the Ack
    /// / client sends it). Unchanged by C1 — `noteVideoAck` takes no facts.
    public static func noteVideoAck(_ phase: Phase) -> Result<Phase, Resc_V3_FatalCode> {
        phase == .profileAccepted ? .success(.videoAckAccepted) : .failure(.protocolViolation)
    }
}
