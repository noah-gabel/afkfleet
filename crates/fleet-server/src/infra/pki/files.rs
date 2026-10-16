//! Writing the PKI's files without ever overwriting one (S12): `pki init`'s
//! four and `pki server-cert`'s two.
//!
//! A new CA would lock out every agent, so an existing file is never
//! replaced, not even in a race:
//! 1. [`check`] finds every problem first and writes nothing: a target or
//!    its temp file that exists (any directory entry, a dangling symlink
//!    too) or a missing folder. No folder is created.
//! 2. [`publish`] writes each file into its own temp file next to it,
//!    `create_new` (owner-only on Unix, before a byte is written), syncs and
//!    closes it, then links it to its name with `hard_link`, which fails if
//!    the name exists, in the order given. Then it removes the temp files and
//!    syncs the folders (on Unix).
//! 3. **A failure partway** removes every temp file it made, and of the
//!    names it linked only those that still hold its bytes; anything else
//!    found there is named and kept.
//!
//! Temp files sit in their target's folder, so on Windows a file inherits
//! that folder's permissions, its only protection there (S20).

use core::fmt;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use subtle::ConstantTimeEq as _;

/// A file to write. Its `Debug` shows the path and the length, never the
/// bytes: they may be a private key.
#[derive(Clone, Copy)]
pub struct NewFile<'a> {
    /// Its path.
    pub path: &'a Path,
    /// Its bytes.
    pub bytes: &'a [u8],
}

impl fmt::Debug for NewFile<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewFile")
            .field("path", &self.path)
            .field("len", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

/// A reason [`check`] refuses, naming its path.
#[derive(Debug, thiserror::Error)]
pub enum FileProblem {
    /// The target exists.
    #[error("{} exists already; it's never overwritten", path.display())]
    Exists {
        /// The target.
        path: PathBuf,
    },
    /// A temp file of an earlier run that didn't finish is there.
    #[error("{} is left from an earlier run that didn't finish; remove it", path.display())]
    LeftOver {
        /// The temp file.
        path: PathBuf,
    },
    /// The target's folder doesn't exist.
    #[error("the folder of {} doesn't exist; create it first", path.display())]
    NoFolder {
        /// The target.
        path: PathBuf,
    },
    /// The path couldn't be checked.
    #[error("{} can't be checked", path.display())]
    Unreadable {
        /// The path.
        path: PathBuf,
        /// The OS's error.
        source: io::Error,
    },
}

/// Why [`publish`] failed.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    /// A temp file couldn't be written.
    #[error("{} couldn't be written; nothing was published", path.display())]
    Write {
        /// The temp file.
        path: PathBuf,
        /// The OS's error.
        source: io::Error,
    },
    /// A file couldn't be linked to its name.
    #[error("{} couldn't be published; the files this run published were removed", path.display())]
    Link {
        /// The target.
        path: PathBuf,
        /// The OS's error.
        source: io::Error,
        /// Paths found holding something else than this run's bytes, kept.
        kept: Vec<PathBuf>,
    },
}

/// The temp file of `target`: `<prefix><file name>` in its folder.
#[must_use]
pub fn temp_path(target: &Path, prefix: &str) -> PathBuf {
    let mut name = OsString::from(prefix);
    if let Some(file_name) = target.file_name() {
        name.push(file_name);
    }
    target.with_file_name(name)
}

