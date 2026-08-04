#!/usr/bin/env python3
"""Generate protocol-v3 two-layer inbound dispatch fixtures (R5, idempotent).

Layer 1 (framing-length gate) and Layer 2 (typed validator/router) are each
implemented independently in Rust (`ubuntu-client/crates/protocol/src/
v3dispatch.rs`) and Swift (`mac-host/Sources/RescCore/V3Dispatch.swift`).
This script encodes the direction/phase-legality tables and the per-field
cap / semantic-range rules from docs/WIRE.md §1 + CONTRACT_ERRATA.md ERR-01
exactly ONCE, as a Python oracle, and emits the cases both language test
suites replay against their own implementation. Agreement with this oracle
on every row is what proves the two implementations agree with each other.

`validate_inbound` (receiver-side) is not the whole picture: D1's acceptance
criterion is "both languages pass shared vectors" for the dispatch component
as a whole, so the sender-side `note_outbound`/`note_video_ack` routing is
covered too, via the same encode-the-table-once-in-Python approach
(`outbound_transition`).

Writes:
  proto/fixtures/dispatch_cases.json          framing / state / raw / outbound / video_ack cases
  proto/fixtures/dispatch/unknown_only_oneof.bin
  proto/fixtures/dispatch/empty_envelope.bin
  proto/fixtures/README.md                    "Dispatch fixtures" section appended/refreshed

`fields` values for `bytes` proto fields are hex strings under a `*_hex` key
(e.g. `profile_hash_hex`); every other field is a plain JSON bool/int/string.
`warm_strength` is a JSON number except for the one non-finite special case,
which uses the sentinel string "NaN" (raw JSON has no NaN/Infinity literal).
`verdict` is either `"accept:<phase>"` (optionally `:learn` suffixed, meaning
`learned_run_id` must equal `env_run_id` — `:learn` never appears on
`outbound`/`video_ack` rows, since neither function takes an envelope to
learn a run id from) or a `resc_v3::FatalCode` name (`VERSION_MISMATCH`,
`PROTOCOL_VIOLATION`, `RECORD_CAP_VIOLATION`, `MALFORMED_FRAMING`).
Framing-row verdicts are the bare string `"accept"` (no phase — Layer 1
only returns a byte count, not a phase).

`outbound` rows are `{name, role, phase, kind, verdict}` — `kind` is one of
the 13 `OutboundKind` names (snake_case), the full `role x phase x kind`
matrix (2 x 6 x 13 = 156 rows), for `note_outbound`. `video_ack` rows are
`{name, phase, verdict}` — all 6 phases, for `note_video_ack` (role-
independent, per its signature).

Idempotent; stdlib only. Prints "unchanged"/"wrote" per file like
tools/gen_profile_fixtures.py / tools/gen_envelope_fixtures.py.
"""

import hashlib
import json
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
FIXTURES_DIR = REPO_ROOT / "proto" / "fixtures"
DISPATCH_DIR = FIXTURES_DIR / "dispatch"
CASES_PATH = FIXTURES_DIR / "dispatch_cases.json"
README_PATH = FIXTURES_DIR / "README.md"

RUN_ID = 0x0102030405060708
VERSION = 3

PHASES = [
    "bootstrap",
    "announced",
    "profile_accepted",
    "profile_rejected",
    "video_ack_accepted",
    "active",
]
ROLES = ["host", "client"]
# Oneof field-number order from proto/control_v3.proto.
KINDS = [
    "display_settings",
    "key_event",
    "host_profile_announce",
    "profile_result",
    "frame_ack",
    "button_event",
    "scroll_event",
    "clock_ping",
    "clock_pong",
    "fatal_report",
    "release_input",
    "heartbeat",
]
# OutboundKind declaration order from the FROZEN design.
OUTBOUND_KINDS = [
    "host_profile_announce",
    "profile_result_accepted",
    "profile_result_rejected",
    "frame_ack",
    "key_event",
    "button_event",
    "scroll_event",
    "release_input",
    "heartbeat",
    "clock_ping",
    "clock_pong",
    "display_settings",
    "fatal_report",
]

