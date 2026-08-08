# Build prompt — execute the milestone plan to done

> Paste this whole file as the opening prompt of a fresh session in the
> `obsidian-ee` repo (`/Users/rc/Projects/workspace/obsidian-ee`). It contains
> the `ultracode` keyword, which is the explicit opt-in the `Workflow` tool
> requires — do not remove it.

---

`ultracode`

## Goal

Drive `docs/design/aws-deploy/03-implementation-plan.md` to done, autonomously,
milestone by milestone, gated on the behavior features in
`docs/design/aws-deploy/04-bdd-test-plan.md`. Author and run **workflows** for
the fan-out phases. Do not ask me what to build — the design set
(`docs/design/aws-deploy/00`–`04`, sourced from `docs/aws-deployment-plan.md`)
is ratified and immutable during execution.

## Definition of done

Not "looks complete." All of these, checkable by command:

1. All **4** exit scenarios green, each run by **its own verbatim gate
   command** from the plan (the Exit criterion block of each milestone section
   in `docs/design/aws-deploy/03-implementation-plan.md`). No substituted
   commands.
2. M4's per-env WS-upgrade probe loop (the `for env in dev staging prod`
   `curl` block in its Exit criterion) passes `101` for all three environments
   in **one** run, against the deployed target — together with the same-digest,
   rollback-drill, and Identify admission checks in that same block.
3. **6/6** residuals closed in the design set, checked against
   `docs/design/aws-deploy/04-bdd-test-plan.md`'s Residual burn-down table —
   closed in the docs, not only in code.
4. Per milestone: `code-review` clean and `simplify` applied **before** it counts
   as done.
5. Per milestone: **its own commit**, message naming it. Load-bearing — the
   quality-gate skills read the scoped diff.
6. CI green, including the existing `ci.yml` (cargo `--locked` build/test) and
   the Integration tier of `04 — BDD Test Plan` (CDK synth assertions,
   actionlint, docker build).
7. `docs/design/aws-deploy/03-implementation-plan.md`'s Residual ledger still
   sums to 6 and every exit-scenario name still matches its Gherkin
   `Scenario:` verbatim (`bash ~/.claude/skills/build-prompt-generator/scripts/check_join_key.sh
   docs/design/aws-deploy/03-implementation-plan.md
   docs/design/aws-deploy/04-bdd-test-plan.md` reports 8 of 8).
8. Every harness change the retrospective decided is applied and in effect.

## Loop shape

**Outer loop lives at session level, not inside a workflow.** Commits, context
clearing, and skill invocations can't happen inside a workflow script, and
`workflow()` nests only one level.

- `TaskCreate` one task per milestone up front — the DAG **is** the work list.
- Next runnable milestone = all dependencies done. M1 (STOPSIGNAL edit) and
  M2 (CDK infra) are independent — run in either order or parallel sessions;
  M3 (release-please + deploy workflow) needs both; M4 (verified rollout)
  needs M3.

  | Milestone | Exit scenario (byte-frozen join key) | Gate lives in | Feature lives in |
  |---|---|---|---|
  | M1 | `relay-container-stops-on-sigint` | `03`, M1 Exit criterion | `04`, M1 feature block |
  | M2 | `cdk-synth-emits-four-stacks` | `03`, M2 Exit criterion | `04`, M2 feature block |
  | M3 | `dev-deploys-without-gate` (also unlocks `staging-waits-for-review`) | `03`, M3 Exit criterion | `04`, M3 feature block |
  | M4 | `prod-deploys-on-release-cut` (also unlocks `handshake-returns-101`, `unauthenticated-identify-rejected`, `rollback-redeploys-old-digest`) | `03`, M4 Exit criterion | `04`, M4 feature block |

- Clear context between milestones. Load only that milestone's **Context to
  load** list plus its feature file.
- Follow `iterative-build-loop` for the harness and
  `superpowers:test-driven-development` for the inner rhythm.

**Per milestone, in order:**

| # | Phase | How |
|---|---|---|
| 1 | Author the feature | Write the BDD Gherkin verbatim into `tests/features/<milestone>.feature` <!-- ASSUMPTION: features dir not fixed by the structural design; confirm or point elsewhere -->, copied from the fenced blocks in `docs/design/aws-deploy/04-bdd-test-plan.md`, plus step-def skeletons. Confirm it fails for the right reason. |
| 2 | Gap analysis | **Workflow**, parallel fan-out. One agent per lens: contract conformance vs `01 — Logic Design`, layout conformance vs `02 — Structural Design`'s canonical tree, existing-code reuse, security, test honesty (does the step assert the behavior or the mechanics?), residual closure. Structured output. |
| 3 | Implement | Subagent per gap (`superpowers:subagent-driven-development`). Strong model for novel logic against frozen contracts; cheap model for mechanical work. `ponytail` governs — climb the ladder before writing. |
| 4 | Gate | Run the verbatim exit command. Red → back to 2. |
| 5 | Adversarial verify | See below. |
| 6 | Quality gate | `/code-review high`, then `/simplify`. Agents: `pr-review-toolkit:code-reviewer`, `everything-claude-code:security-reviewer`, `code-simplifier:code-simplifier` <!-- ASSUMPTION: agent roster chosen from what's installed; swap freely -->. Fix everything CRITICAL/HIGH. |
| 7 | Close residuals | Edit the design set for the residuals this milestone claims (M2: R3, R5, R6 · M3: R4 · M4: R1, R2). `04`'s Residual burn-down table is the checklist. |
| 8 | Commit | One commit, named for the milestone. |
| 9 | Retrospective | See below. **Apply before starting the next milestone.** |
| 10 | Announce | `PushNotification`: milestone reached + the exact command I can run to verify it myself. Then continue to the next runnable milestone by default <!-- ASSUMPTION: continue-by-default per template; flip per the Halt conditions note -->. |

