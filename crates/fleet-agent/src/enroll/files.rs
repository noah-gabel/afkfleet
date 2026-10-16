//! The agent's identity files (Plan.md P10.3; ADR-0019, S18):
//! `[control_plane] key_file` and `cert_file`, written once, owner-only,
//! and never replaced. An identity that exists is refused, never replaced:
//! every enrollment makes a new agent.
//!
//! 1. **Check** ([`Claimed::claim`]): nothing may exist at either path (a
//!    file, a folder, a dangling link: `symlink_metadata`), and their
//!    folders must exist; none is created.
//! 2. **Lock:** `<file>.tmp` is created next to each with `create_new`,
//!    owner-only on Unix, and its handle is kept. One that exists means
//!    another `enroll` runs or one was interrupted: refused, naming it.
//! 3. **The key first** ([`Claimed::write_key`]): written into its temp
//!    file and synced before the call. Every write goes through the handle
//!    from step 2, never the path, so a temp that someone who can write the
//!    folder swaps meanwhile never receives the key. (The links in step 4
//!    are made by path: a swapped temp would be published in its place,
//!    holding none of this run's bytes.)
//! 4. **Publish** ([`Claimed::publish`]): the certificate into its temp
//!    file, synced; then `hard_link`, which never replaces a name, the key
//!    and after it the certificate; then the temps are removed and, on Unix,
//!    the folders synced.
//!
//! A claim that's dropped unpublished removes its temps. A publish that
//! fails partway removes them too, and the key it linked; a file that
//! appeared meanwhile is kept and named.
//!
//! On Windows, the files get no owner-only mode: they inherit their
//! folder's permissions, their only protection there (S20).

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

/// The suffix of the temp files, `<file>.tmp`.
pub const TEMP_SUFFIX: &str = ".tmp";

/// The temp file next to `path`: `<path>.tmp`.
#[must_use]
pub fn temp_path(path: &Path) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(TEMP_SUFFIX);
    PathBuf::from(name)
}

/// Why the identity's files can't be claimed. Each names its path.
#[derive(Debug, thiserror::Error)]
pub enum ClaimError {
    /// Something exists at a target.
    #[error(
        "{} exists already: this host is enrolled, or holds another identity; it's never replaced, so move it away to enroll again",
        path.display()
    )]
    Exists {
        /// The target.
        path: PathBuf,
    },
    /// A target's folder doesn't exist.
    #[error("the folder of {} doesn't exist; create it first", path.display())]
    NoFolder {
        /// The target.
        path: PathBuf,
    },
    /// A temp file exists.
    #[error(
        "{} exists: another `afkfleet-agent enroll` runs, or one was interrupted; remove it if none runs",
        path.display()
    )]
    Locked {
        /// The temp file.
        path: PathBuf,
    },
    /// A path couldn't be checked, or a temp file created.
    #[error("{} can't be checked or created", path.display())]
    Io {
        /// The path.
        path: PathBuf,
        /// The OS's error.
        #[source]
        source: io::Error,
    },
}

/// Why the identity wasn't published.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    /// A temp file couldn't be written.
    #[error("{} can't be written", path.display())]
    Write {
        /// The temp file.
        path: PathBuf,
        /// The OS's error.
        #[source]
        source: io::Error,
    },
    /// A target couldn't be linked: it appeared meanwhile, or the OS failed.
    #[error("{} can't be written; whatever is there now was left as it is", path.display())]
    Link {
        /// The target.
        path: PathBuf,
        /// The OS's error.
        #[source]
        source: io::Error,
    },
}

/// The identity's two files, claimed: their temp files exist and are this
/// run's. Dropping it removes them.
#[derive(Debug)]
pub struct Claimed {
    key: PathBuf,
    cert: PathBuf,
    key_temp: PathBuf,
    cert_temp: PathBuf,
    /// The temps' handles, kept from their creation: every write goes
    /// through them, never through the paths, which someone who can write
    /// the folder could swap for a file of their own.
    key_file: File,
    cert_file: File,
}

