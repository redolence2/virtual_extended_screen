import Foundation
import RescCore
import RescProto
import SwiftProtobuf

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

if failures > 0 {
    print("resc-fixture-check: \(failures) FAILURE(S)")
    exit(1)
}
print("resc-fixture-check: all checks passed")
