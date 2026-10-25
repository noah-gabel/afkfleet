//! One retention run (ADR-0020, S45, S46, R51–R54): it deletes what each
//! phase holds due, in order, and nothing newer than either bound, lines
//! written after a bot's cut was read among them; a quiet run commits
//! nothing; the audit log keeps its own, longer retention; a waiting
//! ingest gets the write connection between two batches; a failed phase
//! keeps its earlier batches and the run goes on; the stop, between batches
//! or while waiting for the connection, commits nothing more; committed
//! batches are at least the pause apart; and the summary records every key.

use core::time::Duration;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditMetadata, AuditOutcome, RecordedMetadata};
use fleet_core::auth::SessionPolicy;
use fleet_core::id::BotId;
use fleet_core::retention::{AuditRetention, ChatBound, ChatRetention, RetentionPolicy};
use fleet_core::system::SecureRandom as _;
use fleet_server::app::retention::{
    Deleted, Phase, RetentionService, Run, STOP_WAIT, SummaryError, Tally,
};
use fleet_server::infra::sqlite::Database;
use fleet_server::ports::audit::{AuditEntryId, AuditLimit, AuditPage};
use fleet_server::ports::store::{Store, UnauditedWrite};
use fleet_testkit::log_capture;
use fleet_testkit::system::{ManualClock, SeededRandom};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio_util::sync::CancellationToken;

use super::fixtures::{
    T0, add_bot, add_invite, add_session, add_token, add_user, at, clear_chat, database, days,
    entries, entry_ids, invite_ids, line_ids, lines, millis, rotations, rotations_of, session_ids,
    token_ids, users_lines,
};
use super::watched::{Failure, WatchedStore};

/// How long a test waits for anything a run should do.
pub(crate) const PATIENCE: Duration = Duration::from_secs(10);

/// When the runs happen: 400 days after [`T0`], so the setup's own audit
/// entries are past the audit cut.
pub(crate) fn now() -> DateTime<Utc> {
    at(T0) + days(400)
}

/// Chat kept 14 days and 100 lines per bot, the audit log 90 days.
pub(crate) fn policy() -> RetentionPolicy {
    policy_keeping(100)
}

/// [`policy`], each bot keeping `lines`.
pub(crate) fn policy_keeping(lines: u32) -> RetentionPolicy {
    RetentionPolicy {
        chat: ChatRetention::try_new(14).unwrap(),
        bound: ChatBound::try_new(lines).unwrap(),
        audit: AuditRetention::try_new(90).unwrap(),
    }
}

/// The retention of `store` by `policy` at `now`, pausing `pause`.
pub(crate) fn service(
    store: &WatchedStore,
    now: DateTime<Utc>,
    policy: RetentionPolicy,
    pause: Duration,
) -> RetentionService {
    RetentionService::new(
        Arc::new(ManualClock::new(now)),
        Arc::new(store.clone()),
        policy,
        SessionPolicy::default(),
        pause,
    )
}

/// Where a run at [`now`] by [`policy`] cuts chat, sessions and the audit log.
fn chat_cut() -> DateTime<Utc> {
    now() - days(14)
}

fn session_cut() -> DateTime<Utc> {
    now() - days(30)
}

fn audit_cut() -> DateTime<Utc> {
    now() - days(90)
}

/// The kinds of delete in the log, in the order they first came.
fn delete_order(log: &[String]) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    for line in log {
        if let Some(kind) = line
            .strip_prefix("delete ")
            .and_then(|rest| rest.split(' ').next())
            && !order.iter().any(|seen| seen == kind)
        {
            order.push(kind.to_owned());
        }
    }
    order
}

/// A run, its stop never cancelled.
async fn run(service: &RetentionService) -> Run {
    service.run_once(&CancellationToken::new()).await
}

