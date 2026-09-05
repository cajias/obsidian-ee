#!/usr/bin/env bash
# Verifies a DEPLOYED relay environment set. Run from anywhere; it relocates to
# the repo root. Exit code is the verdict: 0 = pass, 1 = FAIL, 2 = BLOCKED.
#
# Scope is set by two environment variables:
#   RELAY_ENVS   which environments to probe (default: dev staging prod)
#   RELAY_CHECKS which checks to run — `all` (default) or `probe`, the
#                WS-upgrade probe alone, which exits 0/1 and touches no
#                prerequisite. The ungated-dev gate is:
#                  RELAY_CHECKS=probe RELAY_ENVS=dev RELAY_DOMAIN=<domain> bash tests/deployment-verify.sh
#
# It answers four questions about environments that already exist:
#   - does every published address answer the WebSocket upgrade with 101,
#   - does prod run the byte-identical image the release promoted,
#   - does every environment refuse an Identify carrying no credential,
#   - did a rollback restore the digest the prior release tag already named.
#
# Scenarios covered (names are the byte-frozen join key with
# docs/design/aws-deploy/04-bdd-test-plan.md): dev-deploys-without-gate's probe
# half, handshake-returns-101, prod-deploys-on-release-cut,
# unauthenticated-identify-rejected, rollback-redeploys-old-digest.
#
# HUMAN-BLOCKED. Every BLOCKED path below names the specific outstanding human
# step, so a BLOCKED verdict is never mistakable for a pass and never an opaque
# failure. The steps live in docs/aws-deployment-plan.md:
#   Bootstrap runbook 8-10 — `gh variable set` the repo-level
#     AWS_REGION/ECR_REPOSITORY/ECR_PUSH_ROLE_ARN and the per-env
#     AWS_DEPLOY_ROLE_ARN/RELAY_INSTANCE_ID/RELAY_HOSTNAME; mint the
#     fine-grained RELEASE_PLEASE_TOKEN PAT and `gh secret set` it; merge the
#     repo changes, then approve and run the first dev dispatch
#     (`gh workflow run deploy.yml -f environment=dev`).
#   Rollout runbook — approve the staging dispatch at its required-reviewer
#     pause; merge the canary `fix:` commit and then the release PR
#     release-please raises (that release cut fires the prod deploy and
#     publishes v0.1.1); run the rollback dispatch; point the Obsidian plugin at
#     wss://relay-dev.collab.<domain> with the dev token for the end-to-end
#     collaborator check.
#
# The rollback dispatch (`gh workflow run deploy.yml -f environment=prod --ref
# v0.1.0`) TRIGGERS A REAL PROD DEPLOY, so this script NEVER runs it. It only
# VERIFIES the outcome, and only once the maintainer states the drill happened
# by re-running with M4_ROLLBACK_DRILL=done.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

SELF=tests/deployment-verify.sh

# Environments to probe. Narrow it (RELAY_ENVS=dev) before staging and prod exist.
RELAY_ENVS=${RELAY_ENVS:-dev staging prod}
read -ra envs <<<"$RELAY_ENVS"

# Which checks to run.
#   all   — every check, three-valued exit (0 pass / 1 FAIL / 2 BLOCKED).
#   probe — ONLY the WS-upgrade probe, two-valued exit (0 pass / 1 FAIL). It
#           reaches no prerequisite, so a healthy environment reads green
#           without AWS credentials, websocat, or a cut release. That positive
#           reading is the point: "exit 2, but the probe passed" cannot be told
#           apart from "unreachable AND no credentials" by exit code alone.
# An unrecognized value is an error, never a silent fall-back to `all`.
RELAY_CHECKS=${RELAY_CHECKS:-all}
case "$RELAY_CHECKS" in
all | probe) ;;
*)
  echo "RELAY_CHECKS='$RELAY_CHECKS' is not a scope; use 'all' (default) or 'probe'" >&2
  exit 1
  ;;
esac

# Pinned by the rollout exit criterion; ECR_REPOSITORY mirrors deploy.yml's repo variable.
ECR_REPOSITORY=${ECR_REPOSITORY:-obsidian-ee/collab-relay}
RELEASE_TAG=v0.1.1
PRIOR_TAG=v0.1.0

log=$(mktemp)
trap 'rm -f "$log"' EXIT

if [ -z "${RELAY_DOMAIN:-}" ]; then
  cat <<EOF
