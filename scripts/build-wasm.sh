#!/bin/bash
# Build WASM module for the Obsidian plugin
# Requires: Rust, cargo, wasm-pack

set -e

# Change to project root
cd "$(dirname "$0")/.."

# Colors for output (if terminal supports it)
RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m' # No Color

error() {
    echo -e "${RED}Error: $1${NC}" >&2
    exit 1
}

success() {
    echo -e "${GREEN}$1${NC}"
}

# Validate build environment
echo "Checking build environment..."

# Check for Rust/cargo
if ! command -v cargo &> /dev/null; then
    error "cargo is not installed. Please install Rust: https://rustup.rs/"
fi

# Check for wasm-pack
if ! command -v wasm-pack &> /dev/null; then
    echo "wasm-pack is not installed. Installing..."
    cargo install wasm-pack || error "Failed to install wasm-pack"
fi

# Verify collab-wasm crate exists
WASM_CRATE="crates/collab-wasm"
if [ ! -f "$WASM_CRATE/Cargo.toml" ]; then
    error "collab-wasm crate not found at $WASM_CRATE"
fi

# Create output directory if it doesn't exist
OUTPUT_DIR="plugins/obsidian-ee/src/wasm"
mkdir -p "$OUTPUT_DIR"

echo "Building WASM module..."

# Build WASM with wasm-pack
wasm-pack build "$WASM_CRATE" \
  --target web \
  --out-dir "../../$OUTPUT_DIR" \
  --out-name collab_wasm

# Verify output was created
if [ ! -f "$OUTPUT_DIR/collab_wasm.js" ]; then
    error "Build failed: expected output file not found"
fi

success "WASM build complete: $OUTPUT_DIR/"
