#!/bin/bash
set -e

cd "$(dirname "$0")/.."

# Check if wasm-pack is installed
if ! command -v wasm-pack &> /dev/null; then
    echo "wasm-pack is not installed. Installing..."
    cargo install wasm-pack
fi

# Build WASM with wasm-pack
wasm-pack build crates/collab-wasm \
  --target web \
  --out-dir ../../plugins/obsidian-ee/src/wasm \
  --out-name collab_wasm

echo "WASM build complete: plugins/obsidian-ee/src/wasm/"
