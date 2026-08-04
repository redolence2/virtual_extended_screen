#!/usr/bin/env python3
"""gen_fixtures.py — deterministic generator for RESC's Stage-1
profile-independent binary wire fixtures.

Normative source: docs/WIRE.md (this script must match it exactly; if they
ever disagree, docs/WIRE.md is normative and this script is wrong). See also
IMPLEMENTATION_PLAN_V11.md §5 and CONTRACT_ERRATA.md.

"Profile-independent" (plan v11 §12, Stage-1 freeze scope): these fixtures
use fixed, arbitrary-but-documented constants (session_run_id, the
placeholder profile_hash) rather than any Stage-2-measured profile value
(e.g. max_record_bytes), so they hold regardless of what Stage 2 measures.

Usage:
    python3 tools/gen_fixtures.py

Writes proto/fixtures/*.bin, proto/fixtures/malformed/*.bin, and
proto/fixtures/README.md. Stdlib only.

Deterministic and idempotent: every byte is recomputed from the constants
below on every run and the output files are overwritten, so re-running
produces byte-identical output.
"""

import hashlib
import struct
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
FIXTURES_DIR = REPO_ROOT / "proto" / "fixtures"
MALFORMED_DIR = FIXTURES_DIR / "malformed"

# ---------------------------------------------------------------------------
# Shared constants
# ---------------------------------------------------------------------------

# session_run_id used by every fixture in this generator (docs/WIRE.md §2, §5).
RUN_ID = 0x1122334455667788

# profile_hash placeholder (docs/WIRE.md §9 / CONTRACT_ERRATA.md "Profile
# hash bytes" proof): first 8 bytes of SHA-256 over the §9 canonical JSON,
# prefix 0cc2249662880597.
PROFILE_HASH = bytes.fromhex("0cc2249662880597")
assert len(PROFILE_HASH) == 8

# Magic bytes (docs/WIRE.md §2, §4, §5) — literal, not numeric encodings.
MAGIC_VIDEO_HELLO = bytes.fromhex("52534356")      # "RSCV"
MAGIC_VIDEO_HELLO_ACK = bytes.fromhex("52534341")  # "RSCA"
MAGIC_FRAME_HEADER = bytes.fromhex("5646")         # "VF"
MAGIC_UDP_PREFIX = bytes.fromhex("52455343")       # "RESC"

# VideoHelloAck status bytes (docs/WIRE.md §2).
ACK_OK = 0
ACK_MISMATCH = 1
ACK_BUSY = 2
ACK_INTERNAL = 3

# UDP record types (docs/WIRE.md §5).
UDP_TYPE_CURSOR = 1
UDP_TYPE_MOVE = 2

CONSUMED_BY = "Swift and Rust wire tests, Stage-1"

# ---------------------------------------------------------------------------
# Little-endian packing helpers (docs/WIRE.md global rule: LE everywhere in
# non-protobuf records)
# ---------------------------------------------------------------------------


def u8(v: int) -> bytes:
    return struct.pack("<B", v)


def u16le(v: int) -> bytes:
    return struct.pack("<H", v)


def u32le(v: int) -> bytes:
    return struct.pack("<I", v)


def u64le(v: int) -> bytes:
    return struct.pack("<Q", v)


def i32le(v: int) -> bytes:
    return struct.pack("<i", v)


def f32le(v: float) -> bytes:
    return struct.pack("<f", v)


# ---------------------------------------------------------------------------
# Record builders (docs/WIRE.md §2 VideoHello/VideoHelloAck, §4 frame header,
# §5 UDP move/cursor)
# ---------------------------------------------------------------------------


def build_video_hello() -> bytes:
    """docs/WIRE.md §2 — VideoHello, 32 B, host->client."""
    rec = (
        MAGIC_VIDEO_HELLO
        + u8(3)              # ver
        + u8(0)               # res (reserved)
        + u16le(32)            # len
        + u64le(RUN_ID)        # session_run_id
        + PROFILE_HASH         # profile_hash[8]
        + u64le(0)             # res (reserved)
    )
    assert len(rec) == 32, len(rec)
    return rec


