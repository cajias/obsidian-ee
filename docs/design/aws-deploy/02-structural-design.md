# 02 — Structural Design

This document maps the design of `01 — Logic Design` onto the repository's on-disk layout: which files and directories the deployment adds or edits, and what each one is responsible for. The containers and components named in the Architecture (C4) section of `01 — Logic Design` each find their source-of-truth file here; the behaviors and decisions themselves stay in `00`/`01`, per the intent-versus-implementation split of [[hld-documents-separate-solution-intent-from-implementation-detail-via-a-fixed-7]]. The backbone of this layout is the "Files to create/edit" section of `docs/aws-deployment-plan.md`.

## Repository layout

The canonical tree. Paths marked `(new)` are created by this deployment, `(edit)` are modified; unmarked anchors already exist and stay as they are.

```
obsidian-ee/
├── Cargo.toml · Cargo.lock · rust-toolchain.toml        # existing workspace manifests & toolchain pins
├── clippy.toml · deny.toml · rustfmt.toml               # existing lint/audit config
├── version.txt                          (new)           # single version source, maintained by release-please
├── release-please-config.json           (new)           # release-type "simple", root package
├── .release-please-manifest.json        (new)           # last-released version bookkeeping
├── .github/
│   └── workflows/
│       ├── ci.yml                                       # existing CI (build, lint, test under --locked)
│       ├── integration.yml              (new)           # wires the M1 gate into CI; ci.yml unchanged (plan change recorded in 03, M1 section)
│       ├── release.yml                  (new)           # release-please on push to main (PAT-authenticated)
│       └── deploy.yml                   (new)           # build → promote → deploy lane, all envs
├── crates/                                              # existing workspace crates: collab-cli · collab-core ·
│                                                        #   collab-proto · collab-relay · collab-wasm · collab-watcher
├── docker/
│   ├── Dockerfile.relay                 (edit)          # add STOPSIGNAL SIGINT after USER appuser
│   └── docker-compose.yml                               # existing local-dev compose
├── docs/
│   ├── aws-deployment-plan.md                           # the ratified plan this design set realizes
│   ├── build-prompt.md                  (new)           # opening prompt that drives the milestone build
│   └── design/aws-deploy/
│       ├── 00-project-intent.md                         # existing: vision, tenets, success criteria
│       ├── 01-logic-design.md                           # existing: behavior, decisions, architecture (C4)
│       ├── 02-structural-design.md      (new)           # this document
│       ├── 03-implementation-plan.md    (new)           # milestone DAG for execution
│       └── 04-bdd-test-plan.md          (new)           # behavior gates for each milestone
├── infra/                                               # CDK TypeScript app (vanilla cdk init shape)
│   ├── package.json                     (new)           # npm manifest: aws-cdk-lib, constructs, TypeScript
│   ├── package-lock.json                (new)           # npm lockfile; committed so `npm ci` (M2 gate + CI) is reproducible
│   ├── tsconfig.json                    (new)           # TypeScript compiler settings
│   ├── cdk.json                         (new)           # CDK entrypoint command + context
│   ├── bin/
│   │   └── app.ts                       (new)           # app entry: explicit env, domain constant, stack wiring
│   ├── lib/                                             # existing empty stub, now populated
│   │   ├── shared-stack.ts              (new)           # SharedStack: ECR, OIDC provider, zone, IAM roles
│   │   └── relay-stack.ts               (new)           # RelayStack ×3: instance, SG, EIP, DNS, user-data
│   ├── test/                            (new)           # node:test + aws-cdk-lib/assertions specs backing 04's Integration/Unit tiers
│   │   ├── helpers.ts                   (new)           # shared buildApp() fixture: SharedStack + three RelayStacks
│   │   ├── shared-stack.test.ts         (new)           # R5 push-role vs. deploy-role trust, ECR immutability, OIDC-only
│   │   ├── relay-stack.test.ts          (new)           # R6 SG TCP-only, per-stack instance count, token-parameter scope
│   │   ├── user-data-golden.ts          (new)           # golden literal: the complete rendered boot script relay-stack.test.ts pins
│   │   ├── asset-goldens.ts             (new)           # golden literals: the complete content of each infra/assets/ file
│   │   ├── template-goldens.ts          (new)           # generated: the complete synthesized template of all four stacks
│   │   └── regen-template-goldens.ts    (new)           # rewrites template-goldens.ts from the current synth; sole author of it
│   └── assets/
│       ├── docker-compose.yml           (new)           # on-instance compose unit: caddy + relay
│       ├── Caddyfile.tpl                (new)           # env-templated Caddy site block
│       └── deploy.sh.tpl                (new)           # template of /opt/relay/deploy.sh
├── plugins/ · scripts/ · tests/ · xtask/                # existing workspace dirs (tests/features/ added during M1: BDD feature + step runner)
└── README.md · CLAUDE.md                                # existing top-level docs
```

