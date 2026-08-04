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


PROFILE_SECTION_HEADER = (
    "## Profile fixtures (JSON — generator `tools/gen_profile_fixtures.py`, "
    "except the canonical one)"
)
DISPATCH_SECTION_HEADER = "## Dispatch fixtures"


def update_readme(raw: bytes, escaped: bytes) -> None:
    """Maintain this generator's own README section (order-independent).

    `tools/gen_fixtures.py` rewrites the whole manifest from its template and
    `tools/gen_dispatch_fixtures.py` maintains a trailing section — this one
    removes any existing profile section, then re-inserts it BEFORE the
    dispatch section when present (else appends), so running the three
    generators in any order converges on the same file. (R7 matrix finding:
    the section was originally hand-edited into a generated file and a
    regeneration pass silently dropped it.)
    """
    readme = FIXTURES / "README.md"
    original = readme.read_text(encoding="utf-8")
    text = original

    # Drop any existing profile section: from its header up to the next
    # "## " section header (or EOF).
    if PROFILE_SECTION_HEADER in text:
        start = text.index(PROFILE_SECTION_HEADER)
        rest = text[start + len(PROFILE_SECTION_HEADER):]
        nxt = rest.find("\n## ")
        end = len(text) if nxt == -1 else start + len(PROFILE_SECTION_HEADER) + nxt + 1
        text = text[:start].rstrip("\n") + "\n" + text[end:].lstrip("\n")

    section = "\n".join([
        PROFILE_SECTION_HEADER,
        "",
        "`profile.canonical.json` predates the generator and is pinned by SHA-256 (`docs/WIRE.md` §9);",
        "the generator verifies that pin as a sanity check but never rewrites the file. No profile fixture",
        "has a trailing newline (errata implementation proof 3).",
        "",
        "| Filename | Size (B) | Expected verdict | Consumed by | Notes |",
        "|---|---|---|---|---|",
        "| `profile.canonical.json` | 497 | canonical; ASCII-clean; runtime-rejected only for its"
        " `TBD-A00` placeholder backend (ERR-02) | Swift and Rust profile tests, Stage-1 |"
        " placeholder profile; SHA-256 prefix `0cc2249662880597` |",
        f"| `profile_nonascii_raw.json` | {len(raw)} | ASCII-rule rejection (ERR-07), violation at"
        " `profile_id` | Swift and Rust profile tests, Stage-1 | sorted/minified, real"
        " `sw1-lowdelay` backend, `profile_id` carries U+00E9 as raw UTF-8 — without ERR-07"
        " nothing else is guaranteed to reject it |",
        f"| `profile_nonascii_escaped.json` | {len(escaped)} | ASCII-rule rejection (ERR-07),"
        " violation at `profile_id` | Swift and Rust profile tests, Stage-1 | same document with"
        " the value written as the `\\u00e9` escape — proves detection on the parsed document,"
        " not the byte stream |",
        "",
    ])

    if DISPATCH_SECTION_HEADER in text:
        at = text.index(DISPATCH_SECTION_HEADER)
        new_text = text[:at].rstrip("\n") + "\n\n" + section + "\n" + text[at:]
    else:
        new_text = text.rstrip("\n") + "\n\n" + section

    if new_text != original:
        readme.write_text(new_text, encoding="utf-8")
        print("wrote     README.md (profile section)")
    else:
        print("unchanged README.md (profile section)")


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
    update_readme(raw, escaped)
    return 0


if __name__ == "__main__":
    sys.exit(main())
