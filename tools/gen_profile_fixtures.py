#!/usr/bin/env python3
"""Generate the ERR-07 non-ASCII profile-rejection fixtures (idempotent).

Two fixtures carry the same logical profile — the canonical placeholder
profile with two changes: `profile_id` contains one non-ASCII character
(U+00E9, e-acute) and `decoder_backend` is the real `sw1-lowdelay` id.
The backend is real so that, WITHOUT the ERR-07 ASCII gate, nothing else
in `validate_runtime_profile` would reject the raw fixture — the negative
tests therefore attribute rejection to the ASCII rule alone.

- profile_nonascii_raw.json      the non-ASCII value as raw UTF-8 bytes;
                                 sorted + minified, so it passes sort/minify
                                 canonical-form checks (serde_json emits raw
                                 UTF-8, so its round-trip holds in Rust)
- profile_nonascii_escaped.json  the same value as a \\u00e9 escape — the
                                 alternate JSON encoding of the same string,
                                 proving detection happens on the PARSED
                                 document, not on the byte stream

Both ends must reject BOTH fixtures with their ASCII-rule verdict
(Swift `ProfileError.asciiViolation`, Rust "ERR-07" error string).
Normative rule: CONTRACT_ERRATA.md ERR-07. No trailing newline (errata
implementation proof 3 applies to all profile fixtures).

Also sanity-verifies profile.canonical.json against its pinned SHA-256.
"""

import hashlib
import json
import pathlib
import sys

FIXTURES = pathlib.Path(__file__).resolve().parent.parent / "proto" / "fixtures"

CANONICAL_SHA256 = "0cc22496628805973f8d52292e7f838b95ec023faf658d71dd862f3fbf4ed6ff"

NONASCII_PROFILE_ID = "moyunfei-désk-1"  # one non-ASCII char, U+00E9


def build_profile() -> dict:
    base = json.loads((FIXTURES / "profile.canonical.json").read_bytes())
    base["profile_id"] = NONASCII_PROFILE_ID
    base["decoder_backend"] = "sw1-lowdelay"
    return base


def dump(profile: dict, ensure_ascii: bool) -> bytes:
    # sort_keys + tight separators = the canonical sort/minify shape;
    # ensure_ascii toggles raw UTF-8 vs \uXXXX escape encoding.
    return json.dumps(
        profile, sort_keys=True, separators=(",", ":"), ensure_ascii=ensure_ascii
    ).encode("utf-8")


def write(name: str, data: bytes) -> None:
    path = FIXTURES / name
    if path.exists() and path.read_bytes() == data:
        print(f"unchanged {name} ({len(data)} B, sha256 {hashlib.sha256(data).hexdigest()[:16]})")
        return
    path.write_bytes(data)
    print(f"wrote     {name} ({len(data)} B, sha256 {hashlib.sha256(data).hexdigest()[:16]})")


def main() -> int:
    canonical = (FIXTURES / "profile.canonical.json").read_bytes()
    got = hashlib.sha256(canonical).hexdigest()
    if got != CANONICAL_SHA256:
        print(f"FATAL: profile.canonical.json sha256 {got} != pinned {CANONICAL_SHA256}")
        return 1
    if canonical.endswith(b"\n"):
        print("FATAL: profile.canonical.json has a trailing newline")
        return 1

    profile = build_profile()
    raw = dump(profile, ensure_ascii=False)
    escaped = dump(profile, ensure_ascii=True)

    assert b"\xc3\xa9" in raw and b"\\u00e9" not in raw
    assert b"\\u00e9" in escaped and b"\xc3\xa9" not in escaped
    assert not raw.endswith(b"\n") and not escaped.endswith(b"\n")
    # Same parsed document either way.
    assert json.loads(raw) == json.loads(escaped) == profile

    write("profile_nonascii_raw.json", raw)
    write("profile_nonascii_escaped.json", escaped)
    return 0


if __name__ == "__main__":
    sys.exit(main())
