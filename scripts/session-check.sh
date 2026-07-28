#!/usr/bin/env bash
# Issue #47: machine-checkable session assertion. Gates on received == expected
# via exit code over a real (in-process) relay.
set -euo pipefail
CLI=(cargo run -q -p collab-cli --)
EXPECT="Hello, session!"

echo "== positive: peer receives expected -> exit 0 =="
"${CLI[@]}" session-check --expect "$EXPECT"
echo "positive path OK"

echo "== negative: sender diverges -> non-zero exit =="
if "${CLI[@]}" session-check --expect "$EXPECT" --send "tampered payload"; then
    echo "FAIL: mismatch did not produce a non-zero exit" >&2
    exit 1
fi
echo "negative path OK (mismatch exited non-zero)"
echo "session-check fixture PASSED"
