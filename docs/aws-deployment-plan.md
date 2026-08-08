# AWS Deployment Pipeline for obsidian-ee (collab-relay)

## Context

The repo (github.com/cajias/obsidian-ee, public) is an E2E-encrypted collaborative editing system for Obsidian. Exactly one component needs hosting: **`collab-relay`** — a zero-knowledge WebSocket relay (raw WS on :8080, no HTTP surface, no TLS in-process, in-memory routing state → **hard single-instance-per-env constraint**). A production-ready Dockerfile exists (`docker/Dockerfile.relay`), but there is no release-please, no tags, no deploy workflow, and no IaC (`infra/` is an empty stub). Goal: a safe, secure dev/staging/prod pipeline on AWS with no long-lived credentials.

## Decisions (confirmed with user)

| Decision | Choice |
|---|---|
| Compute | One EC2 t4g.micro (arm64, AL2023) per env + Docker; Caddy container terminates TLS via Let's Encrypt (no ALB/NLB/ACM) |
| IaC | AWS CDK TypeScript in `infra/` |
| Promotion | **Dev**: on-demand `workflow_dispatch`, no gate. **Staging**: `workflow_dispatch` paused by GitHub Environment required-reviewer. **Prod**: fires on `release: published` from release-please (the release cut is the gate; adding a prod reviewer later is one click) |
| Domain | Domain registered outside Route53 → delegate subdomain `collab.<domain>` to a Route53 hosted zone (one-time NS records at registrar); hosts `relay-<env>.collab.<domain>` |
| CI→AWS auth | GitHub OIDC, no stored keys. Per-env deploy roles trust `repo:cajias/obsidian-ee:environment:<env>`; separate ECR push role |
| Instance access | No SSH/port 22. Deploys via SSM SendCommand; break-glass via SSM Session Manager |

## Architecture

```
release.yml  — release-please on push to main (PAT so the release event fires)
deploy.yml   — workflow_dispatch (dev/staging; prod choice = rollback hatch) + release:published (prod)
   build:  ubuntu-24.04-arm (free, public repo) → native arm64 build → ECR (immutable tags, scan-on-push)
   deploy: OIDC role → SSM SendCommand → /opt/relay/deploy.sh → verify WS handshake returns 101

AWS: SharedStack (ECR, OIDC provider, hosted zone, 4 roles) + Relay-{dev,staging,prod} stacks
     (EC2 + EIP + SG 80/443 + instance role + A record + user-data writing compose/Caddyfile/deploy.sh)
On instance: caddy:2 (auto Let's Encrypt) → relay:8080 on the compose network (no host ports on relay)
```

Image strategy: dev/staging deploy `sha-<sha12>` tags; on release, the build job **retags the existing digest** (`docker buildx imagetools create`) to `vX.Y.Z` when that sha was already built — same-digest promotion preserved; builds only if missing.

## Code-level constraints encoded

- **Singleton**: one instance per env; deploy = stop-then-start, drops in-memory offline queue (accepted; clients reconnect).
- **Signals**: relay handles only SIGINT (`crates/collab-relay/src/main.rs:59`) → add `STOPSIGNAL SIGINT` to `docker/Dockerfile.relay` (image already ships `netcat-openbsd` for the TCP healthcheck — verified).
- **Auth**: `RELAY_AUTH_TOKEN` unset = open relay → per-env SecureString `/relay/<env>/auth-token` in SSM Parameter Store, injected by deploy.sh; instance role can read only its own. `RELAY_SUBSCRIBE_AUTHZ` stays off (deadlocks MLS bootstrap).
- **Keepalive**: no WS ping/pong yet (`routing.rs:171`) — idle connections reaped, reconnect expected.
- **101 verify is valid**: relay auth happens after the WS handshake (in `Identify`), so an unauthenticated upgrade still completes.

## Files to create/edit

