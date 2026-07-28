//! Development task runner for obsidian-ee.
//!
//! Usage:
//!   cargo xtask docker-up    # Start Docker Compose environment
//!   cargo xtask docker-down  # Stop Docker Compose environment
//!   cargo xtask e2e          # Run E2E tests (starts Docker if needed)
//!   cargo xtask lint         # Run all linters (clippy, fmt, rust-code-analysis)

use std::env;
use std::process::{Command, ExitCode};
use std::time::Duration;

/// Number of healthcheck polls before giving up (30 * 2s ≈ 60s).
const HEALTHCHECK_RETRIES: u32 = 30;
/// Delay between healthcheck polls.
const HEALTHCHECK_DELAY: Duration = Duration::from_secs(2);

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.is_empty() {
        print_help();
        return ExitCode::SUCCESS;
    }

    match args[0].as_str() {
        "docker-up" | "up" => docker_up(),
        "docker-down" | "down" => docker_down(),
        "e2e" => run_e2e(),
        "lint" => run_lint(),
        "help" | "-h" | "--help" => {
            print_help();
            ExitCode::SUCCESS
        }
        cmd => {
            eprintln!("Unknown command: {cmd}");
            print_help();
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!(
        r"
obsidian-ee development task runner

USAGE:
    cargo xtask <COMMAND>

COMMANDS:
    docker-up, up      Start Docker Compose environment (relay, localstack, redis)
    docker-down, down  Stop Docker Compose environment
    e2e                Run E2E tests (starts Docker if needed)
    lint               Run all linters (clippy, fmt check, rust-code-analysis)
    help               Show this help message
"
    );
}

fn docker_up() -> ExitCode {
    println!("Starting Docker Compose environment...");
    run_cmd("docker", &["compose", "-f", "docker/docker-compose.yml", "up", "-d"])
}

fn docker_down() -> ExitCode {
    println!("Stopping Docker Compose environment...");
    run_cmd("docker", &["compose", "-f", "docker/docker-compose.yml", "down"])
}

fn run_e2e() -> ExitCode {
    println!("Running E2E tests...");

    // Start Docker first
    if docker_up() != ExitCode::SUCCESS {
        eprintln!("Failed to start Docker environment");
        return ExitCode::FAILURE;
    }

    // Gate on the compose healthcheck, not a fixed sleep: a dead relay must fail
    // fast instead of running the wire tests against nothing.
    println!("Waiting for the relay to become healthy...");
    if !wait_until(relay_healthy, HEALTHCHECK_RETRIES, HEALTHCHECK_DELAY) {
        eprintln!("Relay did not become healthy; aborting E2E run (not testing a dead relay).");
        return ExitCode::FAILURE;
    }
    println!("Relay is healthy.");

    // ponytail: shared gate invariant with scripts/e2e-test.sh — gate on the relay
    // healthcheck before running tests, and pass `--include-ignored` so BOTH the
    // in-process and the #[ignore]d wire tests run. Keep this rule in sync across both
    // entry points. The docker-absent case intentionally DIFFERS: this xtask requires
    // docker and returns FAILURE (above), whereas the script degrades to non-ignored tests.
    run_cmd("cargo", &["test", "-p", "e2e-tests", "--", "--include-ignored", "--test-threads=1"])
}

/// True iff `docker compose ps relay` reports the relay as `(healthy)`.
fn relay_healthy() -> bool {
    Command::new("docker")
        .args(["compose", "-f", "docker/docker-compose.yml", "ps", "relay"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("(healthy)"))
}

/// Poll `ready` up to `retries` times, sleeping `delay` between attempts.
/// Returns `true` on the first success, `false` once retries are exhausted.
/// Side-effect-free except for calling `ready` and sleeping.
fn wait_until(mut ready: impl FnMut() -> bool, retries: u32, delay: Duration) -> bool {
    for attempt in 0..retries {
        if ready() {
            return true;
        }
        if attempt + 1 < retries {
            std::thread::sleep(delay);
        }
    }
    false
}

fn run_lint() -> ExitCode {
    println!("Running comprehensive lint checks...\n");

    // 1. Format check
    println!("=== Checking formatting (cargo fmt) ===");
    if run_cmd("cargo", &["fmt", "--all", "--", "--check"]) != ExitCode::SUCCESS {
        eprintln!("\n❌ Format check failed. Run 'cargo fmt --all' to fix.");
        return ExitCode::FAILURE;
    }
    println!("✓ Format check passed\n");

    // 2. Clippy
    println!("=== Running Clippy ===");
    if run_cmd("cargo", &["clippy", "--all-targets", "--all-features", "--", "-D", "warnings"])
        != ExitCode::SUCCESS
    {
        eprintln!("\n❌ Clippy found issues.");
        return ExitCode::FAILURE;
    }
    println!("✓ Clippy passed\n");

    // 3. rust-code-analysis (optional - currently has upstream dep conflicts)
    println!("=== Complexity Analysis ===");
    if is_command_available("rust-code-analysis-cli") {
        run_code_analysis();
    } else {
        println!("ℹ rust-code-analysis-cli not available.");
        println!("  (Note: has upstream tree-sitter dependency conflicts)");
        println!("  Relying on clippy's complexity lints instead:\n");
        println!("  - excessive_nesting (threshold: 3)");
        println!("  - too_many_lines (threshold: 50)");
        println!("  - cognitive_complexity (threshold: 25)\n");
    }

    // 4. Code stats with tokei (optional)
    if is_command_available("tokei") {
        println!("=== Code Statistics (tokei) ===");
        let _ = run_cmd("tokei", &["crates/", "--compact"]);
        println!();
    }

    println!("✓ All lint checks passed!");
    ExitCode::SUCCESS
}

fn is_command_available(cmd: &str) -> bool {
    // Use --version check for cross-platform compatibility (works on Windows too)
    Command::new(cmd).arg("--version").output().is_ok_and(|o| o.status.success())
}

fn run_code_analysis() {
    // Run analysis on each crate's src directory
    let crates = ["collab-core", "collab-relay", "collab-cli", "collab-proto"];

    for krate in crates {
        analyze_crate(krate);
    }
    println!();
}

fn analyze_crate(krate: &str) {
    let src_path = format!("crates/{krate}/src");
    println!("Analyzing {src_path}...");

    let output =
        Command::new("rust-code-analysis-cli").args(["-m", "-O", "json", "-p", &src_path]).output();

    let out = match output {
        Ok(o) => o,
        Err(e) => {
            eprintln!("  Error running analysis: {e}");
            return;
        }
    };

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !stderr.is_empty() {
            eprintln!("  Warning: {stderr}");
        }
        return;
    }

    let Ok(json) = String::from_utf8(out.stdout) else {
        eprintln!("  Warning: Invalid UTF-8 output from analysis for {krate}");
        return;
    };
    report_complexity_issues(&json, krate);
}

fn report_complexity_issues(json: &str, krate: &str) {
    // Parse and report high-complexity functions
    // Thresholds: cyclomatic > 15, cognitive > 25
    let Ok(data) = serde_json::from_str::<serde_json::Value>(json) else {
        eprintln!("  Warning: Failed to parse analysis output for {krate}");
        return;
    };

    let mut issues = Vec::new();
    if let Some(spaces) = data.get("spaces").and_then(|s| s.as_array()) {
        collect_complexity_issues(spaces, &mut issues);
    }

    if issues.is_empty() {
        println!("  ✓ {krate}: No complexity issues found");
        return;
    }

    println!("  ⚠ {krate}: {} complexity issues:", issues.len());
    print_issues(&issues);
}

fn print_issues(issues: &[String]) {
    for issue in issues.iter().take(5) {
        println!("    - {issue}");
    }
    if issues.len() > 5 {
        println!("    ... and {} more", issues.len() - 5);
    }
}

fn collect_complexity_issues(spaces: &[serde_json::Value], issues: &mut Vec<String>) {
    for space in spaces {
        if let Some(issue) = check_function_complexity(space) {
            issues.push(issue);
        }

        // Recurse into nested spaces
        if let Some(nested) = space.get("spaces").and_then(|s| s.as_array()) {
            collect_complexity_issues(nested, issues);
        }
    }
}

fn check_function_complexity(space: &serde_json::Value) -> Option<String> {
    let kind = space.get("kind").and_then(serde_json::Value::as_str)?;
    if kind != "function" {
        return None;
    }

    let name = space.get("name").and_then(serde_json::Value::as_str).unwrap_or("unknown");
    let metrics = space.get("metrics")?;

    let cyclomatic = metrics
        .get("cyclomatic")
        .and_then(|c| c.get("sum"))
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    let cognitive = metrics
        .get("cognitive")
        .and_then(|c| c.get("sum"))
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);

    if cyclomatic > 15.0 {
        Some(format!("{name}: cyclomatic={cyclomatic:.0}"))
    } else if cognitive > 25.0 {
        Some(format!("{name}: cognitive={cognitive:.0}"))
    } else {
        None
    }
}

