# 04 — BDD Test Plan

This document is the test contract for the AWS deployment design set. The behavior features below are written before any implementation, per [[write-gherkin-before-code-not-after]], so the scenarios drive the build instead of documenting what shipped. Every `Then` grounds in a check the Verification section of `docs/aws-deployment-plan.md` actually performs, each feature's narrative names the success criteria of `00 — Project Intent` that its scenarios assert, and the steps speak through the actors defined in `01 — Logic Design` — the **maintainer**, the **collaborator**, and the **release manager** — or through the system boundary they observe.

## Testing model

Three tiers, per [[three-tier-testing-behavior-integration-unit-with-behavior-as-primary-gate]]:

- **Behavior tier** — Gherkin features describing observable behavior at the system boundary: the deploy workflow run, the published WebSocket endpoint, the image registry, the container runtime. This tier is the primary gate: each milestone of `03 — Implementation Plan` names one scenario below as its exit criterion (the Exit criterion line of each milestone section), cross-referenced by scenario name.
- **Integration tier** — checks that run real tools against real definitions with nothing deployed: CDK synth assertions, workflow lint, an image build, a cost estimate review.
- **Unit tier** — small assertions on individual infrastructure constructs.

**Sign-off rule**: a milestone signs off only once its behavior scenario runs green AND the integration and unit tiers run green. The behavior scenarios are the contract dependent milestones rely on; the lower tiers keep the parts honest between milestone gates.

Scenarios speak domain language at the system boundary, per [[gherkin-scenarios-should-describe-behavior-not-ui-mechanics]]: "the release manager approves the staging run" rather than a click path through a console. The boundary here is the workflow run, the `wss://` endpoint, and the registry — surfaces a reader who has never seen the tooling can still follow.

## Behavior tier

Four features, one per milestone of `03 — Implementation Plan`, in milestone order.

### Feature for the M1 — STOPSIGNAL edit milestone

```gherkin
Feature: Relay container shuts down gracefully on stop
  Asserts SC6. A deploy or rollback is stop-then-start on a singleton,
  so the container must exit promptly on the stop signal for recovery
  to stay inside the fifteen-minute budget.

  # The image declares SIGINT as the stop signal; the relay handles only
  # SIGINT, so a container stop lands as a signal the process catches.
  Scenario: relay-container-stops-on-sigint
    Given the maintainer has built the relay image from the repository Dockerfile
    And a relay container is running from that image
    When the maintainer stops the container
    Then the relay process receives the declared stop signal and exits promptly
    And the container reaches the stopped state within the stop-grace period, never by a kill at the timeout
```

### Feature for the M2 — CDK infra + bootstrap runbook milestone

```gherkin
Feature: Infrastructure definition synthesizes the full environment set
  Asserts SC7. The footprint is exactly one shared stack plus three
  single-instance environment stacks — the fixed shape behind the
  fixed, predictable monthly cost under the forty-dollar ceiling.

  # Synth is the system boundary of the infrastructure app: the stack
  # listing is the observable promise of what a deploy would create.
  Scenario: cdk-synth-emits-four-stacks
    Given the maintainer has installed the infrastructure app dependencies
    When the maintainer synthesizes the infrastructure app
    Then the stack listing shows exactly four stacks: RelayShared, Relay-dev, Relay-staging, and Relay-prod
    And each environment stack defines exactly one relay instance
```

### Feature for the M3 — release-please + deploy workflow milestone

```gherkin
Feature: Graded promotion gates on the deploy workflow
  Asserts SC5 and SC1. Dev deploys on demand with no approval pause,
  staging visibly pauses for a named human, and every deploy proves
  the environment answers at the published address before reporting
  success.

  # Dev is the ungated lane: a dispatch runs straight through to a
  # healthy endpoint.
  Scenario: dev-deploys-without-gate
    Given the dev environment carries no required reviewer
    When the maintainer dispatches the deploy workflow at the dev environment
    Then the workflow run proceeds to the deploy job with no approval pause
    And the WebSocket-upgrade probe against the published dev address returns HTTP 101 within the retry budget

  # Staging is the reviewer-gated lane: the pause is observable on the
  # workflow run itself.
  Scenario: staging-waits-for-review
    Given the staging environment names a required reviewer
    When the maintainer dispatches the deploy workflow at the staging environment
    Then the workflow run pauses visibly in the "Waiting for review" state
    And the deploy job proceeds only after the release manager approves the run
    And the WebSocket-upgrade probe against the published staging address returns HTTP 101
```

