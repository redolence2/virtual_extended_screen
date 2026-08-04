#!/usr/bin/env python3
"""Offline trace joiner for the A0.0 R4 identity contract
(A00_REMEDIATION_PLAN.md §4 item 9: "The joiner accepts only exact
identities and records clock offset, delay, uncertainty, fallback status,
and every rejected sample's reason." — hardened per
A00_COMPLETION_REPORT_AMENDED_review.md finding 4.3 / finding 1 and
A00_COMPLETION_REPORT_AMENDED_response_review.md amendments 2/3: exact
decode-side receipt identity, footer-gated completeness, session-global
clock, host-local causal sanity, and a materially stronger PASS predicate.)

Joins two independently-written JSONL files:

- `--host` (mac-host's `host-trace.jsonl`): one `{"t":"frame",...}` record
  per wire-sent frame, binding `frame_id` to its capture identity
  (`generation`/`capture_seq`/`capture_ts_us`/`ts_source`/`uncertainty_us` —
  all `null` together for synthetic sources with no real capture identity,
  e.g. HarnessSender) plus `encode_out_ts_us`/`send_ts_us`/`bytes`/`kf`, and
  exactly one `{"t":"trace_complete","status":"clean",...}` footer.
- `--client` (the Ubuntu client's `client-trace.jsonl`,
  `crates/diagnostics/src/trace.rs`): every line is
  `{"ts_mono_us":u64,"kind":"frame"|"present"|"clock"|"identity_failure"|
  "render_failure"|"trace_complete","fields":{...}}`.
  `"frame"` fields: `recovered_frame_id` (the PTS the decoder recovered for
  THIS emitted frame, resolved from the decode-side receipt ledger — `null`
  is never emitted here; an unrecoverable PTS becomes an `identity_failure`
  record instead), `decode_trigger_frame_id` (the wire frame_id of the
  decode() call that emitted this frame — informational only, NOT the join
  key), `ts_recv_us` (the ledger's assembly-completion receipt for
  `recovered_frame_id`, not the trigger's), `ts_decode_done_us`. `"present"`
  fields: `recovered_frame_id`, `ts_present_us`. `"clock"` fields:
  `offset_us`, `delay_us`, `uncertainty_us`, `seq`. `"identity_failure"`
  fields: `reason` (`duplicate_submit`|`cap_overflow`|`unrecovered_pts`|
  `missing_ledger_entry`), `wire_frame_id`, `recovered_frame_id`.
  `"trace_complete"` fields: `run_token`, `status` (`"clean"`|`"aborted"`),
  plus pending/failure/drop counts.

The join key is exact identity: host `frame_id` == client
`recovered_frame_id`. Never latest-wins — see `join_records()` for the full
accept/reject rule set. Capture/render DROPS (a host frame the client never
received; a joined frame that was never presented) are expected and
counted, not errors. Identity SUBSTITUTION (an id that isn't exactly
1-to-1 on both sides, or a decode that couldn't recover its own PTS) is an
"identity ambiguity"; a client-side decode-ledger failure and a host-local
causal impossibility are their own distinct rejection classes — all three
gate the PASS predicate below.

Fatal (raises `JoinFatalError`, caught by `main()`/callers, process exit 1):
any unparseable non-blank line, or any line that isn't a JSON object, in
either input; a missing, duplicate, or non-clean (`status != "clean"`)
`trace_complete` footer on either side. Corrupt input must fail the run,
never be silently skipped.

PASS predicate (`compute_pass`; printed, and the process exit code) — ALL of:
    zero parse errors                     (structural: see Fatal above)
    both footers present and clean        (structural: see Fatal above)
    identity_ambiguities == 0             (join-level duplicate/unrecovered)
    identity_failure_records == 0         (client decode-side ledger: dup/
                                            cap-overflow/unrecovered/missing)
    zero `causal_violation` rejections
    >= 1 accepted clock sample
    >= 1 presented, exactly joined frame
    offset_us AND offset_uncertainty_us present on every joined sample that
        has a computed e2e_capture_to_present_us
    joined > 0
`never_received` stays an allowed, counted drop — absent from every
condition above.

Deterministic: no wall-clock reads, no randomness — a pure function of the
two input files (or, under `--selftest`, of the in-memory fixtures below).
python3 stdlib only.
"""

import argparse
import json
import statistics
import sys
from pathlib import Path

# Reasons a candidate sample can be rejected instead of joined (§4 item 9:
# "every rejected sample's reason"). `duplicate_identity` and
# `unrecovered_identity` are join-level identity AMBIGUITIES;
# `causal_violation` is a host-local causal-sanity rejection (C3 / review
# finding 4.1's causal-sanity gate); the rest are expected drops/topology
# mismatches, counted but not fatal to the PASS predicate.
REASON_UNRECOVERED = "unrecovered_identity"
REASON_DUPLICATE = "duplicate_identity"
REASON_NO_HOST_RECORD = "no_host_record"
REASON_NEVER_RECEIVED = "never_received"
REASON_PRESENT_WITHOUT_FRAME = "present_without_frame"
REASON_CAUSAL_VIOLATION = "causal_violation"

