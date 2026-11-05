//! One backup file and the backups' folder (ADR-0022, S2, S4, R1, R2).

use core::time::Duration;

use fleet_core::backup::BackupName;
use fleet_server::infra::sqlite::DatabaseOptions;
use fleet_server::infra::sqlite::backup::{
    BackupError, Listing, check_folder, check_placement, list, remove, write,
};
use tokio_util::sync::CancellationToken;

use super::fixtures::{CANARY, at, holds, names, opens_as_a_database, setup};

const BOUND: Duration = Duration::from_secs(60);

fn name(text: &str) -> BackupName {
    BackupName::at(at(text)).unwrap()
}

#[tokio::test]
async fn a_backup_is_a_checked_copy_named_for_its_second() {
    let setup = setup().await;
    let taken = name("2027-01-15T08:00:42Z");

    let written = write(
        &setup.database,
        &setup.backups,
        taken,
        &CancellationToken::new(),
        BOUND,
        &DatabaseOptions::default(),
    )
    .await
    .unwrap();
    setup.db.close().await;

    let file = setup.backups.join("afkfleet-backup-20270115T080042Z.db");
    assert_eq!(written.name, taken);
    assert_eq!(written.file, file);
    assert_eq!(written.bytes, std::fs::metadata(&file).unwrap().len());
    assert_eq!(
        names(&setup.backups),
        ["afkfleet-backup-20270115T080042Z.db"]
    );
    assert!(holds(&file, CANARY));
    assert!(opens_as_a_database(&file).await);
}

#[cfg(unix)]
#[tokio::test]
async fn a_backup_is_readable_only_by_the_servers_user() {
    use std::os::unix::fs::PermissionsExt;
    let setup = setup().await;

    let written = write(
        &setup.database,
        &setup.backups,
        name("2027-01-15T08:00:00Z"),
        &CancellationToken::new(),
        BOUND,
        &DatabaseOptions::default(),
    )
    .await
    .unwrap();

    let mode = std::fs::metadata(&written.file)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[tokio::test]
async fn a_backup_never_replaces_one_of_its_name() {
    let setup = setup().await;
    let file = setup.backups.join("afkfleet-backup-20270115T080000Z.db");
    std::fs::write(&file, "an earlier backup").unwrap();

    let result = write(
        &setup.database,
        &setup.backups,
        name("2027-01-15T08:00:00Z"),
        &CancellationToken::new(),
        BOUND,
        &DatabaseOptions::default(),
    )
    .await;

    assert!(matches!(result, Err(BackupError::Taken { file: taken }) if taken == file));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "an earlier backup");
    assert_eq!(
        names(&setup.backups),
        ["afkfleet-backup-20270115T080000Z.db"]
    );
}

#[tokio::test]
async fn a_stopped_backup_leaves_nothing() {
    let setup = setup().await;
    let stop = CancellationToken::new();
    stop.cancel();

    let result = write(
        &setup.database,
        &setup.backups,
        name("2027-01-15T08:00:00Z"),
        &stop,
        BOUND,
        &DatabaseOptions::default(),
    )
    .await;

    assert!(matches!(result, Err(BackupError::Stopped { .. })));
    assert_eq!(names(&setup.backups), [] as [&str; 0]);
}

/// The bound ends the copy through SQLite's progress handler, mid-copy.
#[tokio::test]
async fn a_backup_past_its_bound_is_stopped_and_leaves_nothing() {
    let setup = setup().await;

    let result = write(
        &setup.database,
        &setup.backups,
        name("2027-01-15T08:00:00Z"),
        &CancellationToken::new(),
        Duration::ZERO,
        &DatabaseOptions::default(),
    )
    .await;

    assert!(matches!(result, Err(BackupError::Stopped { .. })));
    assert_eq!(names(&setup.backups), [] as [&str; 0]);
}

#[tokio::test]
async fn a_backup_into_a_missing_folder_creates_nothing() {
    let setup = setup().await;
    let missing = setup.backups.join("missing");

    let result = write(
        &setup.database,
        &missing,
        name("2027-01-15T08:00:00Z"),
        &CancellationToken::new(),
        BOUND,
        &DatabaseOptions::default(),
    )
    .await;

    assert!(matches!(
        result,
        Err(BackupError::Io {
            action: "create",
            ..
        })
    ));
    assert!(!missing.exists());
}

