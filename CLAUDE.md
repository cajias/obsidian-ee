# Obsidian E2E Collaborative Editing

End-to-end encrypted collaborative document editing using Yrs CRDT and MLS.

## Build & Test

```bash
# Run with clippy lints
cargo lint
```

## Development

### TDD Workflow

This project uses strict TDD:
1. **RED:** Write failing test first
2. **GREEN:** Minimal code to pass
3. **REFACTOR:** Clean up while tests stay green

### Local E2E Testing

```bash
# Start local environment
docker compose -f docker/docker-compose.yml up -d

# Run E2E tests
./scripts/e2e-test.sh

# Stop environment
docker compose -f docker/docker-compose.yml down
```

## Architecture

- **Offline queue**: In-memory today; DynamoDB-backed persistence is planned
  behind a Cargo feature

## Engineering rules (from audit RCA)

These encode failure classes found in past audits that automated linters do not catch.

### Trust-boundary & crypto invariants
Every trust boundary (inbound network message, decrypted payload, any peer- or
relay-supplied field) MUST have a NEGATIVE-path test asserting the attacker case is
REJECTED — not only a positive round-trip. A crypto test proving "same key/context
decrypts" is insufficient alone; add the sibling proving "wrong context FAILS" (a
ciphertext for doc A must be rejected under doc B, even with a shared key).

E2E-encrypted payloads MUST be AEAD-bound to their context via associated data
(document id today; document id + epoch once MLS lands). The relay is an untrusted
zero-knowledge router: a ciphertext valid for one document MUST fail authentication when
applied to another. Bind the LOCALLY-TRUSTED context (e.g. `config.docId`), NEVER a
value taken from the inbound frame.

Encryption is MLS-only: there is no user-supplied or configured key material, so
"reject a placeholder key" is not the guard anymore. The fail-closed invariant is that
NO update is encrypted, sent, or applied before the MLS group is established — an
owner must have created its group and a joiner must have consumed a Welcome. Keep the
guards that make `sendUpdate` refuse (return false, send nothing) without an MLS group,
and cover them with a negative-path test proving a pre-Welcome client emits no frame
and no plaintext ever leaves the client.

When a security audit CONFIRMS a trust-boundary finding, the fix MUST leave a
negative-path regression test behind that is RED before the fix and GREEN after — the
test is the durable artifact that proves the invariant and stops the class from
regressing. An audit that fixes code without adding such a test is not done.

AI security review runs LOCALLY only (`/security-review`, or `Workflow({name:
'security-audit'})`) on the Claude subscription plan — never as a CI action keyed on an
Anthropic API secret. CI gates stay deterministic (fmt, clippy, tests, cargo-deny,
gitleaks); the AI passes are a developer-run step, not a billed pipeline job.

### Filesystem-watcher tests
`notify_debouncer_mini` does NOT deliver a 1:1 filesystem-action→event mapping — a
create can be followed by a content `Modified` in a later debounce window. Tests that
observe watcher events MUST drain until the stream goes quiet and assert the expected
kind is *present* (`.any(|e| e.kind == X)`), never `recv()` exactly one event per action.
The crate's `drain_events`/`collect_events` helpers exist for this.

### Reconnect & connection lifecycle
- Every connect attempt MUST settle its promise/future exactly once — including a retry
  attempt whose socket fails *before* opening. A never-settled connect deadlocks the
  reconnect loop (a dedup guard then returns the stale pending promise forever).
- Session start/stop (and any resource-owning lifecycle command) MUST be idempotent:
  guard against a second start that would orphan the prior client/handle.
- The TS client's reconnect behavior must have state-machine tests mirroring the Rust
  CLI's — reconnect logic is duplicated across the two and has regressed on both sides.

### Resource bounds
Any collection fed by untrusted or network-sourced input MUST be bounded by BYTES, not
just by element count — a per-item count cap with MiB-scale items still permits OOM.
Charge/credit the byte counter on every add/remove path and keep it O(1).

### Partial-success state
A flag or handle that records "established" while only PART of a multi-step setup
succeeded is this codebase's most repeated defect — four instances in one audit
session, every one invisible to a fully green suite.

Set a completion flag only AFTER every step it claims completed has returned. A
teardown may undo only work whose side effect has not yet left the process: once
a frame is on the wire (`register_doc_key` above all, which the relay refuses to
accept twice for the same document), freeing the local state strands it and no
retry can recover.

Scope a teardown to the unit of work. One `try` around a loop over N
side-effecting units either under-cleans (leaks the failed unit) or over-cleans
(destroys units that succeeded); use a per-unit `try`. A safety comment written
in the singular about a plural operation is the tell.

A single boolean cannot honestly represent N independently-established resources.
Derive readiness from the resources themselves rather than latching a flag over
them.

Test the state, not the promise: asserting that a retry RESOLVES proves nothing,
because these failures resolve normally and go quiet. Assert the post-retry state
is USABLE — the handle exists, the registration was sent exactly once.

### Gate assertions
A gate must assert the scenario it names actually HAPPENED, not merely that one failure
mode was absent. The container-stop gate — now `tests/e2e-tests/tests/relay_container_stop.rs`
— was first written asserting `exit_code != 137`, "not SIGKILLed", and so printed
`OK: stopped via SIGINT` for a relay that died at boot and never received the signal, and
for one that panicked mid-shutdown (exit 101). The fix is a positive reading: the container
was `Running` before the stop, AND exited `0`. Prefer asserting the expected value over
excluding a known-bad one, and where a gate depends on a process actually starting, assert
that separately — a command that "succeeds" against nothing is the most expensive kind of
green. The CDK assertions in `infra/test/*.test.ts` (a resource count and four stack names)
and the 101 reading in `tests/deployment-verify.sh` already do this.