/// Every problem that stops writing `targets` with temp files named by
/// `prefix`; empty when there's none.
#[must_use]
pub fn check(targets: &[&Path], prefix: &str) -> Vec<FileProblem> {
    let mut problems = Vec::new();
    for &target in targets {
        let folder = target
            .parent()
            .filter(|folder| !folder.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if !fs::metadata(folder).is_ok_and(|folder| folder.is_dir()) {
            problems.push(FileProblem::NoFolder {
                path: target.to_owned(),
            });
            continue;
        }
        let temp = temp_path(target, prefix);
        for (path, left_over) in [(target, false), (temp.as_path(), true)] {
            // Not following links: a dangling symlink is taken too.
            match fs::symlink_metadata(path) {
                Ok(_) if left_over => problems.push(FileProblem::LeftOver {
                    path: path.to_owned(),
                }),
                Ok(_) => problems.push(FileProblem::Exists {
                    path: path.to_owned(),
                }),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => problems.push(FileProblem::Unreadable {
                    path: path.to_owned(),
                    source,
                }),
            }
        }
    }
    problems
}

/// Writes `files` in their order, each through its temp file named by
/// `prefix`, never overwriting a file. Returns the temp files it couldn't
/// remove afterwards, which hold copies of published files.
///
/// # Errors
/// [`PublishError`], after removing what this run made.
pub fn publish(files: &[NewFile<'_>], prefix: &str) -> Result<Vec<PathBuf>, PublishError> {
    publish_with(files, prefix, |_| {})
}

/// [`publish`], calling `after_link` with each name right after it's
/// linked: the tests' way to change a name meanwhile.
fn publish_with(
    files: &[NewFile<'_>],
    prefix: &str,
    mut after_link: impl FnMut(&Path),
) -> Result<Vec<PathBuf>, PublishError> {
    let mut temps = Vec::with_capacity(files.len());
    for file in files {
        let temp = temp_path(file.path, prefix);
        if let Err((source, created)) = write_new(&temp, file.bytes) {
            if created {
                temps.push(temp.clone());
            }
            remove_all(&temps);
            return Err(PublishError::Write { path: temp, source });
        }
        temps.push(temp);
    }
    let mut published: Vec<&NewFile<'_>> = Vec::with_capacity(files.len());
    for (file, temp) in files.iter().zip(&temps) {
        if let Err(source) = fs::hard_link(temp, file.path) {
            let kept = remove_published(&published);
            remove_all(&temps);
            return Err(PublishError::Link {
                path: file.path.to_owned(),
                source,
                kept,
            });
        }
        after_link(file.path);
        published.push(file);
    }
    let left_over = remove_all(&temps);
    sync_folders(files);
    Ok(left_over)
}

/// Creates `path`, which must not exist, owner-only on Unix before a byte is
/// written, then writes and syncs `bytes`. On failure, whether the file was
/// created (and so is this run's to remove).
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), (io::Error, bool)> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| (error, false))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| (error, true))
}

/// Removes `paths`, this run's temp files; returns those it couldn't.
fn remove_all(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths
        .iter()
        .filter(|path| fs::remove_file(path).is_err())
        .cloned()
        .collect()
}