def build_video_hello_ack(status: int) -> bytes:
    """docs/WIRE.md §2 — VideoHelloAck, 16 B, client->host."""
    rec = (
        MAGIC_VIDEO_HELLO_ACK
        + u8(3)              # ver
        + u8(status)          # status
        + u16le(16)            # len
        + u64le(RUN_ID)        # session_run_id
    )
    assert len(rec) == 16, len(rec)
    return rec


def build_frame_header(flags: int, frame_ordinal: int, capture_seq: int,
                        content_capture_ts_us: int, payload_len: int) -> bytes:
    """docs/WIRE.md §4 — frame record header, 32 B (header only)."""
    rec = (
        MAGIC_FRAME_HEADER
        + u8(32)                          # headerLen
        + u8(flags)                        # flags
        + u64le(frame_ordinal)             # frameOrdinal
        + u32le(capture_seq)               # captureSeq
        + u64le(content_capture_ts_us)     # contentCaptureTs_us
        + u32le(0)                         # res (reserved)
        + u32le(payload_len)               # payloadLen
    )
    assert len(rec) == 32, len(rec)
    return rec


def udp_prefix(record_type: int) -> bytes:
    """docs/WIRE.md §5 — 14 B UDP prefix shared by move and cursor."""
    rec = (
        MAGIC_UDP_PREFIX
        + u8(3)                # ver
        + u8(record_type)       # type: 1=cursor, 2=move
        + u64le(RUN_ID)         # session_run_id
    )
    assert len(rec) == 14, len(rec)
    return rec


def build_move(seq: int, x: int, y: int) -> bytes:
    """docs/WIRE.md §5 — move datagram, 26 B, client->host."""
    rec = udp_prefix(UDP_TYPE_MOVE) + u32le(seq) + i32le(x) + i32le(y)
    assert len(rec) == 26, len(rec)
    return rec


def build_cursor(seq: int, timestamp_us: int, x_px: int, y_px: int,
                  shape_id: int, hotspot_x_px: int, hotspot_y_px: int,
                  cursor_scale: float) -> bytes:
    """docs/WIRE.md §5 — cursor datagram, 43 B, host->client."""
    rec = (
        udp_prefix(UDP_TYPE_CURSOR)
        + u32le(seq)
        + u64le(timestamp_us)
        + i32le(x_px)
        + i32le(y_px)
        + u8(shape_id)
        + u16le(hotspot_x_px)
        + u16le(hotspot_y_px)
        + f32le(cursor_scale)
    )
    assert len(rec) == 43, len(rec)
    return rec


# ---------------------------------------------------------------------------
# Fixture assembly
# ---------------------------------------------------------------------------


def build_valid_fixtures() -> "dict[str, bytes]":
    return {
        "videohello.bin": build_video_hello(),
        "videohelloack_ok.bin": build_video_hello_ack(ACK_OK),
        "videohelloack_mismatch.bin": build_video_hello_ack(ACK_MISMATCH),
        "videohelloack_busy.bin": build_video_hello_ack(ACK_BUSY),
        "videohelloack_internal.bin": build_video_hello_ack(ACK_INTERNAL),
        "frame_header_min.bin": build_frame_header(
            flags=0x01, frame_ordinal=1, capture_seq=0,
            content_capture_ts_us=0, payload_len=0,
        ),
        "move.bin": build_move(seq=1, x=100, y=200),
        "cursor.bin": build_cursor(
            seq=1, timestamp_us=0, x_px=100, y_px=200, shape_id=0,
            hotspot_x_px=0, hotspot_y_px=0, cursor_scale=1.0,
        ),
    }


