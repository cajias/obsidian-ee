#!/usr/bin/env bash
# Design-set integrity guard: the join key and the residual ledger.
#
# Both are definition-of-done criteria for the aws-deploy build, and until now
# neither had an in-repo check — the join key was verified only by a script
# outside the repository, so a reworded scenario name would have shipped
# silently. These names are the join key between the implementation plan and
# the BDD plan: 03 cites them as milestone exit gates, 04 defines them as
# Gherkin scenarios, and the two are matched with `grep -F`. Renaming one is a
# deliberate act that must update both documents AND this list.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

PLAN=docs/design/aws-deploy/03-implementation-plan.md
BDD=docs/design/aws-deploy/04-bdd-test-plan.md

SCENARIOS=(
  relay-container-stops-on-sigint
  cdk-synth-emits-four-stacks
  dev-deploys-without-gate
  staging-waits-for-review
  prod-deploys-on-release-cut
  handshake-returns-101
  unauthenticated-identify-rejected
  rollback-redeploys-old-digest
)

status=0

for s in "${SCENARIOS[@]}"; do
  in_plan=no; in_bdd=no
  grep -qF -- "$s" "$PLAN" && in_plan=yes
  grep -qF -- "$s" "$BDD" && in_bdd=yes
  if [ "$in_plan" = yes ] && [ "$in_bdd" = yes ]; then
    echo "ok: $s cited in both 03 and 04"
  else
    echo "FAIL: $s missing (03=$in_plan, 04=$in_bdd) — scenario names are the byte-frozen join key between the two documents"
    status=1
  fi
done

# 04 defines each scenario exactly once, as a Gherkin `Scenario:` line.
for s in "${SCENARIOS[@]}"; do
  n=$(grep -cE "^[[:space:]]*Scenario: $s\$" "$BDD" || true)
  if [ "$n" != 1 ]; then
    echo "FAIL: 04 declares 'Scenario: $s' $n times, expected exactly 1"
    status=1
  fi
done

# Each committed .feature file must stay a byte-exact copy of its block in 04.
for f in tests/features/*.feature; do
  [ -e "$f" ] || continue
  if python3 - "$f" "$BDD" <<'PY'
import sys
feature, bdd = open(sys.argv[1]).read().strip(), open(sys.argv[2]).read()
sys.exit(0 if feature in bdd else 1)
PY
  then
    echo "ok: $(basename "$f") is byte-identical to its block in 04"
  else
    echo "FAIL: $(basename "$f") has drifted from 04 — feature files are verbatim copies"
    status=1
  fi
done

# The Accepted tradeoffs enumerate exactly six residuals; the ledger must not
# grow or shrink silently. Closing one is an edit to its Status cell, never a
# new or deleted row.
rows=$(grep -c '^| R[0-9]' "$BDD" || true)
if [ "$rows" = 6 ]; then
  echo "ok: residual ledger has exactly 6 rows"
else
  echo "FAIL: residual ledger has $rows rows, expected exactly 6"
  status=1
fi

if [ "$status" = 0 ]; then
  echo "OK: join key holds (8 scenarios, both documents) and the residual ledger is intact"
fi
exit "$status"
