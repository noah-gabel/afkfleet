//! `docker` and `docker compose` for the slow tests (ADR-0014; ADR-0022,
//! U24): the compose e2e test's helpers, shared with the stack's smoke tests
//! and the SFTP account's test.
//!
//! - **Every call has a deadline.** Threads drain its pipes, and a guard
//!   kills the process if the deadline passes or the caller is dropped.
//! - **A [`Compose`] project is the test's own:** every call names its
//!   project and files, so it never touches another stack, and a
//!   [`Cleanup`] takes the project down (`down --volumes`) when it's
//!   dropped, also when the test panics. nextest's kill at its time limit
//!   skips the drop, so a test cleans up first too ([`Compose::down`]).
//! - **Waits never sleep:** a [`LogFollower`] wakes its waiter at each line a
//!   service logs, or at the waiter's own bound.
//!
//! Like every part of this crate, it never panics: a call that can't start,
//! times out or isn't awaited is a [`DockerError`].

use core::fmt;
use core::time::Duration;
use std::io::{BufRead as _, BufReader, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;

use chrono::{DateTime, Utc};
use serde_json::Value;

/// Why a `docker` call gave no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerError(String);

impl fmt::Display for DockerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DockerError {}

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

/// Reads `pipe` to its end on a thread; the text arrives once it closes,
/// which the process's end or its kill guarantees.
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    /// Whether it exited with 0.
    pub success: bool,
    /// Its exit code, if it exited.
    pub code: Option<i32>,
    /// Everything it printed on stdout.
    pub stdout: String,
    /// Everything it printed on stderr.
    pub stderr: String,
}

/// Runs `command` with `stdin`'s bytes on its standard input (none: an
/// empty one), until it exits, or fails once `deadline` passes.
///
/// # Errors
/// [`DockerError`] when it can't start, doesn't end within `deadline`, or
/// can't be awaited.
pub fn finish(
    mut command: Command,
    stdin: Option<&[u8]>,
    deadline: Duration,
) -> Result<Finished, DockerError> {
    let description = format!("{command:?}");
    let mut child = command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| DockerError(format!("{description} couldn't start: {error}")))?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(DockerError(format!("{description} has no pipes")));
    };
    let (stdout, stderr) = (drain(stdout), drain(stderr));
    let input = child.stdin.take();
    let mut running = Running(Some(child));
    if let (Some(mut pipe), Some(bytes)) = (input, stdin) {
        // Dropping the pipe closes it, so the process sees its input end.
        pipe.write_all(bytes)
            .map_err(|error| DockerError(format!("{description}'s stdin: {error}")))?;
    }
    let timed_out = |_| DockerError(format!("{description} didn't end within {deadline:?}"));
    let stdout = stdout.recv_timeout(deadline).map_err(timed_out)?;
    let stderr = stderr.recv_timeout(deadline).map_err(timed_out)?;
    let status = running
        .0
        .take()
        .map(|mut child| child.wait())
        .transpose()
        .map_err(|error| DockerError(format!("{description} couldn't be awaited: {error}")))?;
    Ok(Finished {
        success: status.is_some_and(|status| status.success()),
        code: status.and_then(|status| status.code()),
        stdout,
        stderr,
    })
}

/// `docker <args>` in `dir`.
///
/// # Errors
/// As [`finish`].
pub fn docker(dir: &Path, args: &[&str], deadline: Duration) -> Result<Finished, DockerError> {
    let mut command = Command::new("docker");
    command.current_dir(dir).args(args);
    finish(command, None, deadline)
}

/// A compose project of a test's own: its name, its files and the folder
/// compose runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compose {
    dir: PathBuf,
    project: String,
    files: Vec<String>,
    env_file: Option<PathBuf>,
}

impl Compose {
    /// `project` over `files`, from `dir`.
    #[must_use]
    pub fn new(dir: &Path, project: &str, files: &[&str]) -> Self {
        Self {
            dir: dir.to_owned(),
            project: project.to_owned(),
            files: files.iter().map(|file| (*file).to_owned()).collect(),
            env_file: None,
        }
    }

    /// The same project, reading its variables from `env_file` instead of
    /// the folder's `.env`.
    #[must_use]
    pub fn with_env_file(mut self, env_file: &Path) -> Self {
        self.env_file = Some(env_file.to_owned());
        self
    }