AMBIGUITY_REASONS = (REASON_DUPLICATE, REASON_UNRECOVERED)


class JoinFatalError(Exception):
    """Raised for a defect that must abort the whole run rather than being
    counted/skipped (A00_COMPLETION_REPORT_AMENDED_review.md finding 4.3 /
    FROZEN DESIGN §5): an unparseable (or non-object) line, or a
    missing/duplicate/non-clean `trace_complete` footer on either side.
    Caught by `main()` (real files) and the `--selftest` scenarios
    (in-memory fixtures) alike — both paths run the exact same validation
    logic, never a separate parallel implementation."""


# ---------------------------------------------------------------------------
# Record loading
# ---------------------------------------------------------------------------

def parse_jsonl_lines(lines, source_name):
    """Parses an iterable of raw JSONL lines into a list of dicts. ANY
    unparseable non-blank line, or any line that parses to something other
    than a JSON object, is FATAL — raises `JoinFatalError` immediately
    (A00_COMPLETION_REPORT_AMENDED_review.md finding 4.3: "Corrupt input
    must fail rather than be silently skipped"), not a per-line skip+warn.
    Blank lines are skipped (a trailing newline is not corruption)."""
    records = []
    for lineno, line in enumerate(lines, 1):
        line = line.strip()
        if not line:
            continue
        try:
            parsed = json.loads(line)
        except json.JSONDecodeError as e:
            raise JoinFatalError(f"{source_name}:{lineno}: unparseable line ({e})")
        if not isinstance(parsed, dict):
            raise JoinFatalError(
                f"{source_name}:{lineno}: line is not a JSON object (got {type(parsed).__name__})"
            )
        records.append(parsed)
    return records


def load_jsonl(path):
    """Reads `path` and parses it via `parse_jsonl_lines`. Propagates
    `JoinFatalError` on any unparseable/non-object line, `OSError` if the
    file itself can't be read/found."""
    with open(path, "r") as f:
        return parse_jsonl_lines(f, str(path))


# ---------------------------------------------------------------------------
# Footer validation (C3 / A00_COMPLETION_REPORT_AMENDED_response_review.md
# amendment 3: "a clean/aborted terminal protocol")
# ---------------------------------------------------------------------------

def find_footer(records, source_name, top_level_key, status_of):
    """Finds exactly one `trace_complete` footer among `records`
    (`top_level_key` is `"t"` for the host, `"kind"` for the client — the
    two sides use different top-level shapes; `status_of(footer)` extracts
    the status string, since the host's is top-level and the client's is
    nested under `fields`). Position-independent — a footer is recognized
    by content, not by being the last line. Raises `JoinFatalError` on a
    missing, duplicate, or non-`"clean"` footer. Returns the single footer
    record on success."""
    footers = [r for r in records if r.get(top_level_key) == "trace_complete"]
    if len(footers) == 0:
        raise JoinFatalError(f"{source_name}: missing trace_complete footer")
    if len(footers) > 1:
        raise JoinFatalError(f"{source_name}: duplicate trace_complete footer ({len(footers)} found)")
    footer = footers[0]
    status = status_of(footer)
    if status != "clean":
        raise JoinFatalError(f"{source_name}: trace_complete footer status is {status!r}, not 'clean'")
    return footer


def _host_status(footer):
    return footer.get("status")


def _client_status(footer):
    return (footer.get("fields") or {}).get("status")


# ---------------------------------------------------------------------------
# Core join
# ---------------------------------------------------------------------------

def _build_sample(frame_id, host_rec, client_fields):
    """The per-joined-sample shape (A00_REMEDIATION_PLAN.md §4 item 9's
    field list, plus `clock_seq`/`decode_trigger_frame_id` as documented
    informational additions — neither drops nor overrides any
    explicitly-required field)."""
    return {
        "frame_id": frame_id,
        "capture_ts_us": host_rec.get("capture_ts_us"),
        "ts_source": host_rec.get("ts_source"),
        "capture_uncertainty_us": host_rec.get("uncertainty_us"),
        "encode_out_ts_us": host_rec.get("encode_out_ts_us"),
        "send_ts_us": host_rec.get("send_ts_us"),
        "ts_recv_us": client_fields.get("ts_recv_us"),
        "ts_decode_done_us": client_fields.get("ts_decode_done_us"),
        # Informational only (C3 FROZEN DESIGN §5): the decode() call whose
        # drain happened to emit this frame — NEVER the join key, and may
        # legitimately differ from `frame_id`/`recovered_frame_id` (delayed/
        # multi-output emission). ts_recv_us above always follows the
        # ledger-resolved recovered identity regardless of this value.
        "decode_trigger_frame_id": client_fields.get("decode_trigger_frame_id"),
        "ts_present_us": None,        # filled in if a present record joins
        "offset_us": None,            # filled in from the session-global-best clock sample
        "offset_uncertainty_us": None,
        "clock_seq": None,
        "e2e_capture_to_present_us": None,  # computed once all of the above are known
    }