A CI gate also needs a local counterpart, or it only ever fails after a push. That
counterpart is no longer a mirror to maintain: CI and developers both invoke the make
targets, so a new gate is a line in the target CI already calls, and the two cannot drift.

### Dead code / YAGNI
Keep internal-crate APIs `pub(crate)` (not `pub`) so `rustc`'s `dead_code` lint flags
unused items — `pub` items in a workspace-internal crate are never reported as dead.
Do not add speculative public surface "for later"; a test that exists only to exercise
otherwise-unused code is a signal to delete the code, not keep it.

## Current build state — aws-deploy

Branch `feat/aws-deploy`. Plan: `docs/design/aws-deploy/03-implementation-plan.md`
(M1–M4). Design set `00`–`04` is ratified; changes to it are recorded as numbered
"Plan change" notes inside the milestone sections. Commit shas are deliberately not
listed here — they do not survive a squash merge.

| Milestone | Gate | State |
|---|---|---|
| M1 STOPSIGNAL | `cargo test -p e2e-tests --test relay_container_stop -- --ignored` | green; needs a running Docker daemon to re-verify |
| M2 CDK infra | `make test` | **green, exit 0** |
| M3 release + deploy | `RELAY_CHECKS=probe RELAY_ENVS=dev bash tests/deployment-verify.sh`<br>`RELAY_CHECKS=staging-gate bash tests/deployment-verify.sh` (also needs `RELAY_DOMAIN`) | code-complete; **exit 2 BLOCKED** on human steps |
| M4 verified rollout | `bash tests/deployment-verify.sh` | harness + runbook shipped; **exit 2 BLOCKED** on human steps |

M3 carries two behavior scenarios — the ungated dev lane and the reviewer-gated staging
lane — so its row names two gate commands, one per scenario. The staging one probes the
staging address and reads the GitHub API, so it needs `RELAY_DOMAIN` and an authenticated
`gh`; a missing `gh` exits 2 BLOCKED, a dead staging address exits 1. It never issues the
staging dispatch. See Plan change 7 in `03-implementation-plan.md`.

**Nothing is blocked on the agent.** Every remaining step needs the maintainer's AWS
account.

### Verified green locally (no AWS needed)

```bash
make test    # cargo test --workspace + the infra tsc type check + the infra CDK assertions
make lint    # fmt, clippy, cargo-deny, actionlint, shellcheck
```

The Makefile is the single source of truth, and CI invokes these same targets rather than
restating their steps — so there is nothing to keep in sync and no drift to detect. A new
gate that runs without cloud credentials is a line in `make test` or `make lint`, and CI
picks it up by construction. `make test-e2e` (`cargo xtask e2e` plus
`bash tests/deployment-verify.sh`) stays separate: it needs Docker, and its deployment
half needs AWS plus an authenticated `gh`.

### The one open item

Residual **R1** is the only one not closed: its closing check is `handshake-returns-101`
observed against a deployed target — a reading, not an artifact. It closes when the
rollout runs green. Ledger is 5 closed / 1 pending, deliberately. Do not mark it closed
without the reading.

### Next actions, in order (all human)

1. **Bootstrap AWS** — `docs/aws-deployment-plan.md` → `## Bootstrap runbook`, items 1–7.
   Watch two things: pick a subnet that auto-assigns public IPs **and** sits in `${REGION}a`
   (item 3); and item 7's `PUT /environments` is create-or-replace, so the branch/tag policy
   and the staging reviewer go in the **same** body.
2. **M3 halt steps 8–10** — mint `RELEASE_PLEASE_TOKEN`, `gh variable set` the repo and
   per-env variables, run the first dev dispatch.
3. **Verify M3**, then walk M4:
   ```bash
   export AWS_PROFILE=<profile> AWS_REGION=<region> RELAY_DOMAIN=<apex domain>
   RELAY_CHECKS=probe RELAY_ENVS=dev bash tests/deployment-verify.sh   # 101 from relay-dev
   RELAY_CHECKS=staging-gate bash tests/deployment-verify.sh   # staging paused for its
                                     # reviewer, plus staging 101; needs `gh`. Green on the
                                     # dispatch alone — approval is not a prerequisite
   bash tests/deployment-verify.sh   # all three envs, all four scenarios; names the
                                     # next outstanding step each run
   ```
   M4's ordered procedure: `docs/aws-deployment-plan.md` → `## Rollout runbook (M4)`.
   The script verifies the rollback drill but never issues it — that dispatch is a real
   prod deploy and stays a human step; re-run with `M4_ROLLBACK_DRILL=done` once it has
   happened.

### Conventions this build follows (keep them)

- BDD scenario names are a byte-frozen join key between `03` and `04`; `.feature` files are
  verbatim copies. `xtask/tests/design_integrity.rs` enforces both in CI, and
  `.claude/hooks/design-guard.mjs` enforces them at edit time by running that same test.
- Amending a ratified gate command requires a numbered **Plan change** note in `03` with the
  empirical justification — see the ones already there, through Plan change 7.
- One commit per milestone, so `/code-review` and `/simplify` get a scoped diff.