## Module boundaries

One responsibility per added or edited path, keyed to the tree above.

| Path | Responsibility |
|---|---|
| `version.txt` | Holds the current version as a bare string; release-please bumps it, and it becomes the tag `vX.Y.Z` and CHANGELOG heading. The Rust workspace keeps its own `[workspace.package] version`, so release PRs leave `Cargo.toml`/`Cargo.lock` untouched and `--locked` CI stays green. |
| `release-please-config.json` | Declares the `simple` release strategy for the root package, tags without a component prefix. The undotted default filename lets `release.yml` run with zero config-path arguments. |
| `.release-please-manifest.json` | Records the last released version per path (`{".": "0.1.0"}`); release-please's bookkeeping file. |
| `.github/workflows/release.yml` | Runs `googleapis/release-please-action@v4` on push to `main`, authenticated with the `RELEASE_PLEASE_TOKEN` fine-grained PAT so the published release fires `release:`-triggered workflows (decision D5 in the Key design decisions section of `01 — Logic Design`). |
| `.github/workflows/deploy.yml` | The whole delivery lane: `workflow_dispatch` (dev/staging/prod choice) plus `release: published` → prod; `meta` job resolves env and tags, `build` job does the native arm64 build / skip-if-exists / same-digest retag against ECR, `deploy` job assumes the env-scoped OIDC role, sends the SSM run-command, and verifies the WS-upgrade 101. Per-env concurrency, `id-token: write`. |
| `docker/Dockerfile.relay` | Edit: declare `STOPSIGNAL SIGINT` after `USER appuser`, matching the relay's only handled signal so `docker stop` lands gracefully (the Shutdown contract of the Runtime & permission model section of `01 — Logic Design`). |
| `docs/build-prompt.md` | The opening prompt for a fresh build session: points at this design set and drives `03-implementation-plan.md` to test-verified done under `04-bdd-test-plan.md`'s gates. |
| `docs/design/aws-deploy/02-structural-design.md` | This document: the on-disk layout and module boundaries. |
| `docs/design/aws-deploy/03-implementation-plan.md` | The milestone DAG: ordered, verifiable units of work realizing this layout. |
| `docs/design/aws-deploy/04-bdd-test-plan.md` | Behavior specifications gating each milestone, drawn from the Verification section of `docs/aws-deployment-plan.md`. |
| `infra/package.json` | The CDK app's npm manifest: `aws-cdk-lib`, `constructs`, TypeScript toolchain, and the synth/deploy scripts. Pinned to the smoke-tested set: `aws-cdk-lib` 2.263.0, `aws-cdk` CLI 2.1135.1, TypeScript `~5.9` (ts-node 10.9.2 crashes on TS 7.x). |
| `infra/package-lock.json` | npm lockfile generated by `npm install`; committed so `npm ci` (the M2 gate command, and the CDK-assertions CI job) is reproducible and hermetic. |
| `infra/tsconfig.json` | TypeScript compiler settings for the CDK app, scoped to `infra/`. |
| `infra/cdk.json` | Tells the CDK CLI how to run the app (`bin/app.ts`) and pins feature-flag context. |
| `infra/bin/app.ts` | Composition root: sets explicit `env: {account, region}` plus placeholder `vpcId`/`subnetId`/`availabilityZone` bootstrap constants, defines the domain constant once, and instantiates `SharedStack` plus `Relay-{dev,staging,prod}`. The VPC is referenced by attributes (`Vpc.fromVpcAttributes`), not looked up (`Vpc.fromLookup` would make a live AWS call at synth time, which the credential-less M2 gate and CI job cannot make). |
| `infra/lib/shared-stack.ts` | Account-wide singletons: the immutable scan-on-push ECR repo, the GitHub OIDC provider, the `collab.<domain>` public hosted zone with NS output, the ref-wide ECR push role, and the three environment-scoped deploy roles with tag-conditioned SSM permissions. |
| `infra/lib/relay-stack.ts` | Everything one environment owns: `t4g.micro` AL2023 instance tagged `RelayEnv=<env>`, security group (443+80 inbound), instance role (inline SSM agent-channel + association actions — never AmazonSSMManagedInstanceCore, ECR pull, own token parameter), EIP plus association, the `relay-<env>` A record, and user-data that installs Docker with a pinned compose plugin and writes the three `/opt/relay` files from the templated assets. |
| `infra/test/` | `node:test` + `aws-cdk-lib/assertions` specs backing 04's Integration- and Unit-tier CDK-assertion rows: security group (R6), IAM/OIDC trust scoping (R5), ECR immutability, SSM token-parameter scope, per-stack instance count. |
| `infra/assets/docker-compose.yml` | The on-instance compose unit: `caddy:2` owning 80/443 with certificate volumes, and the relay service (`${RELAY_IMAGE}`, restart policy, stop grace period, auth-token env, nc healthcheck) reachable only on the compose network. |
| `infra/assets/Caddyfile.tpl` | The env-templated Caddy site block: `relay-{{ENV}}.collab.<domain>` reverse-proxying to `relay:8080`, with Let's Encrypt handled by Caddy itself. |
| `infra/assets/deploy.sh.tpl` | Template of `/opt/relay/deploy.sh`, run as root via SSM with the full image ref as `$1`: ECR login, token fetch from Parameter Store, `umask 077` env-file write, compose pull and up, image prune. |