def join_records(host_records, client_records, causal_slack_us=0):
    """Join one (host_records, client_records) pair. Pure function — no I/O,
    no clock reads. Raises `JoinFatalError` if either side's footer is
    missing/duplicate/non-clean (see `find_footer`). Otherwise returns a
    dict:
      joined                  -- list of per-sample dicts (see
                                  _build_sample), sorted by frame_id
      host_frames              -- count of host "t":"frame" records read
      client_frames             -- count of client kind:"frame" records read
      presents                   -- count of client kind:"present" records read
      rejected                    -- {reason: count}
      offset_stats                 -- {sample_count, min_delay_us, median_delay_us}
      identity_ambiguities           -- duplicate_identity + unrecovered_identity counts
      identity_failure_records        -- count of client kind:"identity_failure" records
      presented_joined_count           -- count of joined samples with ts_present_us set
      host_footer / client_footer       -- the validated footer records
    """
    host_footer = find_footer(host_records, "host", "t", _host_status)
    client_footer = find_footer(client_records, "client", "kind", _client_status)

    host_frames = [r for r in host_records if r.get("t") == "frame"]
    client_frame_recs = [r for r in client_records if r.get("kind") == "frame"]
    client_present_recs = [r for r in client_records if r.get("kind") == "present"]
    client_identity_failure_recs = [r for r in client_records if r.get("kind") == "identity_failure"]
    # Well-formed only (dict "fields" with "delay_us") -- the global-best
    # selection below relies on every record here having one, and a torn/
    # malformed line must not crash the join.
    client_clock_recs = [
        r for r in client_records
        if r.get("kind") == "clock" and isinstance(r.get("fields"), dict) and "delay_us" in r["fields"]
    ]

    rejected_counts = {}

    def reject(reason, n=1):
        rejected_counts[reason] = rejected_counts.get(reason, 0) + n

    # unrecovered_identity: filtered out before any id-grouping, since a
    # null recovered_frame_id can't be grouped/joined by identity at all.
    # A correctly-behaving C3 client never emits this (an unrecoverable PTS
    # becomes an identity_failure record instead) — kept defensively for any
    # other/older/hand-crafted trace producer.
    recoverable_client_frames = []
    for r in client_frame_recs:
        fields = r.get("fields") or {}
        if fields.get("recovered_frame_id") is None:
            reject(REASON_UNRECOVERED)
        else:
            recoverable_client_frames.append(r)

    host_by_id = {}
    for r in host_frames:
        host_by_id.setdefault(r.get("frame_id"), []).append(r)

    client_by_id = {}
    for r in recoverable_client_frames:
        fields = r.get("fields") or {}
        client_by_id.setdefault(fields.get("recovered_frame_id"), []).append(r)

    all_ids = sorted(set(host_by_id) | set(client_by_id), key=lambda x: (x is None, x))

    joined_by_id = {}
    for fid in all_ids:
        h = host_by_id.get(fid, [])
        c = client_by_id.get(fid, [])
        # duplicate joins (plan §4 item 9): two host records with one id,
        # two client records recovering one id, or both at once -> every
        # record sharing that ambiguous id is rejected, none of them join.
        if len(h) > 1 or len(c) > 1:
            reject(REASON_DUPLICATE, len(h) + len(c))
            continue
        if len(h) == 1 and len(c) == 1:
            client_fields = c[0].get("fields") or {}
            joined_by_id[fid] = _build_sample(fid, h[0], client_fields)
        elif len(h) == 1 and len(c) == 0:
            reject(REASON_NEVER_RECEIVED)  # allowed drop, not an error
        elif len(h) == 0 and len(c) == 1:
            reject(REASON_NO_HOST_RECORD)

    # Host-local causal sanity (C3 / A00_COMPLETION_REPORT_AMENDED_review.md
    # finding 4.1): capture_ts_us <= encode_out_ts_us + capture_uncertainty_us
    # + causal_slack_us. Host-local terms only -- no cross-machine clock
    # offset belongs here. Skipped (not violated) when either timestamp or
    # the capture uncertainty is null -- the documented synthetic-source
    # case (e.g. HarnessSender) with no real capture identity to check.
    for fid in list(joined_by_id.keys()):
        sample = joined_by_id[fid]
        capture_ts = sample["capture_ts_us"]
        encode_out_ts = sample["encode_out_ts_us"]
        capture_unc = sample["capture_uncertainty_us"]
        if capture_ts is None or encode_out_ts is None or capture_unc is None:
            continue
        if capture_ts > encode_out_ts + capture_unc + causal_slack_us:
            del joined_by_id[fid]
            reject(REASON_CAUSAL_VIOLATION)

    # Present records join through their recovered id to an already-joined
    # sample. Repeated presents of the same id (e.g. cursor-only redraws
    # re-presenting the same last-uploaded frame) are not an error — the
    # first (earliest) present is kept as the sample's presentation time.
    presented_ids = set()
    for r in client_present_recs:
        fields = r.get("fields") or {}
        rid = fields.get("recovered_frame_id")
        if rid is None:
            reject(REASON_UNRECOVERED)
            continue
        sample = joined_by_id.get(rid)
        if sample is None:
            reject(REASON_PRESENT_WITHOUT_FRAME)
            continue
        if rid not in presented_ids:
            sample["ts_present_us"] = fields.get("ts_present_us")
            presented_ids.add(rid)

    # Session-global-best clock sample (A00_COMPLETION_REPORT_AMENDED_review.md
    # finding 4.2 / CONTRACT_ERRATA.md ERR-08 "the minimum-delay sample
    # remains the authoritative offset"): ONE sample, selected once
    # session-wide, applied to EVERY joined sample -- no per-sample window.
    clock_delays = [r["fields"]["delay_us"] for r in client_clock_recs]
    offset_stats = {
        "sample_count": len(clock_delays),
        "min_delay_us": min(clock_delays) if clock_delays else None,
        "median_delay_us": statistics.median(clock_delays) if clock_delays else None,
    }
    global_best = min(client_clock_recs, key=lambda r: r["fields"]["delay_us"], default=None)

    for sample in joined_by_id.values():
        if global_best is not None:
            cf = global_best["fields"]
            sample["offset_us"] = cf.get("offset_us")
            sample["offset_uncertainty_us"] = cf.get("uncertainty_us")
            sample["clock_seq"] = cf.get("seq")

        if (sample["ts_present_us"] is not None
                and sample["capture_ts_us"] is not None
                and sample["ts_source"] is not None
                and sample["offset_us"] is not None):
            # ClockSync's offset = ((t2-t1)+(t3-t4))/2 with t1/t4 client and
            # t2/t3 host, i.e. offset ~= host_clock - client_clock. The
            # client-domain equivalent of a host timestamp is therefore
            # host_ts - offset (sign corrected 2026-08-04 during R4 review:
            # the original frozen spec wrote `+ offset`, which the
            # implementing worker correctly flagged as a sign error).
            sample["e2e_capture_to_present_us"] = (
                sample["ts_present_us"] - (sample["capture_ts_us"] - sample["offset_us"])
            )

    joined = [joined_by_id[fid] for fid in sorted(joined_by_id, key=lambda x: (x is None, x))]
    identity_ambiguities = sum(rejected_counts.get(r, 0) for r in AMBIGUITY_REASONS)
    presented_joined_count = sum(1 for s in joined if s["ts_present_us"] is not None)

    return {
        "joined": joined,
        "host_frames": len(host_frames),
        "client_frames": len(client_frame_recs),
        "presents": len(client_present_recs),
        "rejected": dict(sorted(rejected_counts.items())),
        "offset_stats": offset_stats,
        "identity_ambiguities": identity_ambiguities,
        "identity_failure_records": len(client_identity_failure_recs),
        "presented_joined_count": presented_joined_count,
        "host_footer": host_footer,
        "client_footer": client_footer,
    }


