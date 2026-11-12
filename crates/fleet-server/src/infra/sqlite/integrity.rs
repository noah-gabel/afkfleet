//! [`check`]: SQLite's own `integrity_check` over the database file (Plan.md
//! P13.5, ADR-0033), for `afkfleet-server database check`: what an operator
//! asks after a crash, a full disk or a restore, and what the chaos tests ask
//! after every fault.
//!
//! - **Read-only, on a connection of its own** ([`PoolRole::Backup`]): it
//!   never creates, migrates or checkpoints anything, so it runs beside a
//!   running server or with it stopped, and a `-wal` a killed server left is
//!   read through and stays for the next start. Beside a database nobody has
//!   open, SQLite leaves the `-wal`, holding nothing, and the `-shm` that
//!   every connection to a WAL database makes: a read-only connection can't
//!   remove them, and the next start does.
//! - **The file must exist** (the folder and, on Unix, the permissions
//!   [`super::open`] checks): a missing one is an error, never an empty
//!   database that reads as sound.
//! - **Damage is an answer, not an error.** SQLite lists what it found (at
//!   most 100 lines, its default). A file it can't read as a database at all
//!   (`SQLITE_CORRUPT`, `SQLITE_NOTADB`) is damage too, with SQLite's one
//!   line. Its findings name tables, indexes, pages and row numbers, never a
//!   row's content.
//!
//! # Timeouts
//! None: `integrity_check` reads every page of a local file, as long as that
//! takes, and only an operator's command runs it; stopping the process ends
//! it, and nothing is left behind. While it runs beside a server it holds a
//! read snapshot, so the `-wal` isn't cut back until it ends.

use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions as _, Connection as _};

use super::connect::{DatabaseOptions, PoolRole, connect_options};
use super::{OpenError, file};
use crate::config::DatabaseConfig;

/// What the check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Integrity {
    /// SQLite answered "ok" and nothing else.
    Sound,
    /// SQLite's findings, as it words them: never empty.
    Damaged(Vec<String>),
}

/// Why the database couldn't be checked.
#[derive(Debug, thiserror::Error)]
pub enum IntegrityError {
    /// The file isn't there, or is open to other users.
    #[error("the database can't be opened")]
    File(#[source] OpenError),
    /// SQLite couldn't read it, for another reason than damage: a lock that
    /// never came free, or the OS.
    #[error("the database can't be read")]
    Read(#[source] sqlx::Error),
}

/// SQLite's primary result codes for a file that isn't a sound database:
/// `SQLITE_CORRUPT` and `SQLITE_NOTADB`.
const DAMAGE_CODES: [u32; 2] = [11, 26];

/// What SQLite answers when it finds nothing.
const OK: &str = "ok";

/// Runs `PRAGMA integrity_check` over the database `config` names; see the
/// [module docs](self).
///
/// # Errors
/// [`IntegrityError::File`] when the file or its folder is missing or, on
/// Unix, open to other users; [`IntegrityError::Read`] when SQLite can't
/// read it for another reason than damage.
pub async fn check(
    config: &DatabaseConfig,
    options: &DatabaseOptions,
) -> Result<Integrity, IntegrityError> {
    file::check_existing(&config.path)
        .await
        .map_err(IntegrityError::File)?;
    let connecting = connect_options(
        SqliteConnectOptions::new().filename(&config.path),
        PoolRole::Backup,
        options,
    )
    .connect()
    .await;
    let mut connection = match connecting {
        Ok(connection) => connection,
        Err(error) => return unreadable(error),
    };
    let rows = sqlx::query_scalar!("PRAGMA integrity_check")
        .fetch_all(&mut connection)
        .await;
    let _ = connection.close().await;
    match rows {
        Ok(rows) => {
            let findings: Vec<String> = rows.into_iter().flatten().collect();
            if findings.len() == 1 && findings.first().is_some_and(|only| only == OK) {
                Ok(Integrity::Sound)
            } else if findings.is_empty() {
                // SQLite always answers a line; none at all is no "ok".
                Ok(Integrity::Damaged(vec![
                    "integrity_check answered nothing".to_owned(),
                ]))
            } else {
                Ok(Integrity::Damaged(findings))
            }
        }
        Err(error) => unreadable(error),
    }
}

/// `error` as the check's answer: damage with SQLite's line when its code
/// says the file isn't a sound database, the error otherwise.
fn unreadable(error: sqlx::Error) -> Result<Integrity, IntegrityError> {
    if is_damage(&error) {
        let line = error.as_database_error().map_or_else(
            || error.to_string(),
            |database| database.message().to_owned(),
        );
        Ok(Integrity::Damaged(vec![line]))
    } else {
        Err(IntegrityError::Read(error))
    }
}

/// Whether `error` is SQLite saying the file is damaged: its primary result
/// code, the extended code's low byte, is one of [`DAMAGE_CODES`].
fn is_damage(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|database| database.code())
        .and_then(|code| code.parse::<u32>().ok())
        .is_some_and(|code| DAMAGE_CODES.contains(&(code & 0xFF)))
}
