#!/bin/bash
# RESC Host Launcher
# Edit these defaults to match your setup:

CLIENT_IP="192.168.50.47"
WIDTH=1080
HEIGHT=1920
FPS=60
CODEC="--hevc"
PORT=9870

# Path to the built binary
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="$SCRIPT_DIR/.build/release/remote-display-host"

if [ ! -f "$BIN" ]; then
    # Try debug build
    BIN="$SCRIPT_DIR/.build/debug/remote-display-host"
fi

if [ ! -f "$BIN" ]; then
    osascript -e 'display alert "RESC Host" message "Binary not found. Run: cd mac-host && swift build -c release"'
    exit 1
fi

exec "$BIN" "$WIDTH" "$HEIGHT" "$FPS" $CODEC --client "$CLIENT_IP" --port "$PORT"