#[sqlx::test(migrations = false)]
async fn one_run_deletes_what_each_phase_holds_due_in_order_and_nothing_newer(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let bot = add_bot(&db, user, 1).await;
    lines(&db, bot, 5, chat_cut()).await;
    lines(&db, bot, 75, chat_cut() + millis(1)).await;
    users_lines(&db, bot, user, 75, chat_cut() + millis(1)).await;
    let expired = add_session(&db, user, 1, session_cut(), false).await;
    rotations(&db, expired, 3).await;
    let live = add_session(&db, user, 2, session_cut() + millis(1), false).await;
    rotations(&db, live, 2).await;
    let kept_invite = add_invite(&db, user, 2, audit_cut() + millis(1)).await;
    add_invite(&db, user, 1, audit_cut()).await;
    let kept_token = add_token(&db, 2, audit_cut() + millis(1)).await;
    add_token(&db, 1, audit_cut()).await;
    let old_entries = entry_ids(&db).await.len();
    entries(&db, 2, audit_cut() + millis(1)).await;
    let ids = line_ids(&db, bot).await;
    let store = WatchedStore::new(db.clone());

    let run = run(&service(&store, now(), policy(), Duration::ZERO)).await;

    assert_eq!(
        run.deleted,
        Deleted {
            chat_by_age: 5,
            chat_by_size: 50,
            sessions: 1,
            rotations: 3,
            audit_entries: u64::try_from(old_entries).unwrap(),
            invites: 1,
            enrollments: 1,
        }
    );
    assert_eq!((run.failed, run.stopped), (Vec::new(), false));
    assert_eq!(
        delete_order(&store.log()),
        [
            "rotations",
            "sessions",
            "chat_by_age",
            "chat_by_size",
            "audit",
            "invites",
            "enrollment_tokens"
        ]
    );
    assert_eq!(line_ids(&db, bot).await, ids[55..]);
    assert_eq!(session_ids(&db).await, [live.to_string()]);
    assert_eq!(rotations_of(&db, live).await, 2);
    assert_eq!(entry_ids(&db).await.len(), 2);
    assert_eq!(invite_ids(&db).await, [kept_invite.to_string()]);
    assert_eq!(token_ids(&db).await, [kept_token.to_string()]);
    db.close().await;
}

/// Nothing is due a day after the setup: the run asks for its writes, but
/// commits none, so the commit count never moves.
#[sqlx::test(migrations = false)]
async fn a_quiet_run_commits_nothing(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let bot = add_bot(&db, user, 1).await;
    lines(&db, bot, 50, at(T0)).await;
    let store = WatchedStore::new(db.clone());
    let before = *db.commits().borrow();

    let run = run(&service(&store, at(T0) + days(1), policy(), Duration::ZERO)).await;

    assert_eq!(run.deleted, Deleted::default());
    assert_eq!(*db.commits().borrow(), before);
    assert!(
        !store.log().iter().any(|line| line.starts_with("commit")),
        "{:?}",
        store.log()
    );
    db.close().await;
}

/// Draws a number below `below` from `random`.
fn draw(random: &SeededRandom, below: u32) -> u32 {
    let mut bytes = [0; 4];
    random.fill(&mut bytes).unwrap();
    u32::from_le_bytes(bytes) % below
}

/// Each bot's lines as `(id, received)`, oldest first.
async fn received(db: &Database, bot: BotId) -> Vec<(i64, String)> {
    let bot = bot.to_string();
    sqlx::query!(
        r#"SELECT id AS "id!: i64", at FROM chat_messages WHERE bot_id = ? ORDER BY id"#,
        bot
    )
    .fetch_all(db.read_pool())
    .await
    .unwrap()
    .into_iter()
    .map(|row| (row.id, row.at))
    .collect()
}

/// Ten seeded fleets of three bots, each with lines in random groups around
/// the age cut, users' lines among them: after one run, each bot holds
/// exactly its newest 100 lines received after the cut, never one fewer.
#[sqlx::test(migrations = false)]
async fn nothing_newer_than_either_bound_is_ever_deleted(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let bots = [
        add_bot(&db, user, 1).await,
        add_bot(&db, user, 2).await,
        add_bot(&db, user, 3).await,
    ];
    let store = WatchedStore::new(db.clone());
    let retention = service(&store, now(), policy(), Duration::ZERO);
    let ages = [
        chat_cut() - days(1),
        chat_cut(),
        chat_cut() + millis(1),
        chat_cut() + days(1),
    ];
    let cut = super::fixtures::stored(chat_cut());

    for seed in 0..10 {
        let random = SeededRandom::new(seed);
        for bot in bots {
            for _ in 0..draw(&random, 6) {
                let count = draw(&random, 80);
                let age = ages[usize::try_from(draw(&random, 4)).unwrap()];
                if draw(&random, 2) == 0 {
                    lines(&db, bot, count, age).await;
                } else {
                    users_lines(&db, bot, user, count, age).await;
                }
            }
        }
        let mut expected = Vec::new();
        for bot in bots {
            let newer: Vec<i64> = received(&db, bot)
                .await
                .into_iter()
                .filter(|(_, at)| *at > cut)
                .map(|(id, _)| id)
                .collect();
            expected.push(newer[newer.len().saturating_sub(100)..].to_vec());
        }

        run(&retention).await;

        for (bot, expected) in bots.into_iter().zip(expected) {
            assert_eq!(line_ids(&db, bot).await, expected, "seed {seed}");
        }
        clear_chat(&db).await;
    }
    db.close().await;
}

