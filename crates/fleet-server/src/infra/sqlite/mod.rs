//! The SQLite database: opening it, its two pools and its migrations
//! (Plan.md P6.3, ADR-0005, ADR-0015).
//!
//! # Opening
//! [`open`] is what the server calls at startup (and the `migrate` command,
//! P6.11): it prepares the file, then [`Database::connect`] builds both pools
//! and runs every pending migration before anything else touches the
//! database. [`open_migrated`] is the vault commands' (P9.2): it opens only
//! an existing database whose migrations are all applied, and never creates
//! or migrates one.
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
//! callers bound it: `migrate` through [`Database::close_within`], `serve`
//! within its shutdown deadline.

mod accounts;
mod audit;
mod connect;
mod convert;
mod envelopes;
mod error;
mod file;
mod invites;
mod login_attempts;
mod mfa;
mod sessions;
mod store;
mod users;

use core::time::Duration;
use std::path::PathBuf;

use sqlx::migrate::{MigrateError, Migrator};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

pub use self::connect::{
    BUSY_TIMEOUT, DatabaseOptions, PoolRole, READ_CONNECTIONS, connect_options,
};
pub use self::store::SqliteWriteTx;
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
    /// Running the migrations failed, or ([`open_migrated`]) the database
    /// records a migration that failed halfway (`Dirty`).
    #[error("couldn't migrate the database")]
    Migrate(#[source] MigrateError),
    /// ([`open_migrated`]) The database file doesn't exist.
    #[error("the database {} doesn't exist, and this command never creates one", file.display())]
    MissingFile {
        /// The missing file.
        file: PathBuf,
    },
    /// ([`open_migrated`]) The database lacks migrations this server has.
    #[error(
        "the database has pending migrations (the first is {version}); run `afkfleet-server migrate` first"
    )]
    Pending {
        /// The first migration it lacks.
        version: i64,
    },
}

/// [`Database::close_within`] ran out of time: a connection was still
/// checked out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the database didn't close within {} ms", .limit.as_millis())]
pub struct CloseTimedOut {
    /// How long it waited.
    pub limit: Duration,
}

/// The open database: one write connection and a pool of read connections.
/// It's the server's [`Store`](crate::ports::store::Store). Clones share the
/// pools.
#[derive(Debug, Clone)]
pub struct Database {
    write: SqlitePool,
    read: SqlitePool,
    reads: store::Reads,
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
        let read = read_pool(base, &options).await?;
        Ok(Self::from_pools(write, read))
    }

    /// The database over its two pools.
    fn from_pools(write: SqlitePool, read: SqlitePool) -> Self {
        Self {
            write,
            reads: store::Reads { pool: read.clone() },
            read,
        }
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

    /// [`Database::close`], waiting at most `limit` for checked-out
    /// connections to come back. The pools refuse new connections either
    /// way.
    ///
    /// # Errors
    /// [`CloseTimedOut`] if a connection was still checked out at `limit`.
    pub async fn close_within(&self, limit: Duration) -> Result<(), CloseTimedOut> {
        tokio::time::timeout(limit, self.close())
            .await
            .map_err(|_elapsed| CloseTimedOut { limit })
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

/// Opens the existing database at the configured path without creating or
/// migrating anything, for `vault status` and `vault rotate-key` (P9.2): a
/// key rotation never changes the schema as a side effect.
///
/// The file must exist (with the folder and, on Unix, the permissions
/// [`open`] checks). The read pool checks the applied migrations against
/// [`MIGRATOR`] with plain SELECTs, refusing the database in sqlx's own
/// order: a migration that failed halfway, one this server doesn't know, an
/// edited one, then the first pending one. The write pool is lazy: it opens
/// its connection only at the first [`Store::write`](crate::ports::store::Store::write),
/// so a command that only reads never opens one.
///
/// # Errors
/// [`OpenError::MissingFolder`], [`OpenError::MissingFile`],
/// [`OpenError::Inspect`] or, on Unix, [`OpenError::Permissions`] when the
/// file isn't there or is open to others; [`OpenError::Connect`];
/// [`OpenError::Migrate`] (`Dirty`), [`OpenError::NewerSchema`],
/// [`OpenError::EditedMigration`] or [`OpenError::Pending`] when the
/// migrations don't match this server's.
pub async fn open_migrated(
    config: &DatabaseConfig,
    options: DatabaseOptions,
) -> Result<Database, OpenError> {
    file::check_existing(&config.path).await?;
    let base = SqliteConnectOptions::new().filename(&config.path);
    let read = read_pool(base.clone(), &options).await?;
    let checked = match applied_migrations(&read).await {
        Ok(applied) => schema_state(&applied, &MIGRATOR),
        Err(error) => Err(error),
    };
    if let Err(error) = checked {
        read.close().await;
        return Err(error);
    }
    let write = SqlitePoolOptions::new()
        .max_connections(1)
        .acquire_timeout(options.acquire_timeout())
        .connect_lazy_with(connect_options(base, PoolRole::Write, &options));
    Ok(Database::from_pools(write, read))
}

/// The read pool over `base`, through [`connect_options`].
async fn read_pool(
    base: SqliteConnectOptions,
    options: &DatabaseOptions,
) -> Result<SqlitePool, OpenError> {
    SqlitePoolOptions::new()
        .max_connections(READ_CONNECTIONS)
        .acquire_timeout(options.acquire_timeout())
        .connect_with(connect_options(base, PoolRole::Read, options))
        .await
        .map_err(OpenError::Connect)
}

/// The rows of `_sqlx_migrations`, read without writing: none when the table
/// doesn't exist yet, as in an empty file. sqlx's own `run` would create the
/// table first.
async fn applied_migrations(read: &SqlitePool) -> Result<Vec<Applied>, OpenError> {
    let tables = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!: i64" FROM sqlite_schema WHERE type = 'table' AND name = ?"#,
        "_sqlx_migrations"
    )
    .fetch_one(read)
    .await
    .map_err(OpenError::Connect)?;
    if tables == 0 {
        return Ok(Vec::new());
    }
    sqlx::query_as!(
        Applied,
        r#"SELECT version AS "version!: i64", checksum AS "checksum!: Vec<u8>",
                  success AS "success!: bool"
           FROM _sqlx_migrations ORDER BY version"#
    )
    .fetch_all(read)
    .await
    .map_err(OpenError::Connect)
}

/// A row of `_sqlx_migrations`, as [`schema_state`] needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Applied {
    version: i64,
    checksum: Vec<u8>,
    success: bool,
}

