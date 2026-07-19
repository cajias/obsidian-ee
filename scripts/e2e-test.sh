#!/bin/bash
set -euo pipefail

echo "=== Starting E2E Test Suite ==="

COMPOSE="docker compose -f docker/docker-compose.yml"

# Best-effort: bring up the relay. E2E infra failures must not mask the actual
# test result, so infra bring-up is tolerant; the test command below is not.
if command -v docker >/dev/null 2>&1; then
    if ! $COMPOSE ps --quiet 2>/dev/null | grep -q .; then
        echo "Starting relay via Docker Compose..."
        $COMPOSE up -d || echo "WARN: could not start Docker Compose; Docker-gated tests will be skipped"
    fi

    echo "Waiting for the relay to become healthy..."
    for i in $(seq 1 30); do
        if $COMPOSE ps relay 2>/dev/null | grep -q healthy; then
            echo "Relay is ready!"
            break
        fi
        sleep 2
    done
else
    echo "WARN: docker not available; running only the Docker-independent E2E tests"
fi

echo "Building release binaries..."
cargo build --workspace --release

# Real gate: the Docker-independent full-flow tests must pass. The Docker-gated
# tests are #[ignore]d and only run manually with a live relay.
echo "Running E2E tests..."
cargo test --package e2e-tests --test full_flow

echo "=== E2E Test Suite Complete ==="
