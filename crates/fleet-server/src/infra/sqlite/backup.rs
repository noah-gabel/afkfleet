//! One backup of the database file, and the backups' folder (Plan.md P12.3;
//! ADR-0022, S2, S4, R1, R2, R3).
//!
//! [`write()`] makes one backup named for its second
//! ([`BackupName`]):
//! 1. it creates the temporary dot file, empty, never over an existing one,
//!    and `0600` on Unix, so SQLite fills a file only the server's user can
//!    read;
//! 2. `VACUUM INTO` it from a connection of its own that opens the database
//!    read-only ([`PoolRole::Backup`]): the copy takes only a read lock, never
//!    the one write connection, and holds no freed page's old text;
//! 3. syncs it, then checks it on an `immutable` read-only connection: its
//!    migrations exactly this server's, and `quick_check` answering "ok";
//! 4. publishes it with a hard link, which never replaces a file of that
//!    name, then removes the temporary name and, on Unix, syncs the folder.
//!
//! Anything that fails removes the temporary file, so only a crash leaves
//! one, which pruning removes once it's a day old. A stop, or the bound, ends
//! `VACUUM INTO` through SQLite's progress handler.
//!
//! [`list`] reads the folder itself, never what's below it: only regular
//! files whose whole name is one the server writes count, and links are
//! never followed (ADR-0020, R130). [`remove`] deletes exactly the names it
//! is given.
//!
//! # Timeouts
//! `VACUUM INTO` is bounded by the caller's bound through the progress
//! handler. The rest is local file work on the server's own folder, with no
//! network, as the database's own awaits are (the module docs of
//! [`super`]).

use core::time::Duration;
use std::io;
use std::path::{Path, PathBuf};

use fleet_core::backup::BackupName;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions as _, Connection as _};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::connect::{DatabaseOptions, PoolRole, connect_options};
use super::{MIGRATOR, OpenError, applied_migrations, schema_state};

/// How many SQLite instructions run between two checks of the stop and the
/// bound: about a microsecond's worth, so a stop ends the copy at once, and
/// even the short statements `VACUUM INTO` runs for a small table are checked.
const PROGRESS_STEPS: i32 = 100;

/// A backup [`write()`] finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// Its name.
    pub name: BackupName,
    /// The finished file.
    pub file: PathBuf,
    /// Its size.
    pub bytes: u64,
}

/// What the backups' folder holds that the server wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// The finished backups, as named.
    pub finished: Vec<BackupName>,
    /// The temporary files of backups being written, or left by a crash.
    pub partial: Vec<BackupName>,
}

/// Why a backup, or a look at the backups' folder, failed. Each names the
/// file or folder; none holds anything read from the database.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    /// The backups' folder isn't there.
    #[error("the backups' folder {} doesn't exist", dir.display())]
    MissingFolder {
        /// The folder.
        dir: PathBuf,
    },
    /// The backups' folder is a link, which is never followed.
    #[error("the backups' folder {} is a link; name the folder itself", dir.display())]
    Link {
        /// The folder.
        dir: PathBuf,
    },
    /// The backups' folder is a file.
    #[error("the backups' folder {} isn't a folder", dir.display())]
    NotAFolder {
        /// The path.
        dir: PathBuf,
    },
    /// On disk, the backups' folder holds the database.
    #[error("the backups' folder {} holds the database", dir.display())]
    HoldsDatabase {
        /// The folder.
        dir: PathBuf,
    },
    /// On disk, the backups' folder holds a key file.
    #[error("the backups' folder {} holds the key file {}", dir.display(), file.display())]
    HoldsKey {
        /// The folder.
        dir: PathBuf,
        /// The key file.
        file: PathBuf,
    },
    /// The clock is outside the years a backup's name can hold.
    #[error("the clock's time can't name a backup")]
    Unnamed,
    /// A path SQLite can't be given, which isn't UTF-8.
    #[error("the path {} isn't UTF-8", file.display())]
    NotUtf8 {
        /// The path.
        file: PathBuf,
    },
    /// A file or folder couldn't be read, written, synced, linked or
    /// removed.
    #[error("couldn't {action} {}", file.display())]
    Io {
        /// What was being done, like "create".
        action: &'static str,
        /// The file or folder.
        file: PathBuf,
        /// What the OS reported.
        #[source]
        source: io::Error,
    },
    /// A backup of that name exists already, and is never replaced.
    #[error("the backup {} exists already", file.display())]
    Taken {
        /// The existing file.
        file: PathBuf,
    },
    /// The database couldn't be opened, or `VACUUM INTO` failed.
    #[error("couldn't copy the database into {}", file.display())]
    Copy {
        /// The backup being written.
        file: PathBuf,
        /// What SQLite reported.
        #[source]
        source: sqlx::Error,
    },
    /// The stop, or the bound, ended the copy.
    #[error("the backup into {} was stopped", file.display())]
    Stopped {
        /// The backup being written.
        file: PathBuf,
    },
    /// The new file's migrations aren't exactly this server's.
    #[error("the backup {} doesn't hold this server's schema", file.display())]
    Schema {
        /// The backup.
        file: PathBuf,
        /// Why.
        #[source]
        source: Box<OpenError>,
    },
    /// The new file didn't pass `quick_check`, or couldn't be checked.
    #[error("the backup {} failed its check", file.display())]
    Check {
        /// The backup.
        file: PathBuf,
        /// What SQLite reported, when it reported an error.
        #[source]
        source: Option<sqlx::Error>,
    },
}

