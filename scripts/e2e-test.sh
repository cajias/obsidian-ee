#!/bin/bash
set -euo pipefail

echo "=== Starting E2E Test Suite ==="

COMPOSE="docker compose -f docker/docker-compose.yml"

# ponytail: shared gate invariant with xtask/src/main.rs run_e2e() — gate on the
# relay healthcheck before running tests, and pass `--include-ignored` so BOTH the
# in-process and the #[ignore]d wire tests run. Keep this rule in sync across both
# entry points. The docker-absent/daemon-down case intentionally DIFFERS: this
# script degrades to the non-ignored tests, whereas xtask requires docker and
# returns FAILURE.

echo "Building release binaries..."
cargo build --workspace --release

# `command -v docker` only proves the CLI exists, not that the daemon behind it
# is reachable — `docker compose ps`/`up` would still fail under `set -e` with
# the daemon stopped. `docker info` is the actual liveness check.
if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    if ! $COMPOSE ps --quiet 2>/dev/null | grep -q .; then
        echo "Starting relay via Docker Compose..."
        $COMPOSE up -d
    fi

    echo "Waiting for the relay to become healthy..."
    healthy=0
    for _ in $(seq 1 30); do
        if $COMPOSE ps relay 2>/dev/null | grep -q "(healthy)"; then
            healthy=1
            echo "Relay is healthy."
            break
        fi
        sleep 2
    done

    if [ "$healthy" -ne 1 ]; then
        echo "ERROR: relay never became healthy — refusing to run wire tests against a dead relay." >&2
        exit 1
    fi

    # --include-ignored runs BOTH the regular and the #[ignore]d wire tests;
    # --test-threads=1 avoids relay port contention.
    echo "Running E2E tests (including Docker-gated wire tests)..."
    cargo test --package e2e-tests -- --include-ignored --test-threads=1
else
    echo "SKIP: docker unavailable or daemon not running — wire tests not run"
    echo "Running Docker-independent E2E tests only..."
    cargo test --package e2e-tests
fi

echo "=== E2E Test Suite Complete ==="