    /// The project's name.
    #[must_use]
    pub fn project(&self) -> &str {
        &self.project
    }

    /// The folder compose runs in.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `docker compose -p <project> [--env-file …] --file … <args>`, not
    /// started.
    #[must_use]
    pub fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("docker");
        command
            .current_dir(&self.dir)
            .args(["compose", "-p", &self.project]);
        if let Some(env_file) = &self.env_file {
            command.arg("--env-file").arg(env_file);
        }
        for file in &self.files {
            command.args(["--file", file]);
        }
        command.args(args);
        command
    }

    /// Runs `args` until they end.
    ///
    /// # Errors
    /// As [`finish`].
    pub fn run(&self, args: &[&str], deadline: Duration) -> Result<Finished, DockerError> {
        finish(self.command(args), None, deadline)
    }

    /// Runs `args` with `stdin` on its standard input until they end.
    ///
    /// # Errors
    /// As [`finish`].
    pub fn run_with_stdin(
        &self,
        args: &[&str],
        stdin: &[u8],
        deadline: Duration,
    ) -> Result<Finished, DockerError> {
        finish(self.command(args), Some(stdin), deadline)
    }

    /// `down --volumes --remove-orphans`: the project's containers, networks
    /// and volumes, and nothing else.
    ///
    /// # Errors
    /// As [`finish`].
    pub fn down(&self, deadline: Duration) -> Result<Finished, DockerError> {
        self.run(
            &["down", "--volumes", "--remove-orphans", "--timeout", "10"],
            deadline,
        )
    }

    /// A guard that takes the project down when it's dropped.
    #[must_use]
    pub fn cleanup(&self, deadline: Duration) -> Cleanup {
        Cleanup {
            compose: self.clone(),
            deadline,
        }
    }

    /// Follows `service`'s log.
    ///
    /// # Errors
    /// [`DockerError`] when `docker compose logs --follow` can't start.
    pub fn follow(&self, service: &str) -> Result<LogFollower, DockerError> {
        let mut child = self
            .command(&["logs", "--follow", "--no-color", "--no-log-prefix", service])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| DockerError(format!("logs --follow couldn't start: {error}")))?;
        let Some(stdout) = child.stdout.take() else {
            return Err(DockerError("logs --follow has no stdout".to_owned()));
        };
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
        Ok(LogFollower {
            lines,
            _running: Running(Some(child)),
        })
    }
}

/// Takes its project down when dropped, also when the test panics; see
/// [`Compose::cleanup`].
#[derive(Debug)]
pub struct Cleanup {
    compose: Compose,
    deadline: Duration,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        // Never panics: a panic while the test already panics would abort.
        let _ = self.compose.down(self.deadline);
    }
}

/// A service's log, line by line, as it's written: it only wakes its waiter
/// when something happens, so a wait needs no sleep. Checks read a fresh
/// `logs` instead, since `--follow` may print lines of compose's own.
#[derive(Debug)]
pub struct LogFollower {
    lines: mpsc::Receiver<()>,
    _running: Running,
}

impl LogFollower {
    /// Waits until the service logs a line, or for `at_most`. Once the log
    /// has ended, it returns at once.
    pub fn wait(&self, at_most: Duration) {
        let _ = self.lines.recv_timeout(at_most);
    }
}

/// One run of a container's healthcheck.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    /// When it started.
    pub start: DateTime<Utc>,
    /// When it ended.
    pub end: DateTime<Utc>,
    /// Its exit code.
    pub exit_code: i64,
}

/// What `docker inspect` says about a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerState {
    /// Whether it runs.
    pub running: bool,
    /// When it last started.
    pub started_at: DateTime<Utc>,
    /// Its last exit code.
    pub exit_code: i64,
    /// How often Docker restarted it.
    pub restart_count: u64,
    /// Whether the kernel killed it for its memory limit.
    pub oom_killed: bool,
    /// The healthcheck's last runs; Docker keeps five.
    pub probes: Vec<Probe>,
}