/// 150 lines past the age cut, and 30 more written while the bot's cut is
/// being read: the cut, read before them, deletes the 50 oldest, and the 30
/// newer ones stay with the newest 100.
#[sqlx::test(migrations = false)]
async fn lines_written_after_a_bots_cut_was_read_are_never_deleted(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let bot = add_bot(&db, user, 1).await;
    lines(&db, bot, 150, chat_cut() + millis(1)).await;
    let store = WatchedStore::new(db.clone());
    let retention = service(&store, now(), policy(), Duration::ZERO);
    let mut held = store.hold_next_chat_cut();

    let running = tokio::spawn(async move { retention.run_once(&CancellationToken::new()).await });
    held.reached(PATIENCE).await;
    lines(&db, bot, 30, chat_cut() + millis(1)).await;
    let ids = line_ids(&db, bot).await;
    held.release();
    let run = tokio::time::timeout(PATIENCE, running)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(run.deleted.chat_by_size, 50);
    assert_eq!(line_ids(&db, bot).await, ids[50..]);
    db.close().await;
}

/// Twenty days on, a line and an audit entry both twenty days old: the
/// line is past chat's 14 days, the entry within the audit log's 90.
#[sqlx::test(migrations = false)]
async fn the_audit_log_is_never_cut_before_its_own_retention(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let bot = add_bot(&db, user, 1).await;
    lines(&db, bot, 1, at(T0)).await;
    entries(&db, 1, at(T0)).await;
    let kept = entry_ids(&db).await;
    let store = WatchedStore::new(db.clone());

    let run = run(&service(
        &store,
        at(T0) + days(20),
        policy(),
        Duration::ZERO,
    ))
    .await;

    assert_eq!((run.deleted.chat_by_age, run.deleted.audit_entries), (1, 0));
    assert_eq!(line_ids(&db, bot).await, Vec::<i64>::new());
    assert_eq!(entry_ids(&db).await, kept);
    db.close().await;
}

/// An ingest tick asks for the write connection while the run's first
/// batch holds it: once that batch commits, the tick goes first, before the
/// run's second batch, even without a pause (the pool serves in order).
#[sqlx::test(migrations = false)]
async fn a_waiting_ingest_tick_commits_between_two_batches(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let bot = add_bot(&db, user, 1).await;
    lines(&db, bot, 2500, chat_cut()).await;
    let store = WatchedStore::new(db.clone());
    let retention = service(&store, now(), policy_keeping(200_000), Duration::ZERO);
    let mut held = store.hold_next_delete("chat_by_age");

    let running = tokio::spawn(async move { retention.run_once(&CancellationToken::new()).await });
    held.reached(PATIENCE).await;
    let ingest = store.clone();
    let ticking = tokio::spawn(async move {
        let tx = ingest.write().await.unwrap();
        tx.commit_unaudited(UnauditedWrite::EventBatch)
            .await
            .unwrap();
    });
    store.write_requested(PATIENCE).await;
    held.release();
    tokio::time::timeout(PATIENCE, ticking)
        .await
        .unwrap()
        .unwrap();
    let run = tokio::time::timeout(PATIENCE, running)
        .await
        .unwrap()
        .unwrap();

    let log = store.log();
    let position = |wanted: &str, nth: usize| {
        log.iter()
            .enumerate()
            .filter(|(_, line)| line.as_str() == wanted)
            .nth(nth)
            .map(|(at, _)| at)
            .unwrap()
    };
    assert_eq!(run.deleted.chat_by_age, 2500);
    assert!(
        position("commit RetentionPurge", 0) < position("commit EventBatch", 0)
            && position("commit EventBatch", 0) < position("delete chat_by_age 1000", 1),
        "{log:?}"
    );
    db.close().await;
}