def compute_pass(result):
    """The C3-hardened PASS predicate — see the module doc comment's full
    list. Zero parse errors and both footers clean are enforced
    structurally by `join_records` raising `JoinFatalError` before this is
    ever reached, so they are not re-checked here."""
    joined = result["joined"]
    if result["identity_ambiguities"] != 0:
        return False
    if result["identity_failure_records"] != 0:
        return False
    if result["rejected"].get(REASON_CAUSAL_VIOLATION, 0) != 0:
        return False
    if result["offset_stats"]["sample_count"] < 1:
        return False
    if result["presented_joined_count"] < 1:
        return False
    if len(joined) == 0:
        return False
    for sample in joined:
        if sample["e2e_capture_to_present_us"] is not None:
            if sample["offset_us"] is None or sample["offset_uncertainty_us"] is None:
                return False
    return True


# ---------------------------------------------------------------------------
# main / summary
# ---------------------------------------------------------------------------

def build_summary(result):
    passed = compute_pass(result)
    return {
        "host_frames": result["host_frames"],
        "client_frames": result["client_frames"],
        "presents": result["presents"],
        "joined": len(result["joined"]),
        "presented_joined": result["presented_joined_count"],
        "rejected": result["rejected"],
        "offset_stats": result["offset_stats"],
        "identity_ambiguities": result["identity_ambiguities"],
        "identity_failure_records": result["identity_failure_records"],
        "pass": passed,
    }


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Join host-trace.jsonl + client-trace.jsonl on exact frame identity "
                     "(A00_REMEDIATION_PLAN.md §4 item 9; hardened per C3)."
    )
    ap.add_argument("--host", help="path to mac-host's host-trace.jsonl")
    ap.add_argument("--client", help="path to the Ubuntu client's client-trace.jsonl")
    ap.add_argument("--out", help="path to write joined samples (JSONL)")
    ap.add_argument("--summary", help="path to write the summary (JSON)")
    ap.add_argument("--causal-slack-us", type=int, default=0,
                     help="host-local causal-sanity slack (C3 / review finding 4.1): "
                          "capture_ts_us <= encode_out_ts_us + capture_uncertainty_us + slack")
    ap.add_argument("--selftest", action="store_true",
                     help="run the in-memory synthetic self-test and exit; ignores the other flags")
    args = ap.parse_args()

    if args.selftest:
        return 0 if run_selftest() else 1

    missing = [name for name in ("host", "client", "out", "summary") if getattr(args, name) is None]
    if missing:
        ap.error("--{} is required (or pass --selftest)".format(missing[0]))

    try:
        host_records = load_jsonl(Path(args.host))
        client_records = load_jsonl(Path(args.client))
        result = join_records(host_records, client_records, causal_slack_us=args.causal_slack_us)
    except JoinFatalError as e:
        print(f"join_trace: FATAL: {e}", file=sys.stderr)
        return 1
    except OSError as e:
        print(f"join_trace: {e}", file=sys.stderr)
        return 1

    summary = build_summary(result)

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with open(out_path, "w") as f:
        for sample in result["joined"]:
            f.write(json.dumps(sample) + "\n")

    summary_path = Path(args.summary)
    summary_path.parent.mkdir(parents=True, exist_ok=True)
    with open(summary_path, "w") as f:
        json.dump(summary, f, indent=2, sort_keys=True)
        f.write("\n")

    print(f"join_trace: host_frames={summary['host_frames']} client_frames={summary['client_frames']} "
          f"presents={summary['presents']} joined={summary['joined']} "
          f"presented_joined={summary['presented_joined']} "
          f"identity_ambiguities={summary['identity_ambiguities']} "
          f"identity_failure_records={summary['identity_failure_records']} rejected={summary['rejected']}")
    print(("PASS" if summary["pass"] else "FAIL")
          + f" (identity_ambiguities={summary['identity_ambiguities']}, "
          + f"identity_failure_records={summary['identity_failure_records']}, joined={summary['joined']})")

    return 0 if summary["pass"] else 1