## Layout and scaffold decision

**Decision: extend the repository's existing flat conventions, and give `infra/` the vanilla `cdk init app --language typescript` shape**: `bin/` entrypoint, `lib/` stacks, and `cdk.json`, plus an `assets/` directory for the synth-time-templated user-data files. The canonical tree above is the whole layout; every addition slots beside an existing anchor (release files at the root next to the workspace manifests, workflows beside `ci.yml`, the Dockerfile edit in place, the CDK app filling the `infra/lib/` stub that already anticipates exactly this shape).

Scaffolds reviewed, per the reuse check:

- **`monorepo-cookiecutter`** scaffolds brand-new TypeScript monorepos (a `packages/` tree, root-level prettier and hook config, its own docs skeleton). This deployment adds a deploy pipeline to a working Rust workspace, so the right move is the smallest self-contained addition inside the existing layout; the CDK CLI's own canonical shape gives future maintainers (and `cdk` itself) a directory they recognize immediately.
- **`lint-configs`** carries reusable release conventions, and one applies here as confirmation: it, too, drives releases with release-please from a root-level config. Where it uses a dotted config filename (which obliges the workflow to pass a config path), this repo keeps the plan's undotted `release-please-config.json` (release-please's default lookup name), so `release.yml` stays argument-free. The root placement of the release trio in the tree above follows this convention.

Everything the deployment ships lives in exactly four places: repo-root release files, `.github/workflows/`, one `docker/` edit, and the `infra/` app. That keeps the Rust workspace's own structure byte-identical, makes the deployment reviewable as one small footprint, and matches the "Files to create/edit" section of `docs/aws-deployment-plan.md` one-for-one.
