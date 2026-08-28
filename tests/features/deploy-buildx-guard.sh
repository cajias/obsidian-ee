#!/usr/bin/env bash
# Config-invariant regression test for deploy.yml's image build-and-promote job.
#
# Class of defect this catches: a workflow step whose ACTION DEFAULTS silently
# invalidate a later step's assumption. It has bitten M3 twice.
#
#   1. `docker/setup-buildx-action` defaults to `driver: docker-container` AND
#      `use: true`, so it makes a container-driver builder CURRENT. `docker
#      build` is an alias for `docker buildx build`, so the build then runs on
#      that driver; with no `--load` the image never lands in the local image
#      store, buildx STILL EXITS 0 (it only warns), and the following
#      `docker push` fails with "An image does not exist locally with the tag".
#      The job is currently written with no setup-buildx step at all — buildx is
#      preinstalled on the runner and `imagetools create` is a pure registry
#      operation needing no builder — so this check is conditional: if the step
#      is ever reintroduced it must pin `use: false` or `driver: docker`.
#
#   2. `docker buildx imagetools create` defaults to `--prefer-index=true`,
#      which wraps a single-source image in a NEW image index, so the promoted
#      v-tag would land on a DIFFERENT digest than the sha- tag it promotes and
#      break byte-identity between what staging validated and what prod runs
#      (D6, SC3). The retag must stay a carbon copy: `--prefer-index=false`.
#
# Comment lines are stripped first: the prose in deploy.yml explaining why the
# setup-buildx step is absent must not read as the step being present.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

WORKFLOW=.github/workflows/deploy.yml
status=0

CODE=$(grep -v '^[[:space:]]*#' "$WORKFLOW" || true)

# 1. setup-buildx-action, if used at all, must not become the current builder.
if printf '%s\n' "$CODE" | grep -Eq '^[[:space:]]*-?[[:space:]]*uses:[[:space:]]*docker/setup-buildx-action'; then
  if printf '%s\n' "$CODE" | grep -Eq '^[[:space:]]*(use:[[:space:]]*false|driver:[[:space:]]*docker)[[:space:]]*$'; then
    echo "ok: setup-buildx-action present and pinned (use: false / driver: docker)"
  else
    echo "FAIL: $WORKFLOW uses docker/setup-buildx-action without 'use: false' or 'driver: docker'."
    echo "      Its defaults make a docker-container builder current, so 'docker build'"
    echo "      produces no local image and the following 'docker push' fails."
    status=1
  fi
else
  echo "ok: no setup-buildx-action step (buildx is preinstalled; imagetools needs no builder)"
fi

# 2. The promotion retag must stay a carbon copy of the source manifest.
retag=$(printf '%s\n' "$CODE" | grep -n 'imagetools create' || true)
if [ -z "$retag" ]; then
  echo "FAIL: no 'imagetools create' retag found in $WORKFLOW — promotion must be a retag, never a rebuild."
  status=1
else
  while IFS= read -r line; do
    if printf '%s' "$line" | grep -q -- '--prefer-index=false'; then
      echo "ok: imagetools retag carries --prefer-index=false"
    else
      echo "FAIL: imagetools create without --prefer-index=false: $line"
      echo "      The default (true) rewraps the image in a new index, changing the digest."
      status=1
    fi
  done <<< "$retag"
fi

if [ "$status" -eq 0 ]; then
  echo "OK: deploy.yml image build and promotion config invariants hold"
fi
exit "$status"