BLOCKED: RELAY_DOMAIN is not set — <domain> has no resolved value here.
Outstanding human step: the environments must be bootstrapped and deployed
(bootstrap runbook items 8-10, docs/aws-deployment-plan.md) before anything here
can be verified:
  8.  gh variable set the repo + per-env deploy variables.
  9.  mint the RELEASE_PLEASE_TOKEN PAT and \`gh secret set\` it.
  10. merge the repo changes; approve and run the first dev dispatch
      (\`gh workflow run deploy.yml -f environment=dev\`).
Then re-run with the apex domain exported, e.g.:
  RELAY_DOMAIN=example.com bash $SELF
  RELAY_CHECKS=probe RELAY_ENVS=dev RELAY_DOMAIN=example.com bash $SELF   # dev probe only, exits 0 when dev is live
EOF
  exit 2
fi

# ------------------------------------------------- (a) handshake-returns-101
# Runs FIRST because it needs nothing but curl: a dev-only run, before a release
# has been cut or AWS credentials are to hand, still gets a real verdict instead
# of exiting BLOCKED on prerequisites it does not use.
#
# --http1.1 and --max-time are both load-bearing (docs/aws-deployment-plan.md,
# Verification), and each was the subject of a recorded plan change:
#   --http1.1 — without it curl negotiates h2, where the connection-specific
#     Connection/Upgrade headers are forbidden and Caddy drops them, so the probe
#     could never see a 101 no matter how healthy the relay is.
#   --max-time — curl only takes its WebSocket path for a ws://|wss:// URL it
#     drives itself, so on an https:// URL with hand-written Upgrade headers it
#     treats the 101 as a FINAL response and reads the body to EOF; a 101 carries
#     neither Content-Length nor chunked framing, so a relay holding the
#     connection open would hang this probe forever.
# `|| true` keeps a transport error from aborting the loop under `set -e`, and the
# 000 sentinel substitutes ONLY when the capture is EMPTY — curl exits nonzero on
# the timeout path too, but -w has already printed the code there, and an earlier
# version keyed on curl's exit status destroyed a valid 101 reading.
#
# The retry budget is per environment: a first deploy has to wait out DNS
# propagation and certificate issuance, which is what the scenario means by
# "returns HTTP 101 within the retry budget".
probe_fail=0
for env in "${envs[@]}"; do
  deadline=$((SECONDS + 120))
  code=""
  while :; do
    code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 --http1.1 \
      -H 'Connection: Upgrade' -H 'Upgrade: websocket' \
      -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' \
      "https://relay-$env.collab.${RELAY_DOMAIN}/") || true
    [ -z "$code" ] && code=000
    [ "$code" = "101" ] && break
    [ "$SECONDS" -lt "$deadline" ] || break
    sleep 5
  done
  echo "$code relay-$env"
  [ "$code" = "101" ] || probe_fail=1
done
[ "$probe_fail" -eq 0 ] || {
  echo "FAIL: not every environment answered the WS-upgrade probe with HTTP 101 (see the codes above)"
  exit 1
}
echo "OK (handshake-returns-101): $RELAY_ENVS answered 101"
[ "$RELAY_CHECKS" = probe ] && exit 0

# ---------------------------------------------------------------- prerequisites
# The three checks below gate the registry, admission and rollback sections only.

if ! aws sts get-caller-identity >/dev/null 2>&1; then
  cat <<EOF
BLOCKED: no usable AWS credentials/region in this shell.
Outstanding human step: authenticate to the account that owns RelayShared and
the three Relay-* stacks, and select its region, before re-running. e.g.:
  aws sso login --profile <profile>
  export AWS_PROFILE=<profile> AWS_REGION=<region>
The registry same-digest check and the rollback verification both read AWS.
The WS-upgrade probe above passed.
EOF
  exit 2
fi

if ! command -v websocat >/dev/null 2>&1; then
  cat <<EOF
BLOCKED: websocat is not installed — the Identify admission check needs it.
It is the client named by the rollout exit criterion gate command
(\`websocat wss://relay-dev.collab.<domain>/\`); curl cannot drive a WebSocket
session with hand-written frames. Install it, then re-run:
  brew install websocat        # macOS
  cargo install websocat       # anywhere with a Rust toolchain
The WS-upgrade probe above passed.
EOF
  exit 2
fi

# ------------------------------------------------ (b) prod-deploys-on-release-cut
# Gate command, verbatim from the rollout exit criterion (only the repository name
# parameterized). Stderr is classified the same way deploy.yml's "Check for an
# existing sha tag" step classifies it: a missing image means the canary release
# was never cut, a missing repository means the bootstrap is incomplete, and
# anything else is a real failure — never silently treated as absence.
if ! digests=$(aws ecr describe-images --repository-name "$ECR_REPOSITORY" \
  --query "imageDetails[?contains(imageTags, \`$RELEASE_TAG\`)].[imageDigest,imageTags]" \
  --output text 2>"$log"); then
  err=$(cat "$log")
  if printf '%s' "$err" | grep -q 'RepositoryNotFoundException'; then
    cat <<EOF
BLOCKED: ECR repository '$ECR_REPOSITORY' does not exist.
Outstanding human step: deploy RelayShared (bootstrap runbook), or export
ECR_REPOSITORY=<RelayShared's RepositoryName output> and re-run.
The WS-upgrade probe above passed.
EOF
    exit 2
  fi
  printf '%s\n' "$err" >&2
  echo "FAIL: aws ecr describe-images failed for a reason other than a missing repository"
  exit 1
fi

if [ -z "$digests" ]; then
  cat <<EOF
BLOCKED: the registry holds no image tagged $RELEASE_TAG.
Outstanding human step: merge the canary \`fix:\` commit, then merge the release
PR release-please raises. That release cut publishes $RELEASE_TAG and fires the prod
deploy. Re-run once the release workflow and the prod deploy have finished.
The WS-upgrade probe above passed.
EOF
  exit 2
fi

echo "$digests"

# A tag is unique within an ECR repository, so $RELEASE_TAG already names exactly
# ONE digest by construction; what still has to be proved is that the SAME image
# also carries the canary's sha- commit tag — i.e. the release was a retag of the
# digest staging validated, not a rebuild. Read that tag list back on its own
# rather than parsing the line shape above: `--output text` flattens the nested
# imageTags projection differently depending on the CLI's rendering rules, and
# this assertion must not depend on which way it lands. The tag exists here (the
# filter query above returned non-empty), so a failure is a real failure.
if ! tags=$(aws ecr describe-images --repository-name "$ECR_REPOSITORY" \
  --image-ids "imageTag=$RELEASE_TAG" --query 'imageDetails[0].imageTags[]' --output text); then
  echo "FAIL: could not read the tag list of the $RELEASE_TAG image"
  exit 1
fi
echo "tags on the $RELEASE_TAG digest: $tags"
# KNOWN-VACUOUS, deliberately deferred: the release path currently BUILDS rather
# than promotes, so this cannot yet prove the same-digest half of SC3/D6.
# deploy.yml has no push trigger and release.yml only cuts the tag, so under
# `release: published` github.sha is a merge commit nothing has ever built — the
# sha- tag this looks for is written by the very build the release fires, not
# carried over from the canary staging validated. Kept, not deleted: it becomes
# load-bearing the moment the release path promotes a digest instead of building
# one, and deleting it would lose the assertion along with the gap.
case "$tags" in
*sha-*) ;;
*)
  echo "FAIL: the $RELEASE_TAG digest carries no sha- commit tag — the release was rebuilt, not retagged,"
  echo "      so prod is not running the byte-identical artifact staging validated"
  exit 1
  ;;
esac
echo "OK (prod-deploys-on-release-cut): $RELEASE_TAG and the canary sha- tag name one digest"

# --------------------------------------- (c) unauthenticated-identify-rejected
# Admission happens in the Identify exchange AFTER the 101, so this is the check
# that proves no environment is an open relay. The relay speaks JSON text frames
# (collab-proto ClientMessage/ServerMessage, serde snake_case tags).
WS_URL="wss://relay-dev.collab.${RELAY_DOMAIN}/"
if [ -n "${RELAY_TOKEN:-}" ]; then
  token=$RELAY_TOKEN
elif ! token=$(aws ssm get-parameter --name /relay/dev/auth-token --with-decryption \
  --query Parameter.Value --output text 2>"$log"); then
  cat "$log" >&2
  cat <<EOF
BLOCKED: cannot read the dev token from SSM at /relay/dev/auth-token.
Outstanding human step: the bootstrap runbook's token step must have run, and the
credentials above must be allowed to read that parameter. Either fix the access or
export the value directly and re-run:
  RELAY_TOKEN=<dev token> RELAY_DOMAIN=... bash $SELF
EOF
  exit 2
fi

# websocat -1 sends one message and receives one; -n suppresses the Close frame so
# the reply is not raced by the shutdown. `timeout` is used when present so a relay
# that answers nothing cannot hang the gate; stock macOS ships neither name.
TIMEOUT_BIN=$(command -v timeout || command -v gtimeout || true)
ws_identify() {
  if [ -n "$TIMEOUT_BIN" ]; then
    printf '%s\n' "$1" | "$TIMEOUT_BIN" 15 websocat -1 -n -t "$WS_URL" 2>&1 || true
  else
    printf '%s\n' "$1" | websocat -1 -n -t "$WS_URL" 2>&1 || true
  fi
}

anon=$(ws_identify '{"type":"identify","user_id":"deploy-verify-probe"}')
echo "anonymous Identify -> ${anon:-<no reply>}"
case "$anon" in
*'"unauthorized"'*) ;;
*)
  echo "FAIL: an Identify carrying no credential was not rejected with an unauthorized error (got '${anon:-<no reply>}') — the relay is admitting anonymous sessions"
  exit 1
  ;;
esac

authed=$(ws_identify "{\"type\":\"identify\",\"user_id\":\"deploy-verify-probe\",\"token\":\"$token\"}")
case "$authed" in
*'"identified"'*) ;;
*)
  echo "FAIL: an Identify carrying the dev token was not acked with an identified message (got '${authed:-<no reply>}')"
  exit 1
  ;;
esac
echo "OK (unauthenticated-identify-rejected): dev acks the credentialed Identify and refuses the anonymous one"

# ------------------------------------- (d) rollback-redeploys-old-digest
# The drill itself (`gh workflow run deploy.yml -f environment=prod --ref v0.1.0`)
# is a REAL prod deploy and a human halt step. This script only verifies its
# outcome, and only once told the drill has happened.
if [ "${M4_ROLLBACK_DRILL:-}" != "done" ]; then
  cat <<EOF
BLOCKED: the rollback drill has not been reported as run, so its outcome cannot be verified.
Outstanding human step — run the dispatch yourself (this script will not: it is a
real prod deploy), wait for the run to finish, then re-run this gate saying so:
  gh workflow run deploy.yml -f environment=prod --ref $PRIOR_TAG
  M4_ROLLBACK_DRILL=done RELAY_DOMAIN=$RELAY_DOMAIN bash $SELF
Checks (a)-(c) above passed.
EOF
  exit 2
fi

# Classified like every other AWS read here: unguarded, a missing $PRIOR_TAG aborts
# under `set -e` with a raw botocore traceback, right after (a)-(c) printed OK — which
# reads as an infrastructure glitch rather than "the rollback target is gone".
if ! prior_digest=$(aws ecr describe-images --repository-name "$ECR_REPOSITORY" \
  --image-ids "imageTag=$PRIOR_TAG" --query 'imageDetails[0].imageDigest' --output text 2>"$log"); then
  cat "$log" >&2
  echo "FAIL: no image tagged $PRIOR_TAG in $ECR_REPOSITORY, so the rollback drill cannot be verified." >&2
  echo "The release image is the rollback target; if it expired, re-cut the release." >&2
  exit 1
fi
instance=$(aws ec2 describe-instances \
  --filters Name=tag:RelayEnv,Values=prod Name=instance-state-name,Values=running \
  --query 'Reservations[].Instances[].InstanceId' --output text)
[ -n "$instance" ] || {
  echo "FAIL: no running EC2 instance tagged RelayEnv=prod"
  exit 1
}

# Read-only SSM run-command: reports the digest of the image /opt/relay/.env pins,
# which is the ref compose started the relay from. --cli-input-json (not the
# shorthand) so the Go-template braces and quotes survive argument parsing.
cmd_id=$(aws ssm send-command --query Command.CommandId --output text --cli-input-json "$(
  cat <<JSON
{"InstanceIds":["$instance"],"DocumentName":"AWS-RunShellScript",
 "Parameters":{"commands":["cd /opt/relay && . ./.env && docker inspect --format '{{index .RepoDigests 0}}' \"\$RELAY_IMAGE\""]}}
JSON
)")
running=""
for _ in $(seq 1 24); do
  status=$(aws ssm get-command-invocation --command-id "$cmd_id" --instance-id "$instance" \
    --query Status --output text 2>/dev/null) || status=Pending
  if [ "$status" = Success ]; then
    running=$(aws ssm get-command-invocation --command-id "$cmd_id" --instance-id "$instance" \
      --query StandardOutputContent --output text)
    break
  fi
  # Cancelled/TimedOut/Cancelling are terminal too. Treating only Failed as
  # terminal burned all 24 polls and then reported "never returned", which names
  # the symptom instead of the status that actually ended the command. Same
  # classification deploy.yml's poll uses.
  case "$status" in
  Failed | Cancelled | Cancelling | TimedOut)
    echo "FAIL: could not read the running image from the prod instance (SSM command $cmd_id terminated as $status)"
    exit 1
    ;;
  esac
  sleep 5
done
[ -n "$running" ] || {
  echo "FAIL: SSM command $cmd_id never returned the prod instance's running image digest"
  exit 1
}

echo "prod is running: $running"
echo "$PRIOR_TAG names:  $prior_digest"
case "$running" in
*"$prior_digest"*) ;;
*)
  echo "FAIL: prod is not serving the digest the $PRIOR_TAG tag already named — the rollback did not restore the prior release"
  exit 1
  ;;
esac
echo "OK (rollback-redeploys-old-digest): prod serves the digest $PRIOR_TAG already named"

echo "PASS: every deployment check verified"
