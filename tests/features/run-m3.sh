#!/usr/bin/env bash
# Step-runner for tests/features/m3.feature (dev-deploys-without-gate).
# Run from repo root. Exit code is the verdict.
#
# HUMAN-BLOCKED: this gate cannot run for real until the maintainer completes
# the M3 human-only halt steps — runbook items 8-10 in
# docs/aws-deployment-plan.md (Bootstrap runbook), also listed under M3's
# "Human-only halt steps" in 03-implementation-plan.md:
#   8.  gh variable set — repo-level AWS_REGION/ECR_REPOSITORY/ECR_PUSH_ROLE_ARN
#       and per-env AWS_DEPLOY_ROLE_ARN/RELAY_INSTANCE_ID/RELAY_HOSTNAME.
#   9.  mint the fine-grained RELEASE_PLEASE_TOKEN PAT; gh secret set it.
#   10. merge the repo changes; approve and run the first dev dispatch
#       (gh workflow run deploy.yml -f environment=dev).
# Until then <domain> has no resolved value, so this script reads it from
# RELAY_DOMAIN and exits BLOCKED (not a false pass, not an opaque failure)
# when it is unset.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

if [ -z "${RELAY_DOMAIN:-}" ]; then
  cat <<'EOF'
BLOCKED: RELAY_DOMAIN is not set — this gate is human-blocked.
Outstanding human halt steps (runbook items 8-10, docs/aws-deployment-plan.md):
  8.  gh variable set the repo + per-env deploy variables.
  9.  mint the RELEASE_PLEASE_TOKEN PAT and `gh secret set` it.
  10. merge the repo changes; approve and run the first dev dispatch
      (`gh workflow run deploy.yml -f environment=dev`).
Set RELAY_DOMAIN to the bootstrapped <domain> once dev is deployed, then re-run.
EOF
  exit 2
fi

# Gate command, verbatim from the M3 Exit criterion in 03-implementation-plan.md
# (only <domain> substituted for $RELAY_DOMAIN), retried within the ~2-minute
# budget the plan allows for first-deploy DNS propagation and certificate issuance.
#
# --max-time is load-bearing: curl only takes its WebSocket path for a ws://|wss://
# URL it drives itself, so on an https:// URL with hand-written Upgrade headers it
# treats the 101 as a FINAL response and reads the body to EOF — and a 101 carries
# neither Content-Length nor chunked framing, so a relay holding the connection open
# would hang this probe forever. -w still prints the code on the timeout path, hence
# the sentinel below substitutes only when NOTHING was captured.
deadline=$((SECONDS + 120))
code=""
while [ "$SECONDS" -lt "$deadline" ]; do
  code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 --http1.1 \
    -H 'Connection: Upgrade' -H 'Upgrade: websocket' \
    -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' \
    "https://relay-dev.collab.${RELAY_DOMAIN}/") || true
  [ -z "$code" ] && code=000
  [ "$code" = "101" ] && break
  sleep 5
done

[ "$code" = "101" ] || { echo "FAIL: expected HTTP 101 from the dev WS-upgrade probe, got '${code:-<no response>}'"; exit 1; }
echo "OK: dev WS-upgrade probe returned 101"
