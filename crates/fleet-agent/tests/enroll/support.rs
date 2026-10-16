//! Helpers shared by the `enroll` tests.

use std::fs;
use std::path::{Path, PathBuf};

/// A fresh, empty folder for test `name` under cargo's temp folder.
pub(crate) fn test_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("enroll")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The names in `dir`, sorted.
pub(crate) fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Asserts that `path` is readable and writable by its owner alone (Unix).
pub(crate) fn assert_owner_only(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{}", path.display());
    }
    #[cfg(not(unix))]
    let _ = path;
}