impl ContainerState {
    /// Reads `docker inspect <container>`'s JSON.
    ///
    /// # Errors
    /// [`DockerError`] naming the field that's missing or unreadable.
    pub fn from_inspect(json: &str) -> Result<Self, DockerError> {
        let inspect: Value = serde_json::from_str(json)
            .map_err(|error| DockerError(format!("docker inspect's output isn't JSON: {error}")))?;
        let container = inspect
            .get(0)
            .ok_or_else(|| DockerError("docker inspect named no container".to_owned()))?;
        let field = |value: &Value, key: &str| value.get(key).cloned().unwrap_or(Value::Null);
        let state = field(container, "State");
        let missing = |field: &str| DockerError(format!("docker inspect has no {field}"));
        let time = |value: Value, name: &str| {
            value
                .as_str()
                .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
                .map(|time| time.to_utc())
                .ok_or_else(|| missing(name))
        };
        let log = field(&field(&state, "Health"), "Log");
        let probes = match log.as_array() {
            None => Vec::new(),
            Some(log) => log
                .iter()
                .map(|probe| {
                    Ok(Probe {
                        start: time(field(probe, "Start"), "Health.Log.Start")?,
                        end: time(field(probe, "End"), "Health.Log.End")?,
                        exit_code: field(probe, "ExitCode")
                            .as_i64()
                            .ok_or_else(|| missing("Health.Log.ExitCode"))?,
                    })
                })
                .collect::<Result<_, DockerError>>()?,
        };
        Ok(Self {
            running: field(&state, "Running")
                .as_bool()
                .ok_or_else(|| missing("State.Running"))?,
            started_at: time(field(&state, "StartedAt"), "State.StartedAt")?,
            exit_code: field(&state, "ExitCode")
                .as_i64()
                .ok_or_else(|| missing("State.ExitCode"))?,
            restart_count: field(container, "RestartCount")
                .as_u64()
                .ok_or_else(|| missing("RestartCount"))?,
            oom_killed: field(&state, "OOMKilled").as_bool().unwrap_or(false),
            probes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text).unwrap().to_utc()
    }

    /// The parts of `docker inspect`'s output the tests read, as Docker 29
    /// prints them (Go's RFC 3339 with nanoseconds and trailing zeros cut).
    const INSPECT: &str = r#"[
      {
        "Id": "0123abcd",
        "RestartCount": 2,
        "State": {
          "Status": "running",
          "Running": true,
          "OOMKilled": false,
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
        let state = ContainerState::from_inspect(INSPECT).unwrap();

        assert_eq!(
            state,
            ContainerState {
                running: true,
                started_at: at("2026-10-10T08:00:15.123456789Z"),
                exit_code: 0,
                restart_count: 2,
                oom_killed: false,
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
        let json = r#"[{"RestartCount":0,"State":{"Running":false,"OOMKilled":true,"ExitCode":137,"StartedAt":"2026-10-10T08:00:15Z"}}]"#;

        let state = ContainerState::from_inspect(json).unwrap();

        assert!(!state.running);
        assert!(state.oom_killed);
        assert_eq!(state.exit_code, 137);
        assert_eq!(state.probes, []);
    }

    #[test]
    fn inspect_without_a_field_names_it() {
        let json =
            r#"[{"RestartCount":0,"State":{"ExitCode":0,"StartedAt":"2026-10-10T08:00:15Z"}}]"#;

        let error = ContainerState::from_inspect(json).unwrap_err();

        assert_eq!(error.to_string(), "docker inspect has no State.Running");
    }

    #[test]
    fn a_compose_command_names_its_project_env_file_and_files() {
        let compose = Compose::new(Path::new("deploy"), "afkfleet-smoke", &["a.yaml", "b.yaml"])
            .with_env_file(Path::new("smoke.env"));

        let command = compose.command(&["up", "--wait"]);

        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "compose",
                "-p",
                "afkfleet-smoke",
                "--env-file",
                "smoke.env",
                "--file",
                "a.yaml",
                "--file",
                "b.yaml",
                "up",
                "--wait"
            ]
        );
        assert_eq!(command.get_current_dir(), Some(Path::new("deploy")));
    }

    #[test]
    fn a_command_that_cant_start_is_an_error() {
        let command = Command::new("afkfleet-no-such-program");

        let error = finish(command, None, Duration::from_secs(5)).unwrap_err();

        assert!(error.to_string().contains("couldn't start"), "{error}");
    }
}