FATAL_CODE = {
    "FATAL_UNSPECIFIED": 0,
    "PROFILE_MISMATCH": 1,
    "BUILD_MISMATCH": 2,
    "VERSION_MISMATCH": 3,
    "MALFORMED_FRAMING": 4,
    "RECORD_CAP_VIOLATION": 5,
    "ENCODER_PROPERTY": 6,
    "PROFILE_INVALID": 7,
    "PROTOCOL_VIOLATION": 8,
}

HEX_DIGITS = "0123456789abcdef"


# ===========================================================================
# The oracle — docs/WIRE.md §1 direction/state table as refined by the
# FROZEN design (ERR-01 Active-phase barrier for host-bound input/heartbeat).
# Encoded ONCE here; ubuntu-client/crates/protocol/src/v3dispatch.rs and
# mac-host/Sources/RescCore/V3Dispatch.swift each reimplement it
# independently and are graded against the cases this produces.
# ===========================================================================


def client_transition(kind, phase, fields):
    """Phase after ROLE=client accepts `kind` while in `phase`, or None if
    illegal (receiver-side direction/phase-legality, step 5)."""
    if kind == "host_profile_announce":
        return "announced" if phase == "bootstrap" else None
    if kind == "display_settings":
        return phase if phase in ("video_ack_accepted", "active") else None
    if kind == "heartbeat":
        if phase == "video_ack_accepted":
            return "active"
        if phase == "active":
            return "active"
        return None
    if kind in ("clock_ping", "clock_pong"):
        return phase if phase in ("profile_accepted", "video_ack_accepted", "active") else None
    if kind == "fatal_report":
        return phase  # legal in every phase (client learns run id in bootstrap)
    # profile_result, frame_ack, key_event, button_event, scroll_event,
    # release_input: wrong direction, never legal inbound at client.
    return None


def host_transition(kind, phase, fields):
    """Phase after ROLE=host accepts `kind` while in `phase`, or None if
    illegal. `fields` matters only for profile_result (accepted bool)."""
    if kind == "profile_result":
        if phase != "announced":
            return None
        return "profile_accepted" if fields["accepted"] else "profile_rejected"
    if kind == "frame_ack":
        return phase if phase in ("video_ack_accepted", "active") else None
    if kind in ("key_event", "button_event", "scroll_event", "release_input"):
        # ERR-01: pre-Ack input is never injected — Active only.
        return "active" if phase == "active" else None
    if kind == "heartbeat":
        return "active" if phase == "active" else None
    if kind in ("clock_ping", "clock_pong"):
        return phase if phase in ("profile_accepted", "video_ack_accepted", "active") else None
    if kind == "fatal_report":
        return None if phase == "bootstrap" else phase
    # host_profile_announce, display_settings: wrong direction at host.
    return None


def outbound_transition(role, phase, kind):
    """Phase after ROLE sends `kind` (an OutboundKind name) while in
    `phase`, or None if illegal — the sender-side mirror of
    client_transition/host_transition, for note_outbound. Mirrors the
    tables already written independently in
    ubuntu-client/crates/protocol/src/v3dispatch.rs's `note_outbound` and
    mac-host/Sources/RescCore/V3Dispatch.swift's `noteOutbound`."""
    if role == "host":
        if kind == "host_profile_announce":
            return "announced" if phase == "bootstrap" else None
        if kind == "display_settings":
            return phase if phase in ("video_ack_accepted", "active") else None
        if kind == "heartbeat":
            # ERR-01: the activation send (VideoAckAccepted -> Active).
            if phase == "video_ack_accepted":
                return "active"
            if phase == "active":
                return "active"
            return None
        if kind in ("clock_ping", "clock_pong"):
            return phase if phase in ("profile_accepted", "video_ack_accepted", "active") else None
        if kind == "fatal_report":
            return phase
        # profile_result_accepted, profile_result_rejected, frame_ack,
        # key_event, button_event, scroll_event, release_input: never a
        # legal host-outbound kind.
        return None
    # role == "client"
    if kind == "profile_result_accepted":
        return "profile_accepted" if phase == "announced" else None
    if kind == "profile_result_rejected":
        return "profile_rejected" if phase == "announced" else None
    if kind == "frame_ack":
        return phase if phase in ("video_ack_accepted", "active") else None
    if kind in ("key_event", "button_event", "scroll_event", "release_input"):
        return "active" if phase == "active" else None
    if kind == "heartbeat":
        # ERR-01: client heartbeats armed only post-activation.
        return "active" if phase == "active" else None
    if kind in ("clock_ping", "clock_pong"):
        return phase if phase in ("profile_accepted", "video_ack_accepted", "active") else None
    if kind == "fatal_report":
        return None if phase == "bootstrap" else phase
    # host_profile_announce, display_settings: never a legal client-outbound
    # kind.
    return None


