#!/bin/bash
# Repeated selected-profile doctor + harness evidence runs — fail-closed v2.
#
# C5 rebuild per A00_COMPLETION_REPORT_AMENDED_review.md finding 6 and
# A00_COMPLETION_REPORT_AMENDED_response_review.md amendment 6: unique run
# tokens; every exit captured immediately (including the remote receiver's,
# via an exit-file wrapper — a trailing `echo` after a remote command masks
# its status); the host's REAL doctor_host.json copied and parsed (stdout is
# retained separately as .log); per-run doctor_complete deltas asserted == 1
# (not a confusable global +3); copy/missing-artifact failures fatal; only
# this run's uniquely-named artifacts validated; sender_integrity_pass and
# the receiver's full v2 predicate required — never a bare exit code or the
# legacy `sustained_60hz` name.
set -u
REPO=/Users/moyunfei/Downloads/personal/AGI/remote_extended_screen
BOX=wan@192.168.50.47
EVID="$REPO/evidence/a00/wip"
TOKEN="r3b-$(date +%Y%m%d%H%M%S)-$$"
HOST_LOG="$HOME/Library/Logs/RESC/host.jsonl"
FAILURES=0

die() { echo "FATAL: $*" >&2; exit 1; }
flag() { echo "FAIL: $*" >&2; FAILURES=$((FAILURES + 1)); }

json_field() { # file, python expr over parsed `r`
  python3 -c "import json,sys; r=json.load(open('$1')); print($2)" 2>/dev/null
}

echo "== run token: $TOKEN =="
mkdir -p "$EVID"

echo "== host doctor x3 (each run: exit + report copy + doctor_complete +1) =="
for i in 1 2 3; do
  BEFORE=$(grep -c doctor_complete "$HOST_LOG" 2>/dev/null || echo 0)
  "$REPO/mac-host/.build/debug/remote-display-host" --doctor > "$EVID/$TOKEN-host-doctor-$i.log" 2>&1
  EXIT=$?
  [ $EXIT -eq 0 ] || flag "host doctor run $i exit=$EXIT"
  cp "$HOME/Library/Logs/RESC/doctor_host.json" "$EVID/$TOKEN-host-doctor-$i.json" \
    || die "host doctor run $i: doctor_host.json missing"
  OK=$(json_field "$EVID/$TOKEN-host-doctor-$i.json" "r.get('exit_code')")
  [ "$OK" = "0" ] || flag "host doctor run $i report exit_code=$OK"
  AFTER=$(grep -c doctor_complete "$HOST_LOG" 2>/dev/null || echo 0)
  [ $((AFTER - BEFORE)) -eq 1 ] || flag "host doctor run $i doctor_complete delta $((AFTER - BEFORE)) != 1"
  echo "host doctor run $i: exit=$EXIT report_exit=$OK delta=+$((AFTER - BEFORE))"
done

echo "== client doctor x3 (isolated RESC_LOG_DIR per run) =="
for i in 1 2 3; do
  RDIR="/tmp/$TOKEN-cd$i"
  ssh "$BOX" "cd ~/resc/remote_extended_screen/ubuntu-client && \
    env RESC_LOG_DIR=$RDIR LD_LIBRARY_PATH=\$HOME/ffmpeg7/lib SDL_VIDEODRIVER=dummy \
    ./target/release/remote-display-client --doctor --doctor-backend sw1-lowdelay \
      --sample /home/wan/resc/sample_1080x1920.h265 > /tmp/$TOKEN-cd$i.json 2>/tmp/$TOKEN-cd$i.log" </dev/null
  EXIT=$?
  [ $EXIT -eq 0 ] || flag "client doctor run $i ssh/doctor exit=$EXIT"
  CC=$(ssh "$BOX" "grep -c doctor_complete $RDIR/client.jsonl 2>/dev/null || echo 0" </dev/null)
  [ "$CC" = "1" ] || flag "client doctor run $i isolated doctor_complete count $CC != 1"
  scp -q "$BOX:/tmp/$TOKEN-cd$i.json" "$EVID/$TOKEN-client-doctor-$i.json" \
    || die "client doctor run $i report copy failed"
  echo "client doctor run $i: exit=$EXIT doctor_complete=$CC"
