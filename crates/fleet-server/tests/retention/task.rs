//! The retention task (ADR-0020, U20, R52, R53, R54, R58): it records one
//! `retention.purge` entry after its runs once they deleted something, and
//! one at the stop; a summary waiting for the write connection never holds
//! up the stop; a summary that fails keeps its counts for the next try;
//! and its logs carry counts, never a line's text or its sender. Real time,
//! with intervals of milliseconds; every wait is bounded and wakes on the
//! commit count.

use core::time::Duration;

use fleet_core::audit::{AuditAction, RecordedValue};
use fleet_core::id::BotId;
use fleet_server::app::retention::{RetentionTimings, STOP_WAIT};
use fleet_server::cli::retention::Retention;
use fleet_server::infra::sqlite::Database;
use fleet_server::metrics::Metrics;
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditRecord};
use fleet_server::ports::store::Store;
use fleet_testkit::log_capture;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::time::{Instant, timeout};
use tokio_util::sync::CancellationToken;

use super::fixtures::{add_bot, add_user, database, days, line_ids, lines, millis, said};
use super::service::{PATIENCE, now, policy, service};
use super::watched::{Failure, WatchedStore};

/// The retention entries, newest first.
async fn summaries(db: &Database) -> Vec<AuditRecord> {
    let page = AuditPage {
        before: None,
        limit: AuditLimit::new(100).unwrap(),
    };
    db.audit()
        .list(page)
        .await
        .unwrap()
        .into_iter()
        .filter(|entry| entry.action == AuditAction::RETENTION_PURGE)
        .collect()
}

