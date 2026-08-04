#!/bin/bash
# R4 evidence-gate live run: traced host (Mac) -> traced client (box), 30s,
# then join and evaluate the exact-identity pass predicate.
set -o pipefail
REPO=/Users/moyunfei/Downloads/personal/AGI/remote_extended_screen
BOX=wan@192.168.50.47
HOST_TRACE_DIR="$HOME/Library/Logs/RESC"
EVID="$REPO/evidence/a00/wip"
RUN_TAG=r4-live

cleanup() {
  # Never leave a stray host running (virtual display would persist).
  [ -n "$HOST_PID" ] && kill "$HOST_PID" 2>/dev/null
  ssh "$BOX" 'pkill -f "target/release/remote-display-client"' 2>/dev/null
}
trap cleanup EXIT

echo "== preflight: archive any old host trace =="
mkdir -p "$HOST_TRACE_DIR"
[ -f "$HOST_TRACE_DIR/host-trace.jsonl" ] && mv "$HOST_TRACE_DIR/host-trace.jsonl" "$HOST_TRACE_DIR/host-trace.pre-r4.jsonl"

echo "== preflight: fresh client trace dir on box =="
ssh "$BOX" 'rm -rf /tmp/resc-r4-trace && mkdir -p /tmp/resc-r4-trace'

echo "== start host (Mac, RESC_TRACE=1, HEVC -> 192.168.50.47) =="
cd "$REPO/mac-host"
RESC_TRACE=1 ./.build/debug/remote-display-host --client 192.168.50.47 --hevc \
  > /tmp/r4-host.log 2>&1 &
HOST_PID=$!
echo "host pid: $HOST_PID"
sleep 8
if ! kill -0 "$HOST_PID" 2>/dev/null; then
  echo "HOST DIED EARLY — log tail:"; tail -20 /tmp/r4-host.log; exit 2
fi

echo "== start client (box, RESC_TRACE=1, DISPLAY=:0) =="
# Fire-and-forget: even with full fd redirection the ssh session lingered on
# both prior attempts (2026-08-04), so we background ssh itself and verify
# the client's presence with a separate quick probe instead of trusting the
# launch session to exit.
ssh "$BOX" 'cd ~/resc/remote_extended_screen/ubuntu-client && \
  nohup env RESC_TRACE=1 RESC_LOG_DIR=/tmp/resc-r4-trace DISPLAY=:0 \
    LD_LIBRARY_PATH=$HOME/ffmpeg7/lib \
    ./target/release/remote-display-client -H 192.168.50.125 \
    < /dev/null > /tmp/resc-r4-client.log 2>&1 &' </dev/null >/dev/null 2>&1 &
sleep 5
ssh "$BOX" 'pgrep -f "target/release/remote-display-client" >/dev/null && echo client-running || echo CLIENT-NOT-RUNNING' </dev/null

echo "== streaming for ${STREAM_SECS:-30}s =="
sleep "${STREAM_SECS:-30}"

echo "== stop both =="
ssh "$BOX" 'pkill -f "target/release/remote-display-client"; sleep 1; echo "client log tail:"; tail -8 /tmp/resc-r4-client.log'
kill "$HOST_PID" 2>/dev/null
sleep 2
HOST_PID=""

echo "== collect traces =="
scp -q "$BOX":/tmp/resc-r4-trace/client-trace.jsonl "$EVID/$RUN_TAG-client-trace.jsonl" || { echo "NO CLIENT TRACE"; exit 3; }
cp "$HOST_TRACE_DIR/host-trace.jsonl" "$EVID/$RUN_TAG-host-trace.jsonl" || { echo "NO HOST TRACE"; exit 3; }
wc -l "$EVID/$RUN_TAG-host-trace.jsonl" "$EVID/$RUN_TAG-client-trace.jsonl"

echo "== join =="
python3 "$REPO/tools/join_trace.py" \
  --host "$EVID/$RUN_TAG-host-trace.jsonl" \
  --client "$EVID/$RUN_TAG-client-trace.jsonl" \
  --out "$EVID/$RUN_TAG-joined.jsonl" \
  --summary "$EVID/$RUN_TAG-join-summary.json"
JOIN_EXIT=$?
echo "JOINER EXIT: $JOIN_EXIT"
echo "== summary =="
cat "$EVID/$RUN_TAG-join-summary.json"
echo
echo "== host log tail =="
tail -6 /tmp/r4-host.log
exit $JOIN_EXIT