### Feature for the M4 — Verified rollout milestone

```gherkin
Feature: Verified release rollout, admission control, and rollback
  Asserts SC3, SC5, SC1, SC2, and SC6. Production runs only cut
  releases promoted byte-identical, every environment serves
  credentialed collaborators and refuses everyone else, and a prior
  release restores from the existing digest with no rebuild.

  # The release cut is the prod gate; same-digest promotion is
  # observable in the registry.
  Scenario: prod-deploys-on-release-cut
    Given a fix commit merged to main has produced a release PR
    When the release manager merges the release PR and release v0.1.1 publishes
    Then the deploy workflow fires at the prod environment with the released version
    And the registry shows the v0.1.1 tag and the commit tag naming one and the same image digest

  # The handshake completes before authentication, so an unauthenticated
  # upgrade is a valid liveness probe of every environment.
  Scenario: handshake-returns-101
    Given the maintainer has deployed all three environments
    When the maintainer sends a WebSocket-upgrade request to each published address
    Then every environment answers the upgrade with HTTP status 101

  # Admission happens in the Identify exchange after the handshake; the
  # refusal proves no environment operates as an open relay.
  Scenario: unauthenticated-identify-rejected
    Given the collaborator connects to an environment's published address
    When the collaborator sends an Identify message carrying no valid credential
    Then the relay answers with a rejection and admits no session
    And the collaborator presenting the environment's valid credential in Identify receives an ack and relays a session end to end

  # Rollback is the same machinery pointed at an existing release tag.
  Scenario: rollback-redeploys-old-digest
    Given the registry already holds the digest named by the prior release tag v0.1.0
    When the maintainer dispatches the deploy workflow at prod from the tag v0.1.0
    Then the prod environment runs the digest the v0.1.0 tag already named
    And the workflow run performs no image rebuild
```

## SC traceability table

Every scenario, the success criteria of `00 — Project Intent` asserted, and the milestone gated (per the Milestone DAG section of `03 — Implementation Plan`).

| Scenario | Asserts | Gates milestone |
|---|---|---|
| `relay-container-stops-on-sigint` | SC6 | M1 — STOPSIGNAL edit |
| `cdk-synth-emits-four-stacks` | SC7 | M2 — CDK infra + bootstrap runbook |
| `dev-deploys-without-gate` | SC5, SC1 | M3 — release-please + deploy workflow |
| `staging-waits-for-review` | SC5 | M3 — release-please + deploy workflow |
| `prod-deploys-on-release-cut` | SC3, SC5 | M4 — Verified rollout |
| `handshake-returns-101` | SC1 | M4 — Verified rollout |
| `unauthenticated-identify-rejected` | SC2 | M4 — Verified rollout |
| `rollback-redeploys-old-digest` | SC6 | M4 — Verified rollout |
| *Footnote* — SC4 and SC7 carry integration-tier assertions: SC4's zero-long-lived-credential posture fits no behavior scenario and is asserted by the synth assertions (OIDC-only roles, web ports only); SC7's dollar figure is asserted by the cost estimate review, with `cdk-synth-emits-four-stacks` pinning the fixed footprint behind that figure. | SC4, SC7 | M2 — CDK infra + bootstrap runbook |

With the footnote row counted, each of SC1–SC7 is asserted by at least one check in this plan.

## Integration tier

Checks below the behavior gate, each runnable with nothing deployed:

