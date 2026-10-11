//! `afkfleet-server migrate` (Plan.md P6.11, ADR-0005, ADR-0015): applies
//! every pending migration and exits.
//!
//! It opens the database the way `serve` does, through
//! [`sqlite::open`]: the folder must exist, a
//! missing file is created with owner-only permissions, and every pending
//! migration runs. Then it closes the database within
//! [`DATABASE_CLOSE`]. `serve` migrates at startup
//! too; this command lets an operator migrate before starting a new version.
//!
//! It logs like `serve`: "the database is migrated" at `info`, or one
//! `error`, "the database can't be migrated", with the error's whole chain.

use core::fmt::Display;
use core::time::Duration;

use tracing::{error, info};

use super::DATABASE_CLOSE;
use super::serve::StartError;
use crate::config::DatabaseConfig;
use crate::http::error::chain;
use crate::infra::sqlite::{self, Database, DatabaseOptions};

/// How the migration ended, which decides the process's exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// Every migration is applied and the database closed.
    Migrated,
    /// Something failed; the log says what.
    Failed,
}

impl Exit {
    /// The process's exit code (Plan.md P6.11, ADR-0015).
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Migrated => 0,
            Self::Failed => 1,
        }
    }
}

/// Logs that the migration couldn't run because of `error`, such as the
/// async runtime, and returns [`Exit::Failed`].
pub fn cant_migrate(error: &StartError) -> Exit {
    error!(error = %chain(error), "the database can't be migrated");
    Exit::Failed
}

/// Opens the database at `config`'s path, which applies every pending
/// migration, and closes it.
pub async fn run(config: &DatabaseConfig) -> Exit {
    let path = config.path.display();
    let database = match sqlite::open(config, DatabaseOptions::default()).await {
        Ok(database) => database,
        Err(error) => {
            error!(%path, error = %chain(&error), "the database can't be migrated");
            return Exit::Failed;
        }
    };
    close(&database, &path, DATABASE_CLOSE).await
}

/// Closes the migrated `database` at `path` within `limit`, and logs the
/// result.
async fn close(database: &Database, path: &dyn Display, limit: Duration) -> Exit {
    match database.close_within(limit).await {
        Ok(()) => {
            info!(%path, "the database is migrated");
            Exit::Migrated
        }
        Err(error) => {
            error!(%path, %error, "the database can't be migrated");
            Exit::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use fleet_startup::config::{LogConfig, LogFormat};
    use fleet_startup::telemetry::{NoRules, layer_with};
    use fleet_testkit::log_buffer::LogBuffer;
    use tempfile::TempDir;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;

    /// The server's log layer at `info`, writing into a buffer, as this
    /// thread's default subscriber.
    fn capture() -> (LogBuffer, tracing::subscriber::DefaultGuard) {
        let buffer = LogBuffer::default();
        let config = LogConfig {
            format: LogFormat::Json,
            filter: "info".parse().unwrap(),
        };
        let subscriber = tracing_subscriber::registry().with(layer_with(
            &config,
            &NoRules,
            false,
            buffer.clone(),
        ));
        (buffer, tracing::subscriber::set_default(subscriber))
    }

    #[test]
    fn each_exit_has_its_code() {
        assert_eq!(Exit::Migrated.code(), 0);
        assert_eq!(Exit::Failed.code(), 1);
    }

    #[test]
    fn a_migration_that_cant_run_logs_why_and_fails() {
        let (logs, _guard) = capture();

        let exit = cant_migrate(&StartError::Runtime(io::Error::other("no threads")));

        assert_eq!(exit, Exit::Failed);
        let lines = logs.lines_with("the database can't be migrated").unwrap();
        assert_eq!(lines.len(), 1, "{}", logs.text());
        assert_eq!(lines[0]["level"], "ERROR");
        assert_eq!(
            lines[0]["error"],
            "the async runtime can't be built: no threads"
        );
    }

    // Real time: the database runs on sqlx's worker threads (CLAUDE.md).
    #[tokio::test]
    async fn a_database_that_doesnt_close_in_time_fails_the_migration() {
        let dir = TempDir::new().unwrap();
        let config = DatabaseConfig {
            path: dir.path().join("afkfleet.db"),
        };
        let database = sqlite::open(&config, DatabaseOptions::default())
            .await
            .unwrap();
        let held = database.read_pool().acquire().await.unwrap();
        let (logs, _guard) = capture();

        let exit = close(
            &database,
            &config.path.display(),
            Duration::from_millis(300),
        )
        .await;

        assert_eq!(exit, Exit::Failed);
        let lines = logs.lines_with("the database can't be migrated").unwrap();
        assert_eq!(lines.len(), 1, "{}", logs.text());
        assert_eq!(lines[0]["error"], "the database didn't close within 300 ms");
        drop(held);
        database.close().await;
    }
}