def cap_violation(kind, fields):
    """Step 4: per-field caps -> RECORD_CAP_VIOLATION."""

    def hexlen(key):
        return len(fields[key]) // 2

    if kind in ("host_profile_announce", "profile_result"):
        if len(fields["build_commit"].encode("ascii")) > 256:
            return True
        if hexlen("profile_canonical_hex") > 4096:
            return True
        if hexlen("profile_hash_hex") != 8:
            return True
    if kind == "fatal_report":
        if len(fields.get("component", "").encode("ascii")) > 256:
            return True
        if len(fields.get("native_domain", "").encode("ascii")) > 256:
            return True
        if len(fields.get("summary", "").encode("ascii")) > 2048:
            return True
    return False


def _build_commit_valid(s):
    return len(s) == 40 and all(c in HEX_DIGITS for c in s)


def semantic_violation(kind, fields):
    """Step 6: semantic ranges -> PROTOCOL_VIOLATION."""
    if kind == "button_event":
        if fields["button"] not in (0, 1, 2):
            return True
    if kind == "display_settings":
        w = fields["warm_strength"]
        if isinstance(w, str):  # non-finite sentinel ("NaN")
            return True
        if not (0.0 <= w <= 1.0):
            return True
    if kind == "profile_result":
        accepted = fields["accepted"]
        if (fields["reject_code"] == FATAL_CODE["FATAL_UNSPECIFIED"]) != accepted:
            return True
        if fields["video_listener_ready"] != accepted:
            return True
    if kind in ("host_profile_announce", "profile_result"):
        if not _build_commit_valid(fields["build_commit"]):
            return True
    return False


def oracle(role, phase, kind, fields, expected_run_id, env_run_id, env_version):
    """Full 6-step validate_inbound reference, steps per docs/WIRE.md §1 +
    the FROZEN design's fixed check order. Returns a verdict string."""
    # 1. protocol version
    if env_version != VERSION:
        return "VERSION_MISMATCH"
    # 2. run id
    learn = False
    if expected_run_id is not None:
        if env_run_id != expected_run_id:
            return "PROTOCOL_VIOLATION"
    else:
        learn = True
    # 3. payload absent — not reachable here; every state/raw case with a
    # `kind` carries a present payload by construction.
    # 4. per-field caps
    if cap_violation(kind, fields):
        return "RECORD_CAP_VIOLATION"
    # 5. direction/phase legality
    transition = client_transition if role == "client" else host_transition
    next_phase = transition(kind, phase, fields)
    if next_phase is None:
        return "PROTOCOL_VIOLATION"
    # 6. semantic ranges
    if semantic_violation(kind, fields):
        return "PROTOCOL_VIOLATION"
    return f"accept:{next_phase}" + (":learn" if learn else "")


# ===========================================================================
# Minimal valid field shapes per payload kind.
# ===========================================================================


