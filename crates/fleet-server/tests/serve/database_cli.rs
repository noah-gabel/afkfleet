//! `afkfleet-server database check` as the binary (Plan.md P13.5, ADR-0033):
//! its exit codes, what goes to stdout and stderr, and a run beside a live
//! server.

use std::net::SocketAddr;
use std::path::Path;

use fleet_server::cli::database::{CHECK_EXIT_CODES, SOUND};

use crate::support::{
    closed_port, config_body, config_body_without_database, finish, finish_beside, path_arg,
    running_server, test_dir, write_config,
};

fn check(config: &Path) -> crate::support::Finished {
    finish(&["database", "check", "--config", path_arg(config)])
}

#[test]
fn database_check_s_help_lists_its_exit_codes() {
    let finished = finish(&["database", "check", "--help"]);

    assert_eq!(finished.status.code(), Some(0));
    assert!(
        finished.stdout.contains(CHECK_EXIT_CODES),
        "{}",
        finished.stdout
    );
}

#[test]
fn a_sound_database_is_exit_0_with_the_verdict_alone_on_stdout() {
    let dir = test_dir("database_check_sound");
    let config = write_config(&dir, &config_body(&dir, closed_port()));

    let finished = check(&config);

    assert_eq!(finished.status.code(), Some(0), "{}", finished.stderr);
    assert_eq!(finished.stdout, format!("{SOUND}\n"));
}

#[test]
fn a_damaged_database_is_exit_3_with_the_findings_on_stderr() {
    let dir = test_dir("database_check_damaged");
    let config = write_config(&dir, &config_body(&dir, closed_port()));
    let database = dir.join("afkfleet.db");
    let mut bytes = std::fs::read(&database).unwrap();
    bytes[4096 * 3..4096 * 7].fill(0xAA);
    std::fs::write(&database, bytes).unwrap();

    let finished = check(&config);

    assert_eq!(finished.status.code(), Some(3), "{}", finished.stderr);
    assert_eq!(finished.stdout, "");
    assert!(
        finished
            .stderr
            .contains("afkfleet-server: the database is damaged: SQLite's integrity check found "),
        "{}",
        finished.stderr
    );
    assert!(
        finished.stderr.contains("; restore a backup\n  "),
        "{}",
        finished.stderr
    );
}

#[test]
fn a_missing_database_is_exit_1_and_none_is_created() {
    let dir = test_dir("database_check_missing");
    let config = write_config(&dir, &config_body_without_database(&dir, closed_port()));

    let finished = check(&config);

    assert_eq!(finished.status.code(), Some(1), "{}", finished.stderr);
    assert_eq!(finished.stdout, "");
    assert!(
        finished
            .stderr
            .contains("afkfleet-server: the database can't be checked: "),
        "{}",
        finished.stderr
    );
    assert!(!dir.join("afkfleet.db").exists());
}

/// The check reads a database a running `serve` holds open, and the server
/// goes on answering.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn database_check_works_beside_a_running_server() {
    let dir = test_dir("database_check_beside_serve");
    let (port, _server) = running_server(&dir).await;
    let config = write_config(
        &dir,
        &config_body(&dir, SocketAddr::from(([127, 0, 0, 1], port))),
    );

    let finished = finish_beside(vec![
        "database".to_owned(),
        "check".to_owned(),
        "--config".to_owned(),
        path_arg(&config).to_owned(),
    ])
    .await;
    let health = finish_beside(vec![
        "healthcheck".to_owned(),
        "--config".to_owned(),
        path_arg(&config).to_owned(),
    ])
    .await;

    assert_eq!(finished.status.code(), Some(0), "{}", finished.stderr);
    assert_eq!(finished.stdout, format!("{SOUND}\n"));
    assert_eq!(health.status.code(), Some(0), "{}", health.stdout);
}
