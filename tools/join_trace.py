#!/usr/bin/env python3
"""Offline trace joiner for the A0.0 R4 identity contract
(A00_REMEDIATION_PLAN.md §4 item 9: "The joiner accepts only exact
identities and records clock offset, delay, uncertainty, fallback status,
and every rejected sample's reason.").

Joins two independently-written JSONL files:

- `--host` (mac-host's `host-trace.jsonl`, `Diagnostics/RescTrace.swift`
  `frameSent`): one `{"t":"frame",...}` record per wire-sent frame, binding
  `frame_id` to its capture identity (`generation`/`capture_seq`/
  `capture_ts_us`/`ts_source`/`uncertainty_us` — all `null` together for
  synthetic sources with no real capture identity, e.g. HarnessSender) plus
  `encode_out_ts_us`/`send_ts_us`/`bytes`/`kf`.
- `--client` (the Ubuntu client's `client-trace.jsonl`,
  `crates/diagnostics/src/trace.rs`): every line is
  `{"ts_mono_us":u64,"kind":"frame"|"present"|"clock","fields":{...}}`.
  `"frame"` fields: `wire_frame_id`, `recovered_frame_id` (§4 item 7 — the
  PTS the decoder recovered for THIS emitted frame, `null` if
  unrecoverable), `ts_recv_us` (§4 item 6 — assembly-completion stamp),
  `ts_decode_done_us`. `"present"` fields: `recovered_frame_id`,
  `ts_present_us` (§4 item 8 — stamped adjacent to the actual presentation
  call). `"clock"` fields: `offset_us`, `delay_us`, `uncertainty_us`, `seq`
  (`crates/diagnostics/src/clocksync.rs` `ClockSync::on_pong` output).

The join key is exact identity: host `frame_id` == client
`recovered_frame_id`. Never latest-wins — see `join_records()` for the full
accept/reject rule set. Capture/render DROPS (a host frame the client never
received; a joined frame that was never presented) are expected and
counted, not errors. Identity SUBSTITUTION (an id that isn't exactly
1-to-1 on both sides, or a decode that couldn't recover its own PTS) is an
"identity ambiguity" and is what the pass predicate below actually gates.

Pass predicate (printed, and the process exit code):
    exit 0  iff  identity_ambiguities == 0  and  joined > 0
    exit 1  otherwise
Drops alone never fail it.

Deterministic: no wall-clock reads, no randomness — a pure function of the
two input files (or, under `--selftest`, of the in-memory fixtures below).
python3 stdlib only.
"""

import argparse
import json
import statistics
import sys
from pathlib import Path

# Clock-sample selection window around a joined sample's ts_decode_done_us
# (both in the client's own mono-microsecond domain — see
# crates/diagnostics/src/clocksync.rs's module doc for why only
# *differences* within one clock domain are meaningful).
CLOCK_WINDOW_US = 5_000_000  # +/- 5s

# Reasons a candidate sample can be rejected instead of joined (§4 item 9:
# "every rejected sample's reason"). `duplicate_identity` and
# `unrecovered_identity` are identity AMBIGUITIES (the pass predicate);
# the rest are expected drops/topology mismatches, counted but not fatal.
REASON_UNRECOVERED = "unrecovered_identity"
REASON_DUPLICATE = "duplicate_identity"
REASON_NO_HOST_RECORD = "no_host_record"
REASON_NEVER_RECEIVED = "never_received"
REASON_PRESENT_WITHOUT_FRAME = "present_without_frame"

AMBIGUITY_REASONS = (REASON_DUPLICATE, REASON_UNRECOVERED)


# ---------------------------------------------------------------------------
# Record loading
# ---------------------------------------------------------------------------

def load_jsonl(path):
    """Parse a JSONL file into a list of dicts. Blank lines are skipped;
    lines that fail to parse are skipped with a stderr warning rather than
    aborting the join over one truncated/corrupt line (trace files can be
    torn by a killed process)."""
    records = []
    with open(path, "r") as f:
        for lineno, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            try:
                records.append(json.loads(line))
            except json.JSONDecodeError as e:
                print(f"join_trace: {path}:{lineno}: skipping unparseable line ({e})", file=sys.stderr)
    return records


