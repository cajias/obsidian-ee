#!/usr/bin/env bash
# /opt/relay/deploy.sh — run as root via SSM SendCommand (AWS-RunShellScript).
# $1 is the full image ref, e.g. <acct>.dkr.ecr.<region>.amazonaws.com/obsidian-ee/collab-relay:sha-abc123def456
# Immutable registry tags make a tag ref equivalent to a digest pin.
set -euo pipefail

IMAGE="${1:?usage: deploy.sh <full-image-ref>}"
REGION="{{REGION}}"
TOKEN_PARAM="/relay/{{ENV}}/auth-token"

# Compose auto-loads .env from the project directory only. SSM RunShellScript's
# working directory is not /opt/relay, and without this cd the ${RELAY_IMAGE} /
# ${RELAY_AUTH_TOKEN} substitutions resolve empty; docker-compose.yml's
# `:?` guards then abort `compose up` rather than starting an open relay (SC2).
cd /opt/relay

# Registry login. The registry host is the image ref's first path segment, so
# this stays correct without baking the account id into the template.
aws ecr get-login-password --region "$REGION" \
  | docker login --username AWS --password-stdin "${IMAGE%%/*}"

# The token is fetched fresh at deploy time and never lands in an image, a
# workflow log, or a repository variable.
RELAY_AUTH_TOKEN=$(aws ssm get-parameter --region "$REGION" \
  --name "$TOKEN_PARAM" --with-decryption --query Parameter.Value --output text)

[ -n "${RELAY_AUTH_TOKEN}" ] || { echo "FATAL: empty auth token; refusing to start an unauthenticated relay" >&2; exit 1; }

umask 077
cat > /opt/relay/.env <<EOF
RELAY_IMAGE=${IMAGE}
RELAY_AUTH_TOKEN=${RELAY_AUTH_TOKEN}
EOF

# Length only — never echo the value.
echo "auth token loaded (${#RELAY_AUTH_TOKEN} chars)"

docker compose pull relay
docker compose up -d
docker image prune -af

echo "deployed ${IMAGE}"
