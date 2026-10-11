//! The `afkfleet-server` binary: its command line, the errors it prints
//! before logging exists, its exit codes, `migrate` and `healthcheck`
//! (Plan.md P6.11, ADR-0015).
//!
//! Each test starts the real binary with its own deadline, and a guard kills
//! it if the test ends first. Threads drain its pipes. Configs and databases
//! live in a directory of their own per test, under Cargo's temp directory
//! for tests.

use std::path::Path;

use fleet_server::cli::serve;
use fleet_server::cli::{HEALTHCHECK_EXIT_CODES, MIGRATE_EXIT_CODES, SERVE_EXIT_CODES};
use rstest::rstest;

use crate::support::{
    AbortOnDrop, ChannelSignals, closed_port, config_body, finish, finish_beside, json_lines,
    lines_with, listener, path_arg, server_config, test_dir, toml_path, vault_keys, vault_section,
    write_config,
};

#[rstest]
#[case::serve("serve")]
#[case::migrate("migrate")]
#[case::healthcheck("healthcheck")]
fn a_missing_config_exits_1_with_one_plain_line_on_stderr(#[case] command: &str) {
    let missing = test_dir(&format!("missing_config_{command}")).join("missing.toml");

    let finished = finish(&[command, "--config", path_arg(&missing)]);

    assert_eq!(finished.status.code(), Some(1));
    assert_eq!(finished.stdout, "");
    assert_eq!(
        finished.stderr,
        format!("afkfleet-server: no config file at {}\n", missing.display())
    );
}

#[test]
fn an_invalid_config_lists_every_problem_on_stderr_and_exits_1() {
    let dir = test_dir("invalid_config");
    let config = write_config(
        &dir,
        &format!(
            "[http]\nbind = \"127.0.0.1:0\"\n[database]\npath = \"relative.db\"\n{}",
            vault_section(&dir)
        ),
    );

    let finished = finish(&["serve", "--config", path_arg(&config)]);

    assert_eq!(finished.status.code(), Some(1));
    assert_eq!(finished.stdout, "");
    let lines: Vec<&str> = finished.stderr.lines().collect();
    assert_eq!(lines.len(), 3, "{}", finished.stderr);
    assert_eq!(lines[0], "afkfleet-server: the config is invalid:");
    assert!(
        lines[1].starts_with("  - database.path: "),
        "{}",
        finished.stderr
    );
    assert!(
        lines[2].starts_with("  - http.bind: "),
        "{}",
        finished.stderr
    );
}

#[rstest]
#[case::serve("serve", 2)]
#[case::migrate("migrate", 2)]
#[case::healthcheck("healthcheck", 1)]
fn a_command_without_a_config_is_a_usage_error(#[case] command: &str, #[case] code: i32) {
    let finished = finish(&[command]);

    assert_eq!(finished.status.code(), Some(code));
    assert_eq!(finished.stdout, "");
    assert!(finished.stderr.contains("--config"), "{}", finished.stderr);
}

#[test]
fn an_unknown_command_is_a_usage_error_with_exit_2() {
    let finished = finish(&["sleep"]);

    assert_eq!(finished.status.code(), Some(2));
    assert_eq!(finished.stdout, "");
}

#[rstest]
#[case::serve("serve", SERVE_EXIT_CODES)]
#[case::migrate("migrate", MIGRATE_EXIT_CODES)]
#[case::healthcheck("healthcheck", HEALTHCHECK_EXIT_CODES)]
fn each_commands_help_lists_its_exit_codes(#[case] command: &str, #[case] codes: &str) {
    let finished = finish(&[command, "--help"]);

    assert_eq!(finished.status.code(), Some(0));
    assert!(finished.stdout.contains(codes), "{}", finished.stdout);
}

