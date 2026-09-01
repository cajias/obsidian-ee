---
name: pr-watch
description: Watch a GitHub PR until it is mergeable and drive every review thread to resolved. Use when the user says "watch PR N", "babysit this PR", "resolve the review threads on PR N", "check what's still open on my PR", or "keep PR N healthy until it merges". Works on GitHub PRs via `gh` and the GraphQL reviewThreads API — it replies in-thread and calls resolveReviewThread rather than only pushing a fix commit. This is the GitHub counterpart to the GitLab-only `code-review:mr-*` skills (mr-shepherd, mr-watcher, mr-submit, gitlab-thread-triage), which all shell out to `glab` and cannot run against this repo.
disable-model-invocation: true
---

<!-- disable-model-invocation: this skill posts comments and resolves review
     threads — outward-facing side effects on a real PR. User-invoked only. -->

**A pushed fix does not close a review thread.** GitHub keeps the thread open
until someone replies and resolves it. A watch loop that polls for *new comment
timestamps* sees nothing new and declares victory while the PR is still blocked.
That is the failure this skill exists to prevent.

Merged PR #77 in this repo has two threads still `isResolved: false` — proof
the loop is real, not hypothetical.

Resolve the PR
--------------

Explicit number wins. Otherwise the PR for the current branch:

`gh pr view --json number,url,state,isDraft,mergeable,headRefName`

No PR for the branch → `gh` prints `no pull requests found for branch "<name>"`.
STOP and say so. Do not open one.

`mergeable` is `MERGEABLE` / `CONFLICTING` / `UNKNOWN` (also `UNKNOWN` on an
already-merged PR — read `state` first).

Fetch the review threads
------------------------

`gh pr view --comments` does **not** expose resolution state. Only GraphQL does:

```bash
gh api graphql -f query='
query($owner:String!, $repo:String!, $number:Int!) {
  repository(owner:$owner, name:$repo) {
    pullRequest(number:$number) {
      reviewThreads(first: 100) {
        totalCount
        nodes {
          id
          isResolved
          isOutdated
          path
          line
          comments(first: 20) { nodes { author { login } body createdAt } }
        }
      }
    }
  }
}' -F owner=cajias -F repo=obsidian-ee -F number=77 \
 --jq '.data.repository.pullRequest.reviewThreads.nodes[] | select(.isResolved == false)'
```

Filter on `isResolved == false`. **Never on comment recency** — a thread opened
weeks ago and never answered is exactly the one that blocks the merge.

`isOutdated: true` with `isResolved: false` is an **OPEN thread that still needs
action**. Outdated only means the code under it moved; the reviewer's ask stands.
`line` is `null` on an outdated thread — use `path` plus the comment body.

Triage each open thread
-----------------------

Four dispositions: **fix** / **defer with justification** / **dismiss as
duplicate** / **not resolvable** (a reviewer-bot summary with no actionable ask —
reply and resolve it, there is nothing to change).

The installed `code-review:gitlab-thread-triage` skill holds the disposition
rubric. Its *dispositions* transfer; its `glab` plumbing does not — read it for
the judgment calls, use the `gh` commands here for the mechanics.

Fix
---

Strict TDD (CLAUDE.md): RED, GREEN, REFACTOR. Test first, always.

A confirmed **trust-boundary** finding MUST leave behind a negative-path
regression test that is RED before the fix and GREEN after. That test is the
deliverable, not the fix. Check it with the `negative-path-auditor` agent in
`.claude/agents/`, or the `security-audit` workflow
(`.claude/workflows/security-audit.js`), which names the exact RED-first test
each finding requires.

Local gate before pushing:

```bash
cargo fmt --all -- --check
cargo lint            # alias for `xtask lint`
cargo test --workspace
```

Plugin changes also need, in `plugins/obsidian-ee`:

```bash
npm run lint && npm test && npx tsc --noEmit
```

`npm test` green does not mean types check — run `tsc --noEmit` separately.

Close the loop — BOTH steps, never just the push
------------------------------------------------

**Pushing a fix commit leaves the thread open and the PR blocked.** Per thread,
after the push:

**a. Reply in-thread.** Note the input field is `pullRequestReviewThreadId`, not
`threadId` (that spelling is only the resolve mutation's):

```bash
gh api graphql -f query='
mutation($threadId:ID!, $body:String!) {
  addPullRequestReviewThreadReply(input: {
    pullRequestReviewThreadId: $threadId, body: $body
  }) { comment { url createdAt } }
}' -F threadId=PRRT_xxx -F body='Removed the explanatory comment as asked — abc1234.'
```

Say what changed and cite the commit SHA. A bare "done" makes the reviewer go
digging.

**b. Resolve it**, selecting `thread { isResolved }` so the response proves it
landed:

```bash
gh api graphql -f query='
mutation($threadId:ID!) {
  resolveReviewThread(input: {threadId: $threadId}) {
    thread { id isResolved }
  }
}' -F threadId=PRRT_xxx
```

`resolveReviewThread` also takes an optional `resolutionReason`:
`ADDRESSED` | `WONT_FIX` | `INVALID` — use it for defer and dismiss dispositions
so the record carries the *why*. `unresolveReviewThread` is the undo.

Step (a) without (b) leaves it open. (b) without (a) leaves the reviewer with no
idea what you did. Do both, per thread, every time.

CI
--

`gh pr checks <n>` and `gh run list --branch <branch> --limit 5`.

On failure, follow the repo's CI rule: reproduce, capture logs and exit codes,
trace to a specific line or commit, then fix. **Never** call a failure a "flake",
"CPU contention", or "infra hiccup" without evidence. Cannot reproduce it? Say
"root cause unknown" — do not guess. Compare the failing run's logs against a
passing run's.

Loop and exit
-------------

Repeat until all three hold:

- zero threads with `isResolved == false`
- CI green
- `mergeable: MERGEABLE`

Stop when GitHub reports `state: MERGED` or `CLOSED`. The installed `/loop` skill
can drive the repeat (`/loop 10m /pr-watch 77`).

**Merging is the user's call.** Do not merge unless told to.

Report format
-------------

Per cycle:

```
PR #<n> <state> — mergeable: <MERGEABLE|CONFLICTING|UNKNOWN>
Threads: <before> open → <after> open
  resolved: <id> <path>:<line> — <what changed> (<sha>)
  deferred: <id> <path>:<line> — <justification> (WONT_FIX)
CI: <n> passed, <n> failed <— link on failure>
Remaining: <what is still open, or "nothing — ready to merge">
```

Report an open thread you chose not to touch as open. A cycle that resolves
nothing is a valid report; a cycle that *claims* zero open threads without
re-running the query is not.
