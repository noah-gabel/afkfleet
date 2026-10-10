//! The database file itself: its folder must exist, [`prepare`] creates a
//! missing file with the right permissions, and on Unix it refuses files that
//! other users can read or write.

use std::io;
use std::path::Path;

use super::OpenError;

/// Makes sure `path` is ready to open: its folder exists, the file exists
/// (created here, `0600` on Unix, if it was missing), and on Unix neither the
/// file nor a leftover `-wal` or `-shm` next to it is open to other users.
pub(super) async fn prepare(path: &Path) -> Result<(), OpenError> {
    check_folder(path).await?;
    create_if_missing(path).await?;
    check_permissions(path).await
}

/// Refuses a missing folder, so a typo or a missing volume never gets a fresh
/// database somewhere unexpected. Nothing is created.
async fn check_folder(path: &Path) -> Result<(), OpenError> {
    let folder = path.parent().unwrap_or(path);
    match tokio::fs::metadata(folder).await {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(OpenError::MissingFolder {
            folder: folder.to_owned(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(OpenError::MissingFolder {
            folder: folder.to_owned(),
        }),
        Err(source) => Err(OpenError::Inspect {
            file: folder.to_owned(),
            source,
        }),
    }
}

/// Creates the file if it doesn't exist yet: empty, which SQLite reads as an
/// empty database, and `0600` on Unix. An existing file is left as it is.
async fn create_if_missing(path: &Path) -> Result<(), OpenError> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    match options.open(path).await {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(source) => Err(OpenError::Create {
            file: path.to_owned(),
            source,
        }),
    }
}

/// Refuses the database, `-wal` or `-shm` file if group or others have any
/// permission on it. New `-wal` and `-shm` files get the database's mode from
/// SQLite, but a leftover one (after a crash) keeps its own, and `-wal` holds
/// real data.
#[cfg(unix)]
async fn check_permissions(path: &Path) -> Result<(), OpenError> {
    use std::os::unix::fs::PermissionsExt;

    for file in [
        path.to_owned(),
        sibling(path, "-wal"),
        sibling(path, "-shm"),
    ] {
        match tokio::fs::metadata(&file).await {
            Ok(metadata) => {
                let mode = metadata.permissions().mode() & 0o777;
                if mode & 0o077 != 0 {
                    return Err(OpenError::Permissions { file, mode });
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(OpenError::Inspect { file, source }),
        }
    }
    Ok(())
}

/// On Windows, permissions are ACLs, which this check doesn't model; the
/// database is only as private as its folder.
#[cfg(not(unix))]
fn check_permissions(_path: &Path) -> core::future::Ready<Result<(), OpenError>> {
    core::future::ready(Ok(()))
}

/// The path of the file SQLite keeps next to `path` with `suffix`.
#[cfg(unix)]
fn sibling(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    std::path::PathBuf::from(name)
}