def default_fields(kind):
    if kind == "host_profile_announce":
        return {
            "profile_canonical_hex": "41" * 20,  # 20 ASCII 'A' bytes
            "profile_hash_hex": "0102030405060708",
            "build_commit": "a" * 40,
            "build_dirty": False,
        }
    if kind == "profile_result":
        return {
            "accepted": True,
            "profile_canonical_hex": "41" * 20,
            "profile_hash_hex": "0102030405060708",
            "build_commit": "a" * 40,
            "build_dirty": False,
            "reject_code": FATAL_CODE["FATAL_UNSPECIFIED"],
            "video_listener_ready": True,
        }
    if kind == "frame_ack":
        return {"frame_ordinal": 1}
    if kind == "key_event":
        return {"hid_usage": 4, "is_down": True, "modifiers": 0}
    if kind == "button_event":
        return {"button": 0, "is_down": True, "x_px": 1, "y_px": 1, "modifiers": 0}
    if kind == "scroll_event":
        return {"dx": 1, "dy": -1}
    if kind == "clock_ping":
        return {"t1_mono_us": 1, "seq": 1}
    if kind == "clock_pong":
        return {"t1_mono_us": 1, "t2_mono_us": 2, "t3_mono_us": 3, "seq": 1}
    if kind == "display_settings":
        return {"warm_strength": 0.5}
    if kind == "heartbeat":
        return {"t_mono_us": 1}
    if kind == "fatal_report":
        return {
            "code": FATAL_CODE["PROTOCOL_VIOLATION"],
            "component": "x",
            "native_domain": "",
            "native_code": 0,
            "summary": "y",
        }
    if kind == "release_input":
        return {}
    raise ValueError(f"unknown kind {kind}")


def state_row(name, role, phase, kind, fields, expected_run_id, env_run_id=RUN_ID, env_version=VERSION):
    verdict = oracle(role, phase, kind, fields, expected_run_id, env_run_id, env_version)
    return {
        "name": name,
        "role": role,
        "phase": phase,
        "payload": kind,
        "fields": fields,
        "expected_run_id": expected_run_id,
        "env_run_id": env_run_id,
        "env_version": env_version,
        "verdict": verdict,
    }


# The two cells where a client legitimately does not yet know a candidate
# run id (docs/WIRE.md §1 direction table: HostProfileAnnounce is the first
# bootstrap message; FatalReport is legal "once a candidate run id is
# known" — in bootstrap the envelope's own id becomes that candidate).
LEARN_CELLS = {("client", "bootstrap", "host_profile_announce"), ("client", "bootstrap", "fatal_report")}


def build_matrix_rows():
    rows = []
    for kind in KINDS:
        for phase in PHASES:
            for role in ROLES:
                fields = default_fields(kind)
                expected_run_id = None if (role, phase, kind) in LEARN_CELLS else RUN_ID
                rows.append(state_row(f"{role}_{phase}_{kind}", role, phase, kind, fields, expected_run_id))
    assert len(rows) == len(KINDS) * len(PHASES) * len(ROLES) == 144
    return rows


