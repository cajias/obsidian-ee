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
