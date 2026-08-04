import Foundation
import CryptoKit

/// Canonical PersonalProfile handling (plan v11 §2, CONTRACT_ERRATA.md proof 3).
///
/// Canonical form: UTF-8 JSON, keys sorted lexicographically, no whitespace,
/// base-10 integers, ASCII-only keys and string values (ERR-07 — replaces the
/// former NFC rule; the ASCII check runs on the parsed document BEFORE the
/// canonical-bytes comparison so both language validators return the same
/// verdict on the same bytes regardless of serializer escaping differences).
/// `profile_hash` = first 8 bytes of SHA-256 over the exact canonical bytes,
/// used verbatim as opaque bytes.
///
/// Stage-1 note (ERR-02): the placeholder backend id `TBD-A00` is accepted
/// ONLY by the canonicalization fixture and A0.0 measurement tooling; a
/// normal handshake or final-profile doctor must reject it.
public enum CanonicalProfile {

    public static let profileId = "moyunfei-desk-1"
    public static let placeholderBackend = "TBD-A00"

    /// The two closed decoder-backend configuration ids (ERR-02);
    /// complete option sets frozen in docs/WIRE.md §Backend.
    public static let backendCuvid = "cuvid-lowdelay"
    public static let backendSw1 = "sw1-lowdelay"

    public enum ProfileError: Error, CustomStringConvertible {
        case parse(String)
        case asciiViolation(String)
        case notCanonical
        case placeholderBackend
        case unknownBackend(String)

        public var description: String {
            switch self {
            case .parse(let detail): return "profile parse: \(detail)"
            case .asciiViolation(let detail):
                return "profile violates ASCII-only rule (ERR-07): \(detail)"
            case .notCanonical: return "profile bytes are not in canonical form"
            case .placeholderBackend:
                return "decoder_backend is the TBD-A00 placeholder — rejected outside A0.0 tooling (ERR-02)"
            case .unknownBackend(let backend): return "unknown decoder_backend: \(backend)"
            }
        }
    }

    /// Re-serialize a parsed JSON document into canonical bytes: minified
    /// with lexicographically sorted keys. JSONSerialization without
    /// .prettyPrinted emits no whitespace; .sortedKeys sorts recursively.
    public static func canonicalize(_ object: Any) throws -> Data {
        try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
    }

    /// First 8 bytes of SHA-256 over exactly `bytes` (no trailing newline —
    /// errata proof 3).
    public static func hash8(_ bytes: Data) -> Data {
        Data(SHA256.hash(data: bytes).prefix(8))
    }

    public static func isPlaceholderBackend(_ profile: [String: Any]) -> Bool {
        (profile["decoder_backend"] as? String) == placeholderBackend
    }

    /// ERR-07: every profile key and string value must consist only of ASCII
    /// characters. Walks the PARSED document (covering both raw UTF-8 and
    /// \uXXXX-escaped byte encodings) and returns the path of the first
    /// violation, or nil. Keys are visited in sorted order so the "first"
    /// hit is deterministic.
    public static func firstNonASCII(_ object: Any, path: String = "") -> String? {
        if let dict = object as? [String: Any] {
            for (key, value) in dict.sorted(by: { $0.key < $1.key }) {
                if !key.allSatisfy({ $0.isASCII }) {
                    return "non-ASCII key at \(path)/\(key)"
                }
                if let hit = firstNonASCII(value, path: "\(path)/\(key)") { return hit }
            }
            return nil
        }
        if let array = object as? [Any] {
            for (index, value) in array.enumerated() {
                if let hit = firstNonASCII(value, path: "\(path)[\(index)]") { return hit }
            }
            return nil
        }
        if let string = object as? String, !string.allSatisfy({ $0.isASCII }) {
            return "non-ASCII string value at \(path)"
        }
        return nil
    }

    /// Validate a runtime (non-fixture) profile document: canonical-bytes
    /// round-trip, known backend id.
    @discardableResult
    public static func validateRuntimeProfile(_ bytes: Data) throws -> [String: Any] {
        let parsed: Any
        do {
            parsed = try JSONSerialization.jsonObject(with: bytes)
        } catch {
            throw ProfileError.parse("\(error)")
        }
        guard let object = parsed as? [String: Any] else {
            throw ProfileError.parse("top level is not an object")
        }
        // ERR-07 gate first — before the canonical-bytes comparison — so a
        // non-ASCII profile gets the same verdict from both language
        // validators instead of falling into serializer-specific
        // notCanonical differences.
        if let violation = firstNonASCII(object) {
            throw ProfileError.asciiViolation(violation)
        }
        guard try canonicalize(object) == bytes else {
            throw ProfileError.notCanonical
        }
        if isPlaceholderBackend(object) {
            throw ProfileError.placeholderBackend
        }
        switch object["decoder_backend"] as? String {
        case backendCuvid?, backendSw1?: break
        case let other: throw ProfileError.unknownBackend(other ?? "<missing>")
        }
        return object
    }
}