done

echo "== harness pair x3 (receiver exit captured via exit-file wrapper) =="
for i in 1 2 3; do
  # The launch ssh is itself backgrounded and its liveness verified by a
  # separate probe: a foreground ssh whose remote command backgrounds a
  # process lingers indefinitely despite full fd redirection (the bug that
  # wedged both early live-gate runs AND this script's own first run on
  # 2026-08-05 — same class tools/r4_live_gate.sh fixed). Process matching
  # is by bare name, never pkill/pgrep -f (the ssh'd shell's argv contains
  # the pattern text and self-matches).
  ssh "$BOX" "cd ~/resc/remote_extended_screen/ubuntu-client && rm -f /tmp/$TOKEN-recv-$i.exit && \
    nohup bash -c 'env LD_LIBRARY_PATH=\$HOME/ffmpeg7/lib \
      ./target/release/harness-receiver --listen 0.0.0.0:9871 --backend sw1-lowdelay \
      --json-out /tmp/$TOKEN-recv-$i.json > /tmp/$TOKEN-recv-$i.log 2>&1; \
      echo \$? > /tmp/$TOKEN-recv-$i.exit' < /dev/null > /dev/null 2>&1 &" \
    </dev/null >/dev/null 2>&1 &
  sleep 3
  ssh "$BOX" "pgrep harness-receive >/dev/null" </dev/null \
    || die "harness run $i: receiver not running after launch"
  "$REPO/mac-host/.build/debug/resc-harness-sender" --connect 192.168.50.47 --port 9871 \
      --seconds 10 --json-out "$EVID/$TOKEN-send-$i.json" > "$EVID/$TOKEN-send-$i.log" 2>&1
  SEND_EXIT=$?
  [ $SEND_EXIT -eq 0 ] || flag "harness run $i sender exit=$SEND_EXIT"
  # Receiver exits on its own after the sender disconnects (EOF/tail); wait
  # for its exit file rather than killing it.
  RECV_EXIT=""
  for _ in $(seq 1 20); do
    RECV_EXIT=$(ssh "$BOX" "cat /tmp/$TOKEN-recv-$i.exit 2>/dev/null" </dev/null)
    [ -n "$RECV_EXIT" ] && break
    sleep 1
  done
  [ -n "$RECV_EXIT" ] || die "harness run $i: receiver never wrote its exit file"
  [ "$RECV_EXIT" = "0" ] || flag "harness run $i receiver exit=$RECV_EXIT"
  scp -q "$BOX:/tmp/$TOKEN-recv-$i.json" "$EVID/$TOKEN-recv-$i.json" \
    || die "harness run $i receiver report copy failed"
  echo "harness run $i: sender exit=$SEND_EXIT receiver exit=$RECV_EXIT"
done

echo "== predicate validation (this run's artifacts only) =="
for i in 1 2 3; do
  S="$EVID/$TOKEN-send-$i.json"
  [ "$(json_field "$S" "r.get('sender_integrity_pass')")" = "True" ] \
    || flag "send-$i sender_integrity_pass != true"
  [ "$(json_field "$S" "r.get('harness_report_v')")" = "2" ] || flag "send-$i not v2"
  R="$EVID/$TOKEN-recv-$i.json"
  [ "$(json_field "$R" "r.get('pass')")" = "True" ] || flag "recv-$i pass != true"
  [ "$(json_field "$R" "r.get('report_v')")" = "2" ] || flag "recv-$i not report_v 2"
done

echo "== result =="
if [ $FAILURES -eq 0 ]; then
  echo "R3B ($TOKEN): ALL GREEN"
  exit 0
fi
echo "R3B ($TOKEN): $FAILURES FAILURE(S)"
exit 1
