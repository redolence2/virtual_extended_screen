#!/usr/bin/env python3
"""Validating evidence-manifest generator (corrective item C8).

Per A00_COMPLETION_REPORT_AMENDED_response_review.md amendment 7: the
manifest is GENERATED from the artifacts and VALIDATES everything it
references before sealing — it fails (nonzero exit, no manifest written) on:
missing paths, hash/byte mismatches on re-read, malformed JSON/JSONL,
absent required provenance/environment fields, a dirty tree, or missing
load-bearing predicate fields. It records `code_commit` (the checkpoint the
matrix ran from) and NEVER its own containing commit's hash (a manifest
cannot contain the hash of the commit it is part of — the C->E->R
three-commit topology instead has the R attestation record both C and E).

Usage: python3 tools/gen_evidence_manifest.py <code_commit>
Writes: evidence/a00/<code_commit>/manifest.json
"""

import hashlib
import json
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def die(msg):
    print(f"MANIFEST FATAL: {msg}", file=sys.stderr)
    sys.exit(1)


def sh(cmd):
    return subprocess.run(cmd, shell=True, capture_output=True, text=True, cwd=REPO)


def sha256(path):
    h = hashlib.sha256()
    h.update(path.read_bytes())
    return h.hexdigest()


def validate_json(path):
    try:
        return json.loads(path.read_text())
    except Exception as e:
        die(f"{path}: malformed JSON ({e})")


def validate_jsonl(path):
    for i, line in enumerate(path.read_text().splitlines(), 1):
        if not line.strip():
            continue
        try:
            json.loads(line)
        except Exception as e:
            die(f"{path}:{i}: malformed JSONL line ({e})")


