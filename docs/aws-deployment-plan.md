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
| CI→AWS auth | GitHub OIDC, no stored keys. Per-env deploy roles trust `repo:cajias/obsidian-ee:environment:<env>`, and that trust is real only together with each environment's deployment-branch/tag policy (Bootstrap runbook item 7) — without it, a workflow run from any branch or tag can claim `environment:<env>`'s OIDC subject and assume the role; separate ECR push role |
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
Layout: `package.json`, `tsconfig.json`, `cdk.json`, `bin/app.ts`, `lib/shared-stack.ts`, `lib/relay-stack.ts`, `assets/{docker-compose.yml, Caddyfile.tpl, deploy.sh.tpl}`. `bin/app.ts` must set explicit `env: {account, region}` plus the `VPC_ID` / `SUBNET_ID` / `AVAILABILITY_ZONE` placeholder constants the relay stacks take as props — the VPC is referenced by attributes (`Vpc.fromVpcAttributes`), never `Vpc.fromLookup`, so synth stays credential-free in CI; domain constant defined once.

**SharedStack**: ECR repo `obsidian-ee/collab-relay` (IMMUTABLE, scanOnPush, keep 25); the native L1 `AWS::IAM::OIDCProvider` (`iam.CfnOIDCProvider`) for token.actions.githubusercontent.com — the L2 `OpenIdConnectProvider` construct is deliberately avoided, being a custom resource whose Lambda holds `iam:*OpenIDConnectProvider` on `"*"`; `PublicHostedZone collab.<domain>` (+ NS CfnOutput); ECR push role (sub `repo:cajias/obsidian-ee:*` — deliberately ref-wide since dev builds from arbitrary refs and prod from tags; deploy roles stay env-scoped); 3 deploy roles trusting `…:environment:<env>` with `ssm:SendCommand` on `AWS-RunShellScript` + instances conditioned on `ssm:resourceTag/RelayEnv = <env>` (tag-scoped → no cross-stack instance-ID cycle) + `ssm:GetCommandInvocation` (the SSM action reference defines no resource type for `GetCommandInvocation`, so it cannot be scoped below account-wide by IAM condition — accepted residual, not a closable gap; deploy scripts must never print secret values to command output, which `infra/assets/deploy.sh.tpl` already honors by printing only the auth token's length, never its value).

**RelayStack** (×3): the existing default VPC referenced by attributes (no NAT, no lookup); SG inbound 443+80/tcp only; instance role = **no managed policies at all** (asserted empty by `infra/test/relay-stack.test.ts`) but three inline statements — the SSM agent's own channel and association actions (`ssm:UpdateInstanceInformation`, `ssm:ListInstanceAssociations`, `ssm:ListAssociations`, `ssm:DescribeAssociation`, `ssm:UpdateInstanceAssociationStatus`, `ssm:GetDocument`, `ssm:DescribeDocument`, `ssm:GetManifest`, `ssm:PutInventory`, `ssm:PutComplianceItems`, `ssm:GetDeployablePatchSnapshotForInstance`, the four `ssmmessages:*` and six `ec2messages:*`) on `"*"`, ECR pull, and `ssm:GetParameter` on own `/relay/<env>/auth-token`. `AmazonSSMManagedInstanceCore` is deliberately **not** attached: it carries `ssm:GetParameter*` on `"*"`, which would union over the scoped grant and let any instance read every environment's token; none of the inline agent actions above is a `GetParameter*`, so own-token-only holds for the whole role. `t4g.micro` AL2023 ARM (SSM agent preinstalled), IMDSv2 required, encrypted 8 GiB gp3 root volume (the deploy script writes the decrypted token to `/opt/relay/.env` on it), tag `RelayEnv=<env>`; EIP + association; A record `relay-<env>.collab.<domain>`; user-data installs docker + pinned compose plugin (aarch64 binary — not in AL2023 repos, sha256-verified before use since a release asset is mutable) and writes `/opt/relay/{docker-compose.yml,Caddyfile,deploy.sh}` from synth-time-templated assets. Stack does not start the relay — first deploy run does.

**On-instance compose**: caddy:2 (ports 80/443, `caddy_data`/`caddy_config` volumes) + relay (`image: ${RELAY_IMAGE}`, `restart: unless-stopped`, `stop_grace_period: 15s`, env `RELAY_AUTH_TOKEN=${RELAY_AUTH_TOKEN}`, `RUST_LOG=info`, nc-based healthcheck). Relay has no host ports.

**Caddyfile** (entire file — Caddy proxies WS upgrades natively and auto-provisions Let's Encrypt):
```
relay-{{ENV}}.collab.<domain> {
	reverse_proxy relay:8080
}
```

**`/opt/relay/deploy.sh`** (root via SSM; `$1` = full image ref; immutable tags ≈ digest pinning): ECR login → fetch token from SSM → `umask 077`; write `.env` (`RELAY_IMAGE`, `RELAY_AUTH_TOKEN`) → `docker compose pull relay && docker compose up -d` → `docker image prune -af`. This path is where the minimal (managed-policy-free) instance-role SSM grant first proves out — at the M4 live deploy, not at synth — so if the agent AccessDenied-loops on associations or documents, the fix is to add the missing action to the inline statement in `infra/lib/relay-stack.ts`, never to re-attach `AmazonSSMManagedInstanceCore`.

## Bootstrap runbook (one-time, local, already logged in)

1. Prereqs: Node 20+, AWS CLI, `gh`; pick region.
2. `cd infra && npm install && npx cdk bootstrap aws://<ACCT>/<REGION>`
3. Set the placeholder constants in `bin/app.ts`: `DOMAIN` (the apex domain whose `collab.` subdomain is delegated); `ACCOUNT` and `REGION` (the account and region hosting all three environments); `VPC_ID` and `SUBNET_ID`, found via `aws ec2 describe-vpcs --filters Name=isDefault,Values=true` and `aws ec2 describe-subnets --filters Name=vpc-id,Values=<vpc-id> Name=map-public-ip-on-launch,Values=true --query 'Subnets[].{Id:SubnetId,AZ:AvailabilityZone,PublicIp:MapPublicIpOnLaunch}'` (`AVAILABILITY_ZONE` derives from `REGION` and needs no separate lookup). The `SUBNET_ID` you pick **must** auto-assign public IPs (`MapPublicIpOnLaunch: true`) — the design has no NAT gateway, and the EIP is associated only *after* the instance is running, so it cannot rescue a user-data script that already failed with no route to the internet. The `SUBNET_ID` you pick must also live in availability zone `${REGION}a`, or `AVAILABILITY_ZONE` must be set to that subnet's actual AZ — the stack declares the AZ and the subnet together, and a mismatch is only caught at deploy time. Then `npx cdk deploy RelayShared`; record NS records, ECR URI, role ARNs.
4. Registrar: add NS records for `collab.<domain>`; verify `dig NS collab.<domain> +short`.
5. Per env: `aws ssm put-parameter --name /relay/<env>/auth-token --type SecureString --value "$(openssl rand -base64 32)"` (save values — clients need them).
6. `npx cdk deploy Relay-dev Relay-staging Relay-prod`; record instance IDs + hostnames.
   > Note (R3, accepted): `caddy_data` is a plain Docker named volume on the instance's root EBS volume — replacing an instance (a stack replacement, or a fresh `cdk deploy` after destroy) discards it, and Caddy re-requests a Let's Encrypt certificate on next boot. This also triggers on any `cdk deploy` of a Relay-* stack after AWS publishes a newer AL2023 arm64 AMI: `relay-stack.ts` resolves the image via `MachineImage.latestAmazonLinux2023`, an SSM-latest `ImageId` that re-resolves on every deploy, and `ImageId` is replacement-requiring on `AWS::EC2::Instance` — after such a deploy, re-run the environment's deploy workflow to bring the relay back up. Stay under 5 duplicate-certificate issuances per registered domain per week if replacing the same environment's instance repeatedly.
7. `gh api` create environments dev/staging/prod, each one by the `PUT` that sets its deployment branch/tag policy — without that policy, the per-env deploy-role OIDC trust (see CI→AWS auth above) binds to any branch or tag, not the intended lane. **staging**'s required reviewer (self) rides in that same `PUT` body; run the policy `PUT`s first and never add the reviewer as a separate later call.
   > `PUT /repos/{owner}/{repo}/environments/{env}` is create-**or-replace**: a protection rule absent from the request body is CLEARED, so every future edit of an environment must resend the full body — reviewers included — not just the field being changed.

   Note the `-F` (not `-f`) on the two boolean fields: `-f` sends every value as a JSON string, and `"false"` fails the environments schema — `-F` converts `true`/`false` to real booleans. String fields (`name`, `type`) stay on `-f`:
   - prod, tags matching `v*` only (release cuts):
     ```
     gh api --method PUT repos/cajias/obsidian-ee/environments/prod \
       -F 'deployment_branch_policy[protected_branches]=false' \
       -F 'deployment_branch_policy[custom_branch_policies]=true'
     gh api --method POST repos/cajias/obsidian-ee/environments/prod/deployment-branch-policies \
       -f name='v*' -f type=tag
     ```
   - dev, `main` branch only:
     ```
     gh api --method PUT repos/cajias/obsidian-ee/environments/dev \
       -F 'deployment_branch_policy[protected_branches]=false' \
       -F 'deployment_branch_policy[custom_branch_policies]=true'
     gh api --method POST repos/cajias/obsidian-ee/environments/dev/deployment-branch-policies \
       -f name=main -f type=branch
     ```
   - staging, `main` branch only **plus** the required reviewer. `reviewers` is an array of objects, which the flat `-f`/`-F` field syntax cannot express, so the whole body goes in as one JSON document — which is also what the replace semantics above want:
     ```
     gh api --method PUT repos/cajias/obsidian-ee/environments/staging --input - <<EOF
     {
       "deployment_branch_policy": {
         "protected_branches": false,
         "custom_branch_policies": true
       },
       "reviewers": [{ "type": "User", "id": $(gh api user --jq .id) }]
     }
     EOF
     gh api --method POST repos/cajias/obsidian-ee/environments/staging/deployment-branch-policies \
       -f name=main -f type=branch
     ```
     Confirm both survived: `gh api repos/cajias/obsidian-ee/environments/staging --jq '{reviewers:[.protection_rules[]|select(.type=="required_reviewers")|.reviewers[].reviewer.login], policy:.deployment_branch_policy}'`
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
