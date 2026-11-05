//! The backups' service: when one is due, naming by the clock, pruning
//! (ADR-0022, S3, S4, R1).

use core::time::Duration;
use std::path::PathBuf;
use std::sync::Arc;

use fleet_core::backup::BackupPolicy;
use fleet_server::app::backup::BackupService;
use fleet_server::config::BackupConfig;
use fleet_server::infra::sqlite::DatabaseOptions;
use fleet_server::infra::sqlite::backup::BackupError;
use fleet_testkit::system::ManualClock;
use tokio_util::sync::CancellationToken;

use super::fixtures::{Setup, at, names, setup};

const HOUR: Duration = Duration::from_hours(1);

fn service(setup: &Setup, clock: &Arc<ManualClock>, keep: u32) -> BackupService {
    service_with_keys(setup, clock, keep, Vec::new())
}

fn service_with_keys(
    setup: &Setup,
    clock: &Arc<ManualClock>,
    keep: u32,
    key_files: Vec<PathBuf>,
) -> BackupService {
    BackupService::new(
        Arc::clone(clock) as _,
        setup.database.clone(),
        BackupConfig {
            dir: setup.backups.clone(),
            policy: BackupPolicy::try_new(24, keep).unwrap(),
        },
        key_files,
        DatabaseOptions::default(),
    )
}

#[tokio::test]
async fn a_backup_is_due_without_one_then_once_a_day() {
    let setup = setup().await;
    let clock = Arc::new(ManualClock::new(at("2027-01-15T08:00:00Z")));
    let backups = service(&setup, &clock, 14);

    let at_first = backups.is_due().await.unwrap();
    backups.take(&CancellationToken::new()).await.unwrap();
    let right_after = backups.is_due().await.unwrap();
    clock.advance(Duration::from_secs(24 * 3600 - 1));
    let a_second_short = backups.is_due().await.unwrap();
    clock.advance(Duration::from_secs(1));
    let a_day_on = backups.is_due().await.unwrap();

    assert!(at_first);
    assert!(!right_after);
    assert!(!a_second_short);
    assert!(a_day_on);
}

#[tokio::test]
async fn a_backup_is_named_for_the_clocks_second() {
    let setup = setup().await;
    let clock = Arc::new(ManualClock::new(at("2027-01-15T08:00:42.500Z")));

    let taken = service(&setup, &clock, 14)
        .take(&CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(
        taken.written.file,
        setup.backups.join("afkfleet-backup-20270115T080042Z.db")
    );
    assert_eq!(taken.pruned, 0);
    assert!(taken.prune_failed.is_none());
}

#[tokio::test]
async fn pruning_keeps_the_newest_backups() {
    let setup = setup().await;
    let clock = Arc::new(ManualClock::new(at("2027-01-13T08:00:00Z")));
    let backups = service(&setup, &clock, 2);

    let mut pruned = Vec::new();
    for _ in 0..3 {
        pruned.push(
            backups
                .take(&CancellationToken::new())
                .await
                .unwrap()
                .pruned,
        );
        clock.advance(24 * HOUR);
    }

    assert_eq!(pruned, [0, 0, 1]);
    assert_eq!(
        names(&setup.backups),
        [
            "afkfleet-backup-20270114T080000Z.db",
            "afkfleet-backup-20270115T080000Z.db",
        ]
    );
}

#[tokio::test]
async fn a_leftover_partial_goes_once_a_day_old_and_a_fresh_one_stays() {
    let setup = setup().await;
    let clock = Arc::new(ManualClock::new(at("2027-01-15T08:00:00Z")));
    for file in [
        ".afkfleet-backup-20270114T070000Z.db.partial",
        ".afkfleet-backup-20270115T070000Z.db.partial",
    ] {
        std::fs::write(setup.backups.join(file), "").unwrap();
    }

    service(&setup, &clock, 14)
        .take(&CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(
        names(&setup.backups),
        [
            ".afkfleet-backup-20270115T070000Z.db.partial",
            "afkfleet-backup-20270115T080000Z.db",
        ]
    );
}

/// A clock set back: a backup dated beyond the slack doesn't count, so one
/// is due, and the new one is never pruned for it.
#[tokio::test]
async fn a_backup_dated_ahead_of_the_clock_doesnt_count() {
    let setup = setup().await;
    let clock = Arc::new(ManualClock::new(at("2027-01-15T08:00:00Z")));
    std::fs::write(
        setup.backups.join("afkfleet-backup-20270115T100000Z.db"),
        "",
    )
    .unwrap();
    let backups = service(&setup, &clock, 1);

    let due = backups.is_due().await.unwrap();
    let taken = backups.take(&CancellationToken::new()).await.unwrap();

    assert!(due);
    assert_eq!(taken.pruned, 1);
    assert_eq!(
        names(&setup.backups),
        ["afkfleet-backup-20270115T080000Z.db"]
    );
}

#[tokio::test]
async fn a_clock_past_year_9999_names_no_backup() {
    let setup = setup().await;
    let clock = Arc::new(ManualClock::new(at("9999-12-31T23:59:59Z")));
    clock.advance(Duration::from_secs(1));

    let result = service(&setup, &clock, 14)
        .take(&CancellationToken::new())
        .await;

    assert!(matches!(result, Err(BackupError::Unnamed)));
    assert_eq!(names(&setup.backups), [] as [&str; 0]);
}

#[tokio::test]
async fn the_check_refuses_a_folder_holding_a_key_file() {
    let setup = setup().await;
    let clock = Arc::new(ManualClock::new(at("2027-01-15T08:00:00Z")));
    let key = setup.backups.join("ca.key");

    let clean = service(&setup, &clock, 14).check().await;
    let holding = service_with_keys(&setup, &clock, 14, vec![key])
        .check()
        .await;

    assert!(clean.is_ok());
    assert!(matches!(holding, Err(BackupError::HoldsKey { .. })));
}

#[tokio::test]
async fn the_check_refuses_a_missing_folder() {
    let setup = setup().await;
    let clock = Arc::new(ManualClock::new(at("2027-01-15T08:00:00Z")));
    std::fs::remove_dir(&setup.backups).unwrap();

    let result = service(&setup, &clock, 14).check().await;

    assert!(matches!(result, Err(BackupError::MissingFolder { .. })));
}
