# Claude Code automation recommendations — obsidian-ee

Generated 2026-08-30 against `feat/aws-deploy`, accepted 2026-09-02. Rebasing onto
`origin/main` that same day showed **three of the five had already been built upstream,
better** — this analysis was made from a branch that predated ten `.claude` commits on
main. Two survived. The sections below are kept for the evidence that motivated them.

| Recommendation | Outcome |
|---|---|
| Design-integrity hook | **Kept** — `.claude/hooks/design-guard.mjs`, no upstream equivalent |
| `/gates` | **Kept** — `GATES` + `run_gates` in `xtask/src/main.rs`, wrapped by `.claude/skills/gates/SKILL.md` |
| Rust edit-time hook | **Superseded** by `rustfmt-changed.mjs` + `cargo-clippy-turn.mjs` |
| `negative-path-auditor` | **Superseded** — upstream's is 171 lines and names five invariants; mine was 41 generic ones |
| `rust-ts-parity-reviewer` | **Superseded** by `drift-auditor.md`, which covers the same reconnect pair plus the e2e-gate pair |

The Rust-hook supersession is the instructive one, and upstream measured what this
report only reasoned about. Commit `224538a` moved clippy off the per-edit hook because
it cost **6.3s on every `.rs` edit** and risked its 180s timeout on a cold cache — and
because the per-edit latency budget is what forced `-p <crate>` scoping in the first
place, which *structurally cannot see cross-crate breakage*. Splitting it into an 86ms
`rustfmt`-only PostToolUse hook plus one workspace-wide clippy on Stop is strictly
better than the design below on both axes. Read the section below as the reasoning that
led to a hook, not as a recommendation to restore it.

One deviation that survived: `cargo xtask gates` runs **8** gates, not the 6 this report
listed. `CLAUDE.local.md`'s prose list had omitted the CDK app type-check — the only
type check in the whole pipeline, per `integration.yml`'s own comment — and shellcheck
over the guard scripts. Both are AWS-free and were already in CI, so leaving them out
of a runner named `gates` would have made a green result mean less than it looks.

## Codebase profile

| Dimension | Detected |
|---|---|
| Primary | Rust workspace — 8 members (`collab-{core,relay,cli,proto,wasm,watcher}`, `xtask`, `tests/e2e-tests`), 35 `.rs` files |
| Secondary | TypeScript — Obsidian plugin (esbuild/jest/playwright/eslint) and `infra/` AWS CDK app (`node:test`, ts-node), 28 `.ts` files |
| Crypto/protocol | Yrs CRDT, OpenMLS 0.7, ed25519-dalek, aes-gcm; WASM via pinned wasm-pack |
| Lint posture | `clippy::all = deny`, `pedantic`/`nursery` = warn, `unsafe_code = deny`; `cargo lint` → `xtask lint` |
| CI | `ci.yml`, `integration.yml`, `deploy.yml`, `release.yml` + release-please, cargo-deny, gitleaks, shellcheck, actionlint |
| Local gates | pre-commit (`cargo fmt`/`lint`/`test`), 3 guard scripts + 4 milestone runners in `tests/features/` |
| Existing Claude config | 2 PostToolUse hooks (TS-only), 2 project skills, 1 workflow (`security-audit.js`), **0 agents**, no `.mcp.json` |

---

## ⚡ Hooks

### 1. `rust-check.mjs` — PostToolUse on Rust edits *(highest value)*

**Why:** the repo already decided edit-time feedback is worth paying for, but only for
TypeScript. `eslint-changed.mjs` lints the edited plugin file and `typecheck-plugin.mjs`
runs `tsc --noEmit` over the whole program, both exiting 2 to feed errors straight back.
Rust is the larger and stricter half of this codebase (`clippy::all` at **deny**, plus
`pedantic` + `nursery`) and gets nothing until pre-commit or CI. An agent can write a
dozen `.rs` files and only discover the pedantic violations at commit time, after the
context that produced them is gone.

**Spec** — mirror `eslint-changed.mjs` exactly; it is the in-repo template:

- Read `tool_input.file_path` from stdin; no-op unless it is `crates/**/*.rs`.
- `cargo fmt -- <file>` (fast, unconditional).
- `cargo clippy -p <crate> --all-targets -- -D warnings`, where `<crate>` is the second
  path segment. **Do not** call `cargo lint`: that is `xtask lint` over the whole
  workspace and is far too slow per edit; scoping to the owning crate is what keeps this
  usable.
- Exit 2 with clippy's output on stderr, 0 otherwise.