/// Checks that `dir` is a folder, not a link (the folder itself, never
/// followed). It's never created: a missing volume must stop the server.
///
/// # Errors
/// [`BackupError::MissingFolder`], [`BackupError::Link`],
/// [`BackupError::NotAFolder`], or [`BackupError::Io`] when it can't be read.
pub async fn check_folder(dir: &Path) -> Result<(), BackupError> {
    match tokio::fs::symlink_metadata(dir).await {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(BackupError::Link {
            dir: dir.to_owned(),
        }),
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(BackupError::NotAFolder {
            dir: dir.to_owned(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(BackupError::MissingFolder {
            dir: dir.to_owned(),
        }),
        Err(source) => Err(BackupError::Io {
            action: "read the metadata of",
            file: dir.to_owned(),
            source,
        }),
    }
}

/// Checks on disk what the config checked by its words (S4): the real
/// `dir` is neither the real folder of `database` nor one above it, and
/// holds none of `key_files` (each read through its real folder, so a key
/// file that doesn't exist yet counts too).
///
/// # Errors
/// [`BackupError::HoldsDatabase`], [`BackupError::HoldsKey`], or
/// [`BackupError::Io`] when a real path can't be read.
pub async fn check_placement(
    dir: &Path,
    database: &Path,
    key_files: &[PathBuf],
) -> Result<(), BackupError> {
    let real_dir = real(dir).await?;
    if let Some(folder) = database.parent()
        && real(folder).await?.starts_with(&real_dir)
    {
        return Err(BackupError::HoldsDatabase {
            dir: dir.to_owned(),
        });
    }
    for file in key_files {
        let (Some(folder), Some(name)) = (file.parent(), file.file_name()) else {
            continue;
        };
        let real_folder = match tokio::fs::canonicalize(folder).await {
            Ok(real_folder) => real_folder,
            // A key file whose folder doesn't exist can't be in `dir`.
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(BackupError::Io {
                    action: "resolve",
                    file: folder.to_owned(),
                    source,
                });
            }
        };
        if real_folder.join(name).starts_with(&real_dir) {
            return Err(BackupError::HoldsKey {
                dir: dir.to_owned(),
                file: file.clone(),
            });
        }
    }
    Ok(())
}

/// `path` with every link resolved.
async fn real(path: &Path) -> Result<PathBuf, BackupError> {
    tokio::fs::canonicalize(path)
        .await
        .map_err(|source| BackupError::Io {
            action: "resolve",
            file: path.to_owned(),
            source,
        })
}

/// Writes one backup of `database` into `dir`, named `name`; see the module
/// docs. `stop` or `bound` end the copy.
///
/// # Errors
/// [`BackupError::NotUtf8`]; [`BackupError::Io`] for the temporary file,
/// the sync, the link or the folder; [`BackupError::Copy`] or
/// [`BackupError::Stopped`] for the copy; [`BackupError::Schema`] or
/// [`BackupError::Check`] for the check; [`BackupError::Taken`] when a
/// backup of that name exists. Nothing is left but on a crash.
pub async fn write(
    database: &Path,
    dir: &Path,
    name: BackupName,
    stop: &CancellationToken,
    bound: Duration,
    options: &DatabaseOptions,
) -> Result<Written, BackupError> {
    let partial = dir.join(name.partial_name());
    let file = dir.join(name.file_name());
    create_partial(&partial).await?;
    let written = fill_and_publish(database, &partial, &file, stop, bound, options).await;
    if written.is_err() {
        // Gone already when the link worked; any other error is the one
        // to report.
        let _ = remove_file(&partial).await;
    }
    let bytes = written?;
    Ok(Written { name, file, bytes })
}

/// The steps of [`write()`] after the temporary file exists; its size once
/// published.
async fn fill_and_publish(
    database: &Path,
    partial: &Path,
    file: &Path,
    stop: &CancellationToken,
    bound: Duration,
    options: &DatabaseOptions,
) -> Result<u64, BackupError> {
    let target = partial.to_str().ok_or_else(|| BackupError::NotUtf8 {
        file: partial.to_owned(),
    })?;
    copy(database, target, partial, stop, bound, options).await?;
    sync(partial).await?;
    check(partial, options).await?;
    match tokio::fs::hard_link(partial, file).await {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return Err(BackupError::Taken {
                file: file.to_owned(),
            });
        }
        Err(source) => {
            return Err(BackupError::Io {
                action: "publish",
                file: file.to_owned(),
                source,
            });
        }
    }
    remove_file(partial).await?;
    sync_folder(file).await?;
    let metadata = tokio::fs::metadata(file)
        .await
        .map_err(|source| BackupError::Io {
            action: "read the metadata of",
            file: file.to_owned(),
            source,
        })?;
    Ok(metadata.len())
}

/// Creates the empty temporary file, never over an existing one, `0600` on
/// Unix; SQLite fills an empty file in place, so it keeps the mode.
async fn create_partial(partial: &Path) -> Result<(), BackupError> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
        .open(partial)
        .await
        .map(drop)
        .map_err(|source| BackupError::Io {
            action: "create",
            file: partial.to_owned(),
            source,
        })
}

