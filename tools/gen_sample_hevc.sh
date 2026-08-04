#!/usr/bin/env bash
set -euo pipefail

# Generate the A0.0 decoder-experiment / harness-receiver test stream.
#
# Runs ON THE UBUNTU BOX (needs the libx265-enabled FFmpeg 7.1 build at
# ~/ffmpeg7). Produces an Annex-B HEVC elementary stream: portrait
# 1080x1920 @ 60 Hz, 8 seconds, bframes=0 (matches the profile's
# `frame_reordering: false` — docs/WIRE.md §9), keyint=60/no-open-gop so
# GOP boundaries are predictable for the decoder experiment's
# --stall-every/--stall-ms induced-delay runs.
#
# Usage: gen_sample_hevc.sh
# Output: ~/resc/sample_1080x1920.h265

FFMPEG_HOME="${FFMPEG_HOME:-$HOME/ffmpeg7}"
export LD_LIBRARY_PATH="$FFMPEG_HOME/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
FFMPEG_BIN="$FFMPEG_HOME/bin/ffmpeg"
FFPROBE_BIN="$FFMPEG_HOME/bin/ffprobe"

OUT="$HOME/resc/sample_1080x1920.h265"
mkdir -p "$(dirname "$OUT")"

if [[ ! -x "$FFMPEG_BIN" ]]; then
    echo "error: $FFMPEG_BIN not found or not executable (expected FFmpeg 7.1 at $FFMPEG_HOME; set FFMPEG_HOME to override)" >&2
    exit 1
fi

echo "generating $OUT ..."
"$FFMPEG_BIN" -y \
    -f lavfi -i testsrc2=size=1080x1920:rate=60 \
    -t 8 \
    -c:v libx265 \
    -pix_fmt yuv420p \
    -x265-params keyint=60:min-keyint=60:no-open-gop=1:bframes=0 \
    -f hevc \
    "$OUT"

if [[ ! -s "$OUT" ]]; then
    echo "error: $OUT is empty or missing after encode" >&2
    exit 1
fi

SIZE=$(wc -c < "$OUT" | tr -d ' ')
echo "wrote $OUT ($SIZE bytes)"

if [[ -x "$FFPROBE_BIN" ]]; then
    PACKET_COUNT=$("$FFPROBE_BIN" -v error -select_streams v:0 -count_packets \
        -show_entries stream=nb_read_packets -of csv=p=0 "$OUT")
    echo "packet count (access units): $PACKET_COUNT"
else
    echo "warning: $FFPROBE_BIN not found; skipping packet count" >&2
fi