def build_special_rows():
    rows = []

    def add(name, role, phase, kind, fields, expected_run_id, expect):
        row = state_row(f"special_{name}", role, phase, kind, fields, expected_run_id)
        assert row["verdict"] == expect, f"{name}: oracle produced {row['verdict']!r}, expected {expect!r}"
        rows.append(row)

    # -- version=2 -> VERSION_MISMATCH (host, active, heartbeat: otherwise legal) --
    row = state_row("special_bad_version", "host", "active", "heartbeat", default_fields("heartbeat"), RUN_ID,
                     env_run_id=RUN_ID, env_version=2)
    assert row["verdict"] == "VERSION_MISMATCH"
    rows.append(row)

    # -- run-id mismatch (env 0x99, expected RUN_ID, heartbeat at host Active) --
    row = state_row("special_run_id_mismatch", "host", "active", "heartbeat", default_fields("heartbeat"), RUN_ID,
                     env_run_id=0x99, env_version=VERSION)
    assert row["verdict"] == "PROTOCOL_VIOLATION"
    rows.append(row)

    # -- build_commit format, on an otherwise-legal announce at client Bootstrap --
    f = default_fields("host_profile_announce")
    f39 = dict(f, build_commit="a" * 39)
    add("build_commit_39_chars", "client", "bootstrap", "host_profile_announce", f39, None, "PROTOCOL_VIOLATION")
    f41 = dict(f, build_commit="a" * 41)
    add("build_commit_41_chars", "client", "bootstrap", "host_profile_announce", f41, None, "PROTOCOL_VIOLATION")
    fupper = dict(f, build_commit="A" * 40)
    add("build_commit_uppercase", "client", "bootstrap", "host_profile_announce", fupper, None, "PROTOCOL_VIOLATION")
    f40 = dict(f, build_commit="a" * 40)
    add("build_commit_40_lowercase", "client", "bootstrap", "host_profile_announce", f40, None,
        "accept:announced:learn")

    # -- fatal_report caps (host, active: legal every phase except bootstrap) --
    fr = default_fields("fatal_report")
    fr_component = dict(fr, component="c" * 257)
    add("component_257_bytes", "host", "active", "fatal_report", fr_component, RUN_ID, "RECORD_CAP_VIOLATION")
    fr_summary_over = dict(fr, summary="s" * 2049)
    add("summary_2049_bytes", "host", "active", "fatal_report", fr_summary_over, RUN_ID, "RECORD_CAP_VIOLATION")
    fr_summary_exact = dict(fr, summary="s" * 2048)
    add("summary_2048_bytes", "host", "active", "fatal_report", fr_summary_exact, RUN_ID, "accept:active")

    # -- HostProfileAnnounce byte-field caps, same otherwise-legal context --
    pc = dict(f, profile_canonical_hex="41" * 4097)
    add("profile_canonical_4097_bytes", "client", "bootstrap", "host_profile_announce", pc, None,
        "RECORD_CAP_VIOLATION")
    ph7 = dict(f, profile_hash_hex="01" * 7)
    add("profile_hash_7_bytes", "client", "bootstrap", "host_profile_announce", ph7, None, "RECORD_CAP_VIOLATION")
    ph9 = dict(f, profile_hash_hex="01" * 9)
    add("profile_hash_9_bytes", "client", "bootstrap", "host_profile_announce", ph9, None, "RECORD_CAP_VIOLATION")

    # -- semantic ranges --
    be = dict(default_fields("button_event"), button=3)
    add("button_out_of_range", "host", "active", "button_event", be, RUN_ID, "PROTOCOL_VIOLATION")
    ds15 = dict(default_fields("display_settings"), warm_strength=1.5)
    add("warm_strength_over_one", "client", "video_ack_accepted", "display_settings", ds15, RUN_ID,
        "PROTOCOL_VIOLATION")
    ds_nan = dict(default_fields("display_settings"), warm_strength="NaN")
    add("warm_strength_nan", "client", "video_ack_accepted", "display_settings", ds_nan, RUN_ID,
        "PROTOCOL_VIOLATION")

    # -- ProfileResult accepted/reject_code/video_listener_ready "iff" checks --
    pr_base = default_fields("profile_result")
    pr_bad_reject = dict(pr_base, accepted=True, video_listener_ready=True,
                          reject_code=FATAL_CODE["PROTOCOL_VIOLATION"])
    add("profile_result_accepted_with_reject_code", "host", "announced", "profile_result", pr_bad_reject, RUN_ID,
        "PROTOCOL_VIOLATION")
    pr_bad_ready = dict(pr_base, accepted=False, video_listener_ready=True,
                         reject_code=FATAL_CODE["PROTOCOL_VIOLATION"])
    add("profile_result_rejected_with_listener_ready", "host", "announced", "profile_result", pr_bad_ready, RUN_ID,
        "PROTOCOL_VIOLATION")

    # -- cap-violation on a WRONG-DIRECTION message: proves step 4 < step 5 --
    oversized_announce = dict(f, build_commit="a" * 300)
    add("cap_before_direction_oversized_announce_at_host", "host", "active", "host_profile_announce",
        oversized_announce, RUN_ID, "RECORD_CAP_VIOLATION")

    # -- accepted=false transition at host Announced -> ProfileRejected.
    # Two rows differing only in reject_code, to show the accept rule is
    # "any known nonzero code", not a hardcoded PROTOCOL_VIOLATION check.
    pr_rejected_a = dict(pr_base, accepted=False, video_listener_ready=False,
                          reject_code=FATAL_CODE["PROTOCOL_VIOLATION"])
    add("profile_result_rejected_transition_a", "host", "announced", "profile_result", pr_rejected_a, RUN_ID,
        "accept:profile_rejected")
    pr_rejected_b = dict(pr_base, accepted=False, video_listener_ready=False,
                          reject_code=FATAL_CODE["PROFILE_MISMATCH"])
    add("profile_result_rejected_transition_b", "host", "announced", "profile_result", pr_rejected_b, RUN_ID,
        "accept:profile_rejected")

    return rows