/// Whether the applied migrations `applied` are exactly `migrator`'s, in
/// sqlx's order of checks.
fn schema_state(applied: &[Applied], migrator: &Migrator) -> Result<(), OpenError> {
    if let Some(failed) = applied.iter().find(|row| !row.success) {
        return Err(OpenError::Migrate(MigrateError::Dirty(failed.version)));
    }
    if let Some(unknown) = applied
        .iter()
        .find(|row| !migrator.version_exists(row.version))
    {
        return Err(OpenError::NewerSchema {
            version: unknown.version,
        });
    }
    let ups = || {
        migrator
            .iter()
            .filter(|migration| !migration.migration_type.is_down_migration())
    };
    for migration in ups() {
        let edited = applied.iter().any(|row| {
            row.version == migration.version && row.checksum != migration.checksum.as_ref()
        });
        if edited {
            return Err(OpenError::EditedMigration {
                version: migration.version,
            });
        }
    }
    match ups().find(|migration| !applied.iter().any(|row| row.version == migration.version)) {
        Some(pending) => Err(OpenError::Pending {
            version: pending.version,
        }),
        None => Ok(()),
    }
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

    /// Every migration of this server, as a database that applied them all
    /// records it.
    fn all_applied() -> Vec<Applied> {
        MIGRATOR
            .iter()
            .map(|migration| Applied {
                version: migration.version,
                checksum: migration.checksum.to_vec(),
                success: true,
            })
            .collect()
    }

    #[test]
    fn a_database_with_every_migration_is_current() {
        assert!(schema_state(&all_applied(), &MIGRATOR).is_ok());
    }

    #[test]
    fn a_missing_migration_is_pending_from_the_first_one_missing() {
        let mut applied = all_applied();
        let last = applied.pop().unwrap();
        applied.pop();

        let state = schema_state(&applied, &MIGRATOR);

        assert!(
            matches!(state, Err(OpenError::Pending { version }) if version == last.version - 1),
            "{state:?}"
        );
        assert!(matches!(
            schema_state(&[], &MIGRATOR),
            Err(OpenError::Pending { version: 1 })
        ));
    }

    #[test]
    fn an_edited_migration_comes_before_a_pending_one() {
        let mut applied = all_applied();
        applied.pop();
        applied[1].checksum = vec![0];

        let state = schema_state(&applied, &MIGRATOR);

        assert!(
            matches!(state, Err(OpenError::EditedMigration { version: 2 })),
            "{state:?}"
        );
    }

    #[test]
    fn an_unknown_migration_comes_before_an_edited_one() {
        let mut applied = all_applied();
        applied[0].checksum = vec![0];
        applied.push(Applied {
            version: 9_999,
            checksum: vec![0],
            success: true,
        });

        let state = schema_state(&applied, &MIGRATOR);

        assert!(
            matches!(state, Err(OpenError::NewerSchema { version: 9_999 })),
            "{state:?}"
        );
    }

    #[test]
    fn a_failed_migration_comes_first() {
        let mut applied = all_applied();
        applied[2].success = false;
        applied.push(Applied {
            version: 9_999,
            checksum: vec![0],
            success: true,
        });

        let state = schema_state(&applied, &MIGRATOR);

        assert!(
            matches!(state, Err(OpenError::Migrate(MigrateError::Dirty(3)))),
            "{state:?}"
        );
    }

    #[test]
    fn the_new_errors_name_the_file_and_the_fix() {
        let missing = OpenError::MissingFile {
            file: PathBuf::from("/data/afkfleet.db"),
        };
        let pending = OpenError::Pending { version: 8 };

        assert_eq!(
            missing.to_string(),
            format!(
                "the database {} doesn't exist, and this command never creates one",
                PathBuf::from("/data/afkfleet.db").display()
            )
        );
        assert_eq!(
            pending.to_string(),
            "the database has pending migrations (the first is 8); run `afkfleet-server migrate` first"
        );
    }

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
