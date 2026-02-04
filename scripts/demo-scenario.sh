#!/bin/bash
# Demo scenario for Obsidian E2E Collaborative Editing
set -e

echo "═══════════════════════════════════════════════════════════════════════"
echo "       Obsidian E2E Collaborative Editing Demo"
echo "       End-to-End Encrypted Document Collaboration"
echo "═══════════════════════════════════════════════════════════════════════"
echo ""

# Clean up any previous demo files
rm -f /tmp/demo-*.json 2>/dev/null || true

echo "This demo shows the complete MLS-based E2E encryption flow:"
echo "  • Alice creates a document and generates an invite"
echo "  • Bob generates a key package and receives the invite"
echo "  • Alice and Bob exchange encrypted messages"
echo "  • Neither the relay server nor anyone else can read the content"
echo ""
echo "═══════════════════════════════════════════════════════════════════════"
echo ""

echo "Step 1: Alice creates a new encrypted document"
echo "────────────────────────────────────────────────────────────────────────"
echo "$ cargo run -q -p collab-cli -- init demo-doc --user alice"
cargo run -q -p collab-cli -- init demo-doc --user alice
echo ""

echo "Step 2: Bob generates a key package (MLS credential)"
echo "────────────────────────────────────────────────────────────────────────"
echo "$ cargo run -q -p collab-cli -- keygen --user bob --output /tmp/demo-bob-key.json"
cargo run -q -p collab-cli -- keygen --user bob --output /tmp/demo-bob-key.json
echo ""
echo "Key package saved to /tmp/demo-bob-key.json"
echo ""

echo "Step 3: Alice creates an invite for Bob using his key package"
echo "────────────────────────────────────────────────────────────────────────"
echo "$ cargo run -q -p collab-cli -- invite demo-doc --user alice \\"
echo "    --keypackage /tmp/demo-bob-key.json --output /tmp/demo-invite.json"
cargo run -q -p collab-cli -- invite demo-doc --user alice \
    --keypackage /tmp/demo-bob-key.json --output /tmp/demo-invite.json
echo ""
echo "Invite saved to /tmp/demo-invite.json"
echo ""

echo "Step 4: Bob joins using the invite"
echo "────────────────────────────────────────────────────────────────────────"
echo "$ cargo run -q -p collab-cli -- join /tmp/demo-invite.json --user bob"
cargo run -q -p collab-cli -- join /tmp/demo-invite.json --user bob
echo ""

echo "═══════════════════════════════════════════════════════════════════════"
echo ""
echo "Step 5: Full In-Memory E2E Collaboration Demo"
echo "────────────────────────────────────────────────────────────────────────"
echo "This runs the complete flow: MLS handshake, encrypted editing, sync"
echo ""
echo "$ cargo run -q -p collab-cli -- demo"
cargo run -q -p collab-cli -- demo
echo ""

echo "═══════════════════════════════════════════════════════════════════════"
echo ""
echo "✓ Demo Complete!"
echo ""
echo "What was demonstrated:"
echo "  ✓ MLS (RFC 9420) key exchange between Alice and Bob"
echo "  ✓ End-to-end encrypted document collaboration"
echo "  ✓ Yrs CRDT for conflict-free concurrent editing"
echo "  ✓ Message content is never visible in plaintext on the network"
echo ""
echo "The relay server only sees encrypted bytes - it cannot read content!"
echo "═══════════════════════════════════════════════════════════════════════"