#[test]
fn the_version_is_the_crates() {
    let finished = finish(&["--version"]);

    assert_eq!(finished.status.code(), Some(0));
    assert_eq!(
        finished.stdout.trim_end(),
        concat!("afkfleet-server ", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn migrate_creates_the_database_logs_and_exits_0_every_time() {
    let dir = test_dir("migrate");
    let config = write_config(&dir, &config_body(&dir, closed_port()));
    let database = dir.join("afkfleet.db");

    let first = finish(&["migrate", "--config", path_arg(&config)]);
    let second = finish(&["migrate", "--config", path_arg(&config)]);

    for finished in [&first, &second] {
        assert_eq!(finished.status.code(), Some(0), "{}", finished.stdout);
        assert_eq!(finished.stderr, "");
        let lines = json_lines(&finished.stdout);
        let migrated = lines_with(&lines, "the database is migrated");
        assert_eq!(migrated.len(), 1, "{}", finished.stdout);
        assert_eq!(migrated[0]["level"], "INFO");
        assert_eq!(migrated[0]["path"], database.display().to_string());
    }
    assert!(database.is_file());
}

#[test]
fn migrate_into_a_missing_folder_logs_the_error_and_exits_1() {
    let dir = test_dir("migrate_missing_folder");
    let database = dir.join("no-such-folder").join("afkfleet.db");
    let config = write_config(
        &dir,
        &format!(
            "[database]\npath = {}\n[log]\nformat = \"json\"\n{}",
            toml_path(&database),
            vault_section(&dir)
        ),
    );

    let finished = finish(&["migrate", "--config", path_arg(&config)]);

    assert_eq!(finished.status.code(), Some(1));
    assert_eq!(finished.stderr, "");
    let lines = json_lines(&finished.stdout);
    let failed = lines_with(&lines, "the database can't be migrated");
    assert_eq!(failed.len(), 1, "{}", finished.stdout);
    assert_eq!(failed[0]["level"], "ERROR");
    assert!(
        failed[0]["error"]
            .as_str()
            .unwrap()
            .contains("doesn't exist"),
        "{}",
        finished.stdout
    );
    assert!(!database.parent().unwrap().exists());
}

/// The vault's keys are read before anything else starts, so a bad key file
/// stops `serve` with exit 1 and an error naming the file, never its content
/// (ADR-0016).
#[test]
fn serve_with_a_bad_vault_key_file_logs_the_file_and_exits_1() {
    let dir = test_dir("serve_bad_vault_key");
    let config = write_config(&dir, &config_body(&dir, closed_port()));
    let key = dir.join("vault-key-1");
    std::fs::write(&key, "not-a-key-marker\n").unwrap();

    let finished = finish(&["serve", "--config", path_arg(&config)]);

    assert_eq!(finished.status.code(), Some(1), "{}", finished.stdout);
    assert_eq!(finished.stderr, "");
    let lines = json_lines(&finished.stdout);
    let failed = lines_with(&lines, "the server can't start");
    assert_eq!(failed.len(), 1, "{}", finished.stdout);
    let error = failed[0]["error"].as_str().unwrap();
    assert!(
        error.starts_with("the vault's keys can't be loaded: vault key 1: the file "),
        "{error}"
    );
    assert!(error.contains("vault-key-1"), "{error}");
    assert!(
        !finished.stdout.contains("not-a-key-marker"),
        "{}",
        finished.stdout
    );
    assert!(
        lines_with(&lines, "the server is listening").is_empty(),
        "{}",
        finished.stdout
    );
    assert!(!dir.join("afkfleet.db").exists(), "nothing was opened");
}

#[test]
fn a_healthcheck_with_nothing_listening_is_unhealthy_with_exit_1() {
    let dir = test_dir("healthcheck_unreachable");
    let config = write_config(&dir, &config_body(&dir, closed_port()));

    let finished = finish(&["healthcheck", "--config", path_arg(&config)]);

    assert_eq!(finished.status.code(), Some(1));
    assert_eq!(finished.stderr, "");
    assert_eq!(finished.stdout.lines().count(), 1, "{}", finished.stdout);
    assert!(
        finished.stdout.starts_with("unhealthy: "),
        "{}",
        finished.stdout
    );
}

/// Starts the server in-process on a free loopback port with its database
/// in `dir`, and returns its port and the task, aborted when dropped.
async fn running_server(dir: &Path) -> (u16, AbortOnDrop<serve::Exit>) {
    let (listener, address) = listener().await;
    let config = server_config(dir, address, false);
    let keys = vault_keys(&config);
    let task =
        tokio::spawn(
            async move { serve::run(&config, listener, ChannelSignals::silent(), keys).await },
        );
    (address.port(), AbortOnDrop(task))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_healthcheck_against_a_ready_server_is_healthy_with_exit_0() {
    let dir = test_dir("healthcheck_healthy");
    let (port, _server) = running_server(&dir).await;
    let config = write_config(
        &dir,
        &config_body(&dir, format!("127.0.0.1:{port}").parse().unwrap()),
    );

    let finished = finish_beside(vec![
        "healthcheck".to_owned(),
        "--config".to_owned(),
        path_arg(&config).to_owned(),
    ])
    .await;

    assert_eq!(finished.status.code(), Some(0), "{}", finished.stdout);
    assert_eq!(finished.stderr, "");
    assert_eq!(
        finished.stdout,
        format!("healthy: /health/ready on 127.0.0.1:{port} answered 200\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_healthcheck_of_an_unspecified_bind_asks_loopback_on_its_port() {
    let dir = test_dir("healthcheck_unspecified");
    let (port, _server) = running_server(&dir).await;
    let config = write_config(
        &dir,
        &config_body(&dir, format!("0.0.0.0:{port}").parse().unwrap()),
    );

    let finished = finish_beside(vec![
        "healthcheck".to_owned(),
        "--config".to_owned(),
        path_arg(&config).to_owned(),
    ])
    .await;

    assert_eq!(finished.status.code(), Some(0), "{}", finished.stdout);
    assert_eq!(
        finished.stdout,
        format!("healthy: /health/ready on 127.0.0.1:{port} answered 200\n")
    );
}

#[cfg(unix)]
mod sigterm {
    use core::time::Duration;
    use std::io::{BufRead, BufReader, Read};
    use std::process::Command;
    use std::sync::mpsc;
    use std::thread;

    use fleet_server::cli::healthcheck;
    use serde_json::Value;
    use tokio::time::Instant;

    use super::*;
    use crate::support::{DEADLINE, Running, drain, server};

    /// How many free ports the test tries: another process can take the one
    /// it found before the server binds it.
    const ATTEMPTS: usize = 3;

    /// The dev-mode warning, word for word.
    const DEV_MODE_WARNING: &str = "dev mode is on: development-only features are enabled; never run a production server in dev mode";

    /// Sends each line of `pipe` on a thread, then `None` once it closes.
    fn lines_of(pipe: impl Read + Send + 'static) -> mpsc::Receiver<Option<String>> {
        let (sender, receiver) = mpsc::sync_channel(256);
        thread::spawn(move || {
            for line in BufReader::new(pipe).lines() {
                let Ok(line) = line else { break };
                if sender.send(Some(line)).is_err() {
                    return;
                }
            }
            let _ = sender.send(None);
        });
        receiver
    }

    /// A server that logged "the server is listening".
    struct Listening {
        running: Running,
        lines: mpsc::Receiver<Option<String>>,
        stderr: mpsc::Receiver<String>,
        seen: Vec<String>,
        port: u16,
    }

    /// Starts `serve` in dev mode on a port that was free a moment ago.
    /// `None` if the server exited 1 because that port was taken by then;
    /// any other early end fails the test.
    fn start(attempt: usize) -> Option<Listening> {
        let port = closed_port().port();
        let dir = test_dir(&format!("sigterm_{attempt}"));
        let address = format!("127.0.0.1:{port}").parse().unwrap();
        let config = write_config(
            &dir,
            &format!("dev_mode = true\n{}", config_body(&dir, address)),
        );
        let mut child = server(&["serve", "--config", path_arg(&config)])
            .spawn()
            .unwrap();
        let lines = lines_of(child.stdout.take().unwrap());
        let stderr = drain(child.stderr.take().unwrap());
        let mut running = Running(Some(child));
        let mut seen = Vec::new();
        loop {
            let line = lines
                .recv_timeout(DEADLINE)
                .expect("the server should log before the deadline");
            let Some(line) = line else { break };
            let listening = line.contains("\"the server is listening\"");
            seen.push(line);
            if listening {
                return Some(Listening {
                    running,
                    lines,
                    stderr,
                    seen,
                    port,
                });
            }
        }
        // The output ended before the server listened.
        let status = running.0.take().unwrap().wait().unwrap();
        let stderr = stderr.recv_timeout(DEADLINE).unwrap();
        let taken = status.code() == Some(1)
            && seen.iter().any(|line| {
                let line: Value = serde_json::from_str(line).unwrap_or_default();
                let error = line["error"].as_str().unwrap_or_default();
                line["message"] == "the server can't start"
                    && error.starts_with(&format!("can't listen on {address}: "))
                    && error.contains("Address already in use")
            });
        assert!(
            taken,
            "the server ended before listening ({status}): {seen:#?}\nstderr: {stderr}"
        );
        None
    }

    #[test]
    fn sigterm_stops_the_server_with_exit_0_and_logs_the_stop_last() {
        let Listening {
            mut running,
            lines,
            stderr,
            mut seen,
            port,
        } = (1..=ATTEMPTS)
            .find_map(start)
            .expect("a free port within the attempts");
        let address = format!("127.0.0.1:{port}").parse().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let ready = runtime
            .block_on(healthcheck::get(
                address,
                healthcheck::PATH,
                Duration::from_secs(10),
            ))
            .unwrap();
        assert_eq!(ready.status, 200);

        let pid = running.0.as_ref().unwrap().id().to_string();
        let killed = Command::new("kill").args(["-TERM", &pid]).status().unwrap();
        assert!(killed.success());
        let deadline = Instant::now() + DEADLINE;
        while let Some(line) = lines
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("the server should stop before the deadline")
        {
            seen.push(line);
        }
        let status = running.0.take().unwrap().wait().unwrap();

        let stderr = stderr.recv_timeout(DEADLINE).unwrap();
        assert_eq!(status.code(), Some(0), "stderr: {stderr}");
        let lines: Vec<Value> = seen
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let last = lines.last().unwrap();
        assert_eq!(last["message"], "the server stopped", "{seen:#?}");
        assert_eq!(last["exit_code"], 0);
        let position = |message: &str| {
            lines
                .iter()
                .position(|line| line["message"] == message)
                .unwrap_or_else(|| panic!("no line {message:?}: {seen:#?}"))
        };
        assert!(position(DEV_MODE_WARNING) < position("the server is listening"));
        let shutting_down = &lines[position("shutting down")];
        assert_eq!(shutting_down["signal"], "SIGTERM");
        assert_eq!(lines[position(DEV_MODE_WARNING)]["level"], "WARN");
    }
}
