#!/bin/bash
# Scripted, reproducible screen churn on the RESC virtual display
# (LATENCY_CODE_AUDIT_review.md §8.3: "fixed scripted content" — replaces the
# owner's manual drag so paired runs are comparable).
#
# Mechanism: a Terminal window is moved onto the virtual display and made to
# scroll continuously. Scrolling text is a deterministic, high-entropy encode
# workload (no cursor motion involved — cursor-present attribution stays clean).
#
# Usage: scripted_workload.sh <seconds>   (backgrounds itself, cleans up)
SECS="${1:-60}"

# The virtual display is portrait 1080x1920 points, placed by macOS to the
# right of the built-in screen; x=2000 lands inside it in every arrangement
# we've observed. Bounds are re-derived at run time from the display list.
read -r VX VY VW VH < <(swift - <<'SWIFT' 2>/dev/null
import CoreGraphics
var c: UInt32 = 0
var ids = [CGDirectDisplayID](repeating: 0, count: 16)
CGGetOnlineDisplayList(16, &ids, &c)
for i in 0..<Int(c) where CGDisplayVendorNumber(ids[i]) == 0x0E5C {
    let b = CGDisplayBounds(ids[i])
    print(Int(b.origin.x), Int(b.origin.y), Int(b.width), Int(b.height))
}
SWIFT
)
[ -z "$VW" ] && { echo "RESC virtual display not found"; exit 1; }

# Window geometry: most of the virtual screen, inset a little.
X=$((VX + 40)); Y=$((VY + 60)); W=$((VW - 80)); H=$((VH - 120))

# Content note (2026-08-12): the first version scrolled random base64. That is
# pathologically high-entropy — it produced a 2.7MB 4K keyframe, over the wire's
# then-2MB cap, black-screening the run (see LATENCY_CODE_AUDIT + the limit fix).
# Real desktop content is far more compressible, so the workload now scrolls
# ORDINARY SOURCE TEXT (this repo's own Rust client) — reproducible, motion-rich,
# and representative of what the owner actually looks at.
SRC=/Users/moyunfei/Downloads/personal/AGI/remote_extended_screen/ubuntu-client/src/main.rs
osascript > /dev/null 2>&1 <<OSA
tell application "Terminal"
    set win to do script "printf 'RESC scripted workload\\n'; end=\$(($SECS + \$(date +%s))); while [ \$(date +%s) -lt \$end ]; do cat '$SRC'; done; exit"
    set bounds of front window to {$X, $Y, $((X + W)), $((Y + H))}
end tell
OSA

exit 0