| Check | Tool | Asserts |
|---|---|---|
| Synth snapshot: the stack set is RelayShared plus the three Relay environment stacks | CDK assertions (`make test`) | backs `cdk-synth-emits-four-stacks` |
| Exactly one EC2 instance per environment stack | CDK assertions (`make test`) | the singleton invariant of `01 — Logic Design` |
| Security group ingress allows TCP 80 and 443 only; port 22 appears nowhere | CDK assertions | SC4's no-SSH posture; R6's no-UDP posture |
| Each deploy role trusts only its own GitHub environment; every CI credential is OIDC-issued and expiring | CDK assertions | SC4 |
| Registry repository declares immutable tags and scan on push | CDK assertions | SC3's tag immutability |
| Each instance role reads only its own token parameter | CDK assertions | SC2's per-environment credential isolation |
| Workflow lint over the release and deploy workflows | actionlint | the delivery lane parses cleanly and references valid contexts |
| Deploy's release-tag guard refuses shell metacharacters, embedded newlines and CR | `cargo test -p xtask --test deploy_workflow_guards` | SC4 — the tag reaches SSM `AWS-RunShellScript` as root, so the guard is the trust boundary |
| Deploy's image build and promotion invariants survive action defaults | `cargo test -p xtask --test deploy_workflow_guards` | SC3 — the built and promoted artifact stays the single arm64 digest the deploy resolves |
| Relay image builds, with the SIGINT stop signal declared | `cargo test -p e2e-tests --test relay_container_stop -- --ignored` | backs `relay-container-stops-on-sigint` |
| Cost estimate review: roughly $32/month total across environments, under the $40 ceiling | documented-accepted, reviewed at bootstrap and monthly | SC7 |

## Unit tier

Deliberately minimal: the codebase under test is infrastructure glue, not domain logic, so the unit tier stays small and honest.

- CDK construct assertions: each environment's hostname follows `relay-<env>.collab.<domain>`; each instance carries the `RelayEnv=<env>` tag its deploy role's condition matches; each token parameter path follows `/relay/<env>/auth-token`.
- The tier grows only where a construct gains branching logic worth pinning; a construct with no branches earns no unit test.

## Residual burn-down

The traceability and burn-down table for the ledger in the Residual ledger section of `03 — Implementation Plan`. A residual closes by the check that proves the system behaves acceptably despite the tradeoff, or as documented-accepted where the tradeoff is deliberate and carries no test.

| ID | Residual (short) | Claimed by | Closing check | Status |
|---|---|---|---|---|
| R1 | Brief downtime and offline-queue loss per deploy; no HA | M4 | `handshake-returns-101` green after a stop-then-start deploy proves the interruption stays brief and the service returns | pending the M4 rollout — the closing check is a *reading*, not an artifact: the per-env 101 probe in `tests/deployment-verify.sh` and deploy.yml's own post-deploy verify are both in place, but `handshake-returns-101` has not yet run against a deployed target. Closes when the maintainer completes M4's runbook step 5 green |
| R2 | Idle WebSocket connections reaped (no keepalive yet) | M4 | documented-accepted — keepalive is a named future direction of `01 — Logic Design`; clients reconnect | closed — Future directions bullet in `docs/design/aws-deploy/01-logic-design.md`, plus the client reconnect loop in `plugins/obsidian-ee/src/collab-client.ts` |
| R3 | Instance replacement loses caddy_data; certificates re-issue. Also triggers on any `cdk deploy` of a Relay-* stack after AWS publishes a newer AL2023 arm64 AMI, since the SSM-latest ImageId re-resolves and forces instance replacement — re-run the environment's deploy workflow afterward to bring the relay back up. | M2 | documented-accepted — deliberate tradeoff; the duplicate-certificate limit is noted alongside the bootstrap runbook | closed — runbook note under Bootstrap item 6 in `docs/aws-deployment-plan.md`, plus the `caddy_data` comment in `infra/assets/docker-compose.yml` |
| R4 | Tags predating the deploy workflow cannot be dispatched | M3 | documented-accepted — deliberate consequence of dispatch-from-ref; every tag cut after the workflow lands is dispatchable | closed — Accepted tradeoffs note in docs/aws-deployment-plan.md, plus the R4 comment on deploy.yml's workflow_dispatch trigger |
| R5 | Registry push role trusts all repository refs | M2 | integration check: each deploy role trusts only its own environment, bounding the blast radius one layer down | closed — push-role trust test in `infra/test/shared-stack.test.ts` |
| R6 | HTTP/3 stays off (UDP 443 closed) | M2 | integration check: security group ingress asserts TCP 80 and 443 only | closed — SG assertion in `infra/test/relay-stack.test.ts` |

**Totals**: 6 residuals; claims M2=3, M3=1, M4=2 → 6/6 claimed; 5 closed (R2, R3, R4, R5, R6), 1 pending its reading (R1). R1's closing check is `handshake-returns-101` observed against a deployed target, so it cannot close before M4's rollout runs — the mechanism is shipped, the reading is outstanding.