/// Removes each published name that still holds this run's bytes; returns
/// the others, which are kept.
fn remove_published(published: &[&NewFile<'_>]) -> Vec<PathBuf> {
    let mut kept = Vec::new();
    for file in published {
        let ours = fs::read(file.path).is_ok_and(|bytes| bool::from(bytes.ct_eq(file.bytes)));
        if !ours || fs::remove_file(file.path).is_err() {
            kept.push(file.path.to_owned());
        }
    }
    kept
}

/// Syncs the folders of `files` on Unix, so the new names survive a crash.
/// Best effort: the files themselves are synced, and a failure here doesn't
/// undo them. Windows can't open a folder as a file.
fn sync_folders(files: &[NewFile<'_>]) {
    #[cfg(unix)]
    for file in files {
        if let Some(folder) = file.path.parent()
            && let Ok(folder) = fs::File::open(folder)
        {
            let _ = folder.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = files;
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const PREFIX: &str = "test-tmp-";

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn publishing_writes_each_file_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.key");
        let two = dir.path().join("two.crt");

        assert!(check(&[&one, &two], PREFIX).is_empty());
        let left_over = publish(
            &[
                NewFile {
                    path: &one,
                    bytes: b"first",
                },
                NewFile {
                    path: &two,
                    bytes: b"second",
                },
            ],
            PREFIX,
        )
        .unwrap();

        assert_eq!(left_over, Vec::<PathBuf>::new());
        assert_eq!(fs::read(&one).unwrap(), b"first");
        assert_eq!(fs::read(&two).unwrap(), b"second");
        assert_eq!(names(dir.path()), ["one.key", "two.crt"]);
        #[cfg(unix)]
        for path in [&one, &two] {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn a_temp_file_sits_next_to_its_target() {
        let target = Path::new("folder").join("ca.key");

        assert_eq!(
            temp_path(&target, "pki-init-tmp-"),
            Path::new("folder").join("pki-init-tmp-ca.key")
        );
    }

    #[test]
    fn every_existing_target_and_leftover_is_named_and_nothing_written() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.key");
        let two = dir.path().join("two.crt");
        let three = dir.path().join("three.crt");
        fs::write(&one, b"kept").unwrap();
        fs::create_dir(&two).unwrap();
        fs::write(temp_path(&three, PREFIX), b"left").unwrap();

        let problems = check(&[&one, &two, &three], PREFIX);

        let shown: Vec<String> = problems.iter().map(ToString::to_string).collect();
        assert_eq!(problems.len(), 3, "{shown:?}");
        assert!(matches!(&problems[0], FileProblem::Exists { path } if *path == one));
        assert!(matches!(&problems[1], FileProblem::Exists { path } if *path == two));
        assert!(
            matches!(&problems[2], FileProblem::LeftOver { path } if *path == temp_path(&three, PREFIX))
        );
        assert_eq!(fs::read(&one).unwrap(), b"kept");
    }

    #[test]
    fn a_missing_folder_is_named_and_never_created() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("missing").join("ca.key");

        let problems = check(&[&target], PREFIX);

        assert!(matches!(&problems[..], [FileProblem::NoFolder { path }] if *path == target));
        assert!(!dir.path().join("missing").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_counts_as_existing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("ca.key");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &target).unwrap();

        let problems = check(&[&target], PREFIX);

        assert!(matches!(&problems[..], [FileProblem::Exists { path }] if *path == target));
    }

    #[test]
    fn a_file_appearing_meanwhile_is_kept_and_only_this_run_s_files_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.key");
        let two = dir.path().join("two.key");
        let three = dir.path().join("three.crt");
        assert!(check(&[&one, &two, &three], PREFIX).is_empty());
        fs::write(&three, b"someone else's").unwrap();

        let error = publish(
            &[
                NewFile {
                    path: &one,
                    bytes: b"first",
                },
                NewFile {
                    path: &two,
                    bytes: b"second",
                },
                NewFile {
                    path: &three,
                    bytes: b"third",
                },
            ],
            PREFIX,
        )
        .unwrap_err();

        assert!(
            matches!(&error, PublishError::Link { path, kept, .. } if *path == three && kept.is_empty()),
            "{error:?}"
        );
        assert_eq!(names(dir.path()), ["three.crt"]);
        assert_eq!(fs::read(&three).unwrap(), b"someone else's");
    }

    #[test]
    fn a_published_name_replaced_meanwhile_is_kept_and_named() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.key");
        let two = dir.path().join("two.crt");
        fs::write(&two, b"in the way").unwrap();

        // `one` is published, then replaced by someone else's file before
        // `two`'s link fails on the file in the way.
        let error = publish_with(
            &[
                NewFile {
                    path: &one,
                    bytes: b"first",
                },
                NewFile {
                    path: &two,
                    bytes: b"second",
                },
            ],
            PREFIX,
            |linked| {
                if linked == one {
                    fs::remove_file(&one).unwrap();
                    fs::write(&one, b"replaced meanwhile").unwrap();
                }
            },
        )
        .unwrap_err();

        assert!(
            matches!(&error, PublishError::Link { path, kept, .. } if *path == two && *kept == [one.clone()]),
            "{error:?}"
        );
        assert_eq!(fs::read(&one).unwrap(), b"replaced meanwhile");
        assert_eq!(fs::read(&two).unwrap(), b"in the way");
        assert_eq!(names(dir.path()), ["one.key", "two.crt"]);
    }

    #[test]
    fn a_temp_file_that_cant_be_written_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one.key");
        let two = dir.path().join("two.key");
        assert!(check(&[&one, &two], PREFIX).is_empty());
        fs::create_dir(temp_path(&two, PREFIX)).unwrap();

        let error = publish(
            &[
                NewFile {
                    path: &one,
                    bytes: b"first",
                },
                NewFile {
                    path: &two,
                    bytes: b"second",
                },
            ],
            PREFIX,
        )
        .unwrap_err();

        assert!(
            matches!(&error, PublishError::Write { path, .. } if *path == temp_path(&two, PREFIX)),
            "{error:?}"
        );
        assert_eq!(names(dir.path()), [format!("{PREFIX}two.key")]);
    }

    #[test]
    fn a_new_files_debug_shows_its_path_and_never_its_bytes() {
        let path = Path::new("ca.key");
        let file = NewFile {
            path,
            bytes: b"-----BEGIN PRIVATE KEY-----\nSECRETSECRET\n-----END PRIVATE KEY-----\n",
        };

        let shown = format!("{file:?}");

        assert!(shown.contains("ca.key"), "{shown}");
        assert!(!shown.contains("PRIVATE KEY"), "{shown}");
        assert!(!shown.contains("SECRETSECRET"), "{shown}");
        assert!(!shown.contains("45, 45, 45"), "{shown}");
    }
}
