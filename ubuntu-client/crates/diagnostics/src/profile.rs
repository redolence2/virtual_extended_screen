//! Canonical PersonalProfile handling (plan v11 §2, CONTRACT_ERRATA.md proof 3).
//!
//! Canonical form: UTF-8 JSON, keys sorted lexicographically, no whitespace,
//! base-10 integers, NFC strings (all current values are ASCII, so NFC is
//! trivially satisfied; revisit if a non-ASCII value is ever added).
//! `profile_hash` = first 8 bytes of SHA-256 over the exact canonical bytes,
//! used verbatim as opaque bytes — never reinterpreted as an integer.
//!
//! Stage-1 note (ERR-02): the placeholder backend id `TBD-A00` is accepted
//! ONLY by the canonicalization fixture and A0.0 measurement tooling; a
//! normal handshake or final-profile doctor must reject it.

use sha2::{Digest, Sha256};

pub const PROFILE_ID: &str = "moyunfei-desk-1";
pub const PLACEHOLDER_BACKEND: &str = "TBD-A00";

/// The two closed decoder-backend configuration ids (ERR-02). Their complete
/// option sets are frozen in docs/WIRE.md §Backend.
pub const BACKEND_CUVID: &str = "cuvid-lowdelay";
pub const BACKEND_SW1: &str = "sw1-lowdelay";

/// Re-serialize a parsed JSON document into canonical bytes: minified with
/// lexicographically sorted keys. `serde_json::Value` maps preserve insertion
/// order by default, so we rebuild through `BTreeMap` ordering recursively.
pub fn canonicalize(value: &serde_json::Value) -> Vec<u8> {
    fn sort(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let sorted: std::collections::BTreeMap<_, _> =
                    map.iter().map(|(k, v)| (k.clone(), sort(v))).collect();
                serde_json::Value::Object(sorted.into_iter().collect())
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(sort).collect())
            }
            other => other.clone(),
        }
    }
    // `to_string` on a Value is minified; object iteration order is the
    // (sorted) insertion order rebuilt above.
    sort(value).to_string().into_bytes()
}

/// First 8 bytes of SHA-256 over exactly `bytes` (no trailing newline —
/// errata proof 3).
pub fn hash8(bytes: &[u8]) -> [u8; 8] {
    let digest = Sha256::digest(bytes);
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

/// True when the profile still carries the A0.0 placeholder backend and must
/// therefore be rejected by any normal handshake or doctor run (ERR-02).
pub fn is_placeholder_backend(profile: &serde_json::Value) -> bool {
    profile
        .get("decoder_backend")
        .and_then(|v| v.as_str())
        .map(|s| s == PLACEHOLDER_BACKEND)
        .unwrap_or(false)
}

/// Validate a runtime (non-fixture) profile document: canonical-bytes
/// round-trip, known backend id, and required keys present.
pub fn validate_runtime_profile(bytes: &[u8]) -> Result<serde_json::Value, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| format!("profile parse: {e}"))?;
    if canonicalize(&value) != bytes {
        return Err("profile bytes are not in canonical form".into());
    }
    if is_placeholder_backend(&value) {
        return Err(format!(
            "decoder_backend is the {PLACEHOLDER_BACKEND} placeholder — \
             rejected outside A0.0 tooling (ERR-02)"
        ));
    }
    match value.get("decoder_backend").and_then(|v| v.as_str()) {
        Some(BACKEND_CUVID) | Some(BACKEND_SW1) => {}
        other => return Err(format!("unknown decoder_backend: {other:?}")),
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned by review 11 and CONTRACT_ERRATA.md proof 3.
    const PINNED_SHA256_PREFIX: [u8; 8] = [0x0c, 0xc2, 0x24, 0x96, 0x62, 0x88, 0x05, 0x97];

    fn fixture_bytes() -> Vec<u8> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../proto/fixtures/profile.canonical.json"
        );
        std::fs::read(path).expect("fixture profile.canonical.json must exist")
    }

    #[test]
    fn fixture_hash_matches_pinned_prefix() {
        let bytes = fixture_bytes();
        assert!(
            !bytes.ends_with(b"\n"),
            "fixture must not contain a trailing newline (errata proof 3)"
        );
        assert_eq!(hash8(&bytes), PINNED_SHA256_PREFIX);
    }

    #[test]
    fn fixture_is_already_canonical() {
        let bytes = fixture_bytes();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(canonicalize(&value), bytes);
    }

    #[test]
    fn placeholder_backend_rejected_at_runtime() {
        let bytes = fixture_bytes();
        let err = validate_runtime_profile(&bytes).unwrap_err();
        assert!(err.contains("TBD-A00"), "got: {err}");
    }

    #[test]
    fn canonicalize_sorts_and_minifies() {
        let v: serde_json::Value =
            serde_json::from_str("{\"b\": 2, \"a\": {\"d\": 4, \"c\": 3}}").unwrap();
        assert_eq!(canonicalize(&v), b"{\"a\":{\"c\":3,\"d\":4},\"b\":2}");
    }
}
