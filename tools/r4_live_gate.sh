#!/bin/bash
# R4 evidence-gate live run: traced host (Mac) -> traced client (box), 30s,
# then join and evaluate the exact-identity pass predicate.
#
# Termination (C3 / A00_COMPLETION_REPORT_AMENDED_response_review.md
# amendment 3: "The live runner must request graceful termination, wait for
# clean exits, and fail on timeout/forced kill."): both processes are asked
# to shut down via SIGTERM (the client's trace-mode clean-shutdown path —
# ubuntu-client/src/main.rs — drains its admitted queue, flushes the decoder
# tail through the identity ledger, and seals a `trace_complete` footer
# before exiting; the host's equivalent footer path is owned by a different
# worker and is not assumed to exist yet here — see the touch-scope note
# below), then polled for up to 10s each. A process that needs SIGKILL or
# times out FAILS the gate outright: exit nonzero, no join attempted. This
# replaces the old unconditional pkill/kill + fixed sleep, which accepted
# whatever JSONL prefix a hard-killed process happened to have flushed.
set -o pipefail
REPO=/Users/moyunfei/Downloads/personal/AGI/remote_extended_screen
BOX=wan@192.168.50.47
HOST_TRACE_DIR="$HOME/Library/Logs/RESC"
EVID="$REPO/evidence/a00/wip"
RUN_TAG=r4-live
TERM_TIMEOUT_S=10

cleanup() {
  # Last-resort safety net only — the graceful termination below already
  # handles the expected path. Never leave a stray host/client running
  # (the host's virtual display would persist).
  [ -n "$HOST_PID" ] && kill -9 "$HOST_PID" 2>/dev/null
  ssh "$BOX" 'pkill -9 remote-display' 2>/dev/null
}
trap cleanup EXIT

# NOTE on every remote pgrep/pkill below: deliberately NOT `-f` (full
# command line match). `ssh "$BOX" '... pgrep -f "target/release/
# remote-display-client" ...'` is executed remotely as `sh -c '<that exact
# text>'` — so the invoking shell's OWN argv contains the search pattern
# verbatim, and `-f` matches it, killing/counting the shell running this
# very script instead of (or in addition to) the real target (confirmed
# live: `pkill -9 -f "target/release/remote-display-client"` over ssh
# killed its own `bash -c` invocation, dropping the ssh session with no
# further output). Matching by bare process name instead is immune to
# this: the client's kernel `comm` is `remote-display-` (`comm` truncates
# at 15 bytes), so `remote-display` — a safe prefix — matches only the
# real target, never an ssh/bash wrapper whose comm is `ssh`/`bash`.

# Polls local pid $1 for up to $2 seconds. Echoes nothing; returns 0 if the
# process exited on its own, 1 (after escalating to SIGKILL) on timeout —
# the exact "needs SIGKILL or times out FAILS the gate" rule.
wait_for_local_exit() {
  local pid="$1" timeout_s="$2" waited=0
  while kill -0 "$pid" 2>/dev/null; do
    if [ "$waited" -ge "$timeout_s" ]; then
      echo "TIMEOUT: local pid $pid did not exit within ${timeout_s}s — sending SIGKILL" >&2
      kill -9 "$pid" 2>/dev/null
      return 1
    fi
    sleep 1
    waited=$((waited + 1))
  done
  return 0
}

# Same as wait_for_local_exit, but for a pid living on $BOX (one ssh round
# trip per poll).
wait_for_remote_exit() {
  local pid="$1" timeout_s="$2" waited=0
  while ssh "$BOX" "kill -0 $pid 2>/dev/null" </dev/null; do
    if [ "$waited" -ge "$timeout_s" ]; then
      echo "TIMEOUT: remote pid $pid did not exit within ${timeout_s}s — sending SIGKILL" >&2
      ssh "$BOX" "kill -9 $pid 2>/dev/null" </dev/null
      return 1
    fi
    sleep 1
    waited=$((waited + 1))
  done
  return 0
}

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
# Fire-and-forget the ssh call itself (backgrounded): even with full fd
# redirection on the *client* process, the ssh session lingered on prior
# attempts (2026-08-04) — this launch strategy is unchanged from before.
# What's new: the remote script's stdout (just its `echo $!` line — the
# client process's own stdout is separately redirected to its log file) is
# captured to a file instead of discarded, so this same backgrounded launch
# now also yields the client's real pid for SIGTERM-by-pid below.
CLIENT_PID_FILE="$(mktemp -t r4-client-pid)"
# The `cd` MUST be its own statement, never `cd ... && nohup ... &`: bash
# backgrounds a compound and-list as a SUBSHELL, so `$!` is the wrapper's
# pid, not the client's — SIGTERM then kills the wrapper and orphans the
# nohup'd client, which never runs its shutdown/footer sequence (the C7
# second live-gate failure, 2026-08-05; reproduced in isolation: with
# `cd &&` the pids differ, as separate statements they match).
ssh "$BOX" 'cd ~/resc/remote_extended_screen/ubuntu-client
  nohup env RESC_TRACE=1 RESC_LOG_DIR=/tmp/resc-r4-trace DISPLAY=:0 \
    LD_LIBRARY_PATH=$HOME/ffmpeg7/lib \
    ./target/release/remote-display-client -H 192.168.50.125 \
    < /dev/null > /tmp/resc-r4-client.log 2>&1 &
  echo $!' </dev/null >"$CLIENT_PID_FILE" 2>&1 &