# ---------------------------------------------------------------------------
# --selftest: tiny synthetic in-memory record sets, no files touched.
# ---------------------------------------------------------------------------

def _host_frame(frame_id, capture_ts_us=None, ts_source="sck_pts", uncertainty_us=50,
                 generation=1, capture_seq=1, encode_out_ts_us=0, send_ts_us=0,
                 bytes_=1000, kf=False):
    return {
        "t": "frame", "frame_id": frame_id,
        "generation": generation, "capture_seq": capture_seq,
        "capture_ts_us": capture_ts_us, "ts_source": ts_source, "uncertainty_us": uncertainty_us,
        "encode_out_ts_us": encode_out_ts_us, "send_ts_us": send_ts_us,
        "bytes": bytes_, "kf": kf,
    }


def _host_footer(status="clean", run_token="0123456789abcdef"):
    return {"t": "trace_complete", "run_token": run_token, "status": status}


def _client_frame(ts_mono_us, decode_trigger_frame_id, recovered_frame_id, ts_recv_us, ts_decode_done_us):
    return {
        "ts_mono_us": ts_mono_us, "kind": "frame",
        "fields": {
            "decode_trigger_frame_id": decode_trigger_frame_id, "recovered_frame_id": recovered_frame_id,
            "ts_recv_us": ts_recv_us, "ts_decode_done_us": ts_decode_done_us,
        },
    }


def _client_present(ts_mono_us, recovered_frame_id, ts_present_us):
    return {
        "ts_mono_us": ts_mono_us, "kind": "present",
        "fields": {"recovered_frame_id": recovered_frame_id, "ts_present_us": ts_present_us},
    }


def _client_clock(ts_mono_us, offset_us, delay_us, uncertainty_us, seq):
    return {
        "ts_mono_us": ts_mono_us, "kind": "clock",
        "fields": {"offset_us": offset_us, "delay_us": delay_us,
                   "uncertainty_us": uncertainty_us, "seq": seq},
    }


def _client_identity_failure(ts_mono_us, reason, wire_frame_id, recovered_frame_id=None):
    return {
        "ts_mono_us": ts_mono_us, "kind": "identity_failure",
        "fields": {"reason": reason, "wire_frame_id": wire_frame_id, "recovered_frame_id": recovered_frame_id},
    }


def _client_footer(status="clean", run_token="fedcba9876543210", **extra_fields):
    fields = {"run_token": run_token, "status": status, "pending_identities": 0, "identity_failures": 0}
    fields.update(extra_fields)
    return {"ts_mono_us": 0, "kind": "trace_complete", "fields": fields}


def _happy_path_records():
    """id=1: host frame + client frame + client present + one clock sample.
    Everything about it should join and get a full e2e number. Both sides
    carry a clean footer -- required by every scenario since C3 (footer
    position within the list is irrelevant; find_footer scans by content)."""
    host = [
        _host_frame(1, capture_ts_us=1_000_000, uncertainty_us=50,
                    encode_out_ts_us=1_001_000, send_ts_us=1_002_000, kf=True),
        _host_footer(),
    ]
    client = [
        _client_frame(1_003_500, decode_trigger_frame_id=1, recovered_frame_id=1,
                      ts_recv_us=1_003_000, ts_decode_done_us=1_003_500),
        _client_present(1_004_000, recovered_frame_id=1, ts_present_us=1_004_000),
        _client_clock(1_003_600, offset_us=200, delay_us=40, uncertainty_us=20, seq=7),
        _client_footer(),
    ]
    return host, client