/// `VACUUM INTO` `target` from a read-only connection of its own, ended by
/// `stop` or `bound` through the progress handler.
async fn copy(
    database: &Path,
    target: &str,
    partial: &Path,
    stop: &CancellationToken,
    bound: Duration,
    options: &DatabaseOptions,
) -> Result<(), BackupError> {
    let copy_failed = |source| BackupError::Copy {
        file: partial.to_owned(),
        source,
    };
    if stop.is_cancelled() {
        return Err(BackupError::Stopped {
            file: partial.to_owned(),
        });
    }
    let mut connection = connect_options(
        SqliteConnectOptions::new().filename(database),
        PoolRole::Backup,
        options,
    )
    .connect()
    .await
    .map_err(copy_failed)?;
    let deadline = Instant::now() + bound;
    let token = stop.clone();
    let ended = move || token.is_cancelled() || Instant::now() >= deadline;
    let handler = ended.clone();
    let locked = connection
        .lock_handle()
        .await
        .map(|mut handle| handle.set_progress_handler(PROGRESS_STEPS, move || !handler()));
    if let Err(source) = locked {
        let _ = connection.close().await;
        return Err(copy_failed(source));
    }
    let result = sqlx::query!("VACUUM INTO ?", target)
        .execute(&mut connection)
        .await;
    let _ = connection.close().await;
    match result {
        Ok(_) => Ok(()),
        Err(_) if ended() => Err(BackupError::Stopped {
            file: partial.to_owned(),
        }),
        Err(source) => Err(copy_failed(source)),
    }
}

