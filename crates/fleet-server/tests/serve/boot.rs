//! `serve::run` in-process (Plan.md P6.11, ADR-0015): the wiring `serve`
//! runs, on a listener the test binds itself. Real time, since the database
//! runs on sqlx's worker threads (CLAUDE.md).

use fleet_server::cli::serve::{self, Exit};
use fleet_server::config::LogConfig;
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::log_buffer::LogBuffer;
use tracing_subscriber::layer::SubscriberExt as _;

use crate::support::{listener, server_config, test_dir};

/// The server's log layer at `info`, writing into a buffer, as the default
/// subscriber of this thread. The tests run on tokio's current-thread
/// runtime, so everything `run` logs goes there.
fn capture() -> (LogBuffer, tracing::subscriber::DefaultGuard) {
    let buffer = LogBuffer::default();
    let config = LogConfig {
        format: fleet_server::config::LogFormat::Json,
        filter: "info".parse().unwrap(),
    };
    let subscriber =
        tracing_subscriber::registry().with(layer_with(&config, &NoRules, false, buffer.clone()));
    let guard = tracing::subscriber::set_default(subscriber);
    (buffer, guard)
}

#[tokio::test]
async fn a_missing_database_folder_fails_the_start_before_listening() {
    let dir = test_dir("boot_missing_folder");
    let (listener, address) = listener().await;
    let mut config = server_config(&dir, address, false);
    config.database.path = dir.join("no-such-folder").join("afkfleet.db");
    let (logs, _guard) = capture();

    let exit = serve::run(&config, listener).await;

    assert_eq!(exit, Exit::StartupFailed);
    assert_eq!(exit.code(), 1);
    let lines = logs.json_lines().unwrap();
    let failed = logs.lines_with("the server can't start").unwrap();
    assert_eq!(failed.len(), 1, "{}", logs.text());
    assert_eq!(failed[0]["level"], "ERROR");
    let error = failed[0]["error"].as_str().unwrap();
    assert!(
        error.starts_with("the database can't be opened: the database's folder "),
        "{error}"
    );
    assert!(
        lines
            .iter()
            .all(|line| line["message"] != "the server is listening"),
        "{}",
        logs.text()
    );
}