# ---------------------------------------------------------------------------
# Core join
# ---------------------------------------------------------------------------

def _build_sample(frame_id, host_rec, client_fields):
    """The frozen per-joined-sample shape. Field names/order match
    A00_REMEDIATION_PLAN.md §4 item 9's spec exactly; `clock_seq` is an
    addition beyond that literal list (the design text separately requires
    recording "offset_us, offset_uncertainty_us ... and clock_seq used") —
    kept here since dropping already-computed, explicitly-required
    provenance would be a silent loss of information.
    """
    return {
        "frame_id": frame_id,
        "capture_ts_us": host_rec.get("capture_ts_us"),
        "ts_source": host_rec.get("ts_source"),
        "capture_uncertainty_us": host_rec.get("uncertainty_us"),
        "encode_out_ts_us": host_rec.get("encode_out_ts_us"),
        "send_ts_us": host_rec.get("send_ts_us"),
        "ts_recv_us": client_fields.get("ts_recv_us"),
        "ts_decode_done_us": client_fields.get("ts_decode_done_us"),
        "ts_present_us": None,        # filled in if a present record joins
        "offset_us": None,            # filled in from the selected clock sample
        "offset_uncertainty_us": None,
        "clock_seq": None,
        "e2e_capture_to_present_us": None,  # computed once all of the above are known
    }


def _select_clock(ts_decode_done_us, clock_recs, global_best):
    """The clock record with minimum delay_us within +/-CLOCK_WINDOW_US of
    `ts_decode_done_us`; falls back to the session-global minimum-delay
    sample if none fall in the window. `None` if there are no clock records
    at all."""
    window = [
        r for r in clock_recs
        if abs(r.get("ts_mono_us", 0) - ts_decode_done_us) <= CLOCK_WINDOW_US
    ]
    if window:
        return min(window, key=lambda r: r["fields"]["delay_us"])
    return global_best


def join_records(host_records, client_records):
    """Join one (host_records, client_records) pair. Pure function — no I/O,
    no clock reads. Returns a dict:
      joined              -- list of per-sample dicts (see _build_sample),
                              sorted by frame_id
      host_frames         -- count of host "t":"frame" records read
      client_frames       -- count of client kind:"frame" records read
      presents            -- count of client kind:"present" records read
      rejected            -- {reason: count}
      offset_stats        -- {sample_count, min_delay_us, median_delay_us}
      identity_ambiguities-- duplicate_identity + unrecovered_identity counts
    """
    host_frames = [r for r in host_records if r.get("t") == "frame"]
    client_frame_recs = [r for r in client_records if r.get("kind") == "frame"]
    client_present_recs = [r for r in client_records if r.get("kind") == "present"]
    # Well-formed only (dict "fields" with "delay_us") -- _select_clock()'s
    # min() below relies on every record here having one, and a torn/
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

    # Clock offset selection + e2e computation, per joined sample.
    # client_clock_recs is already filtered to well-formed records above.
    clock_delays = [r["fields"]["delay_us"] for r in client_clock_recs]
    offset_stats = {
        "sample_count": len(clock_delays),
        "min_delay_us": min(clock_delays) if clock_delays else None,
        "median_delay_us": statistics.median(clock_delays) if clock_delays else None,
    }
    global_best = min(client_clock_recs, key=lambda r: r["fields"]["delay_us"], default=None)

    for sample in joined_by_id.values():
        clock = _select_clock(sample["ts_decode_done_us"], client_clock_recs, global_best)
        if clock is not None:
            cf = clock["fields"]
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

    return {
        "joined": joined,
        "host_frames": len(host_frames),
        "client_frames": len(client_frame_recs),
        "presents": len(client_present_recs),
        "rejected": dict(sorted(rejected_counts.items())),
        "offset_stats": offset_stats,
        "identity_ambiguities": identity_ambiguities,
    }


def compute_pass(identity_ambiguities, joined_count):
    """The frozen pass predicate (§4 item 9 / plan §4 "Pass predicate"):
    zero identity ambiguities AND at least one joined sample. Drops alone
    never fail it."""
    return identity_ambiguities == 0 and joined_count > 0


# ---------------------------------------------------------------------------
# main / summary
# ---------------------------------------------------------------------------

