//! `docker` and `docker compose` for the scenario.
//!
//! Every call runs from the repository's root (nextest runs tests in the
//! crate's directory) with a deadline: threads drain its pipes, and a guard
//! kills it if the deadline passes or the test panics. Every compose call
//! names the scenario's own project and both files, so it never touches the
//! dev stack (`afkfleet-dev`) and never publishes the server's port.

use core::time::Duration;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;

use chrono::{DateTime, Utc};
use serde_json::Value;

/// The scenario's compose project.
pub(crate) const PROJECT: &str = "afkfleet-e2e";
/// The agent's image, as compose.dev.yaml names it.
pub(crate) const IMAGE: &str = "afkfleet-agent:dev";
/// The compose files, from the repository's root.
const FILES: [&str; 4] = [
    "--file",
    "deploy/compose.dev.yaml",
    "--file",
    "deploy/compose.isolated.yaml",
];
/// How long a quick call (inspect, logs, exec, ps) may take.
pub(crate) const QUICK: Duration = Duration::from_secs(60);
/// How long `down` may take; the budget keeps this much free at the end.
pub(crate) const TEARDOWN: Duration = Duration::from_secs(60);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Kills a process unless it ended first.
#[derive(Debug)]
struct Running(Option<Child>);

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Reads `pipe` to its end on a thread; the text arrives once it closes.
fn drain(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut text = String::new();
        let _ = pipe.read_to_string(&mut text);
        let _ = sender.send(text);
    });
    receiver
}

/// How a finished call ended.
#[derive(Debug)]
pub(crate) struct Finished {
    pub(crate) success: bool,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

/// Runs `command` until it exits, or fails once `deadline` passes.
fn try_finish(mut command: Command, deadline: Duration) -> Result<Finished, String> {
    let description = format!("{command:?}");
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("{description} couldn't start: {error}"))?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(format!("{description} has no pipes"));
    };
    let (stdout, stderr) = (drain(stdout), drain(stderr));
    let mut running = Running(Some(child));
    let timed_out = |_| format!("{description} didn't end within {deadline:?}");
    let stdout = stdout.recv_timeout(deadline).map_err(timed_out)?;
    let stderr = stderr.recv_timeout(deadline).map_err(timed_out)?;
    let status = running
        .0
        .take()
        .map(|mut child| child.wait())
        .transpose()
        .map_err(|error| format!("{description} couldn't be awaited: {error}"))?;
    Ok(Finished {
        success: status.is_some_and(|status| status.success()),
        stdout,
        stderr,
    })
}

/// Runs `command` until it exits within `deadline`.
fn finish(command: Command, deadline: Duration) -> Finished {
    try_finish(command, deadline).unwrap_or_else(|problem| panic!("{problem}"))
}

/// `docker <args>`.
pub(crate) fn docker(args: &[&str], deadline: Duration) -> Finished {
    let mut command = Command::new("docker");
    command.current_dir(repo_root()).args(args);
    finish(command, deadline)
}

fn compose_command(args: &[&str]) -> Command {
    let mut command = Command::new("docker");
    command
        .current_dir(repo_root())
        .args(["compose", "-p", PROJECT])
        .args(FILES)
        .args(args);
    command
}

/// `docker compose -p afkfleet-e2e --file … --file … <args>`.
pub(crate) fn compose(args: &[&str], deadline: Duration) -> Finished {
    finish(compose_command(args), deadline)
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

/// `down --volumes` for the project; the server's world goes with it.
fn down() -> Result<Finished, String> {
    try_finish(
        compose_command(&["down", "--volumes", "--timeout", "10"]),
        TEARDOWN,
    )
}

/// The project's containers, removed again when this is dropped, also when
/// the test panics. nextest's kill at its time limit skips the drop, so the
/// next run's [`Stack::clean`] is the backstop.
#[derive(Debug)]
pub(crate) struct Stack(());

impl Stack {
    /// Removes whatever an earlier run left of the project.
    pub(crate) fn clean() -> Self {
        let finished = down().unwrap_or_else(|problem| panic!("{problem}"));
        assert!(
            finished.success,
            "the leftover {PROJECT} stack couldn't be removed:\n{}",
            finished.stderr
        );
        Self(())
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        // Never panics: a panic while the test already panics would abort.
        let _ = down();
    }
}

/// The agent's log, line by line, as it's written: it only wakes the test
/// when something happens, so its waits need no sleep. The checks always
/// read a fresh [`logs`] instead, since `--follow` may print lines of
/// Compose's own.
#[derive(Debug)]
pub(crate) struct LogFollower {
    lines: mpsc::Receiver<()>,
    _running: Running,
}

impl LogFollower {
    /// Follows `service`'s log.
    pub(crate) fn start(service: &str) -> Self {
        let mut child =
            compose_command(&["logs", "--follow", "--no-color", "--no-log-prefix", service])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("docker compose logs --follow should start");
        let stdout = child.stdout.take().unwrap();
        // One pending wake-up is enough; the reader never blocks on it.
        let (sender, lines) = mpsc::sync_channel(1);
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if line.is_err() {
                    break;
                }
                let _ = sender.try_send(());
            }
        });
        Self {
            lines,
            _running: Running(Some(child)),
        }
    }

    /// Waits until the service logs a line, or for `at_most`. Once the log
    /// has ended, it returns at once.
    pub(crate) fn wait(&self, at_most: Duration) {
        let _ = self.lines.recv_timeout(at_most);
    }
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