# ===========================================================================
# Layer 1 — framing rows
# ===========================================================================


def build_framing_rows():
    cases = [
        (0, "accept"),
        (1, "accept"),
        (65536, "accept"),
        (65536 + 1, "MALFORMED_FRAMING"),
        (0xFFFFFFFF, "MALFORMED_FRAMING"),
    ]
    # 65535 inserted between 1 and 65536 for readability but kept in the
    # `cases` scan order above via a second explicit entry below.
    cases.insert(2, (65535, "accept"))
    rows = []
    for value, verdict in cases:
        rows.append({
            "name": f"len_{value}",
            "prefix_hex": value.to_bytes(4, "little").hex(),
            "verdict": verdict,
        })
    return rows


# ===========================================================================
# note_outbound / note_video_ack rows — the sender-side mirror of the
# validate_inbound "state" matrix above (D1: "both languages pass shared
# vectors" applies to outbound routing too, not only inbound validation).
# ===========================================================================


def build_outbound_rows():
    rows = []
    for kind in OUTBOUND_KINDS:
        for phase in PHASES:
            for role in ROLES:
                next_phase = outbound_transition(role, phase, kind)
                verdict = f"accept:{next_phase}" if next_phase is not None else "PROTOCOL_VIOLATION"
                rows.append({
                    "name": f"{role}_{phase}_{kind}",
                    "role": role,
                    "phase": phase,
                    "kind": kind,
                    "verdict": verdict,
                })
    assert len(rows) == len(OUTBOUND_KINDS) * len(PHASES) * len(ROLES) == 156
    return rows


def build_video_ack_rows():
    rows = []
    for phase in PHASES:
        verdict = "accept:video_ack_accepted" if phase == "profile_accepted" else "PROTOCOL_VIOLATION"
        rows.append({"name": f"video_ack_{phase}", "phase": phase, "verdict": verdict})
    assert len(rows) == 6
    return rows


# ===========================================================================
# Raw byte-vector rows (proto/fixtures/dispatch/*.bin)
# ===========================================================================


def _varint(v):
    out = bytearray()
    while True:
        b = v & 0x7F
        v >>= 7
        if v:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def _tag(field, wire):
    return _varint((field << 3) | wire)


def _field_varint(field, v):
    return _tag(field, 0) + _varint(v)


def build_raw_fixtures():
    """Returns (files: {relname: bytes}, rows: [...])."""
    files = {}
    rows = []

    # unknown_only_oneof.bin: session_run_id=RUN_ID (field 1), protocol_
    # version=3 (field 2), one unknown field 99 varint=1 (tag (99<<3)|0 =
    # 792 -> varint 0x98 0x06). Decodes to a present Envelope with an
    # ABSENT oneof payload (unknown fields never populate a oneof).
    unknown_only = (
        _field_varint(1, RUN_ID)
        + _field_varint(2, VERSION)
        + _tag(99, 0) + _varint(1)
    )
    assert unknown_only[-3:] == bytes([0x98, 0x06, 0x01]), unknown_only.hex()
    files["unknown_only_oneof.bin"] = unknown_only
    rows.append({
        "name": "unknown_only_oneof_absent_payload",
        "file": "dispatch/unknown_only_oneof.bin",
        "role": "host",
        "phase": "active",
        "expected_run_id": RUN_ID,
        "verdict": "PROTOCOL_VIOLATION",
    })

    # empty_envelope.bin: zero bytes. Per proto3 semantics this decodes to
    # Envelope{session_run_id:0, protocol_version:0, payload:None} — EVERY
    # field at its type's zero value, with no way to encode "field absent
    # from the byte stream but non-zero". protocol_version therefore reads
    # as 0, which fails step 1 (VERSION_MISMATCH) before either the step-2
    # run-id check or the step-3 absent-payload check is ever reached,
    # regardless of role/phase/expected_run_id. Two rows are kept (as
    # distinct role/phase/expected_run_id contexts) to match the spec's
    # request for two raw rows over this file; both resolve to
    # VERSION_MISMATCH — see the deviation note in the implementer's report.
    files["empty_envelope.bin"] = b""
    rows.append({
        "name": "empty_envelope_host_active_some_run_id",
        "file": "dispatch/empty_envelope.bin",
        "role": "host",
        "phase": "active",
        "expected_run_id": RUN_ID,
        "verdict": "VERSION_MISMATCH",
    })
    rows.append({
        "name": "empty_envelope_client_bootstrap_null_run_id",
        "file": "dispatch/empty_envelope.bin",
        "role": "client",
        "phase": "bootstrap",
        "expected_run_id": None,
        "verdict": "VERSION_MISMATCH",
    })

    return files, rows