Seconds-scale latency is already accepted precedent here; `typecheck-plugin.mjs` carries
a `PERF:` comment justifying exactly that tradeoff.

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [
          { "type": "command", "command": "$CLAUDE_PROJECT_DIR/.claude/hooks/rust-check.mjs" }
        ]
      }
    ]
  }
}
```

### 2. Design-integrity guard on edit

**Why:** `design-integrity-guard.sh` enforces three invariants that no linter can catch —
the 8-scenario byte-frozen join key between `03-implementation-plan.md` and
`04-bdd-test-plan.md`, the exactly-6-row residual ledger, and every `tests/features/*.feature`
staying a byte-identical copy of its block in `04`. Today it runs **only** in
`integration.yml`, so a reworded scenario name surfaces at push, one or more sessions
after the edit that caused it. The script takes about a second locally.

**Spec:** a command hook that runs the existing script when the edited path is
`docs/design/aws-deploy/03-implementation-plan.md`, `…/04-bdd-test-plan.md`, or
`tests/features/*.feature`. Note the `.feature` glob: the guard checks those too. No new
logic; the hook is a path filter around a script that already exists.

*Skipped:* `.env` blocking (no `.env` files), lock-file blocking (`Cargo.lock` is
deliberately committed).

---

## 🤖 Subagents

`.claude/agents/` is empty. Both proposals encode rules that currently live only as prose
in `CLAUDE.md`, where they depend on the agent having read and remembered them.

### 1. `negative-path-auditor`

**Why:** the single most load-bearing rule in `CLAUDE.md`: every trust boundary needs a
test asserting the attacker case is *rejected*, and a confirmed security fix must leave
behind a regression test that was RED before it. That rule is enforced by nothing.

**Scope:** given a diff touching `collab-core`, `collab-proto`, `collab-relay`, or
`collab-wasm`, list each new or changed trust boundary and state, per boundary, whether a
negative-path test exists — naming the test or naming what is missing. Read-only.

**Overlap, stated up front:** `.claude/workflows/security-audit.js` and the `security-audit`
skill already cover whole-repo auditing. This is a different invocation point: a
per-diff review gate you can run cheaply on every change, versus a full three-phase audit
you run occasionally. If you would rather not maintain both, the honest answer is to skip
this and just run the audit workflow more often.

### 2. `rust-ts-parity-reviewer`

**Why:** `CLAUDE.md` records that reconnect logic is duplicated across the Rust CLI and the
TS client and **has regressed on both sides**. Duplicated state machines drift silently;
nothing checks them against each other.

**Scope:** when either reconnect implementation changes, diff its state machine against
the other and report divergences: settle-exactly-once on failed connects, idempotent
start/stop, backoff schedule. Read-only; reports, does not edit.

---

## 🎯 Skills

### `/gates` — run the known-green local gate set

**Why:** the list of gates that pass without AWS lives only as prose in `CLAUDE.local.md`:
`design-integrity-guard.sh`, `deploy-tag-guard.sh`, `deploy-buildx-guard.sh`, `run-m2.sh`,
`cd infra && npm test`, `actionlint` on three workflows. Six commands, hand-copied, easy to
run four of.

**The lazier form first:** you already have an `xtask` crate and no Makefile. A
`cargo xtask gates` subcommand next to the existing `xtask lint` puts this on the same
surface the repo already uses, works outside Claude Code, and is CI-callable. Reach for a
skill only if you also want it prompt-invocable as `/gates`; in that case the skill should
be a four-line `SKILL.md` with `disable-model-invocation: true` that shells out to the
xtask target, not a second copy of the list.

*Dropped after checking:* a wasm-build skill. The rustup-PATH gotcha is already fixed at
line 12 of `scripts/build-wasm.sh` (`export PATH="$HOME/.cargo/bin:$PATH"`, with a comment
explaining why). No skill needed.

---

## 🔌 MCP servers — nothing to add

Every slot this codebase would want is already filled: **codegraph** (indexed, `.codegraph/`
present), **aws** (`plugin:aws-core`, and `infra/` is CDK), **context-mode**, **obsidian**,
**claude-in-chrome**. GitHub is covered by the `gh` CLI (2.83.2, already used for the
release/deploy flow); a GitHub MCP server would duplicate it. Playwright is covered by the
plugin's own `@playwright/test` suite plus the Chrome MCP.

## 🧩 Plugins — nothing to add

17 plugins are already loaded (94 skills, 32 agents). The gaps found above are
project-specific, not the sort a general-purpose plugin fills.

---

**Ranked, if you only do two things:** the Rust PostToolUse hook, then the
design-integrity hook. Both are small, both close a gap where an existing local check is
simply not being run at the moment it would help.
