# 03 — Implementation Plan

## Overview

This plan carries the Sequencing line of `docs/aws-deployment-plan.md` — STOPSIGNAL edit first (independent), infra plus runbook, then release-please plus the deploy workflow, then first dev deploy, staging gate check, and canary release — into four milestones arranged per [[structure-implementation-plans-as-sequential-foundation-then-parallel-branches]]: two independent foundations (M1, the Dockerfile edit; M2, the CDK infrastructure and bootstrap runbook) run as parallel branches, converge into M3 (the delivery workflows, which need both the image contract and the provisioned environments), and finish with the sequential cutover M4 (the verified rollout). These are milestones, not tasks: each is a self-contained unit a fresh session executes end to end. The goals of `00 — Project Intent` and `01 — Logic Design` are immutable while this plan executes; every milestone is gated by a named behavior scenario in `04 — BDD Test Plan`, cross-referenced by scenario name.

## Milestone DAG

All four milestones are pending.

```mermaid
graph LR
    M1["M1 — STOPSIGNAL edit"] --> M3["M3 — release-please + deploy workflow"]
    M2["M2 — CDK infra + bootstrap runbook"] --> M3
    M3 --> M4["M4 — Verified rollout"]
```

## M1 — STOPSIGNAL edit

**Depends on:** none.

**Exit criterion** — the behavior scenario `relay-container-stops-on-sigint` in `04 — BDD Test Plan` runs green. Gate command:

```bash
grep -q '^STOPSIGNAL SIGINT' docker/Dockerfile.relay && docker build -f docker/Dockerfile.relay -t relay-test .
```

**Context to load**

- `docs/design/aws-deploy/00-project-intent.md` — Tenets and Success criteria sections.
- `docs/design/aws-deploy/01-logic-design.md` — the Shutdown contract in the Runtime & permission model section (the relay handles SIGINT only).
- `docs/design/aws-deploy/02-structural-design.md` — the `docker/Dockerfile.relay` row of the Module boundaries table.
- `docs/design/aws-deploy/03-implementation-plan.md` (this document) and the `relay-container-stops-on-sigint` scenario in `04 — BDD Test Plan`.
- `docs/aws-deployment-plan.md` — Code-level constraints encoded, and item 3 of Files to create/edit.
- `docker/Dockerfile.relay` — the file to edit.

**Deliverable** — `docker/Dockerfile.relay` declares `STOPSIGNAL SIGINT` immediately after `USER appuser`, with a comment noting the relay handles only SIGINT while docker stop defaults to SIGTERM; the image still builds.

**Human-only halt steps** — none. This milestone is fully executable by the agent.

## M2 — CDK infra + bootstrap runbook

**Depends on:** none.