sleep 5
CLIENT_PID="$(tr -d '[:space:]' < "$CLIENT_PID_FILE" 2>/dev/null)"
rm -f "$CLIENT_PID_FILE"
case "$CLIENT_PID" in
  ''|*[!0-9]*) echo "WARNING: could not capture client pid (got '$CLIENT_PID') — will fall back to pkill -TERM by name at shutdown"; CLIENT_PID="" ;;
  *) echo "client pid: $CLIENT_PID" ;;
esac
ssh "$BOX" 'pgrep remote-display >/dev/null && echo client-running || echo CLIENT-NOT-RUNNING' </dev/null

echo "== streaming for ${STREAM_SECS:-30}s =="
sleep "${STREAM_SECS:-30}"

echo "== stop: client first (full drain/tail/footer while the host lives), then host =="
GATE_FAILED=0

# Termination ORDER is load-bearing (C7 live-gate finding, 2026-08-05):
# SIGTERMing both ends concurrently let the host die first, so the client
# hit its control-channel-EOF exit path before its SIGTERM shutdown
# sequence ran — no client footer, and the joiner (correctly) refused the
# truncated trace. The orderly protocol is sequential: the client completes
# its entire trace-mode shutdown (drain admitted queue -> decoder tail ->
# clean footer) while the host is still alive, and only then does the host
# get its SIGTERM. A host that dies mid-trace outside this script still
# yields a footerless client trace — and that run SHOULD fail the gate.
if [ -n "$CLIENT_PID" ]; then
  ssh "$BOX" "kill -TERM $CLIENT_PID 2>/dev/null" </dev/null
else
  ssh "$BOX" 'pkill -TERM remote-display' </dev/null
fi

if [ -n "$CLIENT_PID" ]; then
  if wait_for_remote_exit "$CLIENT_PID" "$TERM_TIMEOUT_S"; then
    echo "client exited cleanly"
  else
    echo "CLIENT TERMINATION FAILED (forced SIGKILL) — gate FAILS"
    GATE_FAILED=1
  fi
else
  # No pid captured: fall back to polling process presence by name instead
  # of by pid (C3 §6's explicit fallback path).
  waited=0
  while ssh "$BOX" 'pgrep remote-display >/dev/null 2>&1' </dev/null; do
    if [ "$waited" -ge "$TERM_TIMEOUT_S" ]; then
      echo "TIMEOUT: client (no pid) did not exit within ${TERM_TIMEOUT_S}s — sending SIGKILL" >&2
      ssh "$BOX" 'pkill -9 remote-display' </dev/null
      echo "CLIENT TERMINATION FAILED (forced SIGKILL) — gate FAILS"
      GATE_FAILED=1
      break
    fi
    sleep 1
    waited=$((waited + 1))
  done
  [ "$GATE_FAILED" -eq 0 ] && echo "client exited cleanly"
fi
ssh "$BOX" 'echo "client log tail:"; tail -8 /tmp/resc-r4-client.log' </dev/null

# Only now — with the client's footer safely on disk — terminate the host.
kill -TERM "$HOST_PID" 2>/dev/null
if wait_for_local_exit "$HOST_PID" "$TERM_TIMEOUT_S"; then
  echo "host exited cleanly"
else
  echo "HOST TERMINATION FAILED (forced SIGKILL) — gate FAILS"
  GATE_FAILED=1
fi
HOST_PID=""

if [ "$GATE_FAILED" -ne 0 ]; then
  echo "== gate FAILED: termination was not clean on both sides; no join attempted =="
  echo "== host log tail =="
  tail -6 /tmp/r4-host.log
  exit 4
fi

echo "== collect traces =="
scp -q "$BOX":/tmp/resc-r4-trace/client-trace.jsonl "$EVID/$RUN_TAG-client-trace.jsonl" || { echo "NO CLIENT TRACE"; exit 3; }
cp "$HOST_TRACE_DIR/host-trace.jsonl" "$EVID/$RUN_TAG-host-trace.jsonl" || { echo "NO HOST TRACE"; exit 3; }
wc -l "$EVID/$RUN_TAG-host-trace.jsonl" "$EVID/$RUN_TAG-client-trace.jsonl"

echo "== join =="
# --causal-slack-us 16667 (exactly one 60 Hz frame period), documented per
# the review's "explain and bound every violation" rule: ScreenCaptureKit
# stamps a frame's PTS at its vblank-aligned presentation instant, which
# legitimately LEADS encode-completion wall time by up to one frame period
# (delivery + encode finish before the stamped refresh). Measured through
# the contracted CMSyncConvertTime path on 2026-08-05: 24/45 samples led,
# max 9,190 us — every one inside the period. A violation beyond one frame
# period would be a genuine clock/conversion defect and still fails.
python3 "$REPO/tools/join_trace.py" \
  --host "$EVID/$RUN_TAG-host-trace.jsonl" \
  --client "$EVID/$RUN_TAG-client-trace.jsonl" \
  --causal-slack-us 16667 \
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
