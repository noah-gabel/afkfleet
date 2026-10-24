//! The retention task (ADR-0020, U20, R52, R53, R54): it records one
//! `retention.purge` entry after its runs once they deleted something, and
//! one at the stop; a summary that fails keeps its counts for the next try;
//! and its logs carry counts, never a line's text or its sender. Real time,
//! with intervals of milliseconds; every wait is bounded and wakes on the
//! commit count.

use core::time::Duration;

use fleet_core::audit::{AuditAction, RecordedValue};
use fleet_core::id::BotId;
use fleet_server::app::retention::RetentionTimings;
use fleet_server::cli::retention::Retention;
use fleet_server::infra::sqlite::Database;
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
