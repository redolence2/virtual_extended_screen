#!/bin/bash
# Interleaved A/B driver (LATENCY_CODE_AUDIT_review.md §8.3-8.4): runs
# baseline/candidate alternately so monotonic host drift (thermal, load) cancels
# instead of being attributed to the change under test. Injects the scripted
# workload into each run and closes its window afterwards.
#
# Usage: ab_interleaved.sh <tag-prefix> <candidate-CLIENT_ENV>
set -u
REPO=/Users/moyunfei/Downloads/personal/AGI/remote_extended_screen
EVID="$REPO/evidence/zero_copy"
PREFIX="${1:-ab}"
CAND_ENV="${2:-}"
HOST_ARGS="2160 3840 60 --client 192.168.50.47 --hevc --bitrate 50"

run_one() {
  local tag="$1" env="$2"
  echo "=== $tag (CLIENT_ENV='$env') ==="
  CLIENT_ENV="$env" STREAM_SECS=60 RUN_TAG="$tag" EVID="$EVID" HOST_ARGS="$HOST_ARGS" \
    bash "$REPO/tools/r4_live_gate.sh" > "/tmp/$tag-gate.log" 2>&1 &
  local gate_pid=$!
  ssh -o BatchMode=yes -o ConnectTimeout=8 wan@192.168.50.47 'sleep 18' 2>/dev/null
  bash "$REPO/tools/scripted_workload.sh" 60 >/dev/null 2>&1
  wait $gate_pid
  local rc=$?
  osascript -e 'tell application "Terminal" to close (every window whose name does not contain "tail")' >/dev/null 2>&1
  grep -E "^PASS|^FAIL" "/tmp/$tag-gate.log" | head -1
  if [ $rc -eq 0 ]; then
    python3 "$REPO/tools/zc_metrics.py" "$EVID/$tag-joined.jsonl" --out "$EVID/$tag-metrics.json" >/dev/null 2>&1
  fi
  # brief settle between runs
  ssh -o BatchMode=yes -o ConnectTimeout=8 wan@192.168.50.47 'sleep 20' 2>/dev/null
}

run_one "${PREFIX}-base1" ""
run_one "${PREFIX}-cand1" "$CAND_ENV"
run_one "${PREFIX}-base2" ""
run_one "${PREFIX}-cand2" "$CAND_ENV"
echo "=== interleaved A/B complete ==="
