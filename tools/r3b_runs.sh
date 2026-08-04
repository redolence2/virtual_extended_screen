#!/bin/bash
# R3b: >=3x repeated selected-profile doctor + harness evidence runs.
set -o pipefail
REPO=/Users/moyunfei/Downloads/personal/AGI/remote_extended_screen
BOX=wan@192.168.50.47
EVID="$REPO/evidence/a00/wip"
FAIL=0

echo "== host doctor x3 =="
H0=$(grep -c doctor_complete ~/Library/Logs/RESC/host.jsonl)
for i in 1 2 3; do
  "$REPO/mac-host/.build/debug/remote-display-host" --doctor > "$EVID/r3b-host-doctor-$i.json" 2>/dev/null
  echo "host doctor run $i: exit=$?"
  [ ${PIPESTATUS[0]:-$?} -ne 0 ] 2>/dev/null && FAIL=1
done
H1=$(grep -c doctor_complete ~/Library/Logs/RESC/host.jsonl)
echo "host doctor_complete: $H0 -> $H1 (expect +3)"

echo "== client doctor x3 (sw1-lowdelay, selected backend) =="
C0=$(ssh "$BOX" 'grep -c doctor_complete ~/.local/state/resc/client.jsonl 2>/dev/null || echo 0' </dev/null)
for i in 1 2 3; do
  ssh "$BOX" 'cd ~/resc/remote_extended_screen/ubuntu-client && \
    env LD_LIBRARY_PATH=$HOME/ffmpeg7/lib SDL_VIDEODRIVER=dummy \
    ./target/release/remote-display-client --doctor --doctor-backend sw1-lowdelay \
      --sample /home/wan/resc/sample_1080x1920.h265 > /tmp/r3b-client-doctor-'$i'.json 2>/dev/null; echo "client doctor run '$i': exit=$?"' </dev/null
done
C1=$(ssh "$BOX" 'grep -c doctor_complete ~/.local/state/resc/client.jsonl 2>/dev/null || echo 0' </dev/null)
echo "client doctor_complete: $C0 -> $C1 (expect +3)"
scp -q "$BOX":/tmp/r3b-client-doctor-{1,2,3}.json "$EVID/" </dev/null

echo "== harness pair x3 (10s each, window 1) =="
for i in 1 2 3; do
  ssh "$BOX" 'cd ~/resc/remote_extended_screen/ubuntu-client && \
    nohup env LD_LIBRARY_PATH=$HOME/ffmpeg7/lib \
      ./target/release/harness-receiver --listen 0.0.0.0:9871 --backend sw1-lowdelay \
      --json-out /tmp/r3b-recv-'$i'.json < /dev/null > /tmp/r3b-recv-'$i'.log 2>&1 &' </dev/null >/dev/null 2>&1 &
  sleep 3
  "$REPO/mac-host/.build/debug/resc-harness-sender" --connect 192.168.50.47 --port 9871 \
      --seconds 10 --json-out "$EVID/r3b-send-$i.json" > /tmp/r3b-send-$i.log 2>&1
  SEND_EXIT=$?
  echo "harness run $i: sender exit=$SEND_EXIT"
  [ $SEND_EXIT -ne 0 ] && FAIL=1
  sleep 3
  ssh "$BOX" 'pkill -f harness-receiver 2>/dev/null; true' </dev/null
done
scp -q "$BOX":/tmp/r3b-recv-{1,2,3}.json "$EVID/" </dev/null || echo "recv report scp incomplete"

echo "== predicate summary =="
python3 - <<'EOF'
import json, glob, sys
fail = 0
for f in sorted(glob.glob("/Users/moyunfei/Downloads/personal/AGI/remote_extended_screen/evidence/a00/wip/r3b-send-*.json")):
    r = json.load(open(f))
    print(f, "sustained_60hz:", r.get("sustained_60hz"), "sent:", r.get("frames_sent"), "acked:", r.get("frames_acked"),
          "order_viol:", r.get("ack_order_violation"), "write_errors:", r.get("write_errors"))
    if not r.get("sustained_60hz"): fail = 1
for f in sorted(glob.glob("/Users/moyunfei/Downloads/personal/AGI/remote_extended_screen/evidence/a00/wip/r3b-recv-*.json")):
    r = json.load(open(f))
    print(f, "pass:", r.get("pass"), "report_v:", r.get("report_v"))
    if not r.get("pass"): fail = 1
sys.exit(fail)
EOF
PRED=$?
[ $PRED -ne 0 ] && FAIL=1
echo "R3B RESULT: $([ $FAIL -eq 0 ] && echo ALL GREEN || echo FAILURES)"
exit $FAIL
