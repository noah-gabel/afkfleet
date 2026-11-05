//! The task's backups (P12.3; ADR-0022, S3, S6): the step after each run
//! takes one when the newest is an interval old, waits an hour after a
//! failure, takes none after a restart that follows a recent one, and keeps
//! nothing of one the stop ended. Real time with a manual clock, as the
//! retention's other tests (sqlx never runs on paused time).

use core::time::Duration;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fleet_core::auth::SessionPolicy;
use fleet_core::backup::BackupPolicy;
use fleet_server::app::backup::BackupService;
use fleet_server::app::retention::{RetentionService, RetentionTimings};
use fleet_server::cli::retention::{BackupStep, Retention};
use fleet_server::config::BackupConfig;
use fleet_server::infra::sqlite::DatabaseOptions;
use fleet_testkit::system::ManualClock;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tempfile::TempDir;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use super::fixtures::database;
use super::service::{PATIENCE, now, policy};
use super::watched::WatchedStore;

/// The names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// The task over the database at `file`, with backups into `dir` when
/// given, both on `clock`, its runs every 5 ms.
fn task(
    store: &WatchedStore,
    file: &Path,
    dir: Option<&Path>,
    clock: &Arc<ManualClock>,
) -> Retention {
    let service = RetentionService::new(
        Arc::clone(clock) as _,
        Arc::new(store.clone()),
        policy(),
        SessionPolicy::default(),
    );
    let backups = dir.map(|dir| {
        BackupService::new(
            Arc::clone(clock) as _,
            file.to_owned(),
            BackupConfig {
                dir: dir.to_owned(),
                policy: BackupPolicy::default(),
            },
            Vec::new(),
            DatabaseOptions::default(),
        )
    });
    Retention::new(
        service,
        RetentionTimings {
            interval: Duration::from_millis(5),
            pause: Duration::ZERO,
            summary_every: 60,
        },
    )
    .with_backups(backups)
}

/// The database `options` names, a backups' folder for it, and a clock at
/// the retention tests' `now`.
async fn fleet(
    options: SqliteConnectOptions,
) -> (WatchedStore, PathBuf, TempDir, Arc<ManualClock>) {
    let file = options.get_filename().to_owned();
    let db = database(options).await;
    (
        WatchedStore::new(db),
        file,
        TempDir::new().unwrap(),
        Arc::new(ManualClock::new(now())),
    )
}

#[sqlx::test(migrations = false)]
async fn without_backup_dir_the_step_takes_none(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (store, file, _dir, clock) = fleet(options).await;

    let step = task(&store, &file, None, &clock)
        .back_up_when_due(&CancellationToken::new())
        .await;

    assert_eq!(step, BackupStep::Off);
}

#[sqlx::test(migrations = false)]
async fn a_backup_is_taken_when_due_and_then_once_an_interval(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (store, file, dir, clock) = fleet(options).await;
    let task = task(&store, &file, Some(dir.path()), &clock);
    let stop = CancellationToken::new();

    let first = task.back_up_when_due(&stop).await;
    let right_after = task.back_up_when_due(&stop).await;
    clock.advance(Duration::from_hours(24));
    let a_day_on = task.back_up_when_due(&stop).await;

    assert_eq!(
        [first, right_after, a_day_on],
        [BackupStep::Taken, BackupStep::NotDue, BackupStep::Taken]
    );
    assert_eq!(names(dir.path()).len(), 2);
}

#[sqlx::test(migrations = false)]
async fn a_failed_backup_is_tried_again_an_hour_later(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (store, file, dir, clock) = fleet(options).await;
    let backups = dir.path().join("backups");
    let task = task(&store, &file, Some(&backups), &clock);
    let stop = CancellationToken::new();

    let failed = task.back_up_when_due(&stop).await;
    std::fs::create_dir(&backups).unwrap();
    let at_once = task.back_up_when_due(&stop).await;
    clock.advance(Duration::from_mins(59));
    let a_minute_short = task.back_up_when_due(&stop).await;
    clock.advance(Duration::from_mins(1));
    let an_hour_on = task.back_up_when_due(&stop).await;

    assert_eq!(
        [failed, at_once, a_minute_short, an_hour_on],
        [
            BackupStep::Failed,
            BackupStep::Waiting,
            BackupStep::Waiting,
            BackupStep::Taken
        ]
    );
    assert_eq!(names(&backups).len(), 1);
}

#[sqlx::test(migrations = false)]
async fn a_restart_after_a_recent_backup_takes_none(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (store, file, dir, clock) = fleet(options).await;
    let stop = CancellationToken::new();
    task(&store, &file, Some(dir.path()), &clock)
        .back_up_when_due(&stop)
        .await;
    clock.advance(Duration::from_hours(1));

    // A new task, as after a restart, over the same folder.
    let step = task(&store, &file, Some(dir.path()), &clock)
        .back_up_when_due(&stop)
        .await;

    assert_eq!(step, BackupStep::NotDue);
    assert_eq!(names(dir.path()).len(), 1);
}

#[sqlx::test(migrations = false)]
async fn the_stop_ends_a_backup_and_keeps_nothing_of_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (store, file, dir, clock) = fleet(options).await;
    let stop = CancellationToken::new();
    stop.cancel();

    let step = task(&store, &file, Some(dir.path()), &clock)
        .back_up_when_due(&stop)
        .await;

    assert_eq!(step, BackupStep::Stopped);
    assert_eq!(names(dir.path()), [] as [&str; 0]);
}

/// The task itself takes the backup in its loop, after its run, and its
/// stop leaves no temporary file.
#[sqlx::test(migrations = false)]
async fn the_task_backs_up_in_its_loop_and_stops_cleanly(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (store, file, dir, clock) = fleet(options).await;
    let stop = CancellationToken::new();
    let running = tokio::spawn(task(&store, &file, Some(dir.path()), &clock).run(stop.clone()));

    // A backup commits nothing, so this checks the folder at each turn of
    // the runtime, bounded, until a finished backup is there.
    timeout(PATIENCE, async {
        while !names(dir.path())
            .iter()
            .any(|name| name.starts_with("afkfleet-backup-"))
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    stop.cancel();
    timeout(PATIENCE, running).await.unwrap().unwrap();

    let held = names(dir.path());
    assert_eq!(held.len(), 1, "{held:?}");
    assert!(held.iter().all(|name| name.starts_with("afkfleet-backup-")));
}
