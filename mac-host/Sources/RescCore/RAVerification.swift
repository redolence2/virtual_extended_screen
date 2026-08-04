import Foundation

/// HEVC Annex-B NAL-unit scanning for random-access (RA) verification
/// (IMPLEMENTATION_PLAN_V11.md §6: "RA iff NAL 19/20 (CRA rejected);
/// session-first AU carries VPS+SPS+PPS; header claim == parse"). Hoisted
/// out of RemoteDisplayHost/Doctor.swift so FixtureCheck can exercise it
/// against synthetic NAL sequences without a real VideoToolbox encoder;
/// Doctor.swift's `ra_verification` check now calls these functions with
/// unchanged behavior/JSON output.
public struct NalSummary: Equatable {
    public let types: [UInt8]
    public let hasVPS: Bool
    public let hasSPS: Bool
    public let hasPPS: Bool
    public let hasIDR: Bool
    public let hasCRA: Bool
}

/// Minimal Annex-B start-code scanner: returns the HEVC NAL unit type
/// (`(first_byte >> 1) & 0x3F`, ITU-T H.265 §7.3.1.2) found immediately
/// after each `00 00 01` / `00 00 00 01` start code. Doesn't need NAL
/// lengths — RA verification only needs the set of types present.
public func scanAnnexB(_ data: Data) -> NalSummary {
    let raw = [UInt8](data)
    var types: [UInt8] = []
    var i = 0
    let n = raw.count
    while i + 2 < n {
        if raw[i] == 0, raw[i + 1] == 0 {
            var headerStart = -1
            if raw[i + 2] == 1 {
                headerStart = i + 3
            } else if i + 3 < n, raw[i + 2] == 0, raw[i + 3] == 1 {
                headerStart = i + 4
            }
            if headerStart >= 0 {
                if headerStart < n {
                    types.append((raw[headerStart] >> 1) & 0x3F)
                }
                i = headerStart
                continue
            }
        }
        i += 1
    }

    let typeSet = Set(types)
    return NalSummary(
        types: typeSet.sorted(),
        hasVPS: typeSet.contains(32),
        hasSPS: typeSet.contains(33),
        hasPPS: typeSet.contains(34),
        hasIDR: typeSet.contains(19) || typeSet.contains(20),
        hasCRA: typeSet.contains(21)
    )
}

// `Result`'s `Failure` generic parameter requires `Error` conformance, which
// the bare `String` type doesn't have by default; this file's contract
// (spec'd signature `Result<Void, String>`) needs it, so it's added here,
// scoped to this file's use.
extension String: @retroactive Error {}

/// Session-first AU requirement (V11 §6): VPS+SPS+PPS+IDR(19|20) all
/// present, and CRA(21) absent — a CRA anywhere in the AU fails this check
/// even alongside a genuine IDR.
public func validateSessionFirst(_ summary: NalSummary) -> Result<Void, String> {
    if summary.hasCRA {
        return .failure("CRA (type 21) present — session-first AU must use IDR, not CRA")
    }
    guard summary.hasVPS, summary.hasSPS, summary.hasPPS, summary.hasIDR else {
        return .failure(
            "missing required NAL type(s): vps=\(summary.hasVPS) sps=\(summary.hasSPS) "
            + "pps=\(summary.hasPPS) idr=\(summary.hasIDR)")
    }
    return .success(())
}

/// Header keyframe-claim (frame-record `flags` bit0) vs. parsed AU content:
/// the two must agree exactly.
public func keyframeClaimMatches(_ summary: NalSummary, claim: Bool) -> Bool {
    claim == summary.hasIDR
}
