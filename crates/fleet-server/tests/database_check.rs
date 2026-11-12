//! Tests for SQLite's integrity check over the database file (Plan.md
//! P13.5, ADR-0033): `afkfleet-server database check`'s core, which the
//! chaos tests run after every fault.
//!
//! Real time: the database runs on sqlx's worker threads (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::path::{Path, PathBuf};

use fleet_server::config::DatabaseConfig;
use fleet_server::infra::sqlite::integrity::{Integrity, IntegrityError, check};
use fleet_server::infra::sqlite::{DatabaseOptions, OpenError, open};
use tempfile::TempDir;

fn config_in(dir: &Path) -> DatabaseConfig {
    DatabaseConfig {
        path: dir.join("afkfleet.db"),
        backups: None,
    }
}

/// A migrated database in `dir`, closed: one file, no `-wal`.
async fn migrated(dir: &Path) -> DatabaseConfig {
    let config = config_in(dir);
    open(&config, DatabaseOptions::default())
        .await
        .unwrap()
        .close()
        .await;
    config
}

/// The path of the `-wal` or `-shm` file next to `db`.
fn sibling(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// SQLite's default page size, which the server never changes.
const PAGE: usize = 4096;

#[tokio::test]
async fn a_migrated_database_is_sound() {
    let dir = TempDir::new().unwrap();
    let config = migrated(dir.path()).await;

    let found = check(&config, &DatabaseOptions::default()).await;

    assert!(matches!(found, Ok(Integrity::Sound)), "{found:?}");
}

/// The file's bytes stay as they are. Beside a database nobody has open,
/// SQLite itself leaves the two files every connection to a WAL database
/// makes, a `-wal` that holds nothing and its `-shm`: a read-only connection
/// can't remove them, and the next start does.
#[tokio::test]
async fn the_check_changes_no_byte_of_the_database_and_writes_nothing_into_a_wal() {
    let dir = TempDir::new().unwrap();
    let config = migrated(dir.path()).await;
    let before = std::fs::read(&config.path).unwrap();

    check(&config, &DatabaseOptions::default()).await.unwrap();

    assert_eq!(std::fs::read(&config.path).unwrap(), before);
    let wal = sibling(&config.path, "-wal");
    assert_eq!(
        std::fs::metadata(&wal).map_or(0, |found| found.len()),
        0,
        "the check wrote into the -wal"
    );
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.retain(|name| !["afkfleet.db", "afkfleet.db-wal", "afkfleet.db-shm"].contains(&&**name));
    assert_eq!(names, [] as [&str; 0]);
    // The next start takes them as they are, and its close removes them.
    open(&config, DatabaseOptions::default())
        .await
        .unwrap()
        .close()
        .await;
    assert!(!wal.exists());
    assert!(!sibling(&config.path, "-shm").exists());
}

/// What a killed server leaves: the file and a `-wal` that was never
/// checkpointed. The check reads through it, as the next start will.
#[tokio::test]
async fn a_database_with_a_leftover_wal_is_checked_through_it() {
    let dir = TempDir::new().unwrap();
    let live = TempDir::new().unwrap();
    let config = config_in(live.path());
    let database = open(&config, DatabaseOptions::default()).await.unwrap();
    // Copied while the connection is open: the migrations are in the -wal.
    let left = config_in(dir.path());
    std::fs::copy(&config.path, &left.path).unwrap();
    let wal = sibling(&config.path, "-wal");
    assert!(
        std::fs::metadata(&wal).unwrap().len() > 0,
        "the -wal is hot"
    );
    std::fs::copy(&wal, sibling(&left.path, "-wal")).unwrap();
    database.close().await;
    #[cfg(unix)]
    for file in [left.path.clone(), sibling(&left.path, "-wal")] {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let found = check(&left, &DatabaseOptions::default()).await;

    assert!(matches!(found, Ok(Integrity::Sound)), "{found:?}");
    assert!(
        sibling(&left.path, "-wal").exists(),
        "the check never checkpoints: the -wal is the next start's"
    );
}

#[tokio::test]
async fn a_page_overwritten_with_garbage_is_damage_with_sqlite_s_findings() {
    let dir = TempDir::new().unwrap();
    let config = migrated(dir.path()).await;
    let mut bytes = std::fs::read(&config.path).unwrap();
    assert!(bytes.len() > PAGE * 8, "the schema fills more than 8 pages");
    // Pages 4 to 7, well past the header and the schema's root page.
    bytes[PAGE * 3..PAGE * 7].fill(0xAA);
    std::fs::write(&config.path, bytes).unwrap();

    let found = check(&config, &DatabaseOptions::default()).await;

    let Ok(Integrity::Damaged(findings)) = &found else {
        panic!("not damage: {found:?}");
    };
    assert_ne!(findings.len(), 0);
    assert!(
        findings.iter().all(|finding| finding != "ok"),
        "{findings:?}"
    );
}

#[tokio::test]
async fn a_file_that_is_no_database_is_damage() {
    let dir = TempDir::new().unwrap();
    let config = migrated(dir.path()).await;
    let length = std::fs::read(&config.path).unwrap().len();
    std::fs::write(&config.path, vec![0x5A; length]).unwrap();

    let found = check(&config, &DatabaseOptions::default()).await;

    let Ok(Integrity::Damaged(findings)) = &found else {
        panic!("not damage: {found:?}");
    };
    assert_eq!(findings.len(), 1, "{findings:?}");
}

/// A file cut off in the middle, as a copy that ran out of room leaves it.
#[tokio::test]
async fn a_truncated_database_is_damage() {
    let dir = TempDir::new().unwrap();
    let config = migrated(dir.path()).await;
    let bytes = std::fs::read(&config.path).unwrap();
    std::fs::write(&config.path, &bytes[..PAGE * 5]).unwrap();

    let found = check(&config, &DatabaseOptions::default()).await;

    assert!(matches!(found, Ok(Integrity::Damaged(_))), "{found:?}");
}

#[tokio::test]
async fn a_missing_database_is_an_error_and_nothing_is_created() {
    let dir = TempDir::new().unwrap();
    let config = config_in(dir.path());

    let found = check(&config, &DatabaseOptions::default()).await;

    assert!(
        matches!(
            &found,
            Err(IntegrityError::File(OpenError::MissingFile { file })) if *file == config.path
        ),
        "{found:?}"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn a_missing_folder_is_an_error() {
    let dir = TempDir::new().unwrap();
    let config = config_in(&dir.path().join("typo"));

    let found = check(&config, &DatabaseOptions::default()).await;

    assert!(
        matches!(
            &found,
            Err(IntegrityError::File(OpenError::MissingFolder { .. }))
        ),
        "{found:?}"
    );
}

/// An empty file is what `migrate` leaves when it's stopped right after
/// creating it: an empty database, and sound.
#[tokio::test]
async fn an_empty_file_is_sound() {
    let dir = TempDir::new().unwrap();
    let config = config_in(dir.path());
    std::fs::write(&config.path, b"").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&config.path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let found = check(&config, &DatabaseOptions::default()).await;

    assert!(matches!(found, Ok(Integrity::Sound)), "{found:?}");
}
