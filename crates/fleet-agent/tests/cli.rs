//! The `afkfleet-agent` binary: its command line, the errors it prints
//! before logging exists, and its exit codes (Plan.md P5.3; ADR-0014).
//!
//! Each test starts the real binary with its own deadline, and a guard kills
//! the agent if the test ends first, so a hang can't stall the run. Threads
//! drain the agent's pipes, so a full pipe can't block it. Configs live in a
//! directory of their own per test, under Cargo's temp directory for tests.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fleet_agent::cli::RUN_EXIT_CODES;

const BINARY: &str = env!("CARGO_BIN_EXE_afkfleet-agent");

/// How long any one agent may run.
const DEADLINE: Duration = Duration::from_secs(30);

/// Kills the agent unless the test took it out first.
struct Running(Option<Child>);

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The agent with `args`, its pipes captured, and none of the test's own
/// `AFKFLEET_AGENT__…` variables.
fn agent(args: &[&str]) -> Command {
    let mut command = Command::new(BINARY);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("AFKFLEET_AGENT__") {
            command.env_remove(key);
        }
    }
    command
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

/// How a finished agent ended.
struct Finished {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

/// Runs the agent with `args` until it exits, within the deadline.
fn finish(args: &[&str]) -> Finished {
    let mut child = agent(args).spawn().unwrap();
    let stdout = drain(child.stdout.take().unwrap());
    let stderr = drain(child.stderr.take().unwrap());
    let mut running = Running(Some(child));
    let stdout = stdout
        .recv_timeout(DEADLINE)
        .expect("the agent should exit before the deadline");
    let stderr = stderr.recv_timeout(DEADLINE).unwrap();
    let status = running.0.take().unwrap().wait().unwrap();
    Finished {
        status,
        stdout,
        stderr,
    }
}

/// An empty directory of its own for the test `name`.
fn test_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("cli")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes `body` as `agent.toml` in `dir`, with an absolute heartbeat file
/// there too, and returns the config's path.
fn write_config(dir: &Path, body: &str) -> PathBuf {
    let heartbeat = dir.join("agent.alive");
    let config = format!(
        "{body}\n[runtime]\nheartbeat_file = '{}'\n",
        heartbeat.display()
    );
    let path = dir.join("agent.toml");
    fs::write(&path, config).unwrap();
    path
}

fn path_arg(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn a_missing_config_exits_1_with_one_plain_line_on_stderr() {
    let missing = test_dir("missing_config").join("missing.toml");

    let finished = finish(&["run", "--config", path_arg(&missing)]);

    assert_eq!(finished.status.code(), Some(1));
    assert_eq!(finished.stdout, "");
    assert_eq!(
        finished.stderr,
        format!("afkfleet-agent: no config file at {}\n", missing.display())
    );
}

#[test]
fn an_invalid_config_lists_every_problem_on_stderr_and_exits_1() {
    let dir = test_dir("invalid_config");
    let config = write_config(
        &dir,
        "name = \".hidden\"\n\
         [[standalone.bots]]\n\
         username = \"AfkBot1\"\n\
         server = \"127.0.0.1:1\"\n\
         mode = \"sleep\"\n",
    );

    let finished = finish(&["run", "--config", path_arg(&config)]);

    assert_eq!(finished.status.code(), Some(1));
    assert_eq!(finished.stdout, "");
    let lines: Vec<&str> = finished.stderr.lines().collect();
    assert_eq!(lines.len(), 3, "{}", finished.stderr);
    assert_eq!(lines[0], "afkfleet-agent: the config is invalid:");
    assert!(lines[1].starts_with("  - name: "), "{}", finished.stderr);
    assert!(
        lines[2].starts_with("  - standalone.bots[0].mode: "),
        "{}",
        finished.stderr
    );
}

#[test]
fn a_run_without_a_config_is_a_usage_error_with_exit_2() {
    let finished = finish(&["run"]);

    assert_eq!(finished.status.code(), Some(2));
    assert_eq!(finished.stdout, "");
    assert!(finished.stderr.contains("--config"), "{}", finished.stderr);
}

#[test]
fn an_unknown_command_is_a_usage_error_with_exit_2() {
    let finished = finish(&["sleep"]);

    assert_eq!(finished.status.code(), Some(2));
    assert_eq!(finished.stdout, "");
}

#[test]
fn the_help_of_run_lists_the_exit_codes() {
    let finished = finish(&["run", "--help"]);

    assert_eq!(finished.status.code(), Some(0));
    assert!(
        finished.stdout.contains(RUN_EXIT_CODES),
        "{}",
        finished.stdout
    );
}

#[test]
fn the_version_is_the_crates() {
    let finished = finish(&["--version"]);

    assert_eq!(finished.status.code(), Some(0));
    assert_eq!(
        finished.stdout.trim_end(),
        concat!("afkfleet-agent ", env!("CARGO_PKG_VERSION"))
    );
}