impl Claimed {
    /// Checks that `key` and `cert` don't exist and their folders do, and
    /// creates their temp files.
    ///
    /// # Errors
    /// [`ClaimError`], with nothing created.
    pub fn claim(key: &Path, cert: &Path) -> Result<Self, ClaimError> {
        for target in [key, cert] {
            let folder = target
                .parent()
                .filter(|folder| !folder.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            if !fs::metadata(folder).is_ok_and(|folder| folder.is_dir()) {
                return Err(ClaimError::NoFolder {
                    path: target.to_owned(),
                });
            }
            // Not following links: a dangling symlink is taken too.
            match fs::symlink_metadata(target) {
                Ok(_) => {
                    return Err(ClaimError::Exists {
                        path: target.to_owned(),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(ClaimError::Io {
                        path: target.to_owned(),
                        source,
                    });
                }
            }
        }
        let key_temp = temp_path(key);
        let cert_temp = temp_path(cert);
        let key_file = lock(&key_temp)?;
        let cert_file = match lock(&cert_temp) {
            Ok(file) => file,
            Err(error) => {
                drop(key_file);
                let _ = fs::remove_file(&key_temp);
                return Err(error);
            }
        };
        Ok(Self {
            key: key.to_owned(),
            cert: cert.to_owned(),
            key_temp,
            cert_temp,
            key_file,
            cert_file,
        })
    }

    /// Writes `pem` into the key's temp file and syncs it.
    ///
    /// # Errors
    /// [`PublishError::Write`].
    pub fn write_key(&self, pem: &[u8]) -> Result<(), PublishError> {
        write_synced(&self.key_file, &self.key_temp, pem)
    }

    /// Writes `pem` into the certificate's temp file, then links the key
    /// and the certificate to their names and removes the temps.
    ///
    /// # Errors
    /// [`PublishError`], after removing the temps and the key it linked.
    pub fn publish(self, pem: &[u8]) -> Result<(), PublishError> {
        write_synced(&self.cert_file, &self.cert_temp, pem)?;
        // `hard_link` fails when the name exists: nothing is ever replaced.
        fs::hard_link(&self.key_temp, &self.key).map_err(|source| PublishError::Link {
            path: self.key.clone(),
            source,
        })?;
        if let Err(source) = fs::hard_link(&self.cert_temp, &self.cert) {
            // The key this run linked a moment ago.
            let _ = fs::remove_file(&self.key);
            return Err(PublishError::Link {
                path: self.cert.clone(),
                source,
            });
        }
        // Dropping removes the temps; the names hold the files.
        let folder = self.key.parent().map(Path::to_owned);
        drop(self);
        if let Some(folder) = folder {
            sync_folder(&folder);
        }
        Ok(())
    }
}

impl Drop for Claimed {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.key_temp);
        let _ = fs::remove_file(&self.cert_temp);
    }
}

/// Creates the temp file at `path`, owner-only on Unix, and returns its
/// handle; an existing one is [`ClaimError::Locked`].
fn lock(path: &Path) -> Result<File, ClaimError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(ClaimError::Locked {
            path: path.to_owned(),
        }),
        Err(source) => Err(ClaimError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

/// Writes `bytes` into this run's temp file through its handle `file` (at
/// `path`, for the error) and syncs it.
fn write_synced(mut file: &File, path: &Path, bytes: &[u8]) -> Result<(), PublishError> {
    let mut write = || -> io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()
    };
    write().map_err(|source| PublishError::Write {
        path: path.to_owned(),
        source,
    })
}

/// Syncs `folder`, so the new names survive a crash (Unix; Windows can't
/// open a folder as a file).
fn sync_folder(folder: &Path) {
    #[cfg(unix)]
    if let Ok(folder) = fs::File::open(folder) {
        let _ = folder.sync_all();
    }
    #[cfg(not(unix))]
    let _ = folder;
}