**Exit criterion** — the behavior scenario `cdk-synth-emits-four-stacks` in `04 — BDD Test Plan` runs green. Gate commands (the first must print `3`; the second must list `RelayShared`, `Relay-dev`, `Relay-staging`, `Relay-prod` — the stack names the deployment plan's Bootstrap runbook deploys):

```bash
cd infra && npm ci && npx cdk synth 2>/dev/null | grep -c 'AWS::EC2::Instance'
npx cdk list
```

**Context to load**

- `docs/design/aws-deploy/00-project-intent.md` — Tenets (especially T3, T5, T6) and Success criteria.
- `docs/design/aws-deploy/01-logic-design.md` — Architecture (C4), Key design decisions, and Runtime & permission model sections.
- `docs/design/aws-deploy/02-structural-design.md` — the `infra/` portion of the canonical tree and its Module boundaries rows.
- `docs/design/aws-deploy/03-implementation-plan.md` (this document) and the `cdk-synth-emits-four-stacks` scenario in `04 — BDD Test Plan`.
- `docs/aws-deployment-plan.md` — Files to create/edit item 4, and the entire Bootstrap runbook section.
- `infra/package.json`, `infra/tsconfig.json`, `infra/cdk.json`, `infra/bin/app.ts`, `infra/lib/shared-stack.ts`, `infra/lib/relay-stack.ts`, `infra/assets/docker-compose.yml`, `infra/assets/Caddyfile.tpl` (the env-templated Caddy site block), `infra/assets/deploy.sh.tpl` — the files to create.

**Deliverable** — the complete `infra/` CDK TypeScript app: `RelayShared` (ECR, OIDC provider, hosted zone, ECR push role, three env-scoped deploy roles) plus the three `Relay-<env>` stacks (instance, security group, EIP, DNS record, user-data writing the compose unit, Caddy site block, and deploy script from the templated assets), synthesizing clean with explicit account and region.

**Human-only halt steps** — the agent stops and hands each of these to the maintainer (Bootstrap runbook items 1–7 of `docs/aws-deployment-plan.md`):

1. Runbook item 1 — prereqs: Node 20+, AWS CLI, `gh`; pick the region.
2. Runbook item 2 — `cd infra && npm install && npx cdk bootstrap aws://<ACCT>/<REGION>`.
3. Runbook item 3 — set the domain constant in `bin/app.ts`; `npx cdk deploy RelayShared`; record NS records, ECR URI, role ARNs.
4. Runbook item 4 — at the registrar, add the NS records delegating `collab.<domain>`; verify with `dig NS collab.<domain> +short`.
5. Runbook item 5 — per env: `aws ssm put-parameter --name /relay/<env>/auth-token --type SecureString --value "$(openssl rand -base64 32)"`; save the values, clients need them.
6. Runbook item 6 — `npx cdk deploy Relay-dev Relay-staging Relay-prod`; record instance IDs and hostnames.
7. Runbook item 7 — `gh api` create the dev/staging/prod GitHub environments; add the named required reviewer on staging only.

## M3 — release-please + deploy workflow

**Depends on:** M1, M2.

**Exit criterion** — the behavior scenario `dev-deploys-without-gate` in `04 — BDD Test Plan` runs green: after the maintainer approves and runs the first dev dispatch (a human-only halt step below), the deployment plan's WS-upgrade check against `relay-dev` returns HTTP `101`. Gate command:

```bash
curl -s -o /dev/null -w '%{http_code}' \
  -H 'Connection: Upgrade' -H 'Upgrade: websocket' \
  -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' \
  https://relay-dev.collab.<domain>/
```

Expected output: `101` (retry within the ~2-minute budget the deploy workflow allows for first-deploy DNS propagation and certificate issuance).

**Also unlocks:** `staging-waits-for-review`.

**Context to load**

- `docs/design/aws-deploy/00-project-intent.md` — Tenets (especially T2, T3, T4) and Success criteria.
- `docs/design/aws-deploy/01-logic-design.md` — Key design decisions (D4–D7), Interfaces & data contracts, and Runtime & permission model sections.
- `docs/design/aws-deploy/02-structural-design.md` — the release trio and workflow rows of the Module boundaries table.
- `docs/design/aws-deploy/03-implementation-plan.md` (this document) and the `dev-deploys-without-gate` and `staging-waits-for-review` scenarios in `04 — BDD Test Plan`.
- `docs/aws-deployment-plan.md` — Files to create/edit items 1–2, Architecture, and Image strategy.
- `version.txt`, `release-please-config.json`, `.release-please-manifest.json`, `.github/workflows/release.yml`, `.github/workflows/deploy.yml` — the files to create.

**Deliverable** — the versioned release process and the delivery lane: release-please with the `simple` strategy (version file, changelog, tag), `release.yml` on push to main authenticated with the `RELEASE_PLEASE_TOKEN` fine-grained PAT so published releases fire release-triggered workflows, and `deploy.yml` (meta, build with skip-if-exists and same-digest retag, deploy via env-scoped OIDC role and SSM run-command, 101 verify) — merged, with the first dev deploy green and a staging dispatch observed pausing at its required reviewer.

**Human-only halt steps** — the agent stops and hands each of these to the maintainer (Bootstrap runbook items 8–10 of `docs/aws-deployment-plan.md`):

1. Runbook item 9 — mint the fine-grained PAT (contents rw + pull-requests rw, this repo only); `gh secret set RELEASE_PLEASE_TOKEN`.
2. Runbook item 8 — `gh variable set` repo-level (`AWS_REGION`, `ECR_REPOSITORY`, `ECR_PUSH_ROLE_ARN`) and per-env (`AWS_DEPLOY_ROLE_ARN`, `RELAY_INSTANCE_ID`, `RELAY_HOSTNAME`).
3. Runbook item 10 — merge the repo changes; approve and run the first dev dispatch: `gh workflow run deploy.yml -f environment=dev`; then the gate command above verifies.

## M4 — Verified rollout

**Depends on:** M3.

**Exit criterion** — the behavior scenario `prod-deploys-on-release-cut` in `04 — BDD Test Plan` runs green. Gate commands, taken from the Verification section of `docs/aws-deployment-plan.md`:

```bash
# Canary: merge a fix: commit -> release PR (only version.txt/CHANGELOG, CI stays
# green under --locked) -> merge -> v0.1.1 published -> prod deploy fires.
# Same-digest check: v0.1.1 and the canary's sha- tag must name ONE digest.
aws ecr describe-images --repository-name obsidian-ee/collab-relay \
  --query 'imageDetails[?contains(imageTags, `v0.1.1`)].[imageDigest,imageTags]' --output text

# Rollback drill: re-deploys the old digest, no rebuild
gh workflow run deploy.yml -f environment=prod --ref v0.1.0

# Per env: WS-upgrade probe returns 101
for env in dev staging prod; do
  curl -s -o /dev/null -w "%{http_code} relay-$env\n" \
    -H 'Connection: Upgrade' -H 'Upgrade: websocket' \
    -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' \
    "https://relay-$env.collab.<domain>/"
done

# Admission proof: Identify with the env token -> ack; without a token -> rejection
websocat wss://relay-dev.collab.<domain>/
```

**Also unlocks:** `handshake-returns-101`, `unauthenticated-identify-rejected`, `rollback-redeploys-old-digest`.

**Context to load**

- Everything in M3's Context to load list, plus:
- `docs/design/aws-deploy/00-project-intent.md` — Success criteria SC1–SC7 (this milestone proves them).
- `docs/design/aws-deploy/01-logic-design.md` — Runtime data-flow and Interfaces & data contracts sections (the release lane and the Identify admission contract).
- `docs/design/aws-deploy/03-implementation-plan.md` (this document) and the `prod-deploys-on-release-cut`, `handshake-returns-101`, `unauthenticated-identify-rejected`, and `rollback-redeploys-old-digest` scenarios in `04 — BDD Test Plan`.
- `docs/aws-deployment-plan.md` — the Verification and Accepted tradeoffs sections in full.

**Deliverable** — the pipeline proven end to end: a canary `fix:` release reaches prod with `v0.1.1` and its commit tag on the same digest, the rollback drill restores `v0.1.0` from the existing digest with no rebuild, all three environments answer the WS-upgrade probe with 101, and the relay admits credentialed collaborators while refusing everyone else. The system is live and operated.

**Human-only halt steps** — the agent stops and hands each of these to the maintainer or release manager:

1. Approve the staging dispatch at its required-reviewer pause.
2. Merge the canary `fix:` commit, then merge the release PR release-please raises (the release cut that fires the prod deploy).
3. Run the rollback dispatch: `gh workflow run deploy.yml -f environment=prod --ref v0.1.0`.
4. Runbook item 11 — point the Obsidian plugin at `wss://relay-dev.collab.<domain>` with the dev token for the end-to-end collaborator check.

## Residual ledger

The Accepted tradeoffs section of `docs/aws-deployment-plan.md` enumerates exactly **6 residuals** — consequences the design deliberately accepts. Each is claimed by exactly ONE milestone: the milestone whose deliverable makes the residual real and whose gate observes the system behaving acceptably despite it. The burn-down and traceability table for these residuals lives in the Residual burn-down section of `04 — BDD Test Plan`.

| ID | Residual (from Accepted tradeoffs) | Claimed by |
|----|-----|-----|
| R1 | Brief downtime and offline-queue loss per deploy; no HA (inherent to in-memory design) | M4 |
| R2 | Idle WebSocket connections reaped (no keepalive yet) | M4 |
| R3 | Instance replacement loses caddy_data; Let's Encrypt re-issues (5-duplicate-certs/week limit) | M2 |
| R4 | workflow_dispatch runs from the chosen ref, so tags predating the deploy workflow cannot be dispatched | M3 |
| R5 | The registry push role trusts all repo refs (deploy roles remain env-scoped) | M2 |
| R6 | No HTTP/3 (UDP 443 closed) | M2 |

Claims per milestone: M1=0, M2=3, M3=1, M4=2 → **6/6 claimed**.

## Execution notes

- **Context is cleared between milestones.** Each milestone runs in a fresh session; that is why every Context to load list is self-contained — the executing agent reads those files first and needs nothing else from prior sessions.
- **Milestones are immutable during execution.** As with the tenets and goals of `00 — Project Intent`, changing a milestone's scope, exit criterion, or dependencies mid-build is a formal plan change with a written justification, never a silent edit.
- **The harness is the iterative-build-loop skill**: an outer loop over M1–M4 in DAG order (M1 and M2 in either order or in parallel sessions, then M3, then M4), and an inner loop per milestone that iterates until the milestone's behavior scenario in `04 — BDD Test Plan` runs green. Human-only halt steps are halt conditions: the agent stops, states exactly which step it needs, and resumes only after the maintainer confirms it done.