#[tokio::test]
async fn a_backup_of_a_missing_database_leaves_nothing() {
    let setup = setup().await;

    let result = write(
        &setup.backups.join("none.db"),
        &setup.backups,
        name("2027-01-15T08:00:00Z"),
        &CancellationToken::new(),
        BOUND,
        &DatabaseOptions::default(),
    )
    .await;

    assert!(matches!(result, Err(BackupError::Copy { .. })));
    assert_eq!(names(&setup.backups), [] as [&str; 0]);
}

#[tokio::test]
async fn the_backups_folder_must_be_a_folder_that_exists() {
    let setup = setup().await;
    let file = setup.backups.join("a-file");
    std::fs::write(&file, "").unwrap();

    assert!(check_folder(&setup.backups).await.is_ok());
    assert!(matches!(
        check_folder(&setup.backups.join("missing")).await,
        Err(BackupError::MissingFolder { .. })
    ));
    assert!(matches!(
        check_folder(&file).await,
        Err(BackupError::NotAFolder { .. })
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn the_backups_folder_is_never_a_link() {
    let setup = setup().await;
    let link = setup.root.path().join("link");
    std::os::unix::fs::symlink(&setup.backups, &link).unwrap();

    assert!(matches!(
        check_folder(&link).await,
        Err(BackupError::Link { .. })
    ));
}

#[tokio::test]
async fn on_disk_the_backups_folder_holds_neither_the_database_nor_a_key() {
    let setup = setup().await;
    let data = setup.database.parent().unwrap().to_owned();
    let key = setup.backups.join("vault-key-2");
    let elsewhere = setup.root.path().join("vault-key-1");

    assert!(
        check_placement(
            &setup.backups,
            &setup.database,
            core::slice::from_ref(&elsewhere)
        )
        .await
        .is_ok()
    );
    assert!(matches!(
        check_placement(&data, &setup.database, &[]).await,
        Err(BackupError::HoldsDatabase { .. })
    ));
    assert!(matches!(
        check_placement(&setup.backups, &setup.database, &[elsewhere, key.clone()]).await,
        Err(BackupError::HoldsKey { file, .. }) if file == key
    ));
}

#[tokio::test]
async fn the_listing_counts_only_the_servers_own_regular_files() {
    let setup = setup().await;
    let dir = &setup.backups;
    for file in [
        "afkfleet-backup-20270115T080000Z.db",
        "afkfleet-backup-20270114T080000Z.db",
        ".afkfleet-backup-20270115T090000Z.db.partial",
        "afkfleet-backup-20270115T080000Z.db.bak",
        "notes.txt",
    ] {
        std::fs::write(dir.join(file), "").unwrap();
    }
    std::fs::create_dir(dir.join("afkfleet-backup-20270113T080000Z.db")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        dir.join("notes.txt"),
        dir.join("afkfleet-backup-20270112T080000Z.db"),
    )
    .unwrap();

    let listing = list(dir).await.unwrap();

    assert_eq!(
        listing,
        Listing {
            finished: vec![name("2027-01-14T08:00:00Z"), name("2027-01-15T08:00:00Z")],
            partial: vec![name("2027-01-15T09:00:00Z")],
        }
    );
}

#[tokio::test]
async fn removing_takes_exactly_the_names_given() {
    let setup = setup().await;
    let dir = &setup.backups;
    for file in [
        "afkfleet-backup-20270113T080000Z.db",
        "afkfleet-backup-20270114T080000Z.db",
        ".afkfleet-backup-20270114T090000Z.db.partial",
        ".afkfleet-backup-20270115T090000Z.db.partial",
    ] {
        std::fs::write(dir.join(file), "").unwrap();
    }

    remove(
        dir,
        &[name("2027-01-13T08:00:00Z"), name("2027-01-01T00:00:00Z")],
        &[name("2027-01-14T09:00:00Z")],
    )
    .await
    .unwrap();

    assert_eq!(
        names(dir),
        [
            ".afkfleet-backup-20270115T090000Z.db.partial",
            "afkfleet-backup-20270114T080000Z.db",
        ]
    );
}

#[tokio::test]
async fn listing_a_missing_folder_fails_with_its_name() {
    let setup = setup().await;
    let missing = setup.backups.join("missing");

    let result = list(&missing).await;

    assert!(matches!(result, Err(BackupError::Io { file, .. }) if file == missing));
}