def run_selftest() -> bool:
    ok = True

    def check(cond, msg):
        nonlocal ok
        status = "ok" if cond else "FAIL"
        print(f"  [{status}] {msg}")
        if not cond:
            ok = False

    print("join_trace --selftest")

    # --- Scenario A: clean happy-path-only input -> positive pass predicate.
    print("scenario: happy path only")
    host, client = _happy_path_records()
    result = join_records(host, client)
    summary = build_summary(result)
    check(summary["joined"] == 1, f"joined == 1 (got {summary['joined']})")
    check(summary["identity_ambiguities"] == 0,
          f"identity_ambiguities == 0 (got {summary['identity_ambiguities']})")
    check(summary["identity_failure_records"] == 0,
          f"identity_failure_records == 0 (got {summary['identity_failure_records']})")
    check(summary["presented_joined"] == 1, f"presented_joined == 1 (got {summary['presented_joined']})")
    check(summary["rejected"] == {}, f"no rejections (got {summary['rejected']})")
    check(summary["pass"] is True, "pass predicate is True")
    if result["joined"]:
        s = result["joined"][0]
        check(s["ts_present_us"] == 1_004_000, f"ts_present_us == 1004000 (got {s['ts_present_us']})")
        check(s["offset_us"] == 200, f"offset_us == 200 (got {s['offset_us']})")
        check(s["offset_uncertainty_us"] == 20, f"offset_uncertainty_us == 20 (got {s['offset_uncertainty_us']})")
        check(s["clock_seq"] == 7, f"clock_seq == 7 (got {s['clock_seq']})")
        check(s["decode_trigger_frame_id"] == 1,
              f"decode_trigger_frame_id == 1 (got {s['decode_trigger_frame_id']})")
        # capture host-domain -> client-domain is capture - offset, so with
        # the fixture's offset_us=200 the e2e is 400us LARGER than the naive
        # (wrong-sign) 3800 figure: present(1004000) - (capture(1000000) - 200).
        check(s["e2e_capture_to_present_us"] == 4_200,
              f"e2e_capture_to_present_us == 4200 (got {s['e2e_capture_to_present_us']})")

    # --- Scenario B: kitchen sink -- one of each reject reason alongside
    # the same happy-path id=1, in a single combined dataset.
    print("scenario: duplicate / unrecovered / never_received / no_host_record / present_without_frame")
    host, client = _happy_path_records()
    host += [
        # id=2: duplicate_identity -- two host records claim the same wire id.
        _host_frame(2, capture_ts_us=2_000_000, capture_seq=11, encode_out_ts_us=2_001_000, send_ts_us=2_002_000),
        _host_frame(2, capture_ts_us=2_010_000, capture_seq=12, encode_out_ts_us=2_011_000, send_ts_us=2_012_000),
        # id=4: never_received -- host sent it, client trace has nothing for it.
        _host_frame(4, capture_ts_us=4_000_000, capture_seq=13, encode_out_ts_us=4_001_000, send_ts_us=4_002_000),
    ]
    client += [
        # id=3: unrecovered_identity -- decoder couldn't recover this emission's
        # PTS (defensive coverage only -- a real C3 client emits an
        # identity_failure record instead; see scenario J).
        _client_frame(3_003_500, decode_trigger_frame_id=3, recovered_frame_id=None,
                      ts_recv_us=3_003_000, ts_decode_done_us=3_003_500),
        # id=5: no_host_record -- client recovered an id the host never logged.
        _client_frame(5_003_500, decode_trigger_frame_id=5, recovered_frame_id=5,
                      ts_recv_us=5_003_000, ts_decode_done_us=5_003_500),
        # id=99: present_without_frame -- presented an id with no joined frame.
        _client_present(9_004_000, recovered_frame_id=99, ts_present_us=9_004_000),
    ]
    result = join_records(host, client)
    summary = build_summary(result)
    check(summary["joined"] == 1, f"joined == 1 (got {summary['joined']})")
    check(summary["rejected"].get(REASON_DUPLICATE) == 2,
          f"duplicate_identity == 2 (got {summary['rejected'].get(REASON_DUPLICATE)})")
    check(summary["rejected"].get(REASON_UNRECOVERED) == 1,
          f"unrecovered_identity == 1 (got {summary['rejected'].get(REASON_UNRECOVERED)})")
    check(summary["rejected"].get(REASON_NEVER_RECEIVED) == 1,
          f"never_received == 1 (got {summary['rejected'].get(REASON_NEVER_RECEIVED)})")
    check(summary["rejected"].get(REASON_NO_HOST_RECORD) == 1,
          f"no_host_record == 1 (got {summary['rejected'].get(REASON_NO_HOST_RECORD)})")
    check(summary["rejected"].get(REASON_PRESENT_WITHOUT_FRAME) == 1,
          f"present_without_frame == 1 (got {summary['rejected'].get(REASON_PRESENT_WITHOUT_FRAME)})")
    check(summary["identity_ambiguities"] == 3,
          f"identity_ambiguities == 3 (duplicate 2 + unrecovered 1; got {summary['identity_ambiguities']})")
    check(summary["pass"] is False, "pass predicate is False (never_received/no_host_record/"
                                     "present_without_frame are drops, not ambiguities, but the "
                                     "duplicate+unrecovered ambiguities alone must still fail it)")

    # --- Scenario C: no client frame/present/clock records at all (footer
    # only) -> no ambiguities, but also nothing joined; the predicate must
    # not treat an empty run as a pass.
    print("scenario: empty client trace (footer only)")
    result = join_records([_host_frame(1, capture_ts_us=1_000_000), _host_footer()], [_client_footer()])
    summary = build_summary(result)
    check(summary["joined"] == 0, f"joined == 0 (got {summary['joined']})")
    check(summary["identity_ambiguities"] == 0,
          f"identity_ambiguities == 0 (got {summary['identity_ambiguities']})")
    check(summary["pass"] is False, "pass predicate is False despite zero ambiguities (joined == 0)")

    # --- Scenario D (new): decode_trigger_frame_id != recovered_frame_id
    # must still join by recovered_frame_id, carrying the ledger-correct
    # ts_recv_us through untouched (A00_COMPLETION_REPORT_AMENDED_review.md
    # finding 1's exact defect: a frame record must never inherit another
    # frame's receipt just because it shares a triggering decode() call).
    print("scenario: decode_trigger_frame_id != recovered_frame_id still joins on recovered identity")
    host = [
        _host_frame(7, capture_ts_us=7_000_000, encode_out_ts_us=7_001_000, send_ts_us=7_002_000, kf=True),
        _host_footer(),
    ]
    client = [
        # This record's decode_trigger_frame_id (11) is a LATER wire packet
        # than its recovered_frame_id (7) -- e.g. a delayed multi-frame
        # drain emitting frame 7 while decoding input 11. ts_recv_us=7_500
        # is frame 7's OWN ledger receipt, not input 11's.
        _client_frame(7_600, decode_trigger_frame_id=11, recovered_frame_id=7,
                      ts_recv_us=7_500, ts_decode_done_us=7_600),
        _client_present(7_700, recovered_frame_id=7, ts_present_us=7_700),
        _client_clock(7_550, offset_us=0, delay_us=10, uncertainty_us=5, seq=1),
        _client_footer(),
    ]
    result = join_records(host, client)
    summary = build_summary(result)
    check(summary["joined"] == 1, f"joined == 1 (got {summary['joined']})")
    check(summary["pass"] is True, "pass predicate is True")
    if result["joined"]:
        s = result["joined"][0]
        check(s["frame_id"] == 7,
              f"joined by recovered_frame_id 7, not decode_trigger_frame_id 11 (got {s['frame_id']})")
        check(s["decode_trigger_frame_id"] == 11,
              f"decode_trigger_frame_id carried through informationally as 11 (got {s['decode_trigger_frame_id']})")
        check(s["ts_recv_us"] == 7_500,
              f"ts_recv_us == 7500 (frame 7's own ledger receipt, not input 11's; got {s['ts_recv_us']})")

    # --- Scenario E (new): footer missing / duplicate / aborted, each fatal.
    print("scenario: footer missing/duplicate/aborted are fatal")
    host_ok, client_ok = _happy_path_records()

    def expect_fatal(host_recs, client_recs, expect_substr, label):
        try:
            join_records(host_recs, client_recs)
            check(False, f"{label}: expected JoinFatalError, got none")
        except JoinFatalError as e:
            check(expect_substr in str(e), f"{label}: error mentions {expect_substr!r} (got {e!r})")

    expect_fatal([r for r in host_ok if r.get("t") != "trace_complete"], client_ok,
                 "missing", "host footer missing")
    expect_fatal(host_ok, [r for r in client_ok if r.get("kind") != "trace_complete"],
                 "missing", "client footer missing")
    expect_fatal(host_ok + [_host_footer()], client_ok,
                 "duplicate", "host footer duplicate")
    expect_fatal(host_ok, client_ok + [_client_footer()],
                 "duplicate", "client footer duplicate")
    expect_fatal(
        [r for r in host_ok if r.get("t") != "trace_complete"] + [_host_footer(status="aborted")],
        client_ok, "'aborted'", "host footer aborted",
    )
    expect_fatal(
        host_ok,
        [r for r in client_ok if r.get("kind") != "trace_complete"] + [_client_footer(status="aborted")],
        "'aborted'", "client footer aborted",
    )

    # --- Scenario F (new): host-local causal violation.
    print("scenario: causal violation (capture_ts_us > encode_out_ts_us + uncertainty + slack)")
    host = [
        # capture AFTER encode_out -- host-local causal impossibility.
        _host_frame(20, capture_ts_us=1_100_000, uncertainty_us=50,
                    encode_out_ts_us=1_000_000, send_ts_us=1_002_000, kf=True),
        _host_footer(),
    ]
    client = [
        _client_frame(1_003_500, decode_trigger_frame_id=20, recovered_frame_id=20,
                      ts_recv_us=1_003_000, ts_decode_done_us=1_003_500),
        _client_present(1_004_000, recovered_frame_id=20, ts_present_us=1_004_000),
        _client_clock(1_003_600, offset_us=0, delay_us=10, uncertainty_us=5, seq=1),
        _client_footer(),
    ]
    result = join_records(host, client, causal_slack_us=0)
    summary = build_summary(result)
    check(summary["joined"] == 0, f"joined == 0, causally rejected (got {summary['joined']})")
    check(summary["rejected"].get(REASON_CAUSAL_VIOLATION) == 1,
          f"causal_violation == 1 (got {summary['rejected'].get(REASON_CAUSAL_VIOLATION)})")
    check(summary["pass"] is False, "pass predicate is False on a causal violation")
    # Sufficient slack absorbs the same violation (proves --causal-slack-us
    # actually widens the gate rather than being ignored): capture exceeds
    # encode_out+uncertainty by 1_100_000-(1_000_000+50)=99_950us.
    result_slack = join_records(host, client, causal_slack_us=100_000)
    summary_slack = build_summary(result_slack)
    check(summary_slack["joined"] == 1, f"with sufficient slack, joined == 1 (got {summary_slack['joined']})")
    check(REASON_CAUSAL_VIOLATION not in summary_slack["rejected"],
          "with sufficient slack, no causal_violation rejection")
    check(summary_slack["pass"] is True, "with sufficient slack, pass predicate is True")

    # --- Scenario G (new): any unparseable (or non-object) line is fatal,
    # not skipped.
    print("scenario: parse errors are fatal")
    try:
        parse_jsonl_lines(['{"t": "frame", "frame_id": 1}', "not json at all", ""], "test-source")
        check(False, "expected JoinFatalError from an unparseable line, got none")
    except JoinFatalError as e:
        check("test-source:2" in str(e), f"error cites source:lineno (got {e!r})")
    try:
        parse_jsonl_lines(["42"], "test-source-2")
        check(False, "expected JoinFatalError from a non-object JSON line, got none")
    except JoinFatalError as e:
        check("test-source-2:1" in str(e), f"error cites source:lineno for a non-object line (got {e!r})")

    # --- Scenario H (new): zero accepted clock samples fails PASS even
    # though the identity join itself is perfectly clean.
    print("scenario: zero clock samples fails pass")
    host, client = _happy_path_records()
    client = [r for r in client if r.get("kind") != "clock"]
    result = join_records(host, client)
    summary = build_summary(result)
    check(summary["joined"] == 1, f"joined == 1 (got {summary['joined']})")
    check(summary["identity_ambiguities"] == 0, "identity_ambiguities == 0")
    check(result["offset_stats"]["sample_count"] == 0, "zero clock samples")
    check(summary["pass"] is False, "pass predicate is False despite a clean join (no clock sample)")

    # --- Scenario I (new): zero presented joined frames fails PASS even
    # though the identity join itself is perfectly clean.
    print("scenario: zero presented joined frames fails pass")
    host, client = _happy_path_records()
    client = [r for r in client if r.get("kind") != "present"]
    result = join_records(host, client)
    summary = build_summary(result)
    check(summary["joined"] == 1, f"joined == 1 (got {summary['joined']})")
    check(summary["identity_ambiguities"] == 0, "identity_ambiguities == 0")
    check(summary["presented_joined"] == 0, "presented_joined == 0")
    check(summary["pass"] is False, "pass predicate is False despite a clean join (nothing presented)")

    # --- Scenario J (bonus, beyond the literal new-scenario list): a
    # client-side decode ledger identity_failure record alone fails PASS —
    # this is the central invariant the whole C3 corrective cycle exists to
    # enforce, so it is covered explicitly in addition to the six scenarios
    # named in the FROZEN DESIGN.
    print("scenario: a client identity_failure record alone fails pass")
    host, client = _happy_path_records()
    client = client + [_client_identity_failure(1_003_400, "missing_ledger_entry", wire_frame_id=1,
                                                 recovered_frame_id=1)]
    result = join_records(host, client)
    summary = build_summary(result)
    check(summary["identity_ambiguities"] == 0, "identity_ambiguities == 0 (this isn't a join-level ambiguity)")
    check(summary["identity_failure_records"] == 1,
          f"identity_failure_records == 1 (got {summary['identity_failure_records']})")
    check(summary["pass"] is False, "pass predicate is False on a decode-side identity_failure record")

    print("SELFTEST " + ("PASS" if ok else "FAIL"))
    return ok


if __name__ == "__main__":
    sys.exit(main())
