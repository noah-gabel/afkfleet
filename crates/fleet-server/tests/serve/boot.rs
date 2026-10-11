//! `serve::run` in-process (Plan.md P6.11, ADR-0015): the wiring `serve`
//! runs, on a listener the test binds itself. Real time, since the database
//! runs on sqlx's worker threads (CLAUDE.md).

use fleet_server::cli::serve::{self, Exit};

use crate::support::{ChannelSignals, capture, listener, server_config, test_dir};

#[tokio::test]
async fn a_missing_database_folder_fails_the_start_before_listening() {
    let dir = test_dir("boot_missing_folder");
    let (listener, address) = listener().await;
    let mut config = server_config(&dir, address, false);
    config.database.path = dir.join("no-such-folder").join("afkfleet.db");
    let (logs, _guard) = capture();

    let exit = serve::run(&config, listener, ChannelSignals::silent()).await;

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
