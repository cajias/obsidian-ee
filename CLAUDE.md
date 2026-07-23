# Obsidian E2E Collaborative Editing

End-to-end encrypted collaborative document editing using Yrs CRDT and MLS.

## Project Structure

```
obsidian-ee/
├── crates/
│   ├── collab-core/     # Yrs CRDT + MLS encryption
│   ├── collab-relay/    # WebSocket relay server
│   ├── collab-cli/      # CLI client
│   ├── collab-proto/    # Protocol message types
│   ├── collab-wasm/     # WASM bindings for browser/Obsidian clients
│   └── collab-watcher/  # Filesystem watcher for local document sync
├── docker/              # Docker Compose for local dev
├── plugins/obsidian-ee/ # Obsidian plugin
├── tests/e2e-tests/     # End-to-end tests
└── scripts/             # Helper scripts
```

## Build & Test

```bash
# Build all crates
cargo build --workspace

# Run all tests
cargo test --workspace

# Run with clippy lints
cargo lint

# Format code
cargo fmt --all
```

## Development

### TDD Workflow

This project uses strict TDD:
1. **RED:** Write failing test first
2. **GREEN:** Minimal code to pass
3. **REFACTOR:** Clean up while tests stay green

### Worktrees

For parallel development, use git worktrees:
- `obsidian-ee-core` - collab-core development
- `obsidian-ee-relay` - collab-relay development

### Local E2E Testing

```bash
# Start local environment
docker compose -f docker/docker-compose.yml up -d

# Run E2E tests
./scripts/e2e-test.sh

# Stop environment
docker compose -f docker/docker-compose.yml down
```

## Architecture

- **Yrs CRDT**: Conflict-free replicated data types for concurrent editing
- **MLS (RFC 9420)**: End-to-end encryption with forward secrecy
- **WebSocket Relay**: Routes encrypted messages (zero-knowledge); authenticates
  clients (optional bearer token), bounds resources, and queues updates for
  briefly-offline subscribers
- **Offline queue**: In-memory today; DynamoDB-backed persistence is planned
  behind a Cargo feature

## Engineering rules (from audit RCA)

These encode failure classes found in past audits that automated linters do not catch.

### Filesystem-watcher tests
`notify_debouncer_mini` does NOT deliver a 1:1 filesystem-action→event mapping — a
create can be followed by a content `Modified` in a later debounce window. Tests that
observe watcher events MUST drain until the stream goes quiet and assert the expected
kind is *present* (`.any(|e| e.kind == X)`), never `recv()` exactly one event per action.
The crate's `drain_events`/`collect_events` helpers exist for this.

### Reconnect & connection lifecycle
- Every connect attempt MUST settle its promise/future exactly once — including a retry
  attempt whose socket fails *before* opening. A never-settled connect deadlocks the
  reconnect loop (a dedup guard then returns the stale pending promise forever).
- Session start/stop (and any resource-owning lifecycle command) MUST be idempotent:
  guard against a second start that would orphan the prior client/handle.
- The TS client's reconnect behavior must have state-machine tests mirroring the Rust
  CLI's — reconnect logic is duplicated across the two and has regressed on both sides.

### Resource bounds
Any collection fed by untrusted or network-sourced input MUST be bounded by BYTES, not
just by element count — a per-item count cap with MiB-scale items still permits OOM.
Charge/credit the byte counter on every add/remove path and keep it O(1).

### Dead code / YAGNI
Keep internal-crate APIs `pub(crate)` (not `pub`) so `rustc`'s `dead_code` lint flags
unused items — `pub` items in a workspace-internal crate are never reported as dead.
Do not add speculative public surface "for later"; a test that exists only to exercise
otherwise-unused code is a signal to delete the code, not keep it.
