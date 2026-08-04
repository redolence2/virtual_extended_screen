#!/usr/bin/env python3
"""Generate protobuf v3 Envelope fixtures for cross-language round-trip tests.

Hand-encodes proto/control_v3.proto Envelope messages (scalar fields only, so
the encoding is unambiguous: fields in ascending number order, standard varint/
length-delimited wire types). Swift and Rust tests must (a) decode each .bin
and assert the values in envelopes_manifest.json, and (b) re-encode and assert
byte-equality with the fixture (both generators emit fields in field-number
order for these shapes; if an implementation legitimately diverges, the test
falls back to decode-only equality and the divergence must be recorded in
CONTRACT_ERRATA.md).

Idempotent; prints name/size/sha256 per file. Stdlib only.
"""

import hashlib
import json
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
OUT_DIR = REPO_ROOT / "proto" / "fixtures" / "envelopes"

RUN_ID = 0x0102030405060708  # 72623859790382856
PROTOCOL_VERSION = 3


def varint(v: int) -> bytes:
    out = bytearray()
    while True:
        b = v & 0x7F
        v >>= 7
        if v:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def tag(field: int, wire: int) -> bytes:
    return varint((field << 3) | wire)


def field_varint(field: int, v: int) -> bytes:
    return tag(field, 0) + varint(v) if v != 0 else b""  # proto3 default omitted


def field_bytes(field: int, payload: bytes) -> bytes:
    return tag(field, 2) + varint(len(payload)) + payload


def envelope(payload_field: int, payload: bytes) -> bytes:
    return (
        field_varint(1, RUN_ID)
        + field_varint(2, PROTOCOL_VERSION)
        + field_bytes(payload_field, payload)
    )


def build() -> "dict[str, tuple[bytes, dict]]":
    fixtures = {}

    hb = field_varint(1, 1722400000000000)  # t_mono_us
    fixtures["envelope_heartbeat.bin"] = (
        envelope(70, hb),
        {"payload": "heartbeat", "t_mono_us": 1722400000000000},
    )

    ping = field_varint(1, 123456789) + field_varint(2, 7)  # t1_mono_us, seq
    fixtures["envelope_clock_ping.bin"] = (
        envelope(65, ping),
        {"payload": "clock_ping", "t1_mono_us": 123456789, "seq": 7},
    )

    ack = field_varint(1, 42)  # frame_ordinal
    fixtures["envelope_frame_ack.bin"] = (
        envelope(62, ack),
        {"payload": "frame_ack", "frame_ordinal": 42},
    )

    fatal = (
        field_varint(1, 8)                       # code = PROTOCOL_VIOLATION
        + field_bytes(2, b"test")                # component
        + field_bytes(5, b"unit")                # summary
    )
    fixtures["envelope_fatal_report.bin"] = (
        envelope(68, fatal),
        {
            "payload": "fatal_report",
            "code": 8,
            "component": "test",
            "summary": "unit",
        },
    )

    return fixtures


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    fixtures = build()
    manifest = {
        "_comment": "Expected decoded values for envelope round-trip fixtures. "
        "Every envelope: session_run_id = 72623859790382856 (0x0102030405060708), "
        "protocol_version = 3.",
        "session_run_id": RUN_ID,
        "protocol_version": PROTOCOL_VERSION,
        "files": {},
    }
    print("Envelope fixtures:")
    for name, (data, expected) in sorted(fixtures.items()):
        (OUT_DIR / name).write_bytes(data)
        manifest["files"][name] = expected
        digest = hashlib.sha256(data).hexdigest()
        print(f"  {name:<32} size={len(data):>3}  sha256={digest}")
    (OUT_DIR / "envelopes_manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n"
    )
    print(f"Wrote {len(fixtures)} fixture(s) + envelopes_manifest.json under {OUT_DIR}")


if __name__ == "__main__":
    main()