fn run_cmd(cmd: &str, args: &[&str]) -> ExitCode {
    match Command::new(cmd).args(args).status() {
        Ok(status) => {
            if status.success() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("Failed to execute '{cmd}': {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::time::Duration;

    #[test]
    fn never_ready_returns_false() {
        let calls = Cell::new(0u32);
        let ok = wait_until(
            || {
                calls.set(calls.get() + 1);
                false
            },
            5,
            Duration::ZERO,
        );
        assert!(!ok, "exhausting all retries with no success must return false");
        assert_eq!(calls.get(), 5, "ready should be polled exactly `retries` times");
    }

    #[test]
    fn immediately_ready_returns_true() {
        let calls = Cell::new(0u32);
        let ok = wait_until(
            || {
                calls.set(calls.get() + 1);
                true
            },
            5,
            Duration::ZERO,
        );
        assert!(ok, "a first-call success must return true");
        assert_eq!(calls.get(), 1, "must stop polling after the first success");
    }

    #[test]
    fn eventually_ready_returns_true() {
        let calls = Cell::new(0u32);
        let ok = wait_until(
            || {
                let n = calls.get();
                calls.set(n + 1);
                n >= 2 // false on calls 0 and 1, true on call 2
            },
            5,
            Duration::ZERO,
        );
        assert!(ok, "success after transient failures must return true");
        assert_eq!(calls.get(), 3, "false twice then true → polled three times");
    }
}
