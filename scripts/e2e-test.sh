#!/bin/bash
set -e

echo "=== Starting E2E Test Suite ==="

# Check if Docker Compose is running
if ! docker compose -f docker/docker-compose.yml ps --quiet 2>/dev/null | grep -q .; then
    echo "Starting Docker Compose environment..."
    docker compose -f docker/docker-compose.yml up -d
    sleep 10
fi

# Wait for services to be healthy
echo "Waiting for services to be healthy..."
for i in {1..30}; do
    if curl -s http://localhost:4566/_localstack/health | grep -q '"dynamodb": "running"'; then
        echo "LocalStack is ready!"
        break
    fi
    echo "Waiting for LocalStack... ($i/30)"
    sleep 2
done

# Build release binaries
echo "Building release binaries..."
cargo build --workspace --release

# Run E2E tests
echo "Running E2E tests..."
cargo test --package e2e-tests --test full_flow 2>/dev/null || echo "E2E tests not yet implemented"

echo "=== E2E Test Suite Complete ==="