/// Chat's second batch fails to get its write: the first batch stays, the phase is named in one `warn`, the later phases still
/// run, and the next run deletes the rest. Once for a busy store, once for
/// a failing one.
#[sqlx::test(migrations = false)]
async fn a_run_that_fails_midway_keeps_its_batches_and_goes_on(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    log_capture::install().unwrap();
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let store = WatchedStore::new(db.clone());
    let retention = service(&store, now(), policy_keeping(200_000), Duration::ZERO);

    for (n, failure) in [(1, Failure::Busy), (2, Failure::Backend)] {
        let bot = add_bot(&db, user, n).await;
        lines(&db, bot, 2500, chat_cut()).await;
        entries(&db, 3, audit_cut()).await;
        // The run's writes: the replaced tokens' batch, the sessions', then
        // chat's first and second.
        store.fail_write(4, failure);

        let failed = run(&retention).await;
        let after_failure = line_ids(&db, bot).await.len();
        let again = run(&retention).await;

        assert_eq!(failed.deleted.chat_by_age, 1000, "{failure:?}");
        assert_eq!(failed.failed, [Phase::ChatByAge], "{failure:?}");
        assert!(
            failed.deleted.audit_entries >= 3,
            "{failure:?}: the audit phase ran"
        );
        assert_eq!(after_failure, 1500, "{failure:?}");
        assert_eq!(again.deleted.chat_by_age, 1500, "{failure:?}");
        assert_eq!(again.failed, Vec::new(), "{failure:?}");
        assert_eq!(line_ids(&db, bot).await, Vec::<i64>::new(), "{failure:?}");
    }
    assert_eq!(
        log_capture::count("fleet_server::app::retention", "chat_by_age"),
        2,
        "one warn per failed phase, naming it"
    );
    db.close().await;
}

/// The stop during the pause after the first batch: the run ends there,
/// with that batch alone, and no later phase runs.
#[sqlx::test(migrations = false)]
async fn the_stop_between_batches_commits_nothing_more(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let bot = add_bot(&db, user, 1).await;
    lines(&db, bot, 2500, chat_cut()).await;
    entries(&db, 3, audit_cut()).await;
    let audit_before = entry_ids(&db).await;
    let store = WatchedStore::new(db.clone());
    let retention = service(
        &store,
        now(),
        policy_keeping(200_000),
        Duration::from_secs(60),
    );
    let stop = CancellationToken::new();
    let mut commits = db.commits();
    commits.borrow_and_update();

    let stopping = stop.clone();
    let running = tokio::spawn(async move { retention.run_once(&stopping).await });
    tokio::time::timeout(PATIENCE, commits.changed())
        .await
        .unwrap()
        .unwrap();
    stop.cancel();
    let run = tokio::time::timeout(PATIENCE, running)
        .await
        .unwrap()
        .unwrap();

    assert!(run.stopped);
    assert_eq!(run.deleted.chat_by_age, 1000);
    assert_eq!(line_ids(&db, bot).await.len(), 1500);
    assert_eq!(
        entry_ids(&db).await,
        audit_before,
        "the audit phase never ran"
    );
    db.close().await;
}

/// The stop while the run waits for the write connection, which the test
/// holds: once it's free, the run takes it and lets it go uncommitted.
#[sqlx::test(migrations = false)]
async fn the_stop_while_waiting_for_the_write_connection_commits_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let bot = add_bot(&db, user, 1).await;
    lines(&db, bot, 10, chat_cut()).await;
    // The run's first write: an expired session's replaced tokens.
    let expired = add_session(&db, user, 1, session_cut(), false).await;
    rotations(&db, expired, 3).await;
    let store = WatchedStore::new(db.clone());
    let retention = service(&store, now(), policy(), Duration::ZERO);
    let stop = CancellationToken::new();
    let taken = db.write().await.unwrap();
    let before = *db.commits().borrow();

    let stopping = stop.clone();
    let running = tokio::spawn(async move { retention.run_once(&stopping).await });
    store.write_requested(PATIENCE).await;
    stop.cancel();
    drop(taken);
    let run = tokio::time::timeout(PATIENCE, running)
        .await
        .unwrap()
        .unwrap();

    assert!(run.stopped);
    assert_eq!(run.deleted, Deleted::default());
    assert_eq!(line_ids(&db, bot).await.len(), 10);
    assert_eq!(rotations_of(&db, expired).await, 3);
    assert_eq!(*db.commits().borrow(), before);
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn committed_batches_are_at_least_the_pause_apart(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let bot = add_bot(&db, user, 1).await;
    lines(&db, bot, 2500, chat_cut()).await;
    let store = WatchedStore::new(db.clone());
    let pause = Duration::from_millis(50);

    run(&service(&store, now(), policy_keeping(200_000), pause)).await;

    // Chat's three, and the audit log's one: the setup's own entries.
    let times = store.commit_times();
    assert_eq!(times.len(), 4);
    for pair in times.windows(2) {
        assert!(pair[1] - pair[0] >= pause, "{times:?}");
    }
    db.close().await;
}