# ===========================================================================
# Write helpers (idempotent; "unchanged"/"wrote" like the sibling generators)
# ===========================================================================


def write_bytes(path, data, label=None):
    label = label or path.name
    if path.exists() and path.read_bytes() == data:
        print(f"unchanged {label} ({len(data)} B, sha256 {hashlib.sha256(data).hexdigest()[:16]})")
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    print(f"wrote     {label} ({len(data)} B, sha256 {hashlib.sha256(data).hexdigest()[:16]})")


DISPATCH_README_SECTION_HEADER = "## Dispatch fixtures"


def update_readme(cases_size, row_counts, files_meta):
    total_rows = sum(row_counts.values())
    breakdown = ", ".join(f"{n} {key}" for key, n in row_counts.items())
    text = README_PATH.read_text()
    marker = f"\n{DISPATCH_README_SECTION_HEADER}\n"
    if marker in text:
        text = text[: text.index(marker)]
    section = [
        "",
        DISPATCH_README_SECTION_HEADER,
        "",
        "Generated by `tools/gen_dispatch_fixtures.py` (R5) — do not hand-edit; regenerate with",
        "`python3 tools/gen_dispatch_fixtures.py` instead. Exercises the two-layer protocol-v3",
        "inbound dispatch (framing-length gate + typed validator/router) AND the sender-side",
        "note_outbound/note_video_ack routing, implemented independently in",
        "`ubuntu-client/crates/protocol/src/v3dispatch.rs` and",
        "`mac-host/Sources/RescCore/V3Dispatch.swift`; both are graded against these cases.",
        "",
        "| Filename | Size (B) | Contents |",
        "|---|---|---|",
        f"| `dispatch_cases.json` | {cases_size} | {total_rows} rows: {breakdown} (see the file's own "
        "doc comment header in the generator for the exact schema) |",
    ]
    for name, size in files_meta:
        purpose = {
            "unknown_only_oneof.bin": "Envelope with a present run id/version and one unknown field "
            "(number 99) only — decodes to an ABSENT oneof payload",
            "empty_envelope.bin": "zero-byte Envelope — every field decodes to its proto3 zero value",
        }[name]
        section.append(f"| `dispatch/{name}` | {size} | {purpose} |")
    section.append("")
    text = text.rstrip("\n") + "\n" + "\n".join(section)
    new_bytes = text.encode("utf-8")
    if README_PATH.read_bytes() == new_bytes:
        print(f"unchanged README.md ({len(new_bytes)} B)")
        return
    README_PATH.write_bytes(new_bytes)
    print(f"wrote     README.md ({len(new_bytes)} B)")


def main():
    framing = build_framing_rows()
    state = build_matrix_rows() + build_special_rows()
    raw_files, raw = build_raw_fixtures()
    outbound = build_outbound_rows()
    video_ack = build_video_ack_rows()

    cases = {"framing": framing, "state": state, "raw": raw, "outbound": outbound, "video_ack": video_ack}
    cases_bytes = (json.dumps(cases, indent=2, ensure_ascii=True) + "\n").encode("ascii")
    write_bytes(CASES_PATH, cases_bytes, label="dispatch_cases.json")

    files_meta = []
    for name in ("unknown_only_oneof.bin", "empty_envelope.bin"):
        data = raw_files[name]
        write_bytes(DISPATCH_DIR / name, data, label=f"dispatch/{name}")
        files_meta.append((name, len(data)))

    row_counts = {key: len(rows) for key, rows in cases.items()}
    update_readme(len(cases_bytes), row_counts, files_meta)


if __name__ == "__main__":
    main()
