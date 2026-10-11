//! Reading a secret from a file, for every `*_file` key of both binaries
//! (Plan.md P7.4, ADR-0016): the server's vault keys now, its PKI keys and
//! the agent's `key_file` in P10.
//!
//! [`read`] applies one set of rules:
//!
//! - **Only a regular file.** The path is `stat`ed first, following
//!   symlinks, and anything else (a folder, a FIFO, a device) is refused
//!   without opening it: opening a FIFO would block startup forever.
//! - **No gap between the checks and the read.** The file is then opened,
//!   every check runs again on the opened handle's own metadata, and the
//!   bytes are read from that same handle, so a file swapped in between is
//!   never used.
//! - **Owner-only on Unix.** Any group or other bit (`mode & 0o077`) is
//!   refused, as for the database (ADR-0015). Windows uses ACLs, so there's
//!   no mode check there.
//! - **Not empty, at most [`MAX_BYTES`].** The size is checked on the handle,
//!   and the read is capped one byte past the limit, so a file that grows
//!   after the check is still caught.
//! - **One trailing `\n` or `\r\n` is removed,** as editors and `echo` add
//!   one; nothing else is trimmed.
//! - **Errors name the file and the fix, never the content.** The bytes
//!   live in a buffer that's wiped on every path, and they're handed over
//!   in a [`SecretBox`] (redacted `Debug`, zeroized on drop). The buffer is
//!   allocated once at its full size, so reading never leaves an unwiped
//!   copy behind.

use std::fs::{self, File, Metadata};
use std::io::{self, Read as _};
use std::mem;
use std::path::{Path, PathBuf};

use secrecy::SecretBox;
use zeroize::Zeroizing;

/// The largest secret file read: 64 KiB, far above any key or certificate.
pub const MAX_BYTES: usize = 64 * 1024;

/// Why a secret file can't be used. Every message names the file, never any
/// of its bytes.
#[derive(Debug, thiserror::Error)]
#[error("the secret file {} {problem}", path.display())]
pub struct SecretFileError {
    /// The file.
    pub path: PathBuf,
    /// What's wrong with it.
    #[source]
    pub problem: SecretFileProblem,
}

/// What's wrong with a secret file.
#[derive(Debug, thiserror::Error)]
pub enum SecretFileProblem {
    /// It can't be read: it doesn't exist, or the OS refused.
    #[error("can't be read")]
    Unreadable(#[source] io::Error),
    /// It's a folder, a FIFO, a device or anything else but a regular file.
    #[error("isn't a regular file")]
    NotAFile,
    /// Other users may read or write it (Unix).
    #[error(
        "can be read or written by other users (mode {mode:o}): run `chmod 600` on it, owned by the user the process runs as"
    )]
    Permissions {
        /// Its permission bits.
        mode: u32,
    },
    /// It's empty, or holds only a line break.
    #[error("is empty")]
    Empty,
    /// It's larger than [`MAX_BYTES`].
    #[error("is larger than {max} bytes")]
    TooLarge {
        /// The limit.
        max: usize,
    },
}

/// Reads the secret in the file at `path`, by the rules in the
/// [module docs](self).
///
/// # Errors
/// [`SecretFileError`] naming the file and a [`SecretFileProblem`].
pub fn read(path: &Path) -> Result<SecretBox<Vec<u8>>, SecretFileError> {
    let fail = |problem| SecretFileError {
        path: path.to_owned(),
        problem,
    };
    // Before opening: a FIFO would block `open` until a writer appears.
    let before = fs::metadata(path).map_err(|error| fail(SecretFileProblem::Unreadable(error)))?;
    if !before.is_file() {
        return Err(fail(SecretFileProblem::NotAFile));
    }
    let file = File::open(path).map_err(|error| fail(SecretFileProblem::Unreadable(error)))?;
    // Every check again, on what was actually opened.
    let metadata = file
        .metadata()
        .map_err(|error| fail(SecretFileProblem::Unreadable(error)))?;
    check(&metadata).map_err(fail)?;
    // Allocated once at its full size, so the read never moves the bytes
    // and leaves an unwiped copy behind.
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_BYTES + 1));
    file.take(u64::try_from(MAX_BYTES + 1).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .map_err(|error| fail(SecretFileProblem::Unreadable(error)))?;
    if bytes.len() > MAX_BYTES {
        return Err(fail(SecretFileProblem::TooLarge { max: MAX_BYTES }));
    }
    let line_break = if bytes.ends_with(b"\r\n") {
        2
    } else {
        usize::from(bytes.ends_with(b"\n"))
    };
    let len = bytes.len().saturating_sub(line_break);
    bytes.truncate(len);
    if bytes.is_empty() {
        return Err(fail(SecretFileProblem::Empty));
    }
    Ok(SecretBox::new(Box::new(mem::take(&mut *bytes))))
}

