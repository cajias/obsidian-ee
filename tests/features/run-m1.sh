#!/usr/bin/env bash
# Step-runner for tests/features/m1.feature (relay-container-stops-on-sigint).
# Run from repo root. Exit code is the verdict. Covers static gate + runtime stop check.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# Static gate, verbatim from the M1 Exit criterion in 03-implementation-plan.md.
grep -q '^STOPSIGNAL SIGINT' docker/Dockerfile.relay && docker build -f docker/Dockerfile.relay -t relay-test .

# Runtime check (beyond the static gate): the container must stop via SIGINT within
# the grace period, never by SIGKILL at the timeout (exit 137).
docker rm -f relay-m1-check >/dev/null 2>&1 || true
docker run -d --name relay-m1-check relay-test >/dev/null
sleep 2
docker stop -t 10 relay-m1-check >/dev/null
exit_code=$(docker inspect --format '{{.State.ExitCode}}' relay-m1-check)
docker rm relay-m1-check >/dev/null
[ "$exit_code" != "137" ] || { echo "FAIL: container was SIGKILLed at the stop timeout (exit 137)"; exit 1; }
echo "OK: container stopped via SIGINT (exit $exit_code), not killed"
