//! The task's backups (P12.3; ADR-0022, S3, S6): the step after each run
//! takes one when the newest is an interval old, waits an hour after a
//! failure, takes none after a restart that follows a recent one, and keeps
//! nothing of one the stop ended. Real time with a manual clock, as the
//! retention's other tests (sqlx never runs on paused time).

use core::time::Duration;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fleet_core::auth::SessionPolicy;
use fleet_core::backup::{BackupName, BackupPolicy};
use fleet_server::app::backup::BackupService;
use fleet_server::app::retention::{RetentionService, RetentionTimings};
use fleet_server::cli::retention::{BackupStep, Retention};
use fleet_server::config::BackupConfig;
use fleet_server::infra::sqlite::DatabaseOptions;
use fleet_server::metrics::Metrics;
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

/// The value of `series` in `metrics`' render, if it's there.
fn metric(metrics: &Metrics, series: &str) -> Option<String> {
    metrics.render().text().lines().find_map(|line| {
        line.strip_prefix(series)?
            .strip_prefix(' ')
            .map(str::to_owned)
    })
}

/// P12.4 (ADR-0022, R4): a failed backup counts; one taken sets the time
/// of the newest, its size and how long it took.
#[sqlx::test(migrations = false)]
async fn a_failure_counts_and_a_taken_backup_sets_its_time_size_and_duration(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (store, file, dir, clock) = fleet(options).await;
    let backups = dir.path().join("backups");
    let metrics = Metrics::prometheus();
    let task =
        task(&store, &file, Some(&backups), &clock).with_metrics(metrics.recording().clone());
    let stop = CancellationToken::new();

    let failed = task.back_up_when_due(&stop).await;
    std::fs::create_dir(&backups).unwrap();
    clock.advance(Duration::from_hours(1));
    let taken = task.back_up_when_due(&stop).await;

    assert_eq!([failed, taken], [BackupStep::Failed, BackupStep::Taken]);
    assert_eq!(
        metric(&metrics, "afkfleet_backup_failures_total").as_deref(),
        Some("1")
    );
    assert_eq!(
        metric(&metrics, "afkfleet_backup_last_success_timestamp_seconds"),
        Some(
            (now() + chrono::TimeDelta::hours(1))
                .timestamp()
                .to_string()
        )
    );
    let size: u64 = metric(&metrics, "afkfleet_backup_last_size_bytes")
        .unwrap()
        .parse()
        .unwrap();
    let written = names(&backups);
    assert_eq!(
        size,
        std::fs::metadata(backups.join(&written[0])).unwrap().len()
    );
    assert!(metric(&metrics, "afkfleet_backup_last_duration_seconds").is_some());
    assert_eq!(
        metric(&metrics, "afkfleet_backup_prune_failures_total").as_deref(),
        Some("0")
    );
}

/// The newest backup's time is read from the folder's names at the task's
/// start, before its first run (R4): the `backup` command's counts, and one
/// dated past the clock's slack doesn't.
#[sqlx::test(migrations = false)]
async fn the_newest_backup_is_read_from_the_folder_at_the_start(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (store, file, dir, clock) = fleet(options).await;
    let earlier = BackupName::at(now() - chrono::TimeDelta::hours(3)).unwrap();
    let ahead = BackupName::at(now() + chrono::TimeDelta::hours(5)).unwrap();
    for name in [earlier, ahead] {
        std::fs::write(dir.path().join(name.file_name()), "").unwrap();
    }
    let metrics = Metrics::prometheus();
    let stop = CancellationToken::new();
    stop.cancel();

    // Stopped already: only the start's read runs.
    task(&store, &file, Some(dir.path()), &clock)
        .with_metrics(metrics.recording().clone())
        .run(stop)
        .await;

    assert_eq!(
        metric(&metrics, "afkfleet_backup_last_success_timestamp_seconds"),
        Some(earlier.stamp().timestamp().to_string())
    );
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