def build_malformed_fixtures(valid: "dict[str, bytes]") -> "dict[str, bytes]":
    # bad_magic_hello: first byte of videohello.bin bitwise-inverted (XOR
    # 0xFF) — guarantees a magic that can never match "RSCV".
    bad_magic_hello = bytearray(valid["videohello.bin"])
    bad_magic_hello[0] ^= 0xFF
    bad_magic_hello = bytes(bad_magic_hello)
    assert len(bad_magic_hello) == 32

    # bad_length_hello: len field (offset 6-7) forced to 31 instead of 32.
    # Physical record size is unchanged (32 B) — only the self-declared
    # length field disagrees with reality.
    bad_length_hello = bytearray(valid["videohello.bin"])
    bad_length_hello[6:8] = u16le(31)
    bad_length_hello = bytes(bad_length_hello)
    assert len(bad_length_hello) == 32

    # nonzero_reserved_hello: the u8 reserved field (offset 5) set to 1.
    nonzero_reserved_hello = bytearray(valid["videohello.bin"])
    nonzero_reserved_hello[5] = 1
    nonzero_reserved_hello = bytes(nonzero_reserved_hello)
    assert len(nonzero_reserved_hello) == 32

    # unknown_status_ack: status byte = 9, outside the frozen 0-3 range.
    unknown_status_ack = build_video_hello_ack(9)
    assert len(unknown_status_ack) == 16

    # unknown_flag_frame: flags = 0x03 — bit0 (keyframe-claim) plus bit1,
    # which carries no assigned meaning.
    unknown_flag_frame = build_frame_header(
        flags=0x03, frame_ordinal=1, capture_seq=0,
        content_capture_ts_us=0, payload_len=0,
    )
    assert len(unknown_flag_frame) == 32

    # overflow_frame: payloadLen = 0xFFFFFFFF (u32 max). headerLen+payloadLen
    # (checked/widened to u64) vastly exceeds any plausible max_record_bytes,
    # so this fixture's classification holds regardless of the eventual
    # Stage-2 profile value.
    overflow_frame = build_frame_header(
        flags=0x01, frame_ordinal=1, capture_seq=0,
        content_capture_ts_us=0, payload_len=0xFFFFFFFF,
    )
    assert len(overflow_frame) == 32

    # short_move: move.bin truncated by one byte (25 B instead of 26 B).
    short_move = valid["move.bin"][:-1]
    assert len(short_move) == 25

    # long_cursor: cursor.bin with one extra trailing byte (44 B instead of
    # 43 B).
    long_cursor = valid["cursor.bin"] + b"\x00"
    assert len(long_cursor) == 44

    return {
        "bad_magic_hello.bin": bad_magic_hello,
        "bad_length_hello.bin": bad_length_hello,
        "nonzero_reserved_hello.bin": nonzero_reserved_hello,
        "unknown_status_ack.bin": unknown_status_ack,
        "unknown_flag_frame.bin": unknown_flag_frame,
        "overflow_frame.bin": overflow_frame,
        "short_move.bin": short_move,
        "long_cursor.bin": long_cursor,
    }


# ---------------------------------------------------------------------------
# proto/fixtures/README.md manifest
# ---------------------------------------------------------------------------

# (filename relative to proto/fixtures/, classification, one-line description)
VALID_MANIFEST = [
    ("videohello.bin", "valid",
     "VideoHello; session_run_id=0x1122334455667788, profile_hash=0cc2249662880597"),
    ("videohelloack_ok.bin", "valid", "VideoHelloAck; status=0 OK"),
    ("videohelloack_mismatch.bin", "valid", "VideoHelloAck; status=1 MISMATCH"),
    ("videohelloack_busy.bin", "valid", "VideoHelloAck; status=2 BUSY"),
    ("videohelloack_internal.bin", "valid", "VideoHelloAck; status=3 INTERNAL"),
    ("frame_header_min.bin", "valid",
     "frame header; minimum legal (flags=0x01, frameOrdinal=1, payloadLen=0, header only)"),
    ("move.bin", "valid", "move datagram; seq=1, x=100, y=200"),
    ("cursor.bin", "valid", "cursor datagram; seq=1, x=100, y=200, shape=0, hotspot 0/0, scale=1.0"),
]

MALFORMED_MANIFEST = [
    ("malformed/bad_magic_hello.bin", "PROTOCOL_VIOLATION",
     "VideoHello; first magic byte bitwise-inverted (0x52 XOR 0xFF)"),
    ("malformed/bad_length_hello.bin", "PROTOCOL_VIOLATION",
     "VideoHello; len field = 31 (physical record is still 32 B)"),
    ("malformed/nonzero_reserved_hello.bin", "PROTOCOL_VIOLATION",
     "VideoHello; reserved u8 at offset 5 = 1"),
    ("malformed/unknown_status_ack.bin", "PROTOCOL_VIOLATION",
     "VideoHelloAck; status = 9 (outside the frozen 0-3 range)"),
    ("malformed/unknown_flag_frame.bin", "PROTOCOL_VIOLATION",
     "frame header; flags = 0x03 (bit1 has no assigned meaning)"),
    ("malformed/overflow_frame.bin", "RECORD_CAP_VIOLATION",
     "frame header; payloadLen = 0xFFFFFFFF"),
    ("malformed/short_move.bin", "PROTOCOL_VIOLATION",
     "move datagram truncated to 25 B (of 26 B)"),
    ("malformed/long_cursor.bin", "PROTOCOL_VIOLATION",
     "cursor datagram padded to 44 B (of 43 B) with one trailing zero byte"),
]


