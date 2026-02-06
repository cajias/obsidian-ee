# Development Guide

## Prerequisites

- Rust 1.75+ (edition 2021)
- Docker & Docker Compose
- Node.js (for Obsidian plugin development)
- `wasm-pack` (for building WASM module)

## Quick Start

```bash
# Build all crates
cargo build --workspace

# Run all tests
cargo test --workspace

# Run linter (fmt + clippy + complexity analysis)
cargo lint

# Format code
cargo fmt --all
```

## Project Structure

```
obsidian-ee/
├── crates/
│   ├── collab-core/       # CRDT + MLS encryption engine
│   ├── collab-relay/      # WebSocket relay server
│   ├── collab-cli/        # CLI client
│   ├── collab-proto/      # Protocol types
│   ├── collab-wasm/       # WASM bindings
│   └── collab-watcher/    # File system watcher
├── plugins/
│   └── obsidian-ee/       # Obsidian TypeScript plugin
├── tests/
│   └── e2e-tests/         # Integration tests
├── docker/                # Docker Compose + Dockerfiles
├── infra/                 # AWS CDK infrastructure
├── scripts/               # Helper scripts
└── xtask/                 # Development task runner
```

## TDD Workflow

This project follows strict Test-Driven Development:

1. **RED**: Write a failing test that defines the expected behavior
2. **GREEN**: Write the minimal code to make the test pass
3. **REFACTOR**: Clean up while keeping tests green

All modules have comprehensive test suites. Run tests before committing:

```bash
cargo test --workspace
```

## Code Quality

### Linting

```bash
cargo lint  # Runs fmt check + clippy + optional complexity analysis
```

Configured thresholds (in `clippy.toml`):
- Maximum function length: 50 lines
- Maximum nesting depth: 3 levels
- Cognitive complexity: 25
- Maximum function arguments: 7

### Workspace-Wide Lints

From `Cargo.toml`:
- `unsafe_code = "deny"` - No unsafe code allowed
- `unused_must_use = "deny"` - Must handle all Result/Option values
- `clippy::all = "deny"` - All standard clippy lints are errors
- `clippy::pedantic = "warn"` - Pedantic lints are warnings
- `clippy::nursery = "warn"` - Experimental lints are warnings

### Pre-Commit Hooks

Install pre-commit hooks:

```bash
pre-commit install
```

Hooks run automatically on commit:
- `cargo fmt` check
- `cargo clippy` linting
- `cargo test` (lib tests only)
- YAML/TOML validation
- Trailing whitespace and EOF fixes
- Large file detection (>1MB)

## Local Development Environment

### Docker Compose

The local environment includes three services:

```bash
# Start all services
docker compose -f docker/docker-compose.yml up -d

# Stop all services
docker compose -f docker/docker-compose.yml down
```

| Service | Port | Purpose |
|---------|------|---------|
| LocalStack | 4566 | AWS service mocks (DynamoDB, SQS) |
| Redis | 6379 | Presence and pub/sub |
| Relay Server | 8080 | WebSocket relay |

### LocalStack Resources

Automatically created on startup via `docker/localstack-init/init-aws.sh`:

| Resource | Type | Purpose |
|----------|------|---------|
| `Documents` | DynamoDB table | Document state storage |
| `MLSMessages` | DynamoDB table | Offline message queue |
| `collab-updates` | SQS queue | Async update processing |

### Running the Relay Independently

```bash
RELAY_ADDR=127.0.0.1:8080 cargo run -p collab-relay
```

## E2E Testing

### Unit-Level E2E Tests (No Docker)

Security and CRDT tests run without Docker:

```bash
cargo test -p e2e-tests
```

Tests include:
- Semantic security (IND-CPA)
- Zero-knowledge relay verification
- Wrong-key decryption failure
- CRDT convergence
- Bidirectional encrypted sync
- Three-user collaboration

### Integration Tests (Requires Docker)

```bash
# Using the helper script
./scripts/e2e-test.sh

# Or manually
docker compose -f docker/docker-compose.yml up -d
cargo test -p e2e-tests -- --ignored --test-threads=1
docker compose -f docker/docker-compose.yml down
```

### Using xtask

```bash
cargo xtask e2e    # Starts Docker, runs tests, and reports results
```

## WASM Development

### Building the WASM Module

```bash
./scripts/build-wasm.sh
```

This builds the `collab-wasm` crate with `wasm-pack` targeting the web platform and copies output to `plugins/obsidian-ee/src/wasm/`.

### Testing WASM Code

WASM tests run as native Rust tests (not in a browser):

```bash
cargo test -p collab-wasm
```

## Obsidian Plugin Development

The TypeScript plugin lives in `plugins/obsidian-ee/`.

```bash
cd plugins/obsidian-ee
npm install
npm run build    # Build plugin
npm test         # Run Jest tests
```

### Plugin Architecture

```
main.ts          # Plugin entry point, WASM initialization, session management
collab-client.ts # WebSocket client with reconnection logic
editor-sync.ts   # Bridges CollabClient with Obsidian's editor
```

### Plugin Commands

| Command | Description |
|---------|-------------|
| Start Collaboration Session | Connect to relay and sync the current document |
| Stop Collaboration Session | Disconnect and clean up WASM resources |

## Worktrees

For parallel development across crates, use git worktrees:

```bash
git worktree add ../obsidian-ee-core -b feature/core-work
git worktree add ../obsidian-ee-relay -b feature/relay-work
git worktree add ../obsidian-ee-infra -b feature/infra-work
```

## CI/CD Pipeline

GitHub Actions workflow (`.github/workflows/ci.yml`):

```
Push/PR → Check & Lint → Test → E2E Tests → Security Audit
```

| Stage | What It Does |
|-------|-------------|
| Check & Lint | `cargo fmt --check`, `cargo lint`, `cargo build --release` |
| Test | `cargo test --workspace` |
| E2E Tests | Docker Compose up, build release, run E2E tests |
| Security Audit | `cargo deny check advisories` (vulnerability scanning) |

## Security Scanning

```bash
# Install cargo-deny
cargo install cargo-deny

# Run advisory check
cargo deny check advisories

# Check licenses
cargo deny check licenses
```

Allowed licenses (from `deny.toml`): MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, Zlib, Unicode-3.0.

## Demo

Run the in-memory E2E encryption demonstration:

```bash
cargo run -p collab-cli -- demo
```

Or use the demo script for the full CLI workflow:

```bash
./scripts/demo-scenario.sh
```

## Useful Commands Reference

| Command | Description |
|---------|-------------|
| `cargo build --workspace` | Build all crates |
| `cargo test --workspace` | Run all tests |
| `cargo lint` | Format + clippy + analysis |
| `cargo fmt --all` | Format all code |
| `cargo xtask e2e` | Run E2E tests with Docker |
| `cargo xtask up` | Start Docker environment |
| `cargo xtask down` | Stop Docker environment |
| `cargo run -p collab-relay` | Run relay server |
| `cargo run -p collab-cli -- demo` | Run demo |
| `./scripts/build-wasm.sh` | Build WASM module |
| `cargo deny check` | Security audit |
