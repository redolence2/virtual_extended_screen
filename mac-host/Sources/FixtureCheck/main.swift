import Foundation
import RescCore
import RescProto
import SwiftProtobuf

// ===========================================================================
// MARK: - (j) R3a: cross-process lock-contention child branch
//
// MUST run before any other work in this file (checked first, unconditionally,
// per A00_REMEDIATION_PLAN.md §5 R3a). When RESC_LOCK_CONTENTION_CHILD=<path>
// is set, this process is not running the fixture-check suite at all — it is
// the disposable child the "(j) R3a two-process lock contention" check
// further down spawns to prove flock(2) semantics hold across real process
// boundaries. FixtureCheck is an executableTarget (Package.swift) and cannot
// import RemoteDisplayHost's real InstanceLock type — Package.swift is out
// of scope for this change, and RemoteDisplayHost is itself an
// executableTarget (see Package.swift's existing "cannot be depended on" note
// for HarnessSender, which duplicates RescClockBridge.continuousNowUs() for
// the identical reason) — so `fixtureCheckAttemptScratchLock` below
// duplicates InstanceLock.acquire(path:profileId:)'s exact algorithm (open,
// reassert 0600, LOCK_EX|LOCK_NB flock). Exits 20 (INSTANCE_LOCK_HELD,
// matching proto/control_v3.proto's FatalCode) if the lock is already held,
// 0 if acquired. Nothing else in this file runs for this process either way.

/// Duplicates InstanceLock.swift's acquire algorithm against an explicit
/// path. Returns the held fd (left open — caller releases the lock by
/// closing it) or nil if another process already holds it.
func fixtureCheckAttemptScratchLock(path: String) -> Int32? {
    let fd = open(path, O_CREAT | O_RDWR, 0o600)
    guard fd >= 0 else { return nil }
    // Lock hygiene (R3a): reassert 0600 on every open, not only at
    // creation — open()'s mode argument is ignored by the kernel once the
    // file already exists.
    chmod(path, 0o600)
    guard flock(fd, LOCK_EX | LOCK_NB) == 0 else {
        close(fd)
        return nil
    }
    return fd
}

if let contentionPath = ProcessInfo.processInfo.environment["RESC_LOCK_CONTENTION_CHILD"] {
    exit(fixtureCheckAttemptScratchLock(path: contentionPath) != nil ? 0 : 20)
}
// ===========================================================================

// Stage-1 fixture checks: canonicalization (plan v11 §12, CONTRACT_ERRATA.md
// proof 3) plus the six contract-mandated wire test groups (docs/WIRE.md;
// plan v11 §4/§5/§6; CONTRACT_ERRATA.md ERR-04/ERR-05) — golden/malformed
// binary records, protobuf Envelope round-trips, FatalCode classification,
// UDP seq comparators, scroll transform, and RA (NAL) verification. Runs as
// an executable because this machine has CommandLineTools only (no XCTest).
// The Rust twin: `cargo test -p diagnostics` / the v3wire crate's tests.
// Exit 0 = all checks pass; any failure prints the reason and exits 1.

var failures = 0

func check(_ name: String, _ condition: @autoclosure () throws -> Bool) {
    do {
        if try condition() {
            print("ok   \(name)")
        } else {
            print("FAIL \(name)")
            failures += 1
        }
    } catch {
        print("FAIL \(name): \(error)")
        failures += 1
    }
}

// #filePath = <repo>/mac-host/Sources/FixtureCheck/main.swift
let repoRoot = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()  // FixtureCheck
    .deletingLastPathComponent()  // Sources
    .deletingLastPathComponent()  // mac-host
    .deletingLastPathComponent()  // repo root
let fixtureURL = repoRoot.appendingPathComponent("proto/fixtures/profile.canonical.json")
let fixturesDir = repoRoot.appendingPathComponent("proto/fixtures")
let malformedDir = fixturesDir.appendingPathComponent("malformed")
let envelopesDir = fixturesDir.appendingPathComponent("envelopes")

/// Pinned by review 11 and CONTRACT_ERRATA.md proof 3.
let pinnedPrefix = Data([0x0c, 0xc2, 0x24, 0x96, 0x62, 0x88, 0x05, 0x97])