/// The checks on an opened file's metadata: a regular file, owner-only on
/// Unix, not empty, not larger than [`MAX_BYTES`].
fn check(metadata: &Metadata) -> Result<(), SecretFileProblem> {
    if !metadata.is_file() {
        return Err(SecretFileProblem::NotAFile);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = metadata.permissions().mode() & 0o7777;
        if mode & 0o077 != 0 {
            return Err(SecretFileProblem::Permissions { mode });
        }
    }
    if metadata.len() == 0 {
        return Err(SecretFileProblem::Empty);
    }
    if metadata.len() > u64::try_from(MAX_BYTES).unwrap_or(u64::MAX) {
        return Err(SecretFileProblem::TooLarge { max: MAX_BYTES });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rstest::rstest;
    use secrecy::ExposeSecret as _;
    use tempfile::TempDir;

    use super::*;

    /// A marker that must never appear in an error or a `Debug` output.
    const MARKER: &str = "super-secret-marker";

    fn folder() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    /// Writes `content` to `name` in `dir`, owner-only on Unix.
    fn secret_file(dir: &TempDir, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.path().join(name);
        fs::write(&path, content).unwrap();
        set_mode(&path, 0o600);
        path
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(not(unix))]
    fn set_mode(_path: &Path, _mode: u32) {}

    fn problem(result: Result<SecretBox<Vec<u8>>, SecretFileError>) -> SecretFileProblem {
        match result {
            Err(error) => error.problem,
            Ok(_) => panic!("expected an error"),
        }
    }

    #[rstest]
    #[case::plain(b"key material".as_slice(), b"key material".as_slice())]
    #[case::trailing_lf(b"key material\n", b"key material")]
    #[case::trailing_crlf(b"key material\r\n", b"key material")]
    #[case::only_one_lf_removed(b"key material\n\n", b"key material\n")]
    #[case::a_lone_cr_stays(b"key material\r", b"key material\r")]
    #[case::inner_line_breaks_stay(b"line one\nline two\n", b"line one\nline two")]
    #[case::binary(&[0, 159, 146, 150, 255], &[0, 159, 146, 150, 255])]
    fn the_content_is_read_with_one_trailing_line_break_removed(
        #[case] content: &[u8],
        #[case] expected: &[u8],
    ) {
        let dir = folder();
        let path = secret_file(&dir, "secret", content);

        let secret = read(&path).unwrap();

        assert_eq!(secret.expose_secret().as_slice(), expected);
    }

    #[test]
    fn exactly_the_limit_is_read() {
        let dir = folder();
        let path = secret_file(&dir, "secret", &vec![b'a'; MAX_BYTES]);

        let secret = read(&path).unwrap();

        assert_eq!(secret.expose_secret().len(), MAX_BYTES);
    }

    #[test]
    fn one_byte_past_the_limit_is_too_large() {
        let dir = folder();
        let path = secret_file(&dir, "secret", &vec![b'a'; MAX_BYTES + 1]);

        assert!(matches!(
            problem(read(&path)),
            SecretFileProblem::TooLarge { max: MAX_BYTES }
        ));
    }

    #[rstest]
    #[case::empty(b"".as_slice())]
    #[case::only_a_line_break(b"\n")]
    #[case::only_crlf(b"\r\n")]
    fn an_empty_file_is_refused(#[case] content: &[u8]) {
        let dir = folder();
        let path = secret_file(&dir, "secret", content);

        assert!(matches!(problem(read(&path)), SecretFileProblem::Empty));
    }

    #[test]
    fn a_missing_file_is_unreadable() {
        let dir = folder();

        let problem = problem(read(&dir.path().join("missing")));

        assert!(
            matches!(&problem, SecretFileProblem::Unreadable(error) if error.kind() == io::ErrorKind::NotFound),
            "{problem:?}"
        );
    }

    #[test]
    fn a_folder_is_not_a_file() {
        let dir = folder();

        assert!(matches!(
            problem(read(dir.path())),
            SecretFileProblem::NotAFile
        ));
    }

    #[test]
    fn errors_name_the_file_and_never_its_content() {
        let dir = folder();
        let mut content = MARKER.repeat(MAX_BYTES / MARKER.len() + 2).into_bytes();
        content.truncate(MAX_BYTES + 1);
        let path = secret_file(&dir, "too-large", &content);

        let error = read(&path).unwrap_err();

        let shown = format!("{error} {error:?}");
        assert!(shown.contains("too-large"), "{shown}");
        assert!(!shown.contains(MARKER), "{shown}");
    }

    #[test]
    fn the_secret_s_debug_is_redacted() {
        let dir = folder();
        let path = secret_file(&dir, "secret", MARKER.as_bytes());

        let secret = read(&path).unwrap();

        assert!(!format!("{secret:?}").contains(MARKER));
    }

    #[cfg(unix)]
    #[rstest]
    #[case::group_read(0o640)]
    #[case::group_write(0o620)]
    #[case::group_execute(0o610)]
    #[case::other_read(0o604)]
    #[case::other_write(0o602)]
    #[case::other_execute(0o601)]
    #[case::everyone(0o777)]
    fn group_or_other_bits_are_refused(#[case] mode: u32) {
        let dir = folder();
        let path = secret_file(&dir, "secret", MARKER.as_bytes());
        set_mode(&path, mode);

        let error = read(&path).unwrap_err();

        assert!(
            matches!(error.problem, SecretFileProblem::Permissions { mode: found } if found & 0o777 == mode),
            "{error:?}"
        );
        let shown = error.to_string();
        assert!(shown.contains("chmod 600"), "{shown}");
        assert!(!shown.contains(MARKER), "{shown}");
    }

    /// Checks bits, not access, so it holds when the tests run as root too.
    #[cfg(unix)]
    #[rstest]
    #[case::owner_read_write(0o600)]
    #[case::owner_read(0o400)]
    #[case::owner_all(0o700)]
    fn owner_only_modes_pass(#[case] mode: u32) {
        let dir = folder();
        let path = secret_file(&dir, "secret", MARKER.as_bytes());
        set_mode(&path, mode);

        assert!(read(&path).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_secret_file_is_followed() {
        let dir = folder();
        let target = secret_file(&dir, "target", b"key material\n");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let secret = read(&link).unwrap();

        assert_eq!(secret.expose_secret().as_slice(), b"key material");
    }

    /// Opening a FIFO blocks until a writer appears, so the read runs on a
    /// thread of its own and the test fails after a few seconds instead of
    /// hanging (nextest's default profile has no time limit).
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_without_blocking() {
        use std::sync::mpsc;
        use std::thread;
        use std::time::Duration;

        let dir = folder();
        let fifo = dir.path().join("fifo");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success(), "mkfifo failed");
        set_mode(&fifo, 0o600);
        let (sender, receiver) = mpsc::channel();
        let path = fifo.clone();
        thread::spawn(move || {
            let _ = sender.send(read(&path).map(|_| ()).map_err(|error| error.problem));
        });

        let result = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("reading a FIFO must not block");

        assert!(
            matches!(result, Err(SecretFileProblem::NotAFile)),
            "{result:?}"
        );
    }
}
