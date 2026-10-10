//! The SQLite database: opening it, its two pools and its migrations
//! (Plan.md P6.3, ADR-0005, ADR-0015).
//!
//! # Opening
//! [`open`] is what the server calls at startup (and the `migrate` command,
//! P6.11): it prepares the file, then [`Database::connect`] builds both pools
//! and runs every pending migration before anything else touches the
//! database.
//! - **The folder must exist.** A typo in `[database] path` or a missing
//!   volume stops startup instead of creating a fresh, empty database
//!   somewhere unexpected.
//! - **[`open`] creates a missing file itself**, `0600` on Unix, and the pools
//!   never create one, so sqlx never makes a database file with default
//!   permissions. SQLite gives the `-wal` and `-shm` files the database's
//!   mode.
//! - **On Unix, a database, `-wal` or `-shm` file that other users can read or
//!   write stops startup** (the Phase 6 security list: only the service user
//!   reads the database). On Windows, where permissions are ACLs, there's no
//!   such check.
//!
//! # Connections
//! [`connect_options`] owns every per-connection setting, for the server and
//! the tests alike: `foreign_keys=ON`, `busy_timeout=5s`, `synchronous=NORMAL`
//! and `trusted_schema=OFF` (functions with side effects can't run from the
//! schema, which guards against a tampered file).
//! - **The write pool has exactly one connection**, in WAL mode, so writes are
//!   serialized here instead of failing with `SQLITE_BUSY`.
//! - **The read pool has four**, each with `query_only=ON`, so a write through
//!   a read connection fails instead of bypassing the single writer.
//!
//! # Statement logging
//! Set explicitly, since sqlx has logged every statement at `info` in some
//! versions: every statement at `debug`, a slow one (250 ms or more) at
//! `warn`. sqlx logs the SQL text with its `?` placeholders, never the bound
//! values.
//!
//! # Timeouts
//! Database awaits carry no timeout of their own (CLAUDE.md allows a comment
//! instead). Waiting for a connection is bounded by the pools' 5 s acquire
//! timeout, waiting for SQLite's locks by `busy_timeout`, statements are
//! short and local, and P6.6's request timeout bounds every request. Dropping
//! a sqlx future wouldn't stop SQLite's worker thread anyway: the statement
//! could still finish after a timeout. Migrations at startup are unbounded on
//! purpose: they must finish or fail, and a half-applied migration would be
//! worse. [`Database::close`] waits for every checked-out connection, so its
//! caller bounds it (group E's shutdown).

mod connect;
mod file;

use std::path::PathBuf;

use sqlx::migrate::{MigrateError, Migrator};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

pub use self::connect::{
    BUSY_TIMEOUT, DatabaseOptions, PoolRole, READ_CONNECTIONS, connect_options,
};
use crate::config::DatabaseConfig;

/// Every migration under `crates/fleet-server/migrations/`, embedded in the
/// binary. A merged migration is never edited: sqlx records each one's
/// checksum and refuses a database whose migration changed (ADR-0015).
pub static MIGRATOR: Migrator = sqlx::migrate!();

/// Why the database couldn't be opened.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// The folder that should hold the database file doesn't exist.
    #[error("the database's folder {} doesn't exist", folder.display())]
    MissingFolder {
        /// The missing folder.
        folder: PathBuf,
    },
    /// The missing database file couldn't be created.
    #[error("couldn't create the database file {}", file.display())]
    Create {
        /// The file.
        file: PathBuf,
        /// What the OS reported.
        #[source]
        source: std::io::Error,
    },
    /// A file's or folder's metadata couldn't be read.
    #[error("couldn't read the metadata of {}", file.display())]
    Inspect {
        /// The file or folder.
        file: PathBuf,
        /// What the OS reported.
        #[source]
        source: std::io::Error,
    },
    /// On Unix: the database or a file next to it can be read or written by
    /// other users.
    #[error(
        "{} can be read or written by other users (mode {mode:03o}); run `chmod 600` on it. On Docker Desktop for Windows, a bind-mounted Windows folder always shows mode 777 and chmod can't change it: keep the database on a named volume instead",
        file.display()
    )]
    Permissions {
        /// The file that is too open: the database, its `-wal` or its `-shm`.
        file: PathBuf,
        /// Its permission bits.
        mode: u32,
    },
    /// A pool couldn't connect.
    #[error("couldn't connect to the database")]
    Connect(#[source] sqlx::Error),
    /// A migration this database already applied was edited since.
    #[error(
        "migration {version} was edited after this database applied it; merged migrations are never edited"
    )]
    EditedMigration {
        /// The edited migration's version.
        version: i64,
    },
    /// The database has a migration this server doesn't know: a newer server
    /// migrated it.
    #[error(
        "the database has migration {version}, which this server doesn't know: a newer server migrated it"
    )]
    NewerSchema {
        /// The unknown migration's version.
        version: i64,
    },
    /// Running the migrations failed.
    #[error("couldn't migrate the database")]
    Migrate(#[source] MigrateError),
}