/// An integer of `entry`'s metadata.
fn int(entry: &AuditRecord, key: &str) -> i64 {
    entry
        .metadata
        .entries()
        .iter()
        .find_map(|(name, value)| match value {
            RecordedValue::Int(value) if name == key => Some(*value),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{key} in {:?}", entry.metadata))
}

/// Waits, within [`PATIENCE`], until `done` holds, checking again at every
/// commit.
async fn until<F, R>(db: &Database, mut done: F)
where
    F: FnMut() -> R,
    R: Future<Output = bool>,
{
    let deadline = Instant::now() + PATIENCE;
    let mut commits = db.commits();
    loop {
        commits.borrow_and_update();
        if done().await {
            return;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        timeout(left, commits.changed())
            .await
            .expect("done in time")
            .unwrap();
    }
}

/// The task over `store`, its runs `every` few milliseconds, summing up
/// after `summary_every` of them.
fn task(store: &WatchedStore, summary_every: u32) -> Retention {
    Retention::new(
        service(store, now(), policy(), Duration::ZERO),
        RetentionTimings {
            interval: Duration::from_millis(5),
            pause: Duration::ZERO,
            summary_every,
        },
    )
}

/// A bot with 150 lines past chat's cut by a millisecond, so a run deletes
/// 50 by size, and a store watching the database.
async fn fleet(db: &Database) -> (WatchedStore, BotId) {
    let user = add_user(db, 1).await;
    let bot = add_bot(db, user, 1).await;
    lines(db, bot, 150, now() - days(14) + millis(1)).await;
    (WatchedStore::new(db.clone()), bot)
}

#[sqlx::test(migrations = false)]
async fn the_task_sums_up_after_its_runs_once_they_deleted_something(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (store, _) = fleet(&db).await;
    let stop = CancellationToken::new();

    let running = tokio::spawn(task(&store, 3).run(stop.clone()));
    until(&db, || async { !summaries(&db).await.is_empty() }).await;
    stop.cancel();
    timeout(PATIENCE, running).await.unwrap().unwrap();

    let entries = summaries(&db).await;
    assert_eq!(entries.len(), 1, "nothing more at the stop: {entries:?}");
    assert_eq!(int(&entries[0], "chat_by_size"), 50);
    assert_eq!(int(&entries[0], "chat_per_bot"), 100);
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn the_task_sums_up_at_the_stop_what_it_deleted_since(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (store, bot) = fleet(&db).await;
    let stop = CancellationToken::new();

    let running = tokio::spawn(task(&store, 1000).run(stop.clone()));
    until(&db, || async { line_ids(&db, bot).await.len() == 100 }).await;
    stop.cancel();
    timeout(PATIENCE, running).await.unwrap().unwrap();

    let entries = summaries(&db).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(int(&entries[0], "chat_by_size"), 50);
    db.close().await;
}

/// The hourly summary waits for the write connection, which stays taken,
/// when the stop comes: it gives up at once, and the stop's own summary,
/// which waits at most [`STOP_WAIT`], records the same counts once, so the
/// task ends within about a second (R53, R58).
#[sqlx::test(migrations = false)]
async fn a_summary_waiting_for_the_write_connection_never_holds_up_the_stop(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (store, _) = fleet(&db).await;
    let mut held = store.hold_next_write();
    let stop = CancellationToken::new();

    let running = tokio::spawn(task(&store, 1).run(stop.clone()));
    held.reached(PATIENCE).await;
    stop.cancel();
    let ended = timeout(STOP_WAIT + PATIENCE, running).await;
    drop(held);

    ended
        .expect("the task ends within the stop's wait")
        .unwrap();
    let entries = summaries(&db).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(int(&entries[0], "chat_by_size"), 50);
    db.close().await;
}

/// The first summary fails: the next one, a run later, holds what the runs
/// deleted before it too, and it's the only entry.
#[sqlx::test(migrations = false)]
async fn a_summary_that_fails_keeps_its_counts_for_the_next_try(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (store, _) = fleet(&db).await;
    store.fail_next_record(Failure::Busy);
    let stop = CancellationToken::new();

    let running = tokio::spawn(task(&store, 2).run(stop.clone()));
    until(&db, || async { !summaries(&db).await.is_empty() }).await;
    stop.cancel();
    timeout(PATIENCE, running).await.unwrap().unwrap();

    let entries = summaries(&db).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(int(&entries[0], "chat_by_size"), 50);
    db.close().await;
}

/// The value of `series` in `metrics`' render, if it's there.
fn metric(metrics: &Metrics, series: &str) -> Option<String> {
    metrics.render().text().lines().find_map(|line| {
        line.strip_prefix(series)?
            .strip_prefix(' ')
            .map(str::to_owned)
    })
}

/// Waits, within [`PATIENCE`], until `series` reads `value`, checking at
/// each turn of the runtime: the metrics move after a run's last commit.
async fn until_metric(metrics: &Metrics, series: &str, value: &str) {
    timeout(PATIENCE, async {
        while metric(metrics, series).as_deref() != Some(value) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{series} never read {value}"));
}

/// P12.4 (ADR-0020, S45): each run counts the rows it deleted by kind, how
/// it ended and the batches it committed; a run with no failed phase sets
/// the time of the last success, which an alert reads.
#[sqlx::test(migrations = false)]
async fn a_run_counts_its_rows_its_end_and_the_time_of_its_success(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (store, _) = fleet(&db).await;
    let metrics = Metrics::prometheus();
    let stop = CancellationToken::new();

    let running = tokio::spawn(
        task(&store, 60)
            .with_metrics(metrics.recording().clone())
            .run(stop.clone()),
    );
    until_metric(
        &metrics,
        "afkfleet_retention_rows_deleted_total{kind=\"chat_by_size\"}",
        "50",
    )
    .await;
    stop.cancel();
    timeout(PATIENCE, running).await.unwrap().unwrap();

    assert_eq!(
        metric(
            &metrics,
            "afkfleet_retention_runs_total{outcome=\"failed\"}"
        )
        .as_deref(),
        Some("0")
    );
    assert_ne!(
        metric(
            &metrics,
            "afkfleet_retention_runs_total{outcome=\"success\"}"
        )
        .as_deref(),
        Some("0")
    );
    assert_eq!(
        metric(
            &metrics,
            "afkfleet_retention_last_success_timestamp_seconds"
        ),
        Some(now().timestamp().to_string()),
        "the service's clock"
    );
    db.close().await;
}

/// A run whose phase fails counts as failed, the phase by its name, and
/// leaves the time of the last success where it was.
#[sqlx::test(migrations = false)]
async fn a_failed_phase_counts_and_the_run_isnt_a_success(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (store, _) = fleet(&db).await;
    store.fail_write(1, Failure::Backend);
    let metrics = Metrics::prometheus();
    let stop = CancellationToken::new();

    let running = tokio::spawn(
        task(&store, 60)
            .with_metrics(metrics.recording().clone())
            .run(stop.clone()),
    );
    until_metric(
        &metrics,
        "afkfleet_retention_runs_total{outcome=\"failed\"}",
        "1",
    )
    .await;
    stop.cancel();
    timeout(PATIENCE, running).await.unwrap().unwrap();

    assert_eq!(
        metric(
            &metrics,
            "afkfleet_retention_phase_failures_total{phase=\"sessions\"}"
        )
        .as_deref(),
        Some("1")
    );
    db.close().await;
}

/// A player's line with a planted text and sender's name, past the bound:
/// the task deletes it and sums up at the stop, and no log line ever holds
/// either.
#[sqlx::test(migrations = false)]
async fn the_tasks_logs_carry_counts_never_a_lines_text_or_sender(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    log_capture::install().unwrap();
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let bot = add_bot(&db, user, 1).await;
    said(
        &db,
        bot,
        "CanarySender",
        "canary-text-of-a-deleted-line",
        now() - days(15),
    )
    .await;
    let store = WatchedStore::new(db.clone());
    let stop = CancellationToken::new();

    let running = tokio::spawn(task(&store, 1000).run(stop.clone()));
    until(&db, || async { line_ids(&db, bot).await.is_empty() }).await;
    stop.cancel();
    timeout(PATIENCE, running).await.unwrap().unwrap();

    assert_eq!(summaries(&db).await.len(), 1);
    log_capture::check_absent("canary-text-of-a-deleted-line").unwrap();
    log_capture::check_absent("CanarySender").unwrap();
    assert!(log_capture::count("fleet_server::cli::retention", "chat_by_age") > 0);
    db.close().await;
}
