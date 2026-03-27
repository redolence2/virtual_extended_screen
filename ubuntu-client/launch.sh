#!/bin/bash
# RESC Client Launcher
# Edit these defaults to match your setup:

HOST="192.168.50.125"
WIDTH=1080
HEIGHT=1920
PORT=9870
EXTRA_ARGS="--no-flash"

# Path to the built binary
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="$SCRIPT_DIR/target/release/remote-display-client"

if [ ! -f "$BIN" ]; then
    echo "Binary not found at $BIN"
    echo "Run: cd $SCRIPT_DIR && cargo build --release"
    read -p "Press Enter to close..."
    exit 1
fi

exec "$BIN" --host "$HOST" --port "$PORT" --width "$WIDTH" --height "$HEIGHT" $EXTRA_ARGS
