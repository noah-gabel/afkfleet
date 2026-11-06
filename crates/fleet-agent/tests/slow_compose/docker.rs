//! `docker` and `docker compose` for the scenario, over fleet-testkit's
//! shared helpers (ADR-0022, U24).
//!
//! Every call runs from the repository's root (nextest runs tests in the
//! crate's directory) with a deadline, killed if it passes or the test
//! panics. Every compose call names the scenario's own project and both
//! files, so it never touches the dev stack (`afkfleet-dev`) and never
//! publishes the server's port.

use core::time::Duration;
use std::path::{Path, PathBuf};

pub(crate) use fleet_testkit::docker::Probe;
use fleet_testkit::docker::{self as kit, Compose, ContainerState, Finished};

/// The scenario's compose project.
pub(crate) const PROJECT: &str = "afkfleet-e2e";
/// The agent's image, as compose.dev.yaml names it.
pub(crate) const IMAGE: &str = "afkfleet-agent:dev";
/// The compose files, from the repository's root.
const FILES: [&str; 2] = ["deploy/compose.dev.yaml", "deploy/compose.isolated.yaml"];
/// How long a quick call (inspect, logs, exec, ps) may take.
pub(crate) const QUICK: Duration = Duration::from_secs(60);
/// How long `down` may take; the budget keeps this much free at the end.
pub(crate) const TEARDOWN: Duration = Duration::from_secs(60);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The scenario's project.
fn project() -> Compose {
    Compose::new(&repo_root(), PROJECT, &FILES)
}

/// `docker <args>` from the repository's root.
pub(crate) fn docker(args: &[&str], deadline: Duration) -> Finished {
    kit::docker(&repo_root(), args, deadline).unwrap_or_else(|problem| panic!("{problem}"))
}

/// `docker compose -p afkfleet-e2e --file … --file … <args>`.
pub(crate) fn compose(args: &[&str], deadline: Duration) -> Finished {
    project()
        .run(args, deadline)
        .unwrap_or_else(|problem| panic!("{problem}"))
}

/// Like [`compose`], and fails unless the call succeeds; returns its stdout.
pub(crate) fn compose_ok(args: &[&str], deadline: Duration) -> String {
    let finished = compose(args, deadline);
    assert!(
        finished.success,
        "docker compose {args:?} failed:\n{}\n{}",
        finished.stdout, finished.stderr
    );
    finished.stdout
}

/// The project's containers, removed again when this is dropped, also when
/// the test panics. nextest's kill at its time limit skips the drop, so the
/// next run's [`Stack::clean`] is the backstop.
#[derive(Debug)]
pub(crate) struct Stack {
    _cleanup: kit::Cleanup,
}

impl Stack {
    /// Removes whatever an earlier run left of the project.
    pub(crate) fn clean() -> Self {
        let project = project();
        let finished = project
            .down(TEARDOWN)
            .unwrap_or_else(|problem| panic!("{problem}"));
        assert!(
            finished.success,
            "the leftover {PROJECT} stack couldn't be removed:\n{}",
            finished.stderr
        );
        Self {
            _cleanup: project.cleanup(TEARDOWN),
        }
    }
}

/// The agent's log, line by line, as it's written: it only wakes the test
/// when something happens, so its waits need no sleep.
pub(crate) use fleet_testkit::docker::LogFollower;

/// Follows `service`'s log.
pub(crate) fn follow(service: &str) -> LogFollower {
    project()
        .follow(service)
        .expect("docker compose logs --follow should start")
}

/// `service`'s whole log.
pub(crate) fn logs(service: &str) -> String {
    compose_ok(&["logs", "--no-color", "--no-log-prefix", service], QUICK)
}

/// The output of RCON's `command` on the server.
pub(crate) fn rcon(command: &str) -> String {
    compose_ok(&["exec", "-T", "minecraft", "rcon-cli", command], QUICK)
        .trim()
        .to_owned()
}

/// The state of `service`'s container.
pub(crate) fn state(service: &str) -> ContainerState {
    let id = compose_ok(&["ps", "--all", "--quiet", service], QUICK);
    let id = id.trim();
    assert!(!id.is_empty(), "{service} should have a container");
    let finished = docker(&["inspect", id], QUICK);
    assert!(
        finished.success,
        "docker inspect failed: {}",
        finished.stderr
    );
    ContainerState::from_inspect(&finished.stdout).expect("docker inspect should read")
}
