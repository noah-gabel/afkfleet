//! [`connect_options`]: every per-connection setting, in one place, for the
//! server and its tests alike, and the [`DatabaseOptions`] tests may tighten.

use core::time::Duration;

use log::LevelFilter;
use sqlx::ConnectOptions as _;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};

/// How long a connection waits for SQLite's locks before it reports busy.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The number of read connections (ADR-0015). Writes have exactly one.
pub const READ_CONNECTIONS: u32 = 4;

/// The settings around the pools that tests may tighten. The server always
/// uses [`DatabaseOptions::default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatabaseOptions {
    acquire_timeout: Duration,
    slow_statement: Duration,
}

impl DatabaseOptions {
    /// How long a caller waits for a connection before it gets an error
    /// instead: well inside the 15 s request timeout, so a transaction that
    /// holds the one write connection too long shows up as a clear error.
    pub const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

    /// From how long on a statement counts as slow and is logged at `warn`.
    pub const SLOW_STATEMENT: Duration = Duration::from_millis(250);

    /// Replaces the acquire timeout, for tests that wait for it.
    #[must_use]
    pub const fn with_acquire_timeout(self, acquire_timeout: Duration) -> Self {
        Self {
            acquire_timeout,
            ..self
        }
    }

    /// Replaces the slow-statement threshold, for tests of the slow log.
    #[must_use]
    pub const fn with_slow_statement(self, slow_statement: Duration) -> Self {
        Self {
            slow_statement,
            ..self
        }
    }

    /// The acquire timeout of both pools.
    #[must_use]
    pub const fn acquire_timeout(&self) -> Duration {
        self.acquire_timeout
    }

    /// The slow-statement threshold.
    #[must_use]
    pub const fn slow_statement(&self) -> Duration {
        self.slow_statement
    }
}

impl Default for DatabaseOptions {
    fn default() -> Self {
        Self {
            acquire_timeout: Self::ACQUIRE_TIMEOUT,
            slow_statement: Self::SLOW_STATEMENT,
        }
    }
}

/// Which pool a connection belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolRole {
    /// The one connection that writes, in WAL mode.
    Write,
    /// A read connection, which refuses every write (`query_only`).
    Read,
}

/// Applies every per-connection setting for `role` to `base`, which names
/// the file. [`super::Database::connect`] uses it for both pools, and so do
/// the repository tests, so tests and production can't drift apart.
///
/// Every connection gets `foreign_keys=ON`, `busy_timeout=5s`,
/// `synchronous=NORMAL` and `trusted_schema=OFF`, logs every statement at
/// `debug` and slow ones at `warn`, and never creates a missing file
/// ([`super::open`] does that, with the right permissions). The write
/// connection adds `journal_mode=WAL`; read connections add `query_only=ON`.
#[must_use]
pub fn connect_options(
    base: SqliteConnectOptions,
    role: PoolRole,
    options: &DatabaseOptions,
) -> SqliteConnectOptions {
    let common = base
        .create_if_missing(false)
        .foreign_keys(true)
        .busy_timeout(BUSY_TIMEOUT)
        .synchronous(SqliteSynchronous::Normal)
        .pragma("trusted_schema", "OFF")
        .log_statements(LevelFilter::Debug)
        .log_slow_statements(LevelFilter::Warn, options.slow_statement);
    match role {
        PoolRole::Write => common.journal_mode(SqliteJournalMode::Wal),
        PoolRole::Read => common.pragma("query_only", "ON"),
    }
}