def build_summary(result):
    passed = compute_pass(result["identity_ambiguities"], len(result["joined"]))
    return {
        "host_frames": result["host_frames"],
        "client_frames": result["client_frames"],
        "presents": result["presents"],
        "joined": len(result["joined"]),
        "rejected": result["rejected"],
        "offset_stats": result["offset_stats"],
        "identity_ambiguities": result["identity_ambiguities"],
        "pass": passed,
    }


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Join host-trace.jsonl + client-trace.jsonl on exact frame identity "
                     "(A00_REMEDIATION_PLAN.md §4 item 9)."
    )
    ap.add_argument("--host", help="path to mac-host's host-trace.jsonl")
    ap.add_argument("--client", help="path to the Ubuntu client's client-trace.jsonl")
    ap.add_argument("--out", help="path to write joined samples (JSONL)")
    ap.add_argument("--summary", help="path to write the summary (JSON)")
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
    except OSError as e:
        print(f"join_trace: {e}", file=sys.stderr)
        return 1

    result = join_records(host_records, client_records)
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
          f"identity_ambiguities={summary['identity_ambiguities']} rejected={summary['rejected']}")
    print(("PASS" if summary["pass"] else "FAIL")
          + f" (identity_ambiguities={summary['identity_ambiguities']}, joined={summary['joined']})")

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


def _client_frame(ts_mono_us, wire_frame_id, recovered_frame_id, ts_recv_us, ts_decode_done_us):
    return {
        "ts_mono_us": ts_mono_us, "kind": "frame",
        "fields": {
            "wire_frame_id": wire_frame_id, "recovered_frame_id": recovered_frame_id,
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


def _happy_path_records():
    """id=1: host frame + client frame + client present + one clock sample
    in-window. Everything about it should join and get a full e2e number."""
    host = [_host_frame(1, capture_ts_us=1_000_000, uncertainty_us=50,
                         encode_out_ts_us=1_001_000, send_ts_us=1_002_000, kf=True)]
    client = [
        _client_frame(1_003_500, wire_frame_id=1, recovered_frame_id=1,
                      ts_recv_us=1_003_000, ts_decode_done_us=1_003_500),
        _client_present(1_004_000, recovered_frame_id=1, ts_present_us=1_004_000),
        _client_clock(1_003_600, offset_us=200, delay_us=40, uncertainty_us=20, seq=7),
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
    check(summary["rejected"] == {}, f"no rejections (got {summary['rejected']})")
    check(summary["pass"] is True, "pass predicate is True")
    if result["joined"]:
        s = result["joined"][0]
        check(s["ts_present_us"] == 1_004_000, f"ts_present_us == 1004000 (got {s['ts_present_us']})")
        check(s["offset_us"] == 200, f"offset_us == 200 (got {s['offset_us']})")
        check(s["offset_uncertainty_us"] == 20, f"offset_uncertainty_us == 20 (got {s['offset_uncertainty_us']})")
        check(s["clock_seq"] == 7, f"clock_seq == 7 (got {s['clock_seq']})")
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
        # id=3: unrecovered_identity -- decoder couldn't recover this emission's PTS.
        _client_frame(3_003_500, wire_frame_id=3, recovered_frame_id=None,
                      ts_recv_us=3_003_000, ts_decode_done_us=3_003_500),
        # id=5: no_host_record -- client recovered an id the host never logged.
        _client_frame(5_003_500, wire_frame_id=5, recovered_frame_id=5,
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

    # --- Scenario C: no client records at all -> no ambiguities, but also
    # nothing joined; the predicate must not treat an empty run as a pass.
    print("scenario: empty client trace")
    result = join_records([_host_frame(1, capture_ts_us=1_000_000)], [])
    summary = build_summary(result)
    check(summary["joined"] == 0, f"joined == 0 (got {summary['joined']})")
    check(summary["identity_ambiguities"] == 0,
          f"identity_ambiguities == 0 (got {summary['identity_ambiguities']})")
    check(summary["pass"] is False, "pass predicate is False despite zero ambiguities (joined == 0)")

    print("SELFTEST " + ("PASS" if ok else "FAIL"))
    return ok


if __name__ == "__main__":
    sys.exit(main())
