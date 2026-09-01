---
name: issue-start
description: Provision an isolated git worktree + branch from a GitHub issue so parallel agents never share a working directory or race on HEAD. Use when the user says "start issue 76", "start work on #72", "pick up issue N", "set up a worktree for this issue", or "begin #31".
disable-model-invocation: true
---

Two agents that both `git checkout` in the same working directory race on `HEAD`:
one checkout silently moves the other's tree mid-edit, and work lands on the wrong
branch or gets clobbered. Every issue gets its own worktree so the safe path is the
default path instead of something you have to remember. This repo already runs on
worktrees (`CLAUDE.md` names `obsidian-ee-core` and `obsidian-ee-relay`; `.claude/worktrees/`
exists and is gitignored) — this just makes provisioning one step.

User-invoked only (`disable-model-invocation: true`): it mutates the filesystem and
git state, so it never fires on its own inference.

1. Read the issue
-----------------

`gh issue view <n> --json number,title,body,labels,state,assignees`

`state` is `OPEN`/`CLOSED`. If CLOSED, **STOP** and confirm with the user before
proceeding. Summarise the acceptance criteria back before touching git.

2. Derive the branch name
-------------------------

`<type>/<n>-<kebab-title>` — the convention actually in use here (real merged
branches: `feat/30-persist-group-state`, `fix/22-e2e-gate-healthcheck-ignored`,
`ci/49-deploy-gated-wire-tests`, `test/48-over-relay-fail-closed`). Confirm against
`git branch -a` and `gh pr list --state merged --limit 10 --json number,title,headRefName`
rather than assuming.

Type comes from the issue's `track-*` label, falling back to the title prefix, and
maps to this repo's commit types (feat, fix, refactor, docs, test, chore, perf, ci):
`track-feature`/`track-wasm` → feat, `track-ci` → ci, `track-docs` → docs,
`track-security` → fix, `track-hygiene` → chore. `P0`/`P1`/`P2` are priority, not type.

Truncate the slug to ~40 chars. Show the derived name and let the user override.

3. Check for existing work FIRST
--------------------------------

Before creating anything:

- `git worktree list` — is one already provisioned for this issue?
- `git branch -a --list '*<n>*'` — does a local or remote branch exist?
- `gh pr list --search '<n>' --state all --json number,title,headRefName,state`

If any exist, **STOP** and report; offer to enter the existing worktree
(`EnterWorktree` with `path`) instead of creating a duplicate.

A squash-merged branch does **not** register with `git branch --merged`. Use
`git cherry -v main <branch>` to tell whether apparently-unmerged work already landed —
lines prefixed `-` are upstream already.

4. Create the worktree
----------------------

Always branch from a freshly fetched base, never a stale local HEAD:

```
git fetch origin
git worktree add -b <branch> .claude/worktrees/<n>-<slug> origin/main
```

`.claude/worktrees/` is gitignored (`.gitignore:39`, alongside `.worktrees/` at `:38`),
so the new tree never shows up as untracked noise. Re-check that line if the path
scheme changes.

Alternative that also links the branch to the issue on GitHub:
`gh issue develop <n> --name <branch> --base main`. Tradeoff: it creates the branch
server-side (so the issue shows a linked branch), but you still need
`git worktree add <path> <branch>` locally to get a tree — and `--checkout` would
check it out in the *current* directory, which is exactly the race this skill avoids.

5. Bootstrap the worktree
-------------------------

A fresh worktree inherits no build state.

- `cargo build --workspace` (or at minimum `cargo check --workspace`).
- Plugin work: `npm ci` in `plugins/obsidian-ee`, then `./scripts/build-wasm.sh`.
  The WASM artifacts under `plugins/obsidian-ee/src/wasm/` are gitignored
  (`plugins/obsidian-ee/.gitignore:10`) and built on demand, and
  `.claude/hooks/typecheck-plugin.mjs:31-38` skips the type-check with
  `typecheck-plugin: skipped — src/wasm not built` until `src/wasm/collab_wasm.d.ts`
  exists. A skipped type-check is not a green one — build the WASM before you trust it.

State the green baseline **before** any edits (the repo's own rule: establish the
baseline first, or you can't attribute a later failure to your change):

```
cargo fmt --all -- --check
cargo lint                 # = cargo xtask lint, per .cargo/config.toml
cargo test --workspace
```

6. Leave a hand-off note
------------------------

Write `CLAUDE.local.md` in the new worktree with exactly these five fields: issue ref,
branch, worktree path, the exact next command, and what is verified green. A future
session (or another agent) reads that instead of re-deriving state.

7. Then follow the TDD path
---------------------------

Strict TDD here — RED, GREEN, REFACTOR (`CLAUDE.md`). Invoke the
`superpowers:test-driven-development` skill to write the failing test first.

Any change touching a trust boundary (inbound network frame, decrypted payload, any
peer- or relay-supplied field) needs a negative-path test that is RED before the fix
and GREEN after — use `.claude/agents/negative-path-auditor.md`.

Teardown (after the PR merges)
------------------------------

Delete the local branch, delete the remote branch, remove the worktree.

Removing a worktree requires **explicit user confirmation**, and untracked files must
be stashed or backed up first (`git -C <path> status --short` to see what would be
lost). Batch `git worktree remove` is policy-blocked — for more than one, print a
copy-pasteable command and let the user run it:

```
git worktree remove .claude/worktrees/<n>-<slug> && \
  git branch -d <branch> && git push origin --delete <branch>
```
