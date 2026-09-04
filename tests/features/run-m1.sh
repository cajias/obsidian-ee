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
# `docker run -d` exits 0 even when the entrypoint dies immediately, and `docker
# stop` on an already-exited container also exits 0 — so without this assertion a
# relay that panicked at boot reports "OK: stopped via SIGINT" for a signal it
# never received. Confirm the process is actually up before signalling it.
if [ "$(docker inspect --format '{{.State.Running}}' relay-m1-check)" != true ]; then
  echo "FAIL: relay was not running before the stop, so SIGINT was never exercised" >&2
  docker logs relay-m1-check 2>&1 | tail -20 >&2
  docker rm -f relay-m1-check >/dev/null
  exit 1
fi
docker stop -t 10 relay-m1-check >/dev/null
exit_code=$(docker inspect --format '{{.State.ExitCode}}' relay-m1-check)
docker rm relay-m1-check >/dev/null
[ "$exit_code" != "137" ] || { echo "FAIL: container was SIGKILLed at the stop timeout (exit 137)"; exit 1; }
echo "OK: container stopped via SIGINT (exit $exit_code), not killed"
