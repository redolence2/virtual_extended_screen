#!/usr/bin/env python3
"""Zero-copy gate metrics (ZERO_COPY_PLAN_review.md acceptance table).

Reads a joined trace (tools/join_trace.py output) and prints/saves the
per-segment percentiles and the decode ordinal-gap distribution with BOTH
denominators (all joined emissions / presented frames), exactly as the
review requires. This is the "small summary checker" the review asked for —
the shell runner itself claims nothing it doesn't evaluate.

Usage: zc_metrics.py JOINED.jsonl [--out METRICS.json]
"""
import json
import sys
from collections import Counter


def pct(sorted_vals, p):
    if not sorted_vals:
        return None
    k = min(len(sorted_vals) - 1, max(0, int(round(p / 100.0 * (len(sorted_vals) - 1)))))
    return sorted_vals[k]


def main():
    path = sys.argv[1]
    out = None
    if "--out" in sys.argv:
        out = sys.argv[sys.argv.index("--out") + 1]

    rows = [json.loads(l) for l in open(path) if l.strip()]
    segs = {
        "capture_to_encode_out_ms": [],
        "encode_out_to_recv_ms": [],
        "recv_to_decode_done_ms": [],
        "decode_done_to_present_ms": [],
        "e2e_capture_to_present_ms": [],
    }
    gaps_all = Counter()
    gaps_presented = Counter()
    presented = 0

    for r in rows:
        cap, enc = r.get("capture_ts_us"), r.get("encode_out_ts_us")
        recv, dec = r.get("ts_recv_us"), r.get("ts_decode_done_us")
        pres, off = r.get("ts_present_us"), r.get("offset_us")
        e2e = r.get("e2e_capture_to_present_us")
        trig, fid = r.get("decode_trigger_frame_id"), r.get("frame_id")

        if cap is not None and enc is not None:
            segs["capture_to_encode_out_ms"].append((enc - cap) / 1000.0)
        if enc is not None and recv is not None and off is not None:
            segs["encode_out_to_recv_ms"].append((recv + off - enc) / 1000.0)
        if recv is not None and dec is not None:
            segs["recv_to_decode_done_ms"].append((dec - recv) / 1000.0)
        if dec is not None and pres is not None:
            segs["decode_done_to_present_ms"].append((pres - dec) / 1000.0)
        if e2e is not None:
            segs["e2e_capture_to_present_ms"].append(e2e / 1000.0)
        if trig is not None and fid is not None:
            g = trig - fid
            gaps_all[g] += 1
            if pres is not None:
                gaps_presented[g] += 1
        if pres is not None:
            presented += 1

    result = {"file": path, "joined": len(rows), "presented": presented, "segments": {}}
    for name, vals in segs.items():
        vals.sort()
        result["segments"][name] = {
            "n": len(vals),
            "p50": round(pct(vals, 50), 3) if vals else None,
            "p90": round(pct(vals, 90), 3) if vals else None,
            "p99": round(pct(vals, 99), 3) if vals else None,
        }
    result["ordinal_gap_all"] = dict(sorted(gaps_all.items(), key=lambda kv: -kv[1])[:8])
    result["ordinal_gap_presented"] = dict(
        sorted(gaps_presented.items(), key=lambda kv: -kv[1])[:8]
    )
    result["ordinal_gap_mode_all"] = gaps_all.most_common(1)[0][0] if gaps_all else None
    result["ordinal_gap_mode_presented"] = (
        gaps_presented.most_common(1)[0][0] if gaps_presented else None
    )

    print(json.dumps(result, indent=2))
    if out:
        with open(out, "w") as f:
            json.dump(result, f, indent=2)


if __name__ == "__main__":
    main()
