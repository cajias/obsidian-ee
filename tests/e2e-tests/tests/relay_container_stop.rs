//! Container stop behavior of the relay image.
//!
//! Covers the `relay-container-stops-on-sigint` scenario
//! (`tests/features/relay-container-shutdown.feature`): the image declares
//! SIGINT as its stop signal, and a running container reaches the stopped state
//! by that signal rather than by a kill at the stop timeout.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread::sleep;
use std::time::Duration;

const CONTAINER: &str = "relay-container-stop-test";
const IMAGE: &str = "relay-container-stop-test-image";

// Tests run with CWD = tests/e2e-tests/, not the repo root, so derive the root
// from the manifest directory: tests/e2e-tests -> tests -> repo root.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("CARGO_MANIFEST_DIR sits two levels below the repo root")
        .to_path_buf()
}

fn docker(root: &Path, args: &[&str]) -> Output {
    Command::new("docker").current_dir(root).args(args).output().expect("failed to spawn docker")
}

fn inspect(root: &Path, format: &str) -> String {
    let out = docker(root, &["inspect", "--format", format, CONTAINER]);
    assert!(
        out.status.success(),
        "docker inspect {format} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn tail_logs(root: &Path) -> String {
    let out = docker(root, &["logs", CONTAINER]);
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text.lines().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
}

// Removes the container on every exit path, including a panicking assertion —
// a failed run must not leave a container behind for the next one to collide with.
struct ContainerGuard;

impl Drop for ContainerGuard {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", CONTAINER]).output();
    }
}

#[test]
#[ignore = "Requires Docker: builds the relay image and exercises container stop"]
fn relay_container_stops_on_sigint() {
    let root = repo_root();

    // Static gate: the image must declare SIGINT as its stop signal. Line-anchored,
    // like the `grep -q '^STOPSIGNAL SIGINT'` this replaces — a STOPSIGNAL mentioned
    // in a comment or an argument does not count.
    let dockerfile = std::fs::read_to_string(root.join("docker/Dockerfile.relay"))
        .expect("docker/Dockerfile.relay is readable");
    assert!(
        dockerfile.lines().any(|line| line == "STOPSIGNAL SIGINT"),
        "docker/Dockerfile.relay must declare STOPSIGNAL SIGINT"
    );

    let build = docker(&root, &["build", "-f", "docker/Dockerfile.relay", "-t", IMAGE, "."]);
    assert!(
        build.status.success(),
        "building the relay image failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    // Constructed before `docker run` so a run that half-succeeds still cleans up.
    // The guard's Drop does not run on a hard kill, so clear any container a
    // previous killed run left behind — otherwise every later run fails on
    // "name already in use". Status is ignored: nothing to remove is the normal case.
    let _guard = ContainerGuard;
    docker(&root, &["rm", "-f", CONTAINER]);

    let run = docker(&root, &["run", "-d", "--name", CONTAINER, IMAGE]);
    assert!(run.status.success(), "docker run failed:\n{}", String::from_utf8_lossy(&run.stderr));
    sleep(Duration::from_secs(2));

    // `docker run -d` exits 0 even when the entrypoint dies immediately, and
    // `docker stop` on an already-exited container also exits 0 — so without this
    // assertion a relay that panicked at boot reports a clean SIGINT stop for a
    // signal it never received. Confirm the process is actually up before
    // signalling it.
    let running = inspect(&root, "{{.State.Running}}");
    assert_eq!(
        running,
        "true",
        "relay was not running before the stop, so SIGINT was never exercised; last logs:\n{}",
        tail_logs(&root)
    );

    let stop = docker(&root, &["stop", "-t", "10", CONTAINER]);
    assert!(
        stop.status.success(),
        "docker stop failed:\n{}",
        String::from_utf8_lossy(&stop.stderr)
    );

    // collab-relay returns Ok(()) after tokio::signal::ctrl_c (main.rs), so a clean
    // SIGINT stop is deterministically 0. Asserting `!= 137` accepted every other
    // nonzero too, so a relay that panicked WHILE shutting down (exit 101) still read
    // as "stopped via SIGINT". The Running check above and this one close different
    // holes: that one catches a container that never ran, this one a container that
    // ran but did not exit cleanly on the signal — a container exiting 0 immediately
    // would satisfy this check alone.
    let exit_code = inspect(&root, "{{.State.ExitCode}}");
    let note = if exit_code == "137" { " — SIGKILLed at the stop timeout" } else { "" };
    assert_eq!(exit_code, "0", "expected a clean exit 0 on SIGINT, got {exit_code}{note}");
}