## Adversarial verification (phase 5)

A green test is not proof the milestone is done. For each claim of done, spawn
skeptics **prompted to refute**, each on a distinct lens — correctness, security,
does-it-actually-reproduce, over-engineering, design-conformance. Default to
`refuted=true` under uncertainty. Kill the claim if a majority refute. Loop until
two consecutive rounds surface nothing new — not until a fixed count, which misses
the tail.

## autoresearch (two lanes)

`/autoresearch:autoresearch` is a **code-metric optimizer** — `modify → verify →
keep/discard`. It needs `Goal/Scope/Metric/Verify/Iterations` with a *runnable*
Verify. Never invoke it without one.

**Lane 1 — numeric milestone targets.** This plan's gates are mostly binary
(101 or not, 4 stacks or not, 6/6 or not), so Lane 1 applies mainly to any
milestone where phase 4 stalls red for three rounds — the countable Verify is
then that milestone's own gate command.

**Lane 2 — the harness itself.** This is what the outer workflow drives. The
metric is the cost of the loop:

```
/autoresearch
Goal: Cut tokens and wall-clock spent in phases 2 and 5 without weakening the gate
Scope: the workflow scripts and agent prompts driving phases 2 (gap analysis) and 5 (adversarial verify) of this build
Metric: composite = tokens_per_milestone × verify_rounds_to_convergence;
        gate: every exit command still green and no finding class lost
Verify: re-run the current milestone's verbatim gate command AND the join-key check (must stay 8 of 8) after each harness change
Iterations: 5
```

Sources for the numerator: `budget.spent()` inside the workflow, agent counts in
the run's `journal.jsonl`, rounds-to-convergence from phase 5, `rtk gain`.

## Retrospective (phase 9) — the part that compounds

Ask one question: **what made phases 2 and 5 expensive this time?** Repeated
finding, re-litigated decision, rediscovered gotcha, an agent re-reading what a
linter could have told it in zero tokens.

Route each durable learning with `iterative-build-loop`'s table — memory /
`skill-creator` / `hookify` / `update-config`. Then rank candidate remedies by
**guarantee per token**:

```
score = P(deterministically prevents the class) / (per-iteration context cost)
```

Prefer, in this order — the first rung that holds:

1. **Toolchain or lint rule** — P≈1.0, cost 0. A custom lint rule enforcing the
   structural design's dependency graph beats a reviewer checking it forever. Also
   dead-code checks, type-checker project references, markdown/YAML linters, a
   pre-commit hook.
2. **Command-execution hook** (`update-config`) — P≈1.0, cost 0 until it fires.
3. **Behavior-enforcement rule** (`hookify`) — blocks a pattern, no command.
4. **Skill** (`skill-creator`) — P≈0.6, ~0 cost until invoked.
5. **Specialized agent** — capable but pays full context each run.
6. **`CLAUDE.md` prose** — last resort. It is the *most* expensive place a rule
   can live (resident in every context window) and the *least* deterministic.

So the retrospective also runs **downward**: when a rule gets encoded as a lint
rule or hook, **delete the CLAUDE.md prose it replaced** and say so in the commit.
`ponytail-debt` harvests what was deferred; `claudeception` extracts a skill from a
session that earned one.

## Halt conditions

Announce and keep going by default. Stop and ask me only for: **NS-record
delegation at the registrar** · **minting the fine-grained PAT
(`RELEASE_PLEASE_TOKEN`)** · **`cdk bootstrap` / `cdk deploy`** ·
**`aws ssm put-parameter`** (the per-env auth tokens) · **triggering real
deploys** (`gh workflow run`, GitHub environment approvals, merging the canary
and release PRs) · **anything else that touches the real AWS account or
publishes outward**. These are exactly the "Human-only halt steps" each
milestone of `03 — Implementation Plan` lists — state which step you need,
then wait; the step itself is never an automatable gate.

*Flip this if you want a per-milestone hard stop: change phase 10 to halt instead
of continue.*

## Seeds

- The release PR touches only `version.txt`/`CHANGELOG.md`, so cargo `--locked`
  CI stays green by design — never "fix" the release flow to regenerate the
  lockfile.
- `RELAY_SUBSCRIBE_AUTHZ` stays **off** — enabling it deadlocks MLS bootstrap.
  Do not "harden" it on.
- The relay handles **only SIGINT**; docker's default stop signal is SIGTERM.
  `STOPSIGNAL SIGINT` in `docker/Dockerfile.relay` is load-bearing (M1 exists
  for it).
- The unauthenticated `101` probe is a *valid* health check: the WS handshake
  completes before auth, which happens in the `Identify` message. Do not
  "secure" the handshake.
- ECR tags are immutable; promotion retags an existing digest via
  `docker buildx imagetools create` — never rebuild to promote (same-digest is
  a done-criterion).
- The exit-scenario names in `03`/`04` are byte-frozen join keys checked with
  `grep -F` — never reword, capitalize, or "improve" them.
- `<domain>` in the gate commands resolves when the maintainer sets the domain
  constant during the M2 bootstrap halt steps; until then those gates are
  blocked on that halt, not on you.
- Builds target arm64 (`t4g.micro`): the workflow runner is `ubuntu-24.04-arm`.
  MSRV is Rust 1.87.
- Local wasm builds need `PATH="$HOME/.cargo/bin:$PATH"` or wasm-pack/tsc fail
  on stale bindings.
- This machine runs an Obsidian Git auto-backup that races git operations —
  pause it before commit/rebase work; if work disappears, check `git reflog`
  first.