def render_manifest_table(rows, all_files: "dict[str, bytes]") -> str:
    lines = ["| Filename | Size (B) | Expected classification | Consumed by | Notes |",
             "|---|---|---|---|---|"]
    for filename, classification, note in rows:
        size = len(all_files[filename])
        lines.append(f"| `{filename}` | {size} | `{classification}` | {CONSUMED_BY} | {note} |")
    return "\n".join(lines)


def render_readme(all_files: "dict[str, bytes]") -> str:
    return f"""# Stage-1 profile-independent binary wire fixtures

Generated by `tools/gen_fixtures.py` — do not hand-edit; regenerate with
`python3 tools/gen_fixtures.py` instead. Byte layouts and classification
rules are normative in `docs/WIRE.md`; this manifest is descriptive.

All fixtures share `session_run_id = 0x1122334455667788` and, where a
profile hash is needed, the `docs/WIRE.md` §9 placeholder `profile_hash`
`0cc2249662880597`. They are profile-independent: none of them depend on a
Stage-2-measured profile value (plan v11 §12 Stage-1 freeze scope).

Classification values (per `docs/WIRE.md`): `valid`, `MALFORMED_FRAMING`,
`PROTOCOL_VIOLATION`, `RECORD_CAP_VIOLATION`.

## Valid fixtures

{render_manifest_table(VALID_MANIFEST, all_files)}

## Malformed fixtures

{render_manifest_table(MALFORMED_MANIFEST, all_files)}
"""


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def write_file(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


def main() -> None:
    valid = build_valid_fixtures()
    malformed = build_malformed_fixtures(valid)

    all_files: "dict[str, bytes]" = {}
    for name, data in valid.items():
        write_file(FIXTURES_DIR / name, data)
        all_files[name] = data
    for name, data in malformed.items():
        write_file(MALFORMED_DIR / name, data)
        all_files[f"malformed/{name}"] = data

    readme_path = FIXTURES_DIR / "README.md"
    readme_path.write_text(render_readme(all_files), encoding="utf-8")

    print("Generated fixtures:")
    for name in sorted(all_files):
        data = all_files[name]
        digest = hashlib.sha256(data).hexdigest()
        print(f"  {name:45s} size={len(data):3d}  sha256={digest}")
    print(f"  {'README.md':45s} size={len(readme_path.read_bytes()):3d}")

    # Size verification (task requirement): hello 32, acks 16, frame header
    # 32, move 26, cursor 43.
    checks = [
        ("VideoHello (videohello.bin)", len(all_files["videohello.bin"]), 32),
        ("VideoHelloAck (videohelloack_ok.bin)", len(all_files["videohelloack_ok.bin"]), 16),
        ("VideoHelloAck (videohelloack_mismatch.bin)", len(all_files["videohelloack_mismatch.bin"]), 16),
        ("VideoHelloAck (videohelloack_busy.bin)", len(all_files["videohelloack_busy.bin"]), 16),
        ("VideoHelloAck (videohelloack_internal.bin)", len(all_files["videohelloack_internal.bin"]), 16),
        ("frame header (frame_header_min.bin)", len(all_files["frame_header_min.bin"]), 32),
        ("move (move.bin)", len(all_files["move.bin"]), 26),
        ("cursor (cursor.bin)", len(all_files["cursor.bin"]), 43),
    ]
    print("\nSize verification:")
    all_ok = True
    for label, actual, expected in checks:
        ok = actual == expected
        all_ok &= ok
        print(f"  {'OK ' if ok else 'FAIL'} {label}: {actual} (expected {expected})")

    if not all_ok:
        raise SystemExit("gen_fixtures.py: size verification FAILED")

    print(f"\nWrote {len(all_files)} fixture(s) + README.md under {FIXTURES_DIR}")


if __name__ == "__main__":
    main()