/// Checks the new file on an `immutable` read-only connection, so it opens
/// nothing beside it: its migrations exactly this server's, and
/// `quick_check` answering "ok" and nothing else.
async fn check(partial: &Path, options: &DatabaseOptions) -> Result<(), BackupError> {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .acquire_timeout(options.acquire_timeout())
        .connect_with(
            connect_options(
                SqliteConnectOptions::new().filename(partial),
                PoolRole::Backup,
                options,
            )
            .immutable(true),
        )
        .await
        .map_err(|source| BackupError::Check {
            file: partial.to_owned(),
            source: Some(source),
        })?;
    let schema = match applied_migrations(&pool).await {
        Ok(applied) => schema_state(&applied, &MIGRATOR),
        Err(error) => Err(error),
    };
    let quick = sqlx::query_scalar!("PRAGMA quick_check")
        .fetch_all(&pool)
        .await;
    pool.close().await;
    schema.map_err(|source| BackupError::Schema {
        file: partial.to_owned(),
        source: Box::new(source),
    })?;
    match quick {
        Ok(rows)
            if rows.len() == 1 && rows.first().is_some_and(|row| row.as_deref() == Some("ok")) =>
        {
            Ok(())
        }
        Ok(_) => Err(BackupError::Check {
            file: partial.to_owned(),
            source: None,
        }),
        Err(source) => Err(BackupError::Check {
            file: partial.to_owned(),
            source: Some(source),
        }),
    }
}

/// Syncs `file`'s content to the disk, through a handle that may write:
/// Windows refuses to flush one that can only read.
async fn sync(file: &Path) -> Result<(), BackupError> {
    let synced = match tokio::fs::OpenOptions::new().write(true).open(file).await {
        Ok(handle) => handle.sync_all().await,
        Err(error) => Err(error),
    };
    synced.map_err(|source| BackupError::Io {
        action: "sync",
        file: file.to_owned(),
        source,
    })
}

/// On Unix, syncs the folder of `file`, so its new name survives a crash; a
/// folder opens only to read.
#[cfg(unix)]
async fn sync_folder(file: &Path) -> Result<(), BackupError> {
    let Some(folder) = file.parent() else {
        return Ok(());
    };
    let synced = match tokio::fs::File::open(folder).await {
        Ok(handle) => handle.sync_all().await,
        Err(error) => Err(error),
    };
    synced.map_err(|source| BackupError::Io {
        action: "sync",
        file: folder.to_owned(),
        source,
    })
}

/// Windows can't open a folder to sync it; NTFS journals the name itself.
#[cfg(not(unix))]
fn sync_folder(_file: &Path) -> core::future::Ready<Result<(), BackupError>> {
    core::future::ready(Ok(()))
}

/// Removes `file`; one that's gone already counts as removed.
async fn remove_file(file: &Path) -> Result<(), BackupError> {
    match tokio::fs::remove_file(file).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(BackupError::Io {
            action: "remove",
            file: file.to_owned(),
            source,
        }),
    }
}

/// What `dir` holds that the server wrote: regular files only, by their
/// whole name, never a link or anything below the folder.
///
/// # Errors
/// [`BackupError::Io`] when the folder can't be read.
pub async fn list(dir: &Path) -> Result<Listing, BackupError> {
    let read_failed = |source| BackupError::Io {
        action: "read",
        file: dir.to_owned(),
        source,
    };
    let mut entries = tokio::fs::read_dir(dir).await.map_err(read_failed)?;
    let mut listing = Listing::default();
    while let Some(entry) = entries.next_entry().await.map_err(read_failed)? {
        // The entry's own type, never followed.
        let Ok(kind) = entry.file_type().await else {
            continue;
        };
        if !kind.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if let Some(finished) = BackupName::parse(&name) {
            listing.finished.push(finished);
        } else if let Some(partial) = BackupName::parse_partial(&name) {
            listing.partial.push(partial);
        }
    }
    listing.finished.sort_unstable();
    listing.partial.sort_unstable();
    Ok(listing)
}

/// Removes the finished backups `finished` and the temporary files `partial`
/// from `dir`, exactly those names; one gone already counts as removed. It
/// stops at the first that fails.
///
/// # Errors
/// [`BackupError::Io`] naming the file that couldn't be removed.
pub async fn remove(
    dir: &Path,
    finished: &[BackupName],
    partial: &[BackupName],
) -> Result<(), BackupError> {
    for name in finished {
        remove_file(&dir.join(name.file_name())).await?;
    }
    for name in partial {
        remove_file(&dir.join(name.partial_name())).await?;
    }
    Ok(())
}
