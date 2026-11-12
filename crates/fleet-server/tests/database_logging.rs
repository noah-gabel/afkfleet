//! Tests for sqlx's statement logging as the server configures it: every
//! statement at `debug`, a slow one at `warn` (Plan.md P6.3, ADR-0015).
//!
//! sqlx logs on each connection's worker thread, so a subscriber scoped to
//! the test's thread wouldn't see the lines. The tests share one global
//! subscriber with two of fleet-startup's JSON layers, one under a `debug`
//! filter and one under the default `info` filter, and each test looks only
//! for its own statement.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::sync::OnceLock;

use fleet_server::config::{DatabaseConfig, LogConfig, LogFormat};
use fleet_server::infra::sqlite::{Database, DatabaseOptions, WRITE_CONNECTION_REPLACED, open};
use fleet_server::ports::store::Store as _;
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::log_buffer::LogBuffer;
use tempfile::TempDir;
use tracing_subscriber::layer::SubscriberExt;

/// What the `debug` layer and the default `info` layer printed.
struct Buffers {
    debug: LogBuffer,
    info: LogBuffer,
}

fn buffers() -> &'static Buffers {
    static BUFFERS: OnceLock<Buffers> = OnceLock::new();
    BUFFERS.get_or_init(|| {
        let debug = LogBuffer::default();
        let info = LogBuffer::default();
        let debug_config = LogConfig {
            format: LogFormat::Json,
            filter: "debug".parse().unwrap(),
        };
        let subscriber = tracing_subscriber::registry()
            .with(layer_with(&debug_config, &NoRules, false, debug.clone()))
            .with(layer_with(
                &LogConfig::default(),
                &NoRules,
                false,
                info.clone(),
            ));
        tracing::subscriber::set_global_default(subscriber).unwrap();
        Buffers { debug, info }
    })
}

/// The level and target of every line of `buffer` that mentions `needle`
/// anywhere.
fn levels_of(buffer: &LogBuffer, needle: &str) -> Vec<(String, String)> {
    buffer
        .json_lines()
        .unwrap()
        .into_iter()
        .filter(|line| line.to_string().contains(needle))
        .map(|line| {
            (
                line["level"].as_str().unwrap().to_owned(),
                line["target"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

async fn open_with(dir: &TempDir, options: DatabaseOptions) -> Database {
    let config = DatabaseConfig {
        path: dir.path().join("afkfleet.db"),
        backups: None,
    };
    open(&config, options).await.unwrap()
}

#[tokio::test]
async fn every_statement_is_logged_at_debug() {
    let buffers = buffers();
    let dir = TempDir::new().unwrap();
    let db = open_with(&dir, DatabaseOptions::default()).await;

    sqlx::query!("CREATE TABLE debug_probe (x INTEGER)")
        .execute(db.write_pool())
        .await
        .unwrap();
    db.close().await;

    let levels = levels_of(&buffers.debug, "debug_probe");
    assert!(!levels.is_empty(), "{}", buffers.debug.text());
    for (level, target) in &levels {
        assert_eq!((level.as_str(), target.as_str()), ("DEBUG", "sqlx::query"));
    }
}

#[tokio::test]
async fn statements_are_hidden_by_the_default_info_filter() {
    let buffers = buffers();
    let dir = TempDir::new().unwrap();
    let db = open_with(&dir, DatabaseOptions::default()).await;

    sqlx::query!("CREATE TABLE info_probe (x INTEGER)")
        .execute(db.write_pool())
        .await
        .unwrap();
    db.close().await;

    assert_ne!(levels_of(&buffers.debug, "info_probe"), Vec::new());
    assert_eq!(levels_of(&buffers.info, "info_probe"), Vec::new());
}

#[tokio::test]
async fn a_slow_statement_is_logged_at_warn() {
    let buffers = buffers();
    let dir = TempDir::new().unwrap();
    // A zero threshold makes every statement slow.
    let db = open_with(
        &dir,
        DatabaseOptions::default().with_slow_statement(Duration::ZERO),
    )
    .await;

    sqlx::query!("CREATE TABLE slow_probe (x INTEGER)")
        .execute(db.write_pool())
        .await
        .unwrap();
    db.close().await;

    let levels = levels_of(&buffers.info, "slow_probe");
    assert!(!levels.is_empty(), "{}", buffers.info.text());
    for (level, target) in &levels {
        assert_eq!((level.as_str(), target.as_str()), ("WARN", "sqlx::query"));
    }
}

/// A write connection whose transaction SQLite ended by itself is replaced
/// (P13.5, ADR-0033), and the server says so once, at `warn`: a fault it
/// recovered from. SQLite's commit hook refuses one commit here, which fails
/// that `COMMIT` and rolls its transaction back, as a full disk does.
#[tokio::test]
async fn a_replaced_write_connection_is_one_warning() {
    let logs = buffers();
    let dir = TempDir::new().unwrap();
    let db = open_with(&dir, DatabaseOptions::default()).await;
    {
        let mut connection = db.write_pool().acquire().await.unwrap();
        let mut handle = connection.lock_handle().await.unwrap();
        let mut refused = false;
        handle.set_commit_hook(move || core::mem::replace(&mut refused, true));
    }

    let refused = db.retention_batch().await.unwrap().commit().await;
    // The next write gets the new connection, once the old one is settled.
    let next = db.retention_batch().await.unwrap().commit().await;

    assert!(refused.is_err(), "{refused:?}");
    assert!(next.is_ok(), "{next:?}");
    assert_eq!(
        levels_of(&logs.info, WRITE_CONNECTION_REPLACED),
        [(
            "WARN".to_owned(),
            "fleet_server::infra::sqlite::commits".to_owned()
        )]
    );
    db.close().await;
}