### 1. release-please (root)
- `release-please-config.json`: `{"release-type": "simple", "include-component-in-tag": false, "packages": {".": {}}}` — **`simple`, not `rust`**: the workspace uses `[workspace.package] version` inheritance (rust strategy's weak spot), crates are never published, and touching Cargo.toml without Cargo.lock turns release PRs red under `--locked` CI. Version lives in `version.txt` + CHANGELOG + tag `vX.Y.Z`.
- `.release-please-manifest.json`: `{".": "0.1.0"}`
- `.github/workflows/release.yml`: `googleapis/release-please-action@v4` on push to main, with `token: ${{ secrets.RELEASE_PLEASE_TOKEN }}` (fine-grained PAT). **Required**: `GITHUB_TOKEN`-created releases never trigger `release:` workflows — without the PAT, prod deploys never fire.

### 2. `.github/workflows/deploy.yml`
- Triggers: `workflow_dispatch` (choice input dev/staging/prod, default dev; prod choice documented as rollback/redeploy hatch, e.g. `--ref v0.1.0`) + `release: {types: [published]}` → prod.
- `permissions: {id-token: write, contents: read}`; `concurrency` keyed per target env, no cancel-in-progress.
- `meta` job resolves env + tags: `sha-<sha12>` always; release → `deploy_tag = tag_name`.
- `build` job on `ubuntu-24.04-arm`: `configure-aws-credentials@v4` (ECR push role), `amazon-ecr-login@v2`; skip build if `aws ecr describe-images` finds the sha tag (immutable tags — never re-push); on release, retag digest via `docker buildx imagetools create`.
- `deploy` job: `environment: {name: <env>, url: https://<host>}` — staging's required reviewer pauses here; env-scoped `vars.AWS_DEPLOY_ROLE_ARN` / `RELAY_INSTANCE_ID` / `RELAY_HOSTNAME`; runs `aws ssm send-command` → `/opt/relay/deploy.sh <image>`, polls `get-command-invocation` to Success; then retry-loop curl WS-upgrade check expecting HTTP 101 (~2 min budget for first-deploy DNS/ACME).
- Repo-level vars: `AWS_REGION`, `ECR_REPOSITORY`, `ECR_PUSH_ROLE_ARN`.

### 3. `docker/Dockerfile.relay` (edit)
Add after `USER appuser`: `STOPSIGNAL SIGINT` (with a comment: relay only handles SIGINT; docker stop defaults to SIGTERM).

### 4. `infra/` CDK app (new)
Layout: `package.json`, `tsconfig.json`, `cdk.json`, `bin/app.ts`, `lib/shared-stack.ts`, `lib/relay-stack.ts`, `assets/{docker-compose.yml, Caddyfile.tpl, deploy.sh.tpl}`. `bin/app.ts` must set explicit `env: {account, region}` (required by `Vpc.fromLookup`); domain constant defined once.

**SharedStack**: ECR repo `obsidian-ee/collab-relay` (IMMUTABLE, scanOnPush, keep 25); `OpenIdConnectProvider` for token.actions.githubusercontent.com; `PublicHostedZone collab.<domain>` (+ NS CfnOutput); ECR push role (sub `repo:cajias/obsidian-ee:*` — deliberately ref-wide since dev builds from arbitrary refs and prod from tags; deploy roles stay env-scoped); 3 deploy roles trusting `…:environment:<env>` with `ssm:SendCommand` on `AWS-RunShellScript` + instances conditioned on `ssm:resourceTag/RelayEnv = <env>` (tag-scoped → no cross-stack instance-ID cycle) + `ssm:GetCommandInvocation`.

**RelayStack** (×3): default VPC lookup (no NAT); SG inbound 443+80/tcp only; instance role = `AmazonSSMManagedInstanceCore` + ECR pull + `ssm:GetParameter` on own `/relay/<env>/auth-token`; `t4g.micro` AL2023 ARM (SSM agent preinstalled), tag `RelayEnv=<env>`; EIP + association; A record `relay-<env>.collab.<domain>`; user-data installs docker + pinned compose plugin (aarch64 binary — not in AL2023 repos) and writes `/opt/relay/{docker-compose.yml,Caddyfile,deploy.sh}` from synth-time-templated assets. Stack does not start the relay — first deploy run does.

**On-instance compose**: caddy:2 (ports 80/443, `caddy_data`/`caddy_config` volumes) + relay (`image: ${RELAY_IMAGE}`, `restart: unless-stopped`, `stop_grace_period: 15s`, env `RELAY_AUTH_TOKEN=${RELAY_AUTH_TOKEN}`, `RUST_LOG=info`, nc-based healthcheck). Relay has no host ports.

**Caddyfile** (entire file — Caddy proxies WS upgrades natively and auto-provisions Let's Encrypt):
```
relay-{{ENV}}.collab.<domain> {
	reverse_proxy relay:8080
}
```

**`/opt/relay/deploy.sh`** (root via SSM; `$1` = full image ref; immutable tags ≈ digest pinning): ECR login → fetch token from SSM → `umask 077`; write `.env` (`RELAY_IMAGE`, `RELAY_AUTH_TOKEN`) → `docker compose pull relay && docker compose up -d` → `docker image prune -af`.

## Bootstrap runbook (one-time, local, already logged in)

1. Prereqs: Node 20+, AWS CLI, `gh`; pick region.
2. `cd infra && npm install && npx cdk bootstrap aws://<ACCT>/<REGION>`
3. Set domain in `bin/app.ts`; `npx cdk deploy RelayShared`; record NS records, ECR URI, role ARNs.
4. Registrar: add NS records for `collab.<domain>`; verify `dig NS collab.<domain> +short`.
5. Per env: `aws ssm put-parameter --name /relay/<env>/auth-token --type SecureString --value "$(openssl rand -base64 32)"` (save values — clients need them).
6. `npx cdk deploy Relay-dev Relay-staging Relay-prod`; record instance IDs + hostnames.
7. `gh api` create environments dev/staging/prod; add self as required reviewer on **staging** only.
8. `gh variable set` repo-level (`AWS_REGION`, `ECR_REPOSITORY`, `ECR_PUSH_ROLE_ARN`) + per-env (`AWS_DEPLOY_ROLE_ARN`, `RELAY_INSTANCE_ID`, `RELAY_HOSTNAME`).
9. Fine-grained PAT (contents rw + pull-requests rw, this repo only) → `gh secret set RELEASE_PLEASE_TOKEN`.
10. Merge the repo changes; `gh workflow run deploy.yml -f environment=dev`; verify.
11. Point Obsidian plugin at `wss://relay-dev.collab.<domain>` + dev token.

Sequencing: STOPSIGNAL edit first (independent) → infra + runbook 1–8 → release-please + deploy.yml PR + PAT → first dev deploy → staging gate check → canary release.

## Verification

- Per env: curl WS-upgrade to `https://relay-<env>.collab.<domain>/` → expect **101**.
- `websocat wss://relay-dev…` + `Identify` with token → ack; without token → rejection (proves not an open relay).
- Canary: merge a `fix:` commit → release PR (only version.txt/CHANGELOG → `--locked` CI stays green) → merge → `v0.1.1` published → prod deploy fires; ECR shows `v0.1.1` and `sha-…` on the same digest.
- Gates: staging dispatch pauses "Waiting for review"; dev doesn't.
- Rollback drill: `gh workflow run deploy.yml -f environment=prod --ref v0.1.0` (re-deploys old digest, no rebuild).
- Break-glass: `aws ssm start-session --target <instance-id>`.

## Accepted tradeoffs

Brief downtime + offline-queue loss per deploy; no HA (inherent to in-memory design). Idle WS reaped (no keepalive). Instance replacement loses `caddy_data` → LE re-issues (mind the 5-dup-certs/week limit). `workflow_dispatch` runs from the chosen ref, so tags predating deploy.yml can't be dispatched. ECR push role trusts all repo refs (deploy roles remain env-scoped). No HTTP/3 (UDP 443 closed).

## Cost

~$10.40/env/month (t4g.micro $6.13 + EIP $3.65 + EBS $0.64) → **~$32/month total** incl. one Route53 zone ($0.50) + ECR (~$0.30).