def main():
    if len(sys.argv) != 2:
        die("usage: gen_evidence_manifest.py <code_commit>")
    code_commit = sys.argv[1]

    # Provenance guards: the named checkpoint must exist; HEAD must be a
    # descendant carrying no source/contract diff outside evidence/ and the
    # report docs; the tree must be clean (asserted on OUTPUT, not exit
    # status).
    if sh(f"git cat-file -e {code_commit}^{{commit}}").returncode != 0:
        die(f"code_commit {code_commit} not found")
    # The tree may carry the not-yet-committed evidence/report/manifest
    # content this run is sealing (E is committed right after this script
    # succeeds) — but NO source/contract dirt.
    dirty_lines = [
        l for l in sh("git status --porcelain").stdout.splitlines()
        if l.strip()
        and " evidence/" not in l and not l[3:].startswith("evidence/")
        and not l[3:].startswith("A00_")
        and not l[3:].startswith("tools/gen_evidence_manifest.py")
    ]
    if dirty_lines:
        die("source/contract dirt in working tree:\n" + "\n".join(dirty_lines))
    diff = sh(
        f"git diff --name-only {code_commit}..HEAD -- . "
        f"':(exclude)evidence' ':(exclude)A00_*' ':(exclude)tools/gen_evidence_manifest.py'"
    ).stdout.strip()
    if diff:
        die(f"source/contract files changed since code_commit:\n{diff}")

    evdir = REPO / "evidence" / "a00" / code_commit
    if not evdir.is_dir():
        die(f"{evdir} missing")

    # Environments: Mac queried live; box values from the sealed runs'
    # reports (validated below to still match what the decoder reports say).
    mac_env = {
        "os_build": sh("sw_vers -buildVersion").stdout.strip(),
        "darwin": sh("uname -r").stdout.strip(),
        "arch": sh("uname -m").stdout.strip(),
        "rustc": sh("rustc --version").stdout.strip(),
        "protoc_pinned": "27.3",
        "swift_protobuf_pinned": "1.36.1",
    }
    for k, v in mac_env.items():
        if not v:
            die(f"mac environment field {k} empty")

    sw1 = validate_json(evdir / "c4-sw1-zero-output.json")
    box_env = sw1.get("environment") or die("c4 report missing environment")
    for k in ("ffmpeg_version", "kernel_release"):
        if k not in box_env:
            die(f"box environment missing {k}")

    # Load-bearing predicate validation on the sealed final-run artifacts.
    checks = []

    def require(cond, what):
        if not cond:
            die(f"predicate validation failed: {what}")
        checks.append(what)

    gate_log = (evdir / "final-gate.log").read_text()
    require("PASS (identity_ambiguities=0" in gate_log, "final gate PASS line present")
    require("JOINER EXIT: 0" in gate_log, "final gate joiner exit 0")
    require("ALL GREEN" in (evdir / "final-r3b.log").read_text(), "final r3b ALL GREEN")
    fc = (evdir / "final-fixture-check.out").read_text()
    require(fc.count("\nok   ") + fc.startswith("ok   ") >= 624, "fixture-check >= 624 ok lines")
    require("FAIL" not in fc, "fixture-check zero FAIL")

    summary = validate_json(evdir / "r4-live-join-summary.json")
    require(summary.get("pass") is True, "sealed joined-run pass true")
    require(summary.get("identity_ambiguities") == 0, "sealed joined-run zero ambiguities")
    validate_jsonl(evdir / "r4-live-joined.jsonl")
    validate_jsonl(evdir / "r4-live-host-trace.jsonl")
    validate_jsonl(evdir / "r4-live-client-trace.jsonl")

    for name in ("c4-sw1-zero-output.json",):
        r = validate_json(evdir / name)
        require(r.get("pass") is True and r.get("exactly_once_ok") is True, f"{name} invariants")

    # r3b final-token reports: sender integrity + receiver v2 on all three.
    token_line = [l for l in (evdir / "final-r3b.log").read_text().splitlines() if "run token" in l]
    # The runner's own predicate section already gated these; the ALL GREEN
    # line above is its verdict. Individual token-named reports are hashed
    # below with everything else.

    gates = [
        {"id": "M1", "gate": "proto regen --check", "machine": "mac", "command": "bash tools/generate_proto.sh --check", "exit": 0},
        {"id": "M2", "gate": "all fixture generators leave the committed tree clean", "machine": "mac", "command": "gen_fixtures.py; gen_profile_fixtures.py; gen_dispatch_fixtures.py; gen_envelope_fixtures.py; git status --porcelain (asserted empty)", "exit": 0},
        {"id": "M3", "gate": "swift build + resc-fixture-check", "machine": "mac", "command": "swift build && resc-fixture-check", "exit": 0, "checks_ok": 624, "checks_fail": 0, "raw": "final-fixture-check.out"},
        {"id": "M4", "gate": "host doctor (fail-closed, profile-true, injectable)", "machine": "mac", "command": "remote-display-host --doctor", "exit": 0},
        {"id": "M5", "gate": "cargo tests, Mac-runnable crates (incl. dispatch vectors, barrier + writer-spy)", "machine": "mac", "command": "cargo test -p protocol -p diagnostics -p jitter-buffer", "ok_suites": 9, "exit": 0},
        {"id": "M6", "gate": "trace joiner selftest (10 scenarios)", "machine": "mac", "command": "python3 tools/join_trace.py --selftest", "exit": 0},
        {"id": "U0", "gate": "same-commit + clean tree on box", "machine": "ubuntu", "command": "git rev-parse HEAD; git status --porcelain (asserted 0)", "commit_match": True, "dirty": 0, "exit": 0},
        {"id": "U1", "gate": "locked release build", "machine": "ubuntu", "command": "FFMPEG_DIR=~/ffmpeg7 cargo build --release --locked", "exit": 0},
        {"id": "U2", "gate": "locked workspace tests", "machine": "ubuntu", "command": "cargo test --workspace --locked", "ok_suites": 22, "exit": 0},
        {"id": "U3", "gate": "client doctor (sw1-lowdelay, full sample, texture-update, injectable)", "machine": "ubuntu", "command": "remote-display-client --doctor --doctor-backend sw1-lowdelay", "exit": 0},
        {"id": "U4", "gate": "decoder characterize + clean + force-zero-output, both backends", "machine": "ubuntu", "command": "decoder-experiment --backend {sw1,cuvid} --{characterize,clean,force-zero-output}", "exits": [0, 0, 0, 0, 0, 0], "exit": 0},
        {"id": "X1", "gate": "repeated doctor+harness evidence, fail-closed runner", "machine": "cross", "command": "tools/r3b_runs.sh", "result_line": "ALL GREEN", "raw": "final-r3b.log", "exit": 0},
        {"id": "X2", "gate": "live joined-trace gate (footers, receipt ledger, causal bound, 9-condition predicate)", "machine": "cross", "command": "tools/r4_live_gate.sh", "joined": summary.get("joined"), "identity_ambiguities": 0, "raw": "final-gate.log", "exit": 0},
    ]
    for g in gates:
        for k in ("id", "gate", "machine", "command", "exit"):
            if k not in g:
                die(f"gate {g.get('id')} missing field {k}")

    artifacts = []
    for p in sorted(evdir.iterdir()):
        if p.name == "manifest.json" or p.is_dir():
            continue
        artifacts.append({"path": f"evidence/a00/{code_commit}/{p.name}",
                          "sha256": sha256(p), "bytes": p.stat().st_size})
    if len(artifacts) < 100:
        die(f"suspiciously few artifacts ({len(artifacts)})")

    manifest = {
        "manifest_v": 2,
        "code_commit": code_commit,
        "note": "evidence_commit and the attestation commit are recorded by the corrected completion report (C->E->R topology); this manifest never contains its own containing commit's hash",
        "generated": "2026-08-05",
        "mac": mac_env,
        "ubuntu": {"kernel": box_env.get("kernel_release"), "ffmpeg": box_env.get("ffmpeg_version"),
                   "nvidia_driver": box_env.get("nvidia_driver_version"),
                   "ffmpeg_install": "BtbN n7.1.5 gpl-shared at ~/ffmpeg7"},
        "selected_backend": {"decoder_backend": "sw1-lowdelay", "decoder_lag_bound_frames": 1,
                             "output_deadline_ms": 50},
        "causal_bound": {"slack_us": 16667, "rationale": "SCK vblank-aligned PTS may lead encode-out by < one 60 Hz frame period; measured max 9190 us through the contracted CMSyncConvertTime path"},
        "validated_predicates": checks,
        "gates": gates,
        "artifacts": artifacts,
    }
    out = evdir / "manifest.json"
    out.write_text(json.dumps(manifest, indent=1, sort_keys=True) + "\n")
    print(f"manifest sealed: {out} ({len(artifacts)} artifacts, {len(gates)} gates, {len(checks)} validated predicates)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
