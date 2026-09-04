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
# collab-relay returns Ok(()) after tokio::signal::ctrl_c (main.rs), so a clean
# SIGINT stop is deterministically 0. Asserting `!= 137` accepted every other
# nonzero too, so a relay that panicked WHILE shutting down (exit 101) still read
# as "stopped via SIGINT". The Running check above and this one close different
# holes: that one catches a container that never ran, this one a container that
# ran but did not exit cleanly on the signal — a container exiting 0 immediately
# would satisfy this check alone.
[ "$exit_code" = "0" ] || {
  echo "FAIL: expected a clean exit 0 on SIGINT, got $exit_code$([ "$exit_code" = 137 ] && echo ' — SIGKILLed at the stop timeout')" >&2
  exit 1
}
echo "OK: container stopped cleanly via SIGINT (exit 0), not killed"