do {
    let bytes = try Data(contentsOf: fixtureURL)

    check("fixture has no trailing newline (errata proof 3)",
          bytes.last != UInt8(ascii: "\n"))

    check("fixture SHA-256 prefix == 0cc2249662880597",
          CanonicalProfile.hash8(bytes) == pinnedPrefix)

    let object = try JSONSerialization.jsonObject(with: bytes)
    check("fixture is already canonical",
          try CanonicalProfile.canonicalize(object) == bytes)

    check("placeholder backend rejected by runtime validation", {
        do {
            _ = try CanonicalProfile.validateRuntimeProfile(bytes)
            return false  // must NOT validate — placeholder backend
        } catch CanonicalProfile.ProfileError.placeholderBackend {
            return true
        } catch {
            return false  // wrong error kind
        }
    }())

    let sample: [String: Any] = ["b": 2, "a": ["d": 4, "c": 3]]
    check("canonicalize sorts and minifies",
          try String(data: CanonicalProfile.canonicalize(sample), encoding: .utf8)
              == #"{"a":{"c":3,"d":4},"b":2}"#)

    // -- ERR-07: ASCII-only keys and string values --
    check("canonical fixture passes ASCII-only rule (ERR-07)",
          CanonicalProfile.firstNonASCII(object) == nil)

    check("firstNonASCII reports a non-ASCII nested value with its path",
          CanonicalProfile.firstNonASCII(["a": ["b": ["é"]]]) == "non-ASCII string value at /a/b[0]")
    check("firstNonASCII reports a non-ASCII key",
          CanonicalProfile.firstNonASCII(["ké": 1]) == "non-ASCII key at /ké")

    // Both encodings of the same non-ASCII profile_id (raw UTF-8 and
    // é-escaped) must be rejected by the ASCII gate specifically —
    // these fixtures carry a REAL backend id, so without ERR-07 nothing
    // else in validateRuntimeProfile is guaranteed to reject them. The
    // Rust twin asserts the identical verdict on the identical bytes.
    for name in ["profile_nonascii_raw.json", "profile_nonascii_escaped.json"] {
        let nonasciiBytes = try Data(contentsOf: fixturesDir.appendingPathComponent(name))
        check("\(name) rejected by ASCII gate (ERR-07), violation at profile_id", {
            do {
                _ = try CanonicalProfile.validateRuntimeProfile(nonasciiBytes)
                return false  // must not validate
            } catch CanonicalProfile.ProfileError.asciiViolation(let detail) {
                return detail.contains("profile_id")
            } catch {
                return false  // wrong error kind — the ASCII gate must fire first
            }
        }())
    }
} catch {
    print("FAIL reading fixture at \(fixtureURL.path): \(error)")
    failures += 1
}

// ===========================================================================
// Contract-mandated wire test groups (docs/WIRE.md; IMPLEMENTATION_PLAN_V11.md
// §4/§5/§6; CONTRACT_ERRATA.md ERR-04/ERR-05). Six groups: (a) fixture sweep,
// (b) envelope round-trips, (c) FatalCode classification, (d) comparators,
// (e) scroll transform, (f) RA verification. The Rust twin lives under
// ubuntu-client/crates/protocol/src/v3wire.
// ===========================================================================

// MARK: - Helpers

struct FixtureCheckError: Error, CustomStringConvertible {
    let message: String
    var description: String { message }
}

/// True iff `body` throws exactly `WireError.protocolViolation`.
func throwsProtocolViolation<T>(_ body: () throws -> T) -> Bool {
    do {
        _ = try body()
        return false
    } catch WireError.protocolViolation {
        return true
    } catch {
        return false
    }
}

/// True iff `body` throws exactly `WireError.recordCapViolation` with the
/// given `total`/`cap`.
func throwsRecordCapViolation<T>(total: UInt64, cap: UInt64, _ body: () throws -> T) -> Bool {
    do {
        _ = try body()
        return false
    } catch WireError.recordCapViolation(let t, let c) {
        return t == total && c == cap
    } catch {
        return false
    }
}

/// Compares `expected[key]` (a JSON integer) against a `UInt32` wire field.
func checkU32(_ name: String, _ expected: [String: Any], _ key: String, _ actual: UInt32) {
    guard let e = expected[key] as? Int else {
        check(name, false)
        return
    }
    check(name, UInt32(e) == actual)
}

/// Compares `expected[key]` (a JSON integer) against a `UInt64` wire field.
func checkU64(_ name: String, _ expected: [String: Any], _ key: String, _ actual: UInt64) {
    guard let e = expected[key] as? Int else {
        check(name, false)
        return
    }
    check(name, UInt64(e) == actual)
}

// MARK: - (a) Fixture sweep — golden + malformed wire records

// Cap value 2097184 = the docs/WIRE.md §9 placeholder profile's
// max_record_bytes. The fixtures themselves are profile-independent
// (proto/fixtures/README.md) but this is the pinned cap they're checked
// against here.
let maxRecordBytesFixture: UInt64 = 2_097_184

do {
    let expectedRunId: UInt64 = 0x1122334455667788

    // -- golden fixtures (proto/fixtures/README.md "Valid fixtures") --
    let hello = try parseVideoHello(try Data(contentsOf: fixturesDir.appendingPathComponent("videohello.bin")))
    check("videohello.bin: sessionRunId == 0x1122334455667788", hello.sessionRunId == expectedRunId)
    check("videohello.bin: profileHash == 0cc2249662880597", hello.profileHash == pinnedPrefix)

    let ackOk = try parseVideoHelloAck(try Data(contentsOf: fixturesDir.appendingPathComponent("videohelloack_ok.bin")))
    check("videohelloack_ok.bin: status == ok", ackOk.status == .ok)

    let ackMismatch = try parseVideoHelloAck(try Data(contentsOf: fixturesDir.appendingPathComponent("videohelloack_mismatch.bin")))
    check("videohelloack_mismatch.bin: status == mismatch", ackMismatch.status == .mismatch)

    let ackBusy = try parseVideoHelloAck(try Data(contentsOf: fixturesDir.appendingPathComponent("videohelloack_busy.bin")))
    check("videohelloack_busy.bin: status == busy", ackBusy.status == .busy)

    let ackInternal = try parseVideoHelloAck(try Data(contentsOf: fixturesDir.appendingPathComponent("videohelloack_internal.bin")))
    check("videohelloack_internal.bin: status == internal", ackInternal.status == .`internal`)

    let frame = try parseFrameHeader(try Data(contentsOf: fixturesDir.appendingPathComponent("frame_header_min.bin")),
                                      maxRecordBytes: maxRecordBytesFixture)
    check("frame_header_min.bin: flags == 1", frame.flags == 1)
    check("frame_header_min.bin: frameOrdinal == 1", frame.frameOrdinal == 1)
    check("frame_header_min.bin: payloadLen == 0", frame.payloadLen == 0)

    let move = try parseMove(try Data(contentsOf: fixturesDir.appendingPathComponent("move.bin")))
    check("move.bin: seq == 1", move.seq == 1)
    check("move.bin: x == 100", move.x == 100)
    check("move.bin: y == 200", move.y == 200)

    let cursor = try parseCursor(try Data(contentsOf: fixturesDir.appendingPathComponent("cursor.bin")))
    check("cursor.bin: seq == 1", cursor.seq == 1)
    check("cursor.bin: x == 100", cursor.xPx == 100)
    check("cursor.bin: y == 200", cursor.yPx == 200)
    check("cursor.bin: shape == 0", cursor.shapeId == 0)
    check("cursor.bin: scale == 1.0", cursor.cursorScale == 1.0)

    // -- malformed fixtures (proto/fixtures/README.md "Malformed fixtures") --
    check("malformed/bad_magic_hello.bin -> protocolViolation", throwsProtocolViolation {
        try parseVideoHello(try Data(contentsOf: malformedDir.appendingPathComponent("bad_magic_hello.bin")))
    })
    check("malformed/bad_length_hello.bin -> protocolViolation", throwsProtocolViolation {
        try parseVideoHello(try Data(contentsOf: malformedDir.appendingPathComponent("bad_length_hello.bin")))
    })
    check("malformed/nonzero_reserved_hello.bin -> protocolViolation", throwsProtocolViolation {
        try parseVideoHello(try Data(contentsOf: malformedDir.appendingPathComponent("nonzero_reserved_hello.bin")))
    })
    check("malformed/unknown_status_ack.bin -> protocolViolation", throwsProtocolViolation {
        try parseVideoHelloAck(try Data(contentsOf: malformedDir.appendingPathComponent("unknown_status_ack.bin")))
    })
    check("malformed/unknown_flag_frame.bin -> protocolViolation", throwsProtocolViolation {
        try parseFrameHeader(try Data(contentsOf: malformedDir.appendingPathComponent("unknown_flag_frame.bin")),
                              maxRecordBytes: maxRecordBytesFixture)
    })
    check("malformed/overflow_frame.bin -> recordCapViolation(total: 4294967327, cap: 2097184)",
          throwsRecordCapViolation(total: 4_294_967_327, cap: maxRecordBytesFixture) {
        try parseFrameHeader(try Data(contentsOf: malformedDir.appendingPathComponent("overflow_frame.bin")),
                              maxRecordBytes: maxRecordBytesFixture)
    })
    check("malformed/short_move.bin -> protocolViolation", throwsProtocolViolation {
        try parseMove(try Data(contentsOf: malformedDir.appendingPathComponent("short_move.bin")))
    })
    check("malformed/long_cursor.bin -> protocolViolation", throwsProtocolViolation {
        try parseCursor(try Data(contentsOf: malformedDir.appendingPathComponent("long_cursor.bin")))
    })
} catch {
    print("FAIL fixture sweep setup: \(error)")
    failures += 1
}

// MARK: - (b) Envelope round-trips (proto/fixtures/envelopes/*.bin)

do {
    let manifestBytes = try Data(contentsOf: envelopesDir.appendingPathComponent("envelopes_manifest.json"))
    guard let manifest = try JSONSerialization.jsonObject(with: manifestBytes) as? [String: Any],
          let files = manifest["files"] as? [String: Any] else {
        throw FixtureCheckError(message: "envelopes_manifest.json: unexpected structure")
    }

    let expectedRunId: UInt64 = 72_623_859_790_382_856
    let expectedVersion: UInt32 = 3

    for (filename, expectedAny) in files.sorted(by: { $0.key < $1.key }) {
        guard let expected = expectedAny as? [String: Any] else {
            check("envelopes_manifest.json entry for \(filename) is an object", false)
            continue
        }
        let raw = try Data(contentsOf: envelopesDir.appendingPathComponent(filename))
        let envelope = try Resc_V3_Envelope(serializedBytes: raw)

        check("\(filename): sessionRunID == 72623859790382856", envelope.sessionRunID == expectedRunId)
        check("\(filename): protocolVersion == 3", envelope.protocolVersion == expectedVersion)

        let payloadName = expected["payload"] as? String
        switch envelope.payload {
        case .clockPing(let v):
            check("\(filename): payload case == clock_ping", payloadName == "clock_ping")
            checkU32("\(filename): clockPing.seq", expected, "seq", v.seq)
            checkU64("\(filename): clockPing.t1MonoUs", expected, "t1_mono_us", v.t1MonoUs)
        case .frameAck(let v):
            check("\(filename): payload case == frame_ack", payloadName == "frame_ack")
            checkU64("\(filename): frameAck.frameOrdinal", expected, "frame_ordinal", v.frameOrdinal)
        case .heartbeat(let v):
            check("\(filename): payload case == heartbeat", payloadName == "heartbeat")
            checkU64("\(filename): heartbeat.tMonoUs", expected, "t_mono_us", v.tMonoUs)
        case .fatalReport(let v):
            check("\(filename): payload case == fatal_report", payloadName == "fatal_report")
            check("\(filename): fatalReport.code", (expected["code"] as? Int) == v.code.rawValue)
            check("\(filename): fatalReport.component", (expected["component"] as? String) == v.component)
            check("\(filename): fatalReport.summary", (expected["summary"] as? String) == v.summary)
        default:
            check("\(filename): payload decoded to a recognized case", false)
        }

        // Re-encode and assert byte-equality. tools/gen_envelope_fixtures.py
        // and SwiftProtobuf's binary encoder both emit fields in ascending
        // field-number order for these message shapes, so this is expected
        // to hold exactly (see that script's docstring); field-equality
        // above already covers correctness independent of this assertion.
        let reencoded = try envelope.serializedData()
        check("\(filename): re-encode byte-equality", reencoded == raw)
    }
} catch {
    print("FAIL envelope round-trip setup: \(error)")
    failures += 1
}

// MARK: - (c) FatalCode classification (fatal_code_classes.json)

do {
    let bytes = try Data(contentsOf: fixturesDir.appendingPathComponent("fatal_code_classes.json"))
    guard let obj = try JSONSerialization.jsonObject(with: bytes) as? [String: Any],
          let classes = obj["classes"] as? [String: String] else {
        throw FixtureCheckError(message: "fatal_code_classes.json: unexpected structure")
    }

    for fatalCase in Resc_V3_FatalCode.allCases {
        let key = String(fatalCase.rawValue)
        guard let jsonName = classes[key] else {
            check("fatal_code_classes.json has entry for \(fatalCase) (\(fatalCase.rawValue))", false)
            continue
        }
        let expected: FailureClass?
        switch jsonName {
        case "deterministic": expected = .deterministic
        case "transient": expected = .transient
        case "terminal": expected = .terminal
        default: expected = nil  // "unspecified" (code 0), or an unknown label
        }
        check("classify(\(fatalCase.rawValue)) [\(fatalCase)] == \(jsonName)",
              classify(Int32(fatalCase.rawValue)) == expected)
    }

    check("classify(99) == nil (unknown code)", classify(99) == nil)
} catch {
    print("FAIL loading fatal_code_classes.json: \(error)")
    failures += 1
}

// MARK: - (d) Comparators (newerU32 / newerU24) — docs/WIRE.md §5

check("newerU32: equal -> false", newerU32(5, 5) == false)
check("newerU32: forward -> true", newerU32(6, 5) == true)
check("newerU32: wrap-forward (a=5, b=0xFFFFFFF0) -> true", newerU32(5, 0xFFFF_FFF0) == true)
check("newerU32: stale -> false", newerU32(5, 6) == false)
check("newerU32: d == 0x80000000 exactly -> false", newerU32(0x8000_0000, 0) == false)
check("newerU32: d == 0x7FFFFFFF exactly -> true", newerU32(0x7FFF_FFFF, 0) == true)

check("newerU24: equal -> false", newerU24(5, 5) == false)
check("newerU24: forward -> true", newerU24(6, 5) == true)
check("newerU24: wrap-forward (a=5, b=0xFFFFF0) -> true", newerU24(5, 0xFF_FFF0) == true)
check("newerU24: stale -> false", newerU24(5, 6) == false)
check("newerU24: d == 0x800000 exactly -> false", newerU24(0x80_0000, 0) == false)
check("newerU24: d == 0x7FFFFF exactly -> true", newerU24(0x7F_FFFF, 0) == true)
check("newerU24: masks inputs beyond 24 bits (a=0xFF000006, b=5) -> true", newerU24(0xFF00_0006, 5) == true)

// MARK: - (e) Scroll transform (scroll_cases.json, ERR-04)

do {
    let bytes = try Data(contentsOf: fixturesDir.appendingPathComponent("scroll_cases.json"))
    guard let obj = try JSONSerialization.jsonObject(with: bytes) as? [String: Any],
          let cases = obj["cases"] as? [[String: Any]] else {
        throw FixtureCheckError(message: "scroll_cases.json: unexpected structure")
    }
    check("scroll_cases.json has 12 cases", cases.count == 12)

    for c in cases {
        guard let name = c["name"] as? String,
              let dxIn = c["dx"] as? Int, let dyIn = c["dy"] as? Int,
              let rotated = c["rotated"] as? Bool,
              let outDx = c["out_dx"] as? Int, let outDy = c["out_dy"] as? Int else {
            check("scroll case has all expected fields", false)
            continue
        }
        let result = scrollTransform(dx: Int32(dxIn), dy: Int32(dyIn), rotated: rotated)
        check("scroll[\(name)]: dx == \(outDx)", Int(result.dx) == outDx)
        check("scroll[\(name)]: dy == \(outDy)", Int(result.dy) == outDy)
    }
} catch {
    print("FAIL loading scroll_cases.json: \(error)")
    failures += 1
}

// MARK: - (f) RA verification (synthetic NAL sequences)

/// Builds a synthetic Annex-B AU: one NAL unit per `type`, each written as
/// a start code (`startCodeLen` 3 or 4 bytes) followed by a single header
/// byte encoding `type` at bits 1-6 (`(byte >> 1) & 0x3F == type`) — enough
/// for `scanAnnexB` to recover the type; the rest of a real NAL unit's
/// payload is irrelevant to RA verification.
func makeSyntheticAU(_ types: [UInt8], startCodeLen: Int = 4) -> Data {
    var out = Data()
    for t in types {
        out.append(contentsOf: repeatElement(0, count: startCodeLen - 1))
        out.append(1)
        out.append(t << 1)
    }
    return out
}

func isOk(_ result: Result<Void, String>) -> Bool {
    if case .success = result { return true }
    return false
}

check("RA (32,33,34,19) -> ok", isOk(validateSessionFirst(scanAnnexB(makeSyntheticAU([32, 33, 34, 19])))))
check("RA (32,33,34,20) -> ok", isOk(validateSessionFirst(scanAnnexB(makeSyntheticAU([32, 33, 34, 20])))))
check("RA missing PPS -> fail", !isOk(validateSessionFirst(scanAnnexB(makeSyntheticAU([32, 33, 19])))))
check("RA CRA instead of IDR -> fail", !isOk(validateSessionFirst(scanAnnexB(makeSyntheticAU([32, 33, 34, 21])))))
check("RA CRA alongside IDR -> fail", !isOk(validateSessionFirst(scanAnnexB(makeSyntheticAU([32, 33, 34, 19, 21])))))
check("RA empty input -> fail", !isOk(validateSessionFirst(scanAnnexB(Data()))))
check("RA 3-byte start codes still scan correctly",
      isOk(validateSessionFirst(scanAnnexB(makeSyntheticAU([32, 33, 34, 19], startCodeLen: 3)))))

let idrSummary = scanAnnexB(makeSyntheticAU([32, 33, 34, 19]))
check("keyframeClaimMatches: IDR present, claim=true -> match", keyframeClaimMatches(idrSummary, claim: true))
check("keyframeClaimMatches: IDR present, claim=false -> mismatch", !keyframeClaimMatches(idrSummary, claim: false))

let noIdrSummary = scanAnnexB(makeSyntheticAU([32, 33, 34]))
check("keyframeClaimMatches: no IDR, claim=false -> match", keyframeClaimMatches(noIdrSummary, claim: false))
check("keyframeClaimMatches: no IDR, claim=true -> mismatch", !keyframeClaimMatches(noIdrSummary, claim: true))

// ===========================================================================
// MARK: - (g) v3 dispatch (dispatch_cases.json) — remediation item R5
//
// Exercises V3Dispatch (Sources/RescCore/V3Dispatch.swift) over the same
// oracle-generated cases the Rust twin (ubuntu-client/crates/protocol/src/
// v3dispatch.rs) is graded against (proto/fixtures/dispatch_cases.json,
// tools/gen_dispatch_fixtures.py). One "ok <name>" line per row.
// ===========================================================================

/// The four `FatalCode` names this fixture set's verdict vocabulary uses.
func dispatchFatalCode(named name: String) -> Resc_V3_FatalCode {
    switch name {
    case "VERSION_MISMATCH": return .versionMismatch
    case "PROTOCOL_VIOLATION": return .protocolViolation
    case "RECORD_CAP_VIOLATION": return .recordCapViolation
    case "MALFORMED_FRAMING": return .malformedFraming
    default: fatalError("dispatch_cases.json: unknown FatalCode name \(name)")
    }
}

func dispatchPhase(named name: String) -> V3Dispatch.Phase {
    switch name {
    case "bootstrap": return .bootstrap
    case "announced": return .announced
    case "profile_accepted": return .profileAccepted
    case "profile_rejected": return .profileRejected
    case "video_ack_accepted": return .videoAckAccepted
    case "active": return .active
    default: fatalError("dispatch_cases.json: unknown phase \(name)")
    }
}

func dispatchRole(named name: String) -> V3Dispatch.Role {
    switch name {
    case "host": return .host
    case "client": return .client
    default: fatalError("dispatch_cases.json: unknown role \(name)")
    }
}

enum DispatchVerdict {
    case accept(next: V3Dispatch.Phase, learn: Bool)
    case remoteFatal(FailureClass)
    case error(Resc_V3_FatalCode)
}

/// The three `FailureClass` names the "remote_fatal:<class>" verdict
/// vocabulary uses (C1: routing disposition for a fully-valid FatalReport).
func dispatchFailureClass(named name: String) -> FailureClass {
    switch name {
    case "deterministic": return .deterministic
    case "transient": return .transient
    case "terminal": return .terminal
    default: fatalError("dispatch_cases.json: unknown FailureClass name \(name)")
    }
}

func dispatchVerdict(from s: String) -> DispatchVerdict {
    if s.hasPrefix("accept:") {
        let parts = s.dropFirst("accept:".count).split(separator: ":", omittingEmptySubsequences: false)
        return .accept(next: dispatchPhase(named: String(parts[0])), learn: parts.count > 1 && parts[1] == "learn")
    }
    if s.hasPrefix("remote_fatal:") {
        return .remoteFatal(dispatchFailureClass(named: String(s.dropFirst("remote_fatal:".count))))
    }
    return .error(dispatchFatalCode(named: s))
}

/// Decodes a lowercase-hex string (even length) into raw bytes.
func dispatchHexBytes(_ s: String) -> Data {
    var data = Data(capacity: s.count / 2)
    var idx = s.startIndex
    while idx < s.endIndex {
        let next = s.index(idx, offsetBy: 2)
        guard let byte = UInt8(s[idx..<next], radix: 16) else {
            fatalError("dispatch_cases.json: bad hex \(s)")
        }
        data.append(byte)
        idx = next
    }
    return data
}

// -- typed field accessors over a JSONSerialization `fields` object --

func dField(_ fields: [String: Any], _ key: String) -> UInt64 {
    guard let v = fields[key] as? Int else { fatalError("dispatch_cases.json: field \(key) missing/not int") }
    return UInt64(v)
}
func dFieldI(_ fields: [String: Any], _ key: String) -> Int64 {
    guard let v = fields[key] as? Int else { fatalError("dispatch_cases.json: field \(key) missing/not int") }
    return Int64(v)
}
func dFieldB(_ fields: [String: Any], _ key: String) -> Bool {
    guard let v = fields[key] as? Bool else { fatalError("dispatch_cases.json: field \(key) missing/not bool") }
    return v
}
func dFieldS(_ fields: [String: Any], _ key: String) -> String {
    guard let v = fields[key] as? String else { fatalError("dispatch_cases.json: field \(key) missing/not string") }
    return v
}
func dFieldHex(_ fields: [String: Any], _ key: String) -> Data {
    dispatchHexBytes(dFieldS(fields, key))
}
/// `run` fact string ("no_run" | "candidate:<hex16>" | "active:<hex16>")
/// -> `V3Dispatch.RunFact`, per `tools/gen_dispatch_fixtures.py`'s encoding.
func dispatchRunFact(_ s: String) -> V3Dispatch.RunFact {
    if s == "no_run" { return .noRun }
    let parts = s.split(separator: ":", maxSplits: 1)
    guard parts.count == 2, let id = UInt64(parts[1], radix: 16) else {
        fatalError("dispatch_cases.json: bad run fact \(s)")
    }
    switch parts[0] {
    case "candidate": return .candidate(id)
    case "active": return .active(id)
    default: fatalError("dispatch_cases.json: unknown run fact kind \(s)")
    }
}
func dispatchDiagMode(_ s: String) -> V3Dispatch.DiagMode {
    switch s {
    case "normal": return .normal
    case "trace_or_doctor": return .traceOrDoctor
    default: fatalError("dispatch_cases.json: unknown diagnostics mode \(s)")
    }
}
/// Builds the `DispatchFacts` a state/raw/outbound row was graded against
/// from its flattened `run`/`diagnostics`/`oldest_outstanding` fields.
func dispatchFacts(from row: [String: Any]) -> V3Dispatch.DispatchFacts {
    V3Dispatch.DispatchFacts(
        run: dispatchRunFact(dFieldS(row, "run")),
        diagnostics: dispatchDiagMode(dFieldS(row, "diagnostics")),
        oldestOutstandingOrdinal: (row["oldest_outstanding"] as? Int).map { UInt64($0) }
    )
}
/// `warm_strength` is a JSON number except for the one non-finite special
/// case, which uses the sentinel string "NaN" (raw JSON has no NaN literal).
func dFieldWarmStrength(_ fields: [String: Any]) -> Float {
    if let s = fields["warm_strength"] as? String {
        switch s {
        case "NaN": return Float.nan
        case "Infinity": return Float.infinity
        case "-Infinity": return -Float.infinity
        default: fatalError("dispatch_cases.json: unknown warm_strength sentinel \(s)")
        }
    }
    if let d = fields["warm_strength"] as? Double { return Float(d) }
    if let i = fields["warm_strength"] as? Int { return Float(i) }
    fatalError("dispatch_cases.json: warm_strength missing/wrong type")
}

/// Builds the `Resc_V3_Envelope.OneOf_Payload` for one `payload` kind from
/// its `fields` object. Minimal-valid field shapes and hex-string
/// conventions per `tools/gen_dispatch_fixtures.py`'s doc comment header.
func dispatchPayload(kind: String, fields: [String: Any]) -> Resc_V3_Envelope.OneOf_Payload {
    switch kind {
    case "display_settings":
        var m = Resc_V3_DisplaySettings()
        m.warmStrength = dFieldWarmStrength(fields)
        return .displaySettings(m)
    case "key_event":
        var m = Resc_V3_KeyEvent()
        m.hidUsage = UInt32(dField(fields, "hid_usage"))
        m.isDown = dFieldB(fields, "is_down")
        m.modifiers = UInt32(dField(fields, "modifiers"))
        return .keyEvent(m)
    case "host_profile_announce":
        var m = Resc_V3_HostProfileAnnounce()
        m.profileCanonical = dFieldHex(fields, "profile_canonical_hex")
        m.profileHash = dFieldHex(fields, "profile_hash_hex")
        m.buildCommit = dFieldS(fields, "build_commit")
        m.buildDirty = dFieldB(fields, "build_dirty")
        return .hostProfileAnnounce(m)
    case "profile_result":
        var m = Resc_V3_ProfileResult()
        m.accepted = dFieldB(fields, "accepted")
        m.profileCanonical = dFieldHex(fields, "profile_canonical_hex")
        m.profileHash = dFieldHex(fields, "profile_hash_hex")
        m.buildCommit = dFieldS(fields, "build_commit")
        m.buildDirty = dFieldB(fields, "build_dirty")
        let rejectCode = Int(dFieldI(fields, "reject_code"))
        m.rejectCode = Resc_V3_FatalCode(rawValue: rejectCode) ?? .UNRECOGNIZED(rejectCode)
        m.videoListenerReady = dFieldB(fields, "video_listener_ready")
        return .profileResult(m)
    case "frame_ack":
        var m = Resc_V3_FrameAck()
        m.frameOrdinal = dField(fields, "frame_ordinal")
        return .frameAck(m)
    case "button_event":
        var m = Resc_V3_ButtonEvent()
        m.button = UInt32(dField(fields, "button"))
        m.isDown = dFieldB(fields, "is_down")
        m.xPx = Int32(dFieldI(fields, "x_px"))
        m.yPx = Int32(dFieldI(fields, "y_px"))
        m.modifiers = UInt32(dField(fields, "modifiers"))
        return .buttonEvent(m)
    case "scroll_event":
        var m = Resc_V3_ScrollEvent()
        m.dx = Int32(dFieldI(fields, "dx"))
        m.dy = Int32(dFieldI(fields, "dy"))
        return .scrollEvent(m)
    case "clock_ping":
        var m = Resc_V3_ClockPing()
        m.t1MonoUs = dField(fields, "t1_mono_us")
        m.seq = UInt32(dField(fields, "seq"))
        return .clockPing(m)
    case "clock_pong":
        var m = Resc_V3_ClockPong()
        m.t1MonoUs = dField(fields, "t1_mono_us")
        m.t2MonoUs = dField(fields, "t2_mono_us")
        m.t3MonoUs = dField(fields, "t3_mono_us")
        m.seq = UInt32(dField(fields, "seq"))
        return .clockPong(m)
    case "fatal_report":
        var m = Resc_V3_FatalReport()
        let code = Int(dFieldI(fields, "code"))
        m.code = Resc_V3_FatalCode(rawValue: code) ?? .UNRECOGNIZED(code)
        m.component = dFieldS(fields, "component")
        m.nativeDomain = (fields["native_domain"] as? String) ?? ""
        m.nativeCode = dFieldI(fields, "native_code")
        m.summary = dFieldS(fields, "summary")
        return .fatalReport(m)
    case "release_input":
        return .releaseInput(Resc_V3_ReleaseInput())
    case "heartbeat":
        var m = Resc_V3_Heartbeat()
        m.tMonoUs = dField(fields, "t_mono_us")
        return .heartbeat(m)
    default:
        fatalError("dispatch_cases.json: unknown payload kind \(kind)")
    }
}

/// Shared assertion for one state/raw case: run `validateInbound` and
/// compare its result against the row's `verdict` string. One check() call
/// per row, matching this file's existing "one ok-line per row" style.
func assertDispatchCase(
    _ name: String,
    role: V3Dispatch.Role,
    phase: V3Dispatch.Phase,
    facts: V3Dispatch.DispatchFacts,
    env: Resc_V3_Envelope,
    verdict: String
) {
    let result = V3Dispatch.validateInbound(role: role, phase: phase, facts: facts, env: env)
    switch dispatchVerdict(from: verdict) {
    case .accept(let next, let learn):
        switch result {
        case .success(.accepted(let gotNext, let learnedCandidate)):
            let expectedLearned: UInt64? = learn ? env.sessionRunID : nil
            check("dispatch[\(name)]: accept -> \(next), learnedCandidate == \(String(describing: expectedLearned))",
                  gotNext == next && learnedCandidate == expectedLearned)
        case .success(let other):
            check("dispatch[\(name)]: expected accept(\(next)), got \(other)", false)
        case .failure(let e):
            check("dispatch[\(name)]: expected accept(\(next)), got \(e)", false)
        }
    case .remoteFatal(let cls):
        switch result {
        case .success(.remoteFatal(let gotCls)):
            check("dispatch[\(name)]: remoteFatal(\(cls))", gotCls == cls)
        case .success(let other):
            check("dispatch[\(name)]: expected remoteFatal(\(cls)), got \(other)", false)
        case .failure(let e):
            check("dispatch[\(name)]: expected remoteFatal(\(cls)), got \(e)", false)
        }
    case .error(let code):
        switch result {
        case .success:
            check("dispatch[\(name)]: expected \(code), got accept", false)
        case .failure(let e):
            check("dispatch[\(name)]: \(code)", e == code)
        }
    }
}

func dispatchOutboundKind(named name: String) -> V3Dispatch.OutboundKind {
    switch name {
    case "host_profile_announce": return .hostProfileAnnounce
    case "profile_result_accepted": return .profileResultAccepted
    case "profile_result_rejected": return .profileResultRejected
    case "frame_ack": return .frameAck
    case "key_event": return .keyEvent
    case "button_event": return .buttonEvent
    case "scroll_event": return .scrollEvent
    case "release_input": return .releaseInput
    case "heartbeat": return .heartbeat
    case "clock_ping": return .clockPing
    case "clock_pong": return .clockPong
    case "display_settings": return .displaySettings
    case "fatal_report": return .fatalReport
    default: fatalError("dispatch_cases.json: unknown OutboundKind name \(name)")
    }
}

/// D1 acceptance ("both languages pass shared vectors") requires
/// noteOutbound to be vector-covered like validateInbound. One check()
/// call per row, same style as assertDispatchCase.
func assertOutboundCase(_ name: String, role: V3Dispatch.Role, phase: V3Dispatch.Phase,
                         facts: V3Dispatch.DispatchFacts, kind: V3Dispatch.OutboundKind, verdict: String) {
    let result = V3Dispatch.noteOutbound(role: role, phase: phase, facts: facts, kind: kind)
    switch dispatchVerdict(from: verdict) {
    case .accept(let next, _):
        switch result {
        case .success(let n): check("outbound[\(name)]: accept -> \(next)", n == next)
        case .failure(let e): check("outbound[\(name)]: expected accept(\(next)), got \(e)", false)
        }
    case .remoteFatal:
        check("outbound[\(name)]: note_outbound never produces a remote_fatal verdict", false)
    case .error(let code):
        switch result {
        case .success: check("outbound[\(name)]: expected \(code), got accept", false)
        case .failure(let e): check("outbound[\(name)]: \(code)", e == code)
        }
    }
}

/// Same D1 rationale as `assertOutboundCase`, for noteVideoAck's 6-row table.
func assertVideoAckCase(_ name: String, phase: V3Dispatch.Phase, verdict: String) {
    let result = V3Dispatch.noteVideoAck(phase)
    switch dispatchVerdict(from: verdict) {
    case .accept(let next, _):
        switch result {
        case .success(let n): check("videoAck[\(name)]: accept -> \(next)", n == next)
        case .failure(let e): check("videoAck[\(name)]: expected accept(\(next)), got \(e)", false)
        }
    case .remoteFatal:
        check("videoAck[\(name)]: note_video_ack never produces a remote_fatal verdict", false)
    case .error(let code):
        switch result {
        case .success: check("videoAck[\(name)]: expected \(code), got accept", false)
        case .failure(let e): check("videoAck[\(name)]: \(code)", e == code)
        }
    }
}

do {
    let dispatchBytes = try Data(contentsOf: fixturesDir.appendingPathComponent("dispatch_cases.json"))
    guard let dispatchObj = try JSONSerialization.jsonObject(with: dispatchBytes) as? [String: Any] else {
        throw FixtureCheckError(message: "dispatch_cases.json: unexpected top-level structure")
    }

    // -- framing (Layer 1: frameBodyLen) --
    let framingCases = dispatchObj["framing"] as? [[String: Any]] ?? []
    check("dispatch_cases.json framing has 6 rows", framingCases.count == 6)
    for row in framingCases {
        guard let name = row["name"] as? String,
              let prefixHex = row["prefix_hex"] as? String,
              let verdict = row["verdict"] as? String else {
            check("dispatch framing row has expected fields", false)
            continue
        }
        let prefixBytes = Array(dispatchHexBytes(prefixHex))
        guard prefixBytes.count == 4 else {
            check("dispatch framing[\(name)]: prefix_hex decodes to 4 bytes", false)
            continue
        }
        let prefix = (prefixBytes[0], prefixBytes[1], prefixBytes[2], prefixBytes[3])
        let result = V3Dispatch.frameBodyLen(prefix)
        if verdict == "accept" {
            let expectedLen = Int(UInt32(prefixBytes[0]) | (UInt32(prefixBytes[1]) << 8)
                                   | (UInt32(prefixBytes[2]) << 16) | (UInt32(prefixBytes[3]) << 24))
            switch result {
            case .success(let len): check("dispatch framing[\(name)]: accept len \(expectedLen)", len == expectedLen)
            case .failure(let e): check("dispatch framing[\(name)]: expected accept, got \(e)", false)
            }
        } else {
            let expectedCode = dispatchFatalCode(named: verdict)
            switch result {
            case .success: check("dispatch framing[\(name)]: expected \(verdict), got accept", false)
            case .failure(let e): check("dispatch framing[\(name)]: \(verdict)", e == expectedCode)
            }
        }
    }

    // -- state (Layer 2: validateInbound over the 144-cell matrix + 66 C1 special rows) --
    let stateCases = dispatchObj["state"] as? [[String: Any]] ?? []
    check("dispatch_cases.json state has 210 rows", stateCases.count == 210)
    for row in stateCases {
        guard let name = row["name"] as? String,
              let roleStr = row["role"] as? String,
              let phaseStr = row["phase"] as? String,
              let kind = row["payload"] as? String,
              let fields = row["fields"] as? [String: Any],
              let verdict = row["verdict"] as? String else {
            check("dispatch state row has expected fields", false)
            continue
        }
        var env = Resc_V3_Envelope()
        env.sessionRunID = dField(row, "env_run_id")
        env.protocolVersion = UInt32(dField(row, "env_version"))
        env.payload = dispatchPayload(kind: kind, fields: fields)
        assertDispatchCase(name, role: dispatchRole(named: roleStr), phase: dispatchPhase(named: phaseStr),
                            facts: dispatchFacts(from: row), env: env, verdict: verdict)
    }

    // -- raw (Layer 2 over hand-encoded byte vectors: absent-payload / zero-byte envelopes) --
    let rawCases = dispatchObj["raw"] as? [[String: Any]] ?? []
    check("dispatch_cases.json raw has 3 rows", rawCases.count == 3)
    for row in rawCases {
        guard let name = row["name"] as? String,
              let file = row["file"] as? String,
              let roleStr = row["role"] as? String,
              let phaseStr = row["phase"] as? String,
              let verdict = row["verdict"] as? String else {
            check("dispatch raw row has expected fields", false)
            continue
        }
        do {
            let raw = try Data(contentsOf: fixturesDir.appendingPathComponent(file))
            let env = try Resc_V3_Envelope(serializedBytes: raw)
            assertDispatchCase(name, role: dispatchRole(named: roleStr), phase: dispatchPhase(named: phaseStr),
                                facts: dispatchFacts(from: row), env: env, verdict: verdict)
        } catch {
            check("dispatch raw[\(name)]: decode \(file)", false)
        }
    }

    // -- outbound (sender-side mirror: noteOutbound over the full 13-kind x 6-phase x 2-role
    // matrix plus the C1 diagnostics/facts.run special rows) --
    let outboundCases = dispatchObj["outbound"] as? [[String: Any]] ?? []
    check("dispatch_cases.json outbound has 166 rows", outboundCases.count == 166)
    for row in outboundCases {
        guard let name = row["name"] as? String,
              let roleStr = row["role"] as? String,
              let phaseStr = row["phase"] as? String,
              let kindStr = row["kind"] as? String,
              let verdict = row["verdict"] as? String else {
            check("dispatch outbound row has expected fields", false)
            continue
        }
        assertOutboundCase(name, role: dispatchRole(named: roleStr), phase: dispatchPhase(named: phaseStr),
                            facts: dispatchFacts(from: row), kind: dispatchOutboundKind(named: kindStr),
                            verdict: verdict)
    }

    // -- video_ack (noteVideoAck over all 6 phases) --
    let videoAckCases = dispatchObj["video_ack"] as? [[String: Any]] ?? []
    check("dispatch_cases.json video_ack has 6 rows", videoAckCases.count == 6)
    for row in videoAckCases {
        guard let name = row["name"] as? String,
              let phaseStr = row["phase"] as? String,
              let verdict = row["verdict"] as? String else {
            check("dispatch video_ack row has expected fields", false)
            continue
        }
        assertVideoAckCase(name, phase: dispatchPhase(named: phaseStr), verdict: verdict)
    }
} catch {
    print("FAIL loading dispatch_cases.json: \(error)")
    failures += 1
}

// MARK: - (h) ERR-01 activation-barrier scheduling proof (R2a)

// CONTRACT_ERRATA.md ERR-01, proven against the R5 phase model (the single
// state machine both endpoints share — no second, drifting one). The
// single-cell legality of every (role, phase, payload) pair is already
// vector-graded in section (g); these traces add the *sequence* dimension:
// walking the two TCP handlers' events in specific schedules — including
// the reordered schedule the model must surface as a violation instead of
// silently arming input. Twin: ubuntu-client/crates/protocol/tests/err01_barrier.rs.
//
// C1 (corrective-cycle item closing review finding F2): validateInbound and
// noteOutbound now take a DispatchFacts context. The traces below are
// unchanged in meaning; each step is threaded with the DispatchFacts
// consistent with the role/phase at that point (run confirmed once
// profileAccepted is reached, candidate before that at the host; these
// traces never exercise ClockPing/ClockPong so diagnostics stays .normal
// throughout; the oldest outstanding ordinal fixed at frameAckEnv's
// ordinal wherever a FrameAck envelope is validated).

do {
    let runId: UInt64 = 0x0102_0304_0506_0708

    func mkEnv(_ fill: (inout Resc_V3_Envelope) -> Void) -> Resc_V3_Envelope {
        var e = Resc_V3_Envelope()
        e.sessionRunID = runId
        e.protocolVersion = 3
        fill(&e)
        return e
    }
    let heartbeatEnv = mkEnv { $0.heartbeat = Resc_V3_Heartbeat() }
    let displaySettingsEnv = mkEnv { $0.displaySettings = .with { $0.warmStrength = 0.5 } }
    let keyEventEnv = mkEnv {
        $0.keyEvent = .with { k in k.hidUsage = 4; k.isDown = true; k.modifiers = 0 }
    }
    let frameAckEnv = mkEnv { $0.frameAck = .with { $0.frameOrdinal = 1 } }
    let profileResultAcceptedEnv = mkEnv {
        $0.profileResult = .with { p in
            p.accepted = true
            p.profileCanonical = Data(repeating: UInt8(ascii: "x"), count: 20)
            p.profileHash = Data([1, 2, 3, 4, 5, 6, 7, 8])
            p.buildCommit = String(repeating: "a", count: 40)
            p.buildDirty = false
            p.rejectCode = .fatalUnspecified
            p.videoListenerReady = true
        }
    }

    // The client-side sends ERR-01 forbids before activation (FrameAck is
    // deliberately not gated — decoded frames race the activation signal).
    let clientInputKinds: [V3Dispatch.OutboundKind] =
        [.keyEvent, .buttonEvent, .scrollEvent, .releaseInput, .heartbeat]

    // The client's run is confirmed (active) for the whole client trace
    // (1-3 below) -- it starts at profileAccepted and never revisits
    // bootstrap/announced.
    let clientFacts = V3Dispatch.DispatchFacts(run: .active(runId), diagnostics: .normal,
                                                oldestOutstandingOrdinal: nil)
    // The host trace (4) walks bootstrap -> announced -> profileAccepted ->
    // videoAckAccepted -> active; facts.run tracks that (candidate before
    // profileAccepted, active from profileAccepted on -- the host owns its
    // candidate id from process start, so it is never noRun). The oldest
    // outstanding ordinal is fixed at frameAckEnv's ordinal (1) throughout,
    // irrelevant to every payload kind but FrameAck.
    func hostFacts(_ phase: V3Dispatch.Phase) -> V3Dispatch.DispatchFacts {
        let run: V3Dispatch.RunFact = (phase == .bootstrap || phase == .announced)
            ? .candidate(runId) : .active(runId)
        return V3Dispatch.DispatchFacts(run: run, diagnostics: .normal, oldestOutstandingOrdinal: 1)
    }

    func clientInputArmed(_ phase: V3Dispatch.Phase) -> Bool {
        clientInputKinds.allSatisfy {
            if case .success(.active) = V3Dispatch.noteOutbound(role: .client, phase: phase, facts: clientFacts, kind: $0) {
                return true
            }
            return false
        }
    }
    func clientInputFullyDisarmed(_ phase: V3Dispatch.Phase) -> Bool {
        clientInputKinds.allSatisfy {
            if case .failure = V3Dispatch.noteOutbound(role: .client, phase: phase, facts: clientFacts, kind: $0) {
                return true
            }
            return false
        }
    }
    func inbound(_ role: V3Dispatch.Role, _ phase: V3Dispatch.Phase, _ facts: V3Dispatch.DispatchFacts,
                 _ env: Resc_V3_Envelope) -> Result<V3Dispatch.Dispatch, Resc_V3_FatalCode> {
        V3Dispatch.validateInbound(role: role, phase: phase, facts: facts, env: env)
    }
    func acceptedNext(_ r: Result<V3Dispatch.Dispatch, Resc_V3_FatalCode>) -> V3Dispatch.Phase? {
        if case .success(.accepted(let next, _)) = r { return next }
        return nil
    }
    func isProtocolViolation(_ r: Result<V3Dispatch.Dispatch, Resc_V3_FatalCode>) -> Bool {
        if case .failure(.protocolViolation) = r { return true }
        return false
    }

    // -- Trace 1: correct schedule, step by step --
    var phase = V3Dispatch.Phase.profileAccepted
    check("ERR-01 client t0 (ProfileAccepted): input fully disarmed", clientInputFullyDisarmed(phase))
    check("ERR-01 client t0: FrameAck send illegal pre-Ack", {
        if case .failure = V3Dispatch.noteOutbound(role: .client, phase: phase, facts: clientFacts, kind: .frameAck) { return true }
        return false
    }())

    check("ERR-01 client A: noteVideoAck -> videoAckAccepted", {
        if case .success(.videoAckAccepted) = V3Dispatch.noteVideoAck(phase) {
            phase = .videoAckAccepted
            return true
        }
        return false
    }())
    check("ERR-01 client t1 (barrier window): input fully disarmed", clientInputFullyDisarmed(phase))
    check("ERR-01 client t1: FrameAck send legal (races activation by design)", {
        if case .success(.videoAckAccepted) = V3Dispatch.noteOutbound(role: .client, phase: phase, facts: clientFacts, kind: .frameAck) { return true }
        return false
    }())
    check("ERR-01 client D: DisplaySettings accepted, phase unchanged",
          acceptedNext(inbound(.client, phase, clientFacts, displaySettingsEnv)) == .videoAckAccepted)
    check("ERR-01 client t1 after D: input still disarmed", clientInputFullyDisarmed(phase))
    check("ERR-01 client H: activation heartbeat -> active", {
        guard let next = acceptedNext(inbound(.client, phase, clientFacts, heartbeatEnv)), next == .active else { return false }
        phase = next
        return true
    }())
    check("ERR-01 client t2: first post-barrier input + heartbeat armed", clientInputArmed(phase))
    check("ERR-01 client t2: liveness heartbeat keeps active",
          acceptedNext(inbound(.client, phase, clientFacts, heartbeatEnv)) == .active)

    // -- Trace 2: reordered handlers surfaced as violations --
    check("ERR-01 client reordered: heartbeat in ProfileAccepted -> PROTOCOL_VIOLATION",
          isProtocolViolation(inbound(.client, .profileAccepted, clientFacts, heartbeatEnv)))
    check("ERR-01 client reordered: DisplaySettings in ProfileAccepted -> PROTOCOL_VIOLATION",
          isProtocolViolation(inbound(.client, .profileAccepted, clientFacts, displaySettingsEnv)))
    check("ERR-01 client reordered: input stays disarmed", clientInputFullyDisarmed(.profileAccepted))

    // -- Trace 3: prefix sweep over [A, D, H] — armed iff H processed --
    enum BarrierEv { case a, d, h }
    let schedule: [BarrierEv] = [.a, .d, .h]
    for cut in 0...schedule.count {
        var p = V3Dispatch.Phase.profileAccepted
        var activated = false
        var walkOk = true
        for ev in schedule.prefix(cut) {
            switch ev {
            case .a:
                guard case .success(let next) = V3Dispatch.noteVideoAck(p) else { walkOk = false; break }
                p = next
            case .d:
                guard let next = acceptedNext(inbound(.client, p, clientFacts, displaySettingsEnv)) else { walkOk = false; break }
                p = next
            case .h:
                guard let next = acceptedNext(inbound(.client, p, clientFacts, heartbeatEnv)) else { walkOk = false; break }
                activated = true
                p = next
            }
        }
        check("ERR-01 client prefix[\(cut)]: walk legal", walkOk)
        check("ERR-01 client prefix[\(cut)]: armed == activated",
              clientInputArmed(p) == activated)
        if !activated {
            check("ERR-01 client prefix[\(cut)]: fully disarmed pre-activation",
                  clientInputFullyDisarmed(p))
        }
    }

    // -- Trace 4: host side --
    var hostPhase = V3Dispatch.Phase.bootstrap
    check("ERR-01 host: announce Bootstrap -> Announced", {
        if case .success(.announced) = V3Dispatch.noteOutbound(role: .host, phase: hostPhase, facts: hostFacts(hostPhase), kind: .hostProfileAnnounce) {
            hostPhase = .announced
            return true
        }
        return false
    }())
    check("ERR-01 host: ProfileResult(accepted) -> profileAccepted", {
        guard let next = acceptedNext(inbound(.host, hostPhase, hostFacts(hostPhase), profileResultAcceptedEnv)), next == .profileAccepted else { return false }
        hostPhase = next
        return true
    }())
    check("ERR-01 host: no heartbeat send before video Ack", {
        if case .failure = V3Dispatch.noteOutbound(role: .host, phase: hostPhase, facts: hostFacts(hostPhase), kind: .heartbeat) { return true }
        return false
    }())
    check("ERR-01 host: noteVideoAck -> videoAckAccepted", {
        if case .success(.videoAckAccepted) = V3Dispatch.noteVideoAck(hostPhase) {
            hostPhase = .videoAckAccepted
            return true
        }
        return false
    }())
    check("ERR-01 host pre-activation: rogue KeyEvent -> PROTOCOL_VIOLATION",
          isProtocolViolation(inbound(.host, hostPhase, hostFacts(hostPhase), keyEventEnv)))
    check("ERR-01 host pre-activation: client heartbeat -> PROTOCOL_VIOLATION",
          isProtocolViolation(inbound(.host, hostPhase, hostFacts(hostPhase), heartbeatEnv)))
    check("ERR-01 host pre-activation: FrameAck racing activation accepted",
          acceptedNext(inbound(.host, hostPhase, hostFacts(hostPhase), frameAckEnv)) == .videoAckAccepted)
    check("ERR-01 host activation send -> active", {
        if case .success(.active) = V3Dispatch.noteOutbound(role: .host, phase: hostPhase, facts: hostFacts(hostPhase), kind: .heartbeat) {
            hostPhase = .active
            return true
        }
        return false
    }())
    check("ERR-01 host post-barrier: first input accepted",
          acceptedNext(inbound(.host, hostPhase, hostFacts(hostPhase), keyEventEnv)) == .active)
    check("ERR-01 host post-barrier: client heartbeat accepted",
          acceptedNext(inbound(.host, hostPhase, hostFacts(hostPhase), heartbeatEnv)) == .active)
    check("ERR-01 host post-barrier: FrameAck accepted",
          acceptedNext(inbound(.host, hostPhase, hostFacts(hostPhase), frameAckEnv)) == .active)
}

// MARK: - (i) R2b: generation slot + cursor clock

// A00_REMEDIATION_PLAN.md §4 items 1–3 + work item R2b; CONTRACT_ERRATA.md
// "Implementation proofs required" — late capture callbacks (generation
// binding: a callback from a torn-down capture session must not populate a
// newer run's slot) and cursor timestamp_us (sender-local diagnostic time
// in the host continuous-monotonic domain; sequence number, never the
// timestamp, governs ordering). GenerationalFrameSlot<Int> stands in for
// the host's GenerationalFrameSlot<CVPixelBuffer> — no CVPixelBuffer or
// ScreenCaptureKit dependency is needed to exercise the generation/store/
// take state machine. R2b is Mac-only (no capture pipeline on the Rust
// side), so there is no Rust twin for this section.

func mkFrame(_ gen: UInt64, _ seq: UInt64, pixels: Int, ts: UInt64 = 1000) -> CapturedFrame<Int> {
    CapturedFrame(pixels: pixels, generation: gen, captureSeq: seq, captureTsUs: ts,
                  tsSource: .sckPts, uncertaintyUs: 0)
}

do {
    // -- basic store + take --
    let slot = GenerationalFrameSlot<Int>()
    let gen1 = slot.beginGeneration()
    let f1 = mkFrame(gen1, 1, pixels: 111, ts: 1000)
    check("R2b slot: store into a fresh generation == .stored", slot.store(f1) == .stored)
    let taken = slot.tryTake()
    check("R2b slot: tryTake returns the stored frame's generation", taken?.generation == gen1)
    check("R2b slot: tryTake returns the stored frame's captureSeq", taken?.captureSeq == 1)
    check("R2b slot: tryTake returns the stored frame's captureTsUs", taken?.captureTsUs == 1000)
    check("R2b slot: tryTake returns the stored frame's payload", taken?.pixels == 111)
}

do {
    // -- latest-wins within one generation --
    let slot = GenerationalFrameSlot<Int>()
    let gen1 = slot.beginGeneration()
    check("R2b slot: first store == .stored", slot.store(mkFrame(gen1, 1, pixels: 1)) == .stored)
    check("R2b slot: second same-generation store == .storedReplacingDropped",
          slot.store(mkFrame(gen1, 2, pixels: 2)) == .storedReplacingDropped)
    check("R2b slot: latest-wins replacement counted in dropCount", slot.dropCount == 1)
    let taken = slot.tryTake()
    check("R2b slot: tryTake returns ONLY the latest identity (seq 2)", taken?.captureSeq == 2)
    check("R2b slot: the dropped identity (seq 1) is never seen again", slot.tryTake() == nil)
}

do {
    // -- LATE CALLBACK: CONTRACT_ERRATA.md late-capture-callbacks proof --
    // teardown -> new run -> late callback bound to the torn-down run must
    // not populate the new run's slot.
    let slot = GenerationalFrameSlot<Int>()
    let gen1 = slot.beginGeneration()
    slot.endGeneration()                    // teardown of run 1
    let gen2 = slot.beginGeneration()       // run 2 starts
    check("R2b slot: late callback bound to the torn-down generation is rejected",
          slot.store(mkFrame(gen1, 99, pixels: -1)) == .rejectedStale)
    check("R2b slot: staleRejectCount incremented", slot.staleRejectCount == 1)
    check("R2b slot: slot untouched by the stale store", slot.tryTake() == nil)
    check("R2b slot: a store bound to the CURRENT generation still succeeds",
          slot.store(mkFrame(gen2, 1, pixels: 42)) == .stored)
    let taken = slot.tryTake()
    check("R2b slot: tryTake returns ONLY the gen2 identity",
          taken?.generation == gen2 && taken?.pixels == 42)
}

do {
    // -- teardown discards a held, never-consumed frame --
    let slot = GenerationalFrameSlot<Int>()
    let gen1 = slot.beginGeneration()
    check("R2b slot: store before teardown == .stored", slot.store(mkFrame(gen1, 1, pixels: 7)) == .stored)
    slot.endGeneration()
    check("R2b slot: teardown discards the held frame (tryTake nil)", slot.tryTake() == nil)
    check("R2b slot: teardown discard counted in dropCount", slot.dropCount == 1)
}

do {
    // -- no-current-token window --
    let slot = GenerationalFrameSlot<Int>()
    let gen1 = slot.beginGeneration()
    slot.endGeneration()
    check("R2b slot: store during the no-current-token window is rejectedStale",
          slot.store(mkFrame(gen1, 1, pixels: 0)) == .rejectedStale)
}

do {
    // -- frameCount counts only successful stores --
    let slot = GenerationalFrameSlot<Int>()
    let gen1 = slot.beginGeneration()
    _ = slot.store(mkFrame(gen1, 1, pixels: 1))          // .stored
    _ = slot.store(mkFrame(gen1, 2, pixels: 2))          // .storedReplacingDropped
    _ = slot.store(mkFrame(gen1 + 1, 3, pixels: 3))      // .rejectedStale (not the current token)
    check("R2b slot: frameCount excludes stale rejections", slot.frameCount == 2)
}

// -- CursorPacket: v1 wire byte layout (docs/WIRE.md legacy schema) --
// FixtureCheck depends only on RescCore/RescProto/SwiftProtobuf
// (Package.swift) and cannot import the RemoteDisplayHost executable
// target, so ProtocolConstants.magic/protocolVersion/packetTypeCursorUpdate
// are mirrored here as literals rather than referenced directly.
let cursorMagic: [UInt8] = [0x52, 0x45, 0x53, 0x43] // "RESC" — ProtocolConstants.magic
let cursorVersion: UInt8 = 1                        // ProtocolConstants.protocolVersion
let cursorPacketType: UInt8 = 1                     // ProtocolConstants.packetTypeCursorUpdate

func u32LE(_ d: Data, _ o: Int) -> UInt32 {
    let b = [UInt8](d)
    return UInt32(b[o]) | (UInt32(b[o + 1]) << 8) | (UInt32(b[o + 2]) << 16) | (UInt32(b[o + 3]) << 24)
}
func u64LE(_ d: Data, _ o: Int) -> UInt64 {
    let b = [UInt8](d)
    var v: UInt64 = 0
    for i in 0..<8 { v |= UInt64(b[o + i]) << (8 * i) }
    return v
}
func i32LE(_ d: Data, _ o: Int) -> Int32 { Int32(bitPattern: u32LE(d, o)) }

do {
    let packet = CursorPacket.build(magic: cursorMagic, version: cursorVersion, packetType: cursorPacketType,
                                     seq: 7, timestampUs: 123_456_789, x: 111, y: -22, shape: 3)
    check("CursorPacket.build returns exactly 35 bytes", packet.count == 35)

    let bytes = [UInt8](packet)
    check("CursorPacket bytes 0..3 == magic", Array(bytes[0..<4]) == cursorMagic)
    check("CursorPacket byte 4 == protocol version", bytes[4] == cursorVersion)
    check("CursorPacket byte 5 == packetTypeCursorUpdate", bytes[5] == cursorPacketType)

    check("CursorPacket round-trip: seq", u32LE(packet, 6) == 7)
    check("CursorPacket round-trip: timestamp_us", u64LE(packet, 10) == 123_456_789)
    check("CursorPacket round-trip: x_px", i32LE(packet, 18) == 111)
    check("CursorPacket round-trip: y_px", i32LE(packet, 22) == -22)
    check("CursorPacket round-trip: shape_id", bytes[26] == 3)
}

do {
    // -- monotonicity property: injected strictly-increasing clock --
    // CursorTracker.sendUpdate is a thin wrapper: nowUs() for the
    // timestamp, then CursorPacket.build for the bytes (CursorTracker.swift).
    // That composition — a clock closure feeding build's timestampUs
    // parameter — is the tested seam; driving CursorTracker itself would
    // need a live UDP socket, which this check binary does not open.
    var fakeNow: UInt64 = 1_000_000
    let nowUs: () -> UInt64 = { defer { fakeNow += 1_000 }; return fakeNow }

    let p1 = CursorPacket.build(magic: cursorMagic, version: cursorVersion, packetType: cursorPacketType,
                                 seq: 1, timestampUs: nowUs(), x: 0, y: 0, shape: 0)
    let p2 = CursorPacket.build(magic: cursorMagic, version: cursorVersion, packetType: cursorPacketType,
                                 seq: 2, timestampUs: nowUs(), x: 0, y: 0, shape: 0)
    let p3 = CursorPacket.build(magic: cursorMagic, version: cursorVersion, packetType: cursorPacketType,
                                 seq: 3, timestampUs: nowUs(), x: 0, y: 0, shape: 0)

    let t1 = u64LE(p1, 10), t2 = u64LE(p2, 10), t3 = u64LE(p3, 10)
    check("CursorPacket monotonicity: consecutive packets carry strictly increasing timestamps under an injected increasing clock",
          t1 < t2 && t2 < t3)
}

do {
    // -- SEPARATE property: seq governs ordering, NEVER the timestamp
    //    (CONTRACT_ERRATA.md cursor timestamp_us proof) --
    let fakeClockValues: [UInt64] = [100, 50, 200] // deliberately non-monotonic
    let packets = (0..<3).map { i in
        CursorPacket.build(magic: cursorMagic, version: cursorVersion, packetType: cursorPacketType,
                            seq: UInt32(i + 1), timestampUs: fakeClockValues[i], x: 0, y: 0, shape: 0)
    }
    let seqs = packets.map { u32LE($0, 6) }
    let timestamps = packets.map { u64LE($0, 10) }

    check("CursorPacket seq strictly increments 1,2,3 even under a non-monotonic clock", seqs == [1, 2, 3])
    check("CursorPacket timestamps reflect the non-monotonic clock as fed (not reordered)",
          timestamps == fakeClockValues)
    check("CursorPacket: ordering authority is seq, never timestamp (errata cursor proof — timestamp went DOWN between packet 1 and 2, seq did not)",
          seqs == [1, 2, 3] && timestamps[1] < timestamps[0])
}

// ===========================================================================
// MARK: - (j) R3a: fail-closed host infrastructure
//
// A00_REMEDIATION_PLAN.md §5 R3a. Two RescCore pieces are directly
// unit-testable here: HarnessVerdict's pure predicate function and
// VideoEncoder's new checked-status EncoderError cases. The two-process
// lock-contention test proves the flock(2) mechanism InstanceLock.swift's
// acquire(path:profileId:) depends on holds across real process boundaries
// (see the RESC_LOCK_CONTENTION_CHILD branch at the very top of this file
// for why it duplicates InstanceLock's algorithm rather than calling it
// directly). Doctor.swift, RescLog.flushNow(), and HarnessSender's pump are
// exercised only by the manual verification runs (RESC_DOCTOR_INJECT=... and
// the harness sender/receiver pair) — none of those types live in a module
// this executable target can import.
// ===========================================================================

// -- HarnessVerdict.evaluate: baseline + each argument alone flips it --

check("HarnessVerdict: all-clean run -> pass",
      HarnessVerdict.evaluate(sent: 100, acked: 100, outstanding: 0, orderViolations: 0, writeErrors: 0))
check("HarnessVerdict: zero-frame run must NOT pass vacuously -> fail",
      !HarnessVerdict.evaluate(sent: 0, acked: 0, outstanding: 0, orderViolations: 0, writeErrors: 0))
check("HarnessVerdict: acked < sent alone -> fail",
      !HarnessVerdict.evaluate(sent: 100, acked: 99, outstanding: 0, orderViolations: 0, writeErrors: 0))
check("HarnessVerdict: acked > sent alone -> fail",
      !HarnessVerdict.evaluate(sent: 100, acked: 101, outstanding: 0, orderViolations: 0, writeErrors: 0))
check("HarnessVerdict: nonzero outstanding alone -> fail",
      !HarnessVerdict.evaluate(sent: 100, acked: 100, outstanding: 1, orderViolations: 0, writeErrors: 0))
check("HarnessVerdict: nonzero ack-order violations alone -> fail",
      !HarnessVerdict.evaluate(sent: 100, acked: 100, outstanding: 0, orderViolations: 1, writeErrors: 0))
check("HarnessVerdict: nonzero write errors alone -> fail",
      !HarnessVerdict.evaluate(sent: 100, acked: 100, outstanding: 0, orderViolations: 0, writeErrors: 1))
check("HarnessVerdict: every predicate failing simultaneously -> fail",
      !HarnessVerdict.evaluate(sent: 100, acked: 90, outstanding: 5, orderViolations: 2, writeErrors: 1))

// -- VideoEncoder.EncoderError: checked-status descriptions (requirement 1) --

check("EncoderError.sessionCreationFailed description names the status",
      "\(VideoEncoder.EncoderError.sessionCreationFailed(-12345))".contains("-12345"))
check("EncoderError.propertySetFailed description names both the key and the status", {
    let d = "\(VideoEncoder.EncoderError.propertySetFailed(key: "AverageBitRate", status: -12902))"
    return d.contains("AverageBitRate") && d.contains("-12902")
}())
check("EncoderError.prepareFailed description names the status",
      "\(VideoEncoder.EncoderError.prepareFailed(-12903))".contains("-12903"))

// -- Two-process lock contention (R3a): proves flock(2) semantics hold
// across process boundaries — the same mechanism InstanceLock.swift's
// acquire(path:profileId:) uses (open + 0600-reassert + LOCK_EX|LOCK_NB
// flock, duplicated at the top of this file as fixtureCheckAttemptScratchLock
// since this target cannot import InstanceLock directly).

do {
    let scratchPath = NSTemporaryDirectory()
        + "resc-fixture-check-lock-contention-\(ProcessInfo.processInfo.processIdentifier).lock"
    try? FileManager.default.removeItem(atPath: scratchPath)
    defer { try? FileManager.default.removeItem(atPath: scratchPath) }

    // Deliberately wrong permissions before the first open, proving the
    // same 0600-reassertion-on-open fix applied to the real
    // InstanceLock.swift (lock hygiene, R3a) — not just at creation time.
    guard FileManager.default.createFile(atPath: scratchPath, contents: nil) else {
        throw FixtureCheckError(message: "could not create scratch lock file at \(scratchPath)")
    }
    try FileManager.default.setAttributes([.posixPermissions: 0o644], ofItemAtPath: scratchPath)

    guard let parentFd = fixtureCheckAttemptScratchLock(path: scratchPath) else {
        throw FixtureCheckError(message: "parent could not acquire its own scratch lock")
    }
    check("R3a lock contention: parent acquires the scratch lock", true)

    let permsAfterOpen = (try FileManager.default.attributesOfItem(atPath: scratchPath))[.posixPermissions] as? Int
    check("R3a lock contention: 0600 reasserted on open even though the file pre-existed as 0644",
          permsAfterOpen == 0o600)

    let fixtureCheckExecutableURL: URL = {
        if let p = Bundle.main.executablePath { return URL(fileURLWithPath: p) }
        return URL(fileURLWithPath: CommandLine.arguments[0])
    }()

    func spawnContentionChild() throws -> Int32 {
        let process = Process()
        process.executableURL = fixtureCheckExecutableURL
        process.environment = ProcessInfo.processInfo.environment.merging(
            ["RESC_LOCK_CONTENTION_CHILD": scratchPath]) { _, new in new }
        try process.run()
        process.waitUntilExit()
        return process.terminationStatus
    }

    let childWhileHeld = try spawnContentionChild()
    check("R3a lock contention: child exits 20 (INSTANCE_LOCK_HELD) while the parent holds the lock",
          childWhileHeld == 20)

    close(parentFd)

    let childAfterRelease = try spawnContentionChild()
    check("R3a lock contention: child exits 0 after the parent releases the lock",
          childAfterRelease == 0)
} catch {
    print("FAIL R3a lock contention setup: \(error)")
    failures += 1
}

// MARK: - (h2) ERR-01 write-level barrier proof (C2 writer spy)

// A00_COMPLETION_REPORT_AMENDED_review.md finding 3: the (h) traces prove the
// phase MODEL rejects pre-activation sends; the normative requirement is
// about the WRITE boundary. A deterministic scheduler + writer spy around
// the shared outbound gate records every send ATTEMPT (kind, written?), so
// the retained attempt traces prove the gate — not scheduling luck —
// prevented every pre-activation write, across named reordered cross-TCP
// schedules. Twin: ubuntu-client/crates/protocol/tests/err01_writer_spy.rs.

do {
    let runId: UInt64 = 0x0102_0304_0506_0708
    let spyFacts = V3Dispatch.DispatchFacts(run: .active(runId), diagnostics: .normal,
                                            oldestOutstandingOrdinal: nil)

    struct SpyAttempt: Equatable {
        let kind: V3Dispatch.OutboundKind
        let written: Bool
    }

    final class ClientGate {
        var phase = V3Dispatch.Phase.profileAccepted
        var attempts: [SpyAttempt] = []
        let facts: V3Dispatch.DispatchFacts
        init(_ facts: V3Dispatch.DispatchFacts) { self.facts = facts }

        func trySend(_ kind: V3Dispatch.OutboundKind) {
            switch V3Dispatch.noteOutbound(role: .client, phase: phase, facts: facts, kind: kind) {
            case .success(let next):
                attempts.append(SpyAttempt(kind: kind, written: true))
                phase = next
            case .failure:
                attempts.append(SpyAttempt(kind: kind, written: false))
            }
        }
        func onVideoAckNoted() {
            if case .success(let next) = V3Dispatch.noteVideoAck(phase) { phase = next }
        }
        func onInboundHeartbeat(_ env: Resc_V3_Envelope) -> Bool {
            if case .success(.accepted(let next, _)) =
                V3Dispatch.validateInbound(role: .client, phase: phase, facts: facts, env: env) {
                phase = next
                return true
            }
            return false
        }
        var writes: [V3Dispatch.OutboundKind] { attempts.filter { $0.written }.map { $0.kind } }
    }

    var hb = Resc_V3_Envelope()
    hb.sessionRunID = runId
    hb.protocolVersion = 3
    hb.heartbeat = Resc_V3_Heartbeat()

    // -- S1: correct schedule --
    let g1 = ClientGate(spyFacts)
    g1.trySend(.keyEvent)
    g1.trySend(.heartbeat)
    g1.onVideoAckNoted()
    g1.trySend(.keyEvent)
    g1.trySend(.frameAck)
    g1.trySend(.heartbeat)
    check("C2 S1: activation accepted in videoAckAccepted", g1.onInboundHeartbeat(hb))
    g1.trySend(.keyEvent)
    g1.trySend(.heartbeat)
    check("C2 S1: retained attempt trace exact", g1.attempts == [
        SpyAttempt(kind: .keyEvent, written: false),
        SpyAttempt(kind: .heartbeat, written: false),
        SpyAttempt(kind: .keyEvent, written: false),
        SpyAttempt(kind: .frameAck, written: true),
        SpyAttempt(kind: .heartbeat, written: false),
        SpyAttempt(kind: .keyEvent, written: true),
        SpyAttempt(kind: .heartbeat, written: true),
    ])
    check("C2 S1: writes are FrameAck then first post-barrier input",
          g1.writes == [.frameAck, .keyEvent, .heartbeat])

    // -- S2: control-first (reordered) --
    let g2 = ClientGate(spyFacts)
    check("C2 S2: activation before ack-note rejected", !g2.onInboundHeartbeat(hb))
    g2.trySend(.keyEvent)
    g2.onVideoAckNoted()
    g2.trySend(.keyEvent)
    check("C2 S2: activation accepted after ack-note", g2.onInboundHeartbeat(hb))
    g2.trySend(.keyEvent)
    check("C2 S2: attempt trace exact (two refused, one written)", g2.attempts == [
        SpyAttempt(kind: .keyEvent, written: false),
        SpyAttempt(kind: .keyEvent, written: false),
        SpyAttempt(kind: .keyEvent, written: true),
    ])

    // -- S3: delayed-activation sweep --
    let attemptsPerRun = 5
    for activationAt in 0...attemptsPerRun {
        let g = ClientGate(spyFacts)
        g.onVideoAckNoted()
        for i in 0..<attemptsPerRun {
            if i == activationAt { _ = g.onInboundHeartbeat(hb) }
            g.trySend(.keyEvent)
        }
        let expectedWrites = attemptsPerRun - min(activationAt, attemptsPerRun)
        check("C2 S3[\(activationAt)]: writes == post-activation attempts (\(expectedWrites))",
              g.writes.count == expectedWrites && g.writes.allSatisfy { $0 == .keyEvent })
    }

    // -- S4: host activation write + first input accepted --
    var hostPhase = V3Dispatch.Phase.profileAccepted
    var hostAttempts: [SpyAttempt] = []
    func hostTryActivation() {
        switch V3Dispatch.noteOutbound(role: .host, phase: hostPhase, facts: spyFacts, kind: .heartbeat) {
        case .success(let next): hostAttempts.append(SpyAttempt(kind: .heartbeat, written: true)); hostPhase = next
        case .failure: hostAttempts.append(SpyAttempt(kind: .heartbeat, written: false))
        }
    }
    hostTryActivation() // refused pre-ack-note
    if case .success(let next) = V3Dispatch.noteVideoAck(hostPhase) { hostPhase = next }
    hostTryActivation() // written
    check("C2 S4: exactly one activation write, only after ack-note", hostAttempts == [
        SpyAttempt(kind: .heartbeat, written: false),
        SpyAttempt(kind: .heartbeat, written: true),
    ] && hostPhase == .active)
    var key = Resc_V3_Envelope()
    key.sessionRunID = runId
    key.protocolVersion = 3
    key.keyEvent = .with { k in k.hidUsage = 4; k.isDown = true; k.modifiers = 0 }
    check("C2 S4: first post-barrier input accepted", {
        if case .success(.accepted(let next, _)) =
            V3Dispatch.validateInbound(role: .host, phase: hostPhase, facts: spyFacts, env: key) {
            return next == .active
        }
        return false
    }())
}

if failures > 0 {
    print("resc-fixture-check: \(failures) FAILURE(S)")
    exit(1)
}
print("resc-fixture-check: all checks passed")
