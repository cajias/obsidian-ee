#!/usr/bin/env bash
# Step-runner for tests/features/m2.feature (cdk-synth-emits-four-stacks).
# Run from repo root. Exit code is the verdict.
# M2 Exit criterion (as amended by the plan change recorded in 03, M2 section).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# Gate commands, verbatim from the M2 Exit criterion in 03-implementation-plan.md.
# The plan runs both lines in one shell (cd infra persists into the second line);
# here each is wrapped in a subshell and teed so its output reaches the CI log
# AND can be asserted on, without altering the command text itself. The trailing
# `|| true`: `grep -c` exits 1 on a zero count and a bare non-zero exit under
# `set -e` would kill the script before its own descriptive FAIL line prints —
# the exact failure this gate exists to report.
#
# The count below is an AGGREGATE across all synthesized templates; the
# per-stack "exactly one relay instance" assertion lives in
# infra/test/relay-stack.test.ts (same CI job), so this is a smoke check.
log=$(mktemp)
trap 'rm -f "$log"' EXIT

# First: must print 3.
(cd infra && npm ci && npx cdk synth --quiet && cat cdk.out/*.template.json | grep -c 'AWS::EC2::Instance') | tee "$log" || true
instance_count=$(tail -n 1 "$log")  # count is the last stdout line; npm ci's install summary precedes it
[ "$instance_count" = "3" ] || { echo "FAIL: expected cdk synth to report 3 AWS::EC2::Instance resources, got $instance_count"; exit 1; }

# Second: must list RelayShared, Relay-dev, Relay-staging, Relay-prod.
(cd infra && npx cdk list) | tee "$log" || true
stack_list=$(cat "$log")
for stack in RelayShared Relay-dev Relay-staging Relay-prod; do
  echo "$stack_list" | grep -qx "$stack" || { echo "FAIL: cdk list missing stack $stack"; exit 1; }
done

echo "OK: cdk synth emits 3 EC2 instances and cdk list shows all four stacks"
