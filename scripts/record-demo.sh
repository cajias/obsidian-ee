#!/bin/bash
# Record the demo using asciinema
set -e

# Check for asciinema
if ! command -v asciinema &> /dev/null; then
    echo "Installing asciinema..."
    pip install asciinema
fi

# Build first to avoid compilation output in recording
echo "Building release binaries first..."
cargo build --release -p collab-cli 2>&1 | tail -3

echo ""
echo "Recording demo to demo.cast..."
echo "Press Ctrl+D when done, or let the script finish naturally."
echo ""

asciinema rec demo.cast \
    --title "Obsidian E2E Collaborative Editing" \
    --command "bash scripts/demo-scenario.sh" \
    --idle-time-limit 2 \
    --overwrite

echo ""
echo "═══════════════════════════════════════════════════════════════════════"
echo "Demo recorded to demo.cast"
echo ""
echo "To play:   asciinema play demo.cast"
echo "To upload: asciinema upload demo.cast"
echo ""
echo "To convert to GIF (requires agg):"
echo "  agg demo.cast demo.gif --cols 100 --rows 35"
echo "═══════════════════════════════════════════════════════════════════════"
