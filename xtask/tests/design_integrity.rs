//! Design-set integrity guard: the join key and the residual ledger.
//!
//! Both are definition-of-done criteria for the aws-deploy build. The scenario
//! names are the join key between the implementation plan and the BDD plan: 03
//! cites them as milestone exit gates, 04 defines them as Gherkin scenarios,
//! and the two are matched literally. Renaming one is a deliberate act that
//! must update both documents AND the `SCENARIOS` list below — before this
//! check existed in-repo, the join key was verified only by a script outside
//! the repository, so a reworded scenario name would have shipped silently.

use std::fs;
use std::path::{Path, PathBuf};

/// Scenario names, byte-frozen. Both documents must cite every one of them.
const SCENARIOS: [&str; 8] = [
    "relay-container-stops-on-sigint",
    "cdk-synth-emits-four-stacks",
    "dev-deploys-without-gate",
    "staging-waits-for-review",
    "prod-deploys-on-release-cut",
    "handshake-returns-101",
    "unauthenticated-identify-rejected",
    "rollback-redeploys-old-digest",
];

/// Integration tests run with the CWD set to the package root (`xtask/`), so
/// every repo path is built from the manifest dir's parent, never from `"../"`.
fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/ always has a workspace-root parent")
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn plan() -> String {
    read("docs/design/aws-deploy/03-implementation-plan.md")
}

fn bdd() -> String {
    read("docs/design/aws-deploy/04-bdd-test-plan.md")
}

#[test]
fn every_scenario_is_cited_in_both_documents() {
    let (plan, bdd) = (plan(), bdd());
    let missing: Vec<String> = SCENARIOS
        .iter()
        .map(|s| (s, plan.contains(*s), bdd.contains(*s)))
        .filter(|(_, in_plan, in_bdd)| !(*in_plan && *in_bdd))
        .map(|(s, in_plan, in_bdd)| format!("{s} missing (03={in_plan}, 04={in_bdd})"))
        .collect();
    assert!(
        missing.is_empty(),
        "scenario names are the byte-frozen join key between 03 and 04:\n{}",
        missing.join("\n")
    );
}

#[test]
fn bdd_plan_declares_each_scenario_exactly_once() {
    let bdd = bdd();
    let wrong: Vec<String> = SCENARIOS
        .iter()
        .map(|s| {
            let header = format!("Scenario: {s}");
            let n = bdd.lines().filter(|l| l.trim_start() == header).count();
            (s, n)
        })
        .filter(|(_, n)| *n != 1)
        .map(|(s, n)| format!("04 declares 'Scenario: {s}' {n} times, expected exactly 1"))
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn feature_files_are_verbatim_copies_of_their_block_in_04() {
    let bdd = bdd();
    let dir = repo_root().join("tests/features");
    let features: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()))
        .map(|e| e.expect("directory entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "feature"))
        .collect();

    // An empty glob must not pass vacuously: the invariant is that the
    // committed .feature files are copies, which says nothing if there are none.
    assert!(
        !features.is_empty(),
        "no .feature files under {} — the verbatim-copy invariant would pass vacuously",
        dir.display()
    );

    let drifted: Vec<String> = features
        .iter()
        .filter(|p| !bdd.contains(read_feature(p).trim()))
        .map(|p| format!("{} has drifted from 04", p.display()))
        .collect();
    assert!(
        drifted.is_empty(),
        "feature files are verbatim copies of their block in 04:\n{}",
        drifted.join("\n")
    );
}

fn read_feature(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

#[test]
fn residual_ledger_has_exactly_six_rows() {
    // The Accepted tradeoffs enumerate exactly six residuals; the ledger must
    // not grow or shrink silently. Closing one is an edit to its Status cell,
    // never a new or deleted row.
    let rows = bdd()
        .lines()
        .filter(|l| {
            l.strip_prefix("| R").is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        })
        .count();
    assert_eq!(rows, 6, "residual ledger has {rows} rows, expected exactly 6");
}