/// A tally of two runs, recorded: one `retention.purge` entry, the server's
/// own (no actor, IP or target), with every key and the runs' sums.
#[sqlx::test(migrations = false)]
async fn the_summary_records_every_key_with_the_runs_sums(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let store = WatchedStore::new(db.clone());
    let retention = service(&store, now(), policy(), Duration::ZERO);
    let since = now() - days(1);
    let cuts = policy().cuts(now(), &SessionPolicy::default());
    let mut tally = Tally::new(since);
    for n in 1..=2 {
        tally.add(&Run {
            cuts,
            deleted: Deleted {
                chat_by_age: n,
                chat_by_size: 2 * n,
                sessions: 3 * n,
                rotations: 4 * n,
                audit_entries: 5 * n,
                invites: 6 * n,
                enrollments: 7 * n,
            },
            audit_through: Some(AuditEntryId::new(40 + i64::try_from(n).unwrap())),
            failed: vec![Phase::Invites],
            stopped: false,
        });
    }

    retention.record(&tally).await.unwrap();

    let page = AuditPage {
        before: None,
        limit: AuditLimit::new(1).unwrap(),
    };
    let entry = db.audit().list(page).await.unwrap().remove(0);
    let expected = AuditMetadata::new()
        .time("since", since)
        .unwrap()
        .time("chat_before", cuts.chat)
        .unwrap()
        .int("chat_by_age", 3)
        .unwrap()
        .int("chat_per_bot", 100)
        .unwrap()
        .int("chat_by_size", 6)
        .unwrap()
        .time("sessions_before", cuts.sessions)
        .unwrap()
        .int("sessions", 9)
        .unwrap()
        .int("rotations", 12)
        .unwrap()
        .time("audit_before", cuts.audit)
        .unwrap()
        .int("audit_entries", 15)
        .unwrap()
        .int("audit_through", 42)
        .unwrap()
        .int("invites", 18)
        .unwrap()
        .int("enrollments", 21)
        .unwrap()
        .int("phases_failed", 2)
        .unwrap();
    assert_eq!(entry.action, AuditAction::RETENTION_PURGE);
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!((entry.actor, entry.ip, entry.target), (None, None, None));
    assert_eq!(entry.metadata, RecordedMetadata::from(&expected));
    db.close().await;
}

/// The stop's summary while the test holds the write connection: it gives
/// up after its wait, busy, and records nothing.
#[sqlx::test(migrations = false)]
async fn the_stops_summary_waits_at_most_its_bound(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let store = WatchedStore::new(db.clone());
    let retention = service(&store, now(), policy(), Duration::ZERO);
    let mut tally = Tally::new(now());
    tally.add(&Run {
        cuts: policy().cuts(now(), &SessionPolicy::default()),
        deleted: Deleted {
            chat_by_age: 1,
            ..Deleted::default()
        },
        audit_through: None,
        failed: Vec::new(),
        stopped: false,
    });
    let taken = db.write().await.unwrap();
    let started = tokio::time::Instant::now();

    let recorded = retention.record_at_stop(&tally).await;

    assert!(matches!(recorded, Err(SummaryError::Busy)), "{recorded:?}");
    assert!(started.elapsed() >= STOP_WAIT);
    assert!(started.elapsed() < STOP_WAIT + PATIENCE);
    drop(taken);
    assert!(
        !store.log().contains(&"commit audited".to_owned()),
        "{:?}",
        store.log()
    );
    db.close().await;
}