/// One run of a container's healthcheck.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Probe {
    pub(crate) start: DateTime<Utc>,
    pub(crate) end: DateTime<Utc>,
    pub(crate) exit_code: i64,
}

/// What `docker inspect` says about a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContainerState {
    pub(crate) running: bool,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) exit_code: i64,
    pub(crate) restart_count: u64,
    /// The healthcheck's last runs; Docker keeps five.
    pub(crate) probes: Vec<Probe>,
}

impl ContainerState {
    /// Reads `docker inspect <container>`'s JSON.
    pub(crate) fn from_inspect(json: &str) -> Self {
        let inspect: Value = serde_json::from_str(json).expect("docker inspect should print JSON");
        let container = &inspect[0];
        let state = &container["State"];
        let time = |value: &Value| {
            let text = value.as_str().unwrap_or_default();
            DateTime::parse_from_rfc3339(text)
                .unwrap_or_else(|error| panic!("{text:?} should be an RFC 3339 time: {error}"))
                .to_utc()
        };
        let probes = state["Health"]["Log"]
            .as_array()
            .map(|log| {
                log.iter()
                    .map(|probe| Probe {
                        start: time(&probe["Start"]),
                        end: time(&probe["End"]),
                        exit_code: probe["ExitCode"]
                            .as_i64()
                            .expect("a probe has an exit code"),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            running: state["Running"]
                .as_bool()
                .expect("inspect shows whether it runs"),
            started_at: time(&state["StartedAt"]),
            exit_code: state["ExitCode"]
                .as_i64()
                .expect("inspect shows the exit code"),
            restart_count: container["RestartCount"]
                .as_u64()
                .expect("inspect shows the restarts"),
            probes,
        }
    }
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
    ContainerState::from_inspect(&finished.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text).unwrap().to_utc()
    }

    /// The parts of `docker inspect`'s output the scenario reads, as Docker
    /// 29 prints them (Go's RFC 3339 with nanoseconds and trailing zeros cut).
    const INSPECT: &str = r#"[
      {
        "Id": "0123abcd",
        "RestartCount": 2,
        "State": {
          "Status": "running",
          "Running": true,
          "ExitCode": 0,
          "StartedAt": "2026-10-10T08:00:15.123456789Z",
          "FinishedAt": "2026-10-10T08:00:12.5Z",
          "Health": {
            "Status": "healthy",
            "FailingStreak": 0,
            "Log": [
              {
                "Start": "2026-10-10T08:00:20.1+00:00",
                "End": "2026-10-10T08:00:20.4+00:00",
                "ExitCode": 1,
                "Output": "starting"
              },
              {
                "Start": "2026-10-10T08:00:25.1Z",
                "End": "2026-10-10T08:00:25.35Z",
                "ExitCode": 0,
                "Output": "ok"
              }
            ]
          }
        }
      }
    ]"#;

    #[test]
    fn inspect_gives_the_state_and_the_probes() {
        let state = ContainerState::from_inspect(INSPECT);

        assert_eq!(
            state,
            ContainerState {
                running: true,
                started_at: at("2026-10-10T08:00:15.123456789Z"),
                exit_code: 0,
                restart_count: 2,
                probes: vec![
                    Probe {
                        start: at("2026-10-10T08:00:20.1Z"),
                        end: at("2026-10-10T08:00:20.4Z"),
                        exit_code: 1,
                    },
                    Probe {
                        start: at("2026-10-10T08:00:25.1Z"),
                        end: at("2026-10-10T08:00:25.35Z"),
                        exit_code: 0,
                    },
                ],
            }
        );
    }

    #[test]
    fn a_container_without_a_healthcheck_has_no_probes() {
        let json = r#"[{"RestartCount":0,"State":{"Running":false,"ExitCode":137,"StartedAt":"2026-10-10T08:00:15Z"}}]"#;

        let state = ContainerState::from_inspect(json);

        assert!(!state.running);
        assert_eq!(state.exit_code, 137);
        assert_eq!(state.probes, []);
    }
}