/// The open database: one write connection and a pool of read connections.
/// Clones share the pools.
#[derive(Debug, Clone)]
pub struct Database {
    write: SqlitePool,
    read: SqlitePool,
}

impl Database {
    /// Connects to the existing database file `base` names: builds the write
    /// pool, runs every pending migration through it, then builds the read
    /// pool, each through [`connect_options`]. [`open`] calls it after
    /// preparing the file; tests that get a database from `#[sqlx::test]`
    /// call it directly, so they run with the production settings.
    ///
    /// # Errors
    /// [`OpenError::Connect`] when a pool can't connect (also when the file
    /// doesn't exist: it's never created here), [`OpenError::EditedMigration`],
    /// [`OpenError::NewerSchema`] or [`OpenError::Migrate`] when the
    /// migrations fail.
    pub async fn connect(
        base: SqliteConnectOptions,
        options: DatabaseOptions,
    ) -> Result<Self, OpenError> {
        let write = SqlitePoolOptions::new()
            .max_connections(1)
            .acquire_timeout(options.acquire_timeout())
            .connect_with(connect_options(base.clone(), PoolRole::Write, &options))
            .await
            .map_err(OpenError::Connect)?;
        // Unbounded on purpose: a migration must finish or fail (module docs).
        MIGRATOR.run(&write).await.map_err(migrate_error)?;
        let read = SqlitePoolOptions::new()
            .max_connections(READ_CONNECTIONS)
            .acquire_timeout(options.acquire_timeout())
            .connect_with(connect_options(base, PoolRole::Read, &options))
            .await
            .map_err(OpenError::Connect)?;
        Ok(Self { write, read })
    }

    /// The write pool: exactly one connection.
    #[must_use]
    pub fn write_pool(&self) -> &SqlitePool {
        &self.write
    }

    /// The read pool: [`READ_CONNECTIONS`] connections that refuse writes.
    #[must_use]
    pub fn read_pool(&self) -> &SqlitePool {
        &self.read
    }

    /// Closes the read pool, then the write pool, waiting for checked-out
    /// connections to come back. The last connection to close checkpoints the
    /// WAL and removes the `-wal` and `-shm` files.
    pub async fn close(&self) {
        self.read.close().await;
        self.write.close().await;
    }
}

/// Opens the database at the configured path: prepares the file (see the
/// module docs), then [`Database::connect`]s with `options`.
///
/// # Errors
/// [`OpenError::MissingFolder`], [`OpenError::Create`],
/// [`OpenError::Inspect`] or, on Unix, [`OpenError::Permissions`] when the
/// file isn't ready, and every error of [`Database::connect`].
pub async fn open(
    config: &DatabaseConfig,
    options: DatabaseOptions,
) -> Result<Database, OpenError> {
    file::prepare(&config.path).await?;
    Database::connect(SqliteConnectOptions::new().filename(&config.path), options).await
}

/// Maps sqlx's migration errors, giving the two an operator must understand
/// their own variants.
fn migrate_error(error: MigrateError) -> OpenError {
    match error {
        MigrateError::VersionMismatch(version) => OpenError::EditedMigration { version },
        MigrateError::VersionMissing(version) => OpenError::NewerSchema { version },
        other => OpenError::Migrate(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edited_migration_has_its_own_error() {
        assert!(matches!(
            migrate_error(MigrateError::VersionMismatch(3)),
            OpenError::EditedMigration { version: 3 }
        ));
    }

    #[test]
    fn a_migration_this_server_doesnt_know_means_a_newer_schema() {
        assert!(matches!(
            migrate_error(MigrateError::VersionMissing(9)),
            OpenError::NewerSchema { version: 9 }
        ));
    }

    #[test]
    fn other_migration_errors_keep_sqlxs_error() {
        assert!(matches!(
            migrate_error(MigrateError::Dirty(2)),
            OpenError::Migrate(MigrateError::Dirty(2))
        ));
    }

    #[test]
    fn the_permission_error_names_the_file_and_the_fix() {
        let error = OpenError::Permissions {
            file: PathBuf::from("/data/afkfleet.db-wal"),
            mode: 0o644,
        };

        let message = error.to_string();

        assert!(
            message.starts_with(
                "/data/afkfleet.db-wal can be read or written by other users (mode 644)"
            ),
            "{message}"
        );
        assert!(message.contains("chmod 600"), "{message}");
        assert!(message.contains("named volume"), "{message}");
    }
}
