//! The per-username lockout at service level (Plan.md P7.10, P7.12's
//! "lockout counting", §7.4, ADR-0016): the threshold's failure locks for a
//! minute, each further one doubles the lock up to an hour, a locked name is
//! refused without being counted, a streak is forgotten a day after its last
//! failure, one attempt per name is in flight, an unknown name locks exactly
//! like a real one, and the typed name is stored nowhere.
//!
//! A real database from `#[sqlx::test]`, the real HMAC keys over a fixed
//! lockout key, and a `ManualClock`; time isn't paused (CLAUDE.md). D's
//! login drives the same service over HTTP.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::net::IpAddr;
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, TimeZone as _, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget, RecordedValue};
use fleet_core::auth::LockoutPolicy;
use fleet_core::authz::Role;
use fleet_core::id::UserId;
use fleet_core::value::Username;
use fleet_server::app::lockout::{LockoutError, LockoutRefusals, LockoutService, PURGE_BATCH};
use fleet_server::http::error::ApiError;
use fleet_server::infra::crypto::attempt_keys::HmacAttemptKeys;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditRecord, NewAuditEntry};
use fleet_server::ports::login_attempts::{AttemptKeys as _, LoginAttempt};
use fleet_server::ports::store::{Store as _, StoreError, UnauditedWrite};
use fleet_server::ports::users::{NewUser, PasswordHash};
use fleet_startup::config::{LogConfig, LogFormat};
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::log_buffer::LogBuffer;
use fleet_testkit::system::ManualClock;
use secrecy::{SecretBox, SecretString};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt as _;

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// The lockout key every test uses.
const LOCKOUT_KEY: [u8; 32] = [0x42; 32];
/// The client every failure comes from.
const CLIENT: &str = "192.0.2.10";

fn start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2027, 1, 15, 8, 0, 0).unwrap()
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn name(text: &str) -> Username {
    Username::try_from(text).unwrap()
}

fn keys() -> HmacAttemptKeys {
    HmacAttemptKeys::new(SecretBox::new(Box::new(LOCKOUT_KEY)))
}

struct Setup {
    service: LockoutService,
    clock: ManualClock,
    db: Database,
    alice: UserId,
}

async fn setup(options: SqliteConnectOptions) -> Setup {
    let db = Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap();
    let clock = ManualClock::new(start());
    let service = LockoutService::new(
        Arc::new(clock.clone()),
        Arc::new(db.clone()),
        Arc::new(keys()),
        LockoutPolicy::default(),
    );
    let alice = UserId::new_v7(start(), [1; 10]).unwrap();
    let mut tx = db.write().await.unwrap();
    tx.users()
        .insert(&NewUser {
            id: alice,
            username: name("alice"),
            password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
            role: Role::Member,
            created_at: start(),
        })
        .await
        .unwrap();
    tx.commit(test_entry()).await.unwrap();
    Setup {
        service,
        clock,
        db,
        alice,
    }
}

/// What a test commits with: a test-only name, never listed in
/// `audit_actions!` (D's login commits with `auth.login`).
fn test_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        start(),
        AuditAction::try_from("test.login_attempt").unwrap(),
        AuditOutcome::Failure,
    )
}

impl Setup {
    /// One failed attempt for `text` as D's login makes it: begin, then the
    /// failure inside the attempt's transaction, committed with its own
    /// entry. When the lock it started ends, if it started one.
    async fn fail(&self, text: &str, target: Option<UserId>) -> Option<DateTime<Utc>> {
        let attempt = self.service.begin(&name(text)).await.unwrap();
        let mut tx = self.db.write().await.unwrap();
        let lock = attempt
            .record_failure(&mut *tx, target, CLIENT.parse().unwrap())
            .await
            .unwrap();
        tx.commit(test_entry()).await.unwrap();
        lock
    }

    /// Fails `n` times for `text`.
    async fn fail_times(&self, text: &str, target: Option<UserId>, n: u32) {
        for _ in 0..n {
            self.fail(text, target).await;
        }
    }

    /// Why an attempt for `text` can't start, or `None` if it can.
    async fn refusal(&self, text: &str) -> Option<LockoutError> {
        self.service.begin(&name(text)).await.err()
    }

    /// The rest of `text`'s lock, or `None` when it isn't locked.
    async fn locked_for(&self, text: &str) -> Option<Duration> {
        match self.refusal(text).await {
            None => None,
            Some(LockoutError::Locked { retry_after }) => Some(retry_after),
            Some(other) => panic!("not a lock: {other:?}"),
        }
    }

    /// `text`'s stored streak.
    async fn row(&self, text: &str) -> Option<LoginAttempt> {
        self.db
            .login_attempts()
            .find(&keys().key(&name(text)))
            .await
            .unwrap()
    }

    async fn entries(&self) -> Vec<AuditRecord> {
        self.db
            .audit()
            .list(AuditPage {
                before: None,
                limit: AuditLimit::new(100).unwrap(),
            })
            .await
            .unwrap()
    }

    async fn lockout_entries(&self) -> Vec<AuditRecord> {
        self.entries()
            .await
            .into_iter()
            .filter(|entry| entry.action == AuditAction::SECURITY_LOCKOUT)
            .collect()
    }
}

// --- Counting ------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn four_failures_dont_lock_and_the_fifth_locks_for_a_minute(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    for _ in 0..4 {
        assert_eq!(setup.fail("alice", Some(setup.alice)).await, None);
        assert_eq!(setup.locked_for("alice").await, None);
    }

    let lock = setup.fail("alice", Some(setup.alice)).await;

    assert_eq!(lock, Some(start() + TimeDelta::seconds(60)));
    assert_eq!(setup.locked_for("alice").await, Some(secs(60)));
}

#[sqlx::test(migrations = false)]
async fn each_further_failure_doubles_the_lock_up_to_an_hour(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup.fail_times("alice", Some(setup.alice), 4).await;
    let mut locks = Vec::new();

    for _ in 0..8 {
        setup.fail("alice", Some(setup.alice)).await;
        let lock = setup.locked_for("alice").await.unwrap();
        locks.push(lock.as_secs());
        setup.clock.advance(lock);
    }

    assert_eq!(locks, [60, 120, 240, 480, 960, 1920, 3600, 3600]);
}

#[sqlx::test(migrations = false)]
async fn a_locked_name_is_refused_and_the_refusal_isnt_counted(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup.fail_times("alice", Some(setup.alice), 5).await;

    for _ in 0..10 {
        assert!(matches!(
            setup.refusal("alice").await,
            Some(LockoutError::Locked { .. })
        ));
    }

    assert_eq!(setup.row("alice").await.unwrap().failures, 5);
    setup.clock.advance(secs(60));
    setup.fail("alice", Some(setup.alice)).await;
    assert_eq!(setup.locked_for("alice").await, Some(secs(120)), "the 6th");
}

#[sqlx::test(migrations = false)]
async fn a_lock_ends_at_its_exact_millisecond(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup.fail_times("alice", Some(setup.alice), 5).await;

    setup.clock.advance(Duration::from_millis(59_999));
    assert_eq!(
        setup.locked_for("alice").await,
        Some(Duration::from_millis(1))
    );
    setup.clock.advance(Duration::from_millis(1));
    assert_eq!(setup.locked_for("alice").await, None);
}

#[sqlx::test(migrations = false)]
async fn a_streak_a_day_old_is_forgotten(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    setup.fail_times("alice", Some(setup.alice), 4).await;
    setup.clock.advance(Duration::from_hours(24));

    setup.fail_times("alice", Some(setup.alice), 4).await;

    assert_eq!(setup.row("alice").await.unwrap().failures, 4);
    assert_eq!(setup.locked_for("alice").await, None);
    setup.fail("alice", Some(setup.alice)).await;
    assert_eq!(setup.locked_for("alice").await, Some(secs(60)));
}

#[sqlx::test(migrations = false)]
async fn every_spelling_of_a_name_counts_on_its_one_streak(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    for spelling in ["alice", "Alice", "ALICE", "aLiCe", "alice"] {
        setup.fail(spelling, Some(setup.alice)).await;
    }

    assert_eq!(setup.locked_for("Alice").await, Some(secs(60)));
}

#[sqlx::test(migrations = false)]
async fn a_success_forgets_the_streak(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    setup.fail_times("alice", Some(setup.alice), 4).await;
    let attempt = setup.service.begin(&name("alice")).await.unwrap();
    let mut tx = setup.db.write().await.unwrap();

    attempt.clear(&mut *tx).await.unwrap();
    tx.commit(test_entry()).await.unwrap();

    assert_eq!(setup.row("alice").await, None);
    drop(attempt);
    setup.fail_times("alice", Some(setup.alice), 4).await;
    assert_eq!(
        setup.locked_for("alice").await,
        None,
        "it counts from 1 again"
    );
}

// --- Unknown names ---------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn an_unknown_name_locks_exactly_like_a_real_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let mut real = Vec::new();
    let mut unknown = Vec::new();

    for _ in 0..7 {
        real.push(setup.fail("alice", Some(setup.alice)).await);
        unknown.push(setup.fail("nobody", None).await);
        let real_lock = setup.locked_for("alice").await;
        assert_eq!(real_lock, setup.locked_for("nobody").await);
        setup.clock.advance(real_lock.unwrap_or(secs(1)));
    }

    assert_eq!(real, unknown);
    let (alice, nobody) = (
        setup.row("alice").await.unwrap(),
        setup.row("nobody").await.unwrap(),
    );
    assert_eq!(alice.failures, nobody.failures);
    assert_eq!(alice.locked_until, nobody.locked_until);
}

#[sqlx::test(migrations = false)]
async fn the_typed_name_is_stored_nowhere(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let file = options.get_filename().to_owned();
    let setup = setup(options).await;
    // A valid username that looks like a password someone typed there.
    let typed = "Hunter2-Secret";
    let (logs, guard) = capture();

    setup.fail_times(typed, None, 5).await;

    drop(guard);
    assert!(
        !logs.text().to_lowercase().contains("hunter2"),
        "{}",
        logs.text()
    );
    setup.db.close().await;
    for suffix in ["", "-wal", "-shm"] {
        let mut path = file.clone().into_os_string();
        path.push(suffix);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes).to_lowercase();
        assert!(!text.contains("hunter2"), "the name is in {path:?}");
    }
}

// --- One attempt in flight -----------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_second_attempt_for_a_name_in_flight_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let first = setup.service.begin(&name("alice")).await.unwrap();

    assert!(matches!(
        setup.refusal("ALICE").await,
        Some(LockoutError::InFlight)
    ));
    assert!(
        setup.refusal("bob").await.is_none(),
        "another name isn't held"
    );
    drop(first);
    assert!(setup.refusal("alice").await.is_none(), "released on drop");
}

#[sqlx::test(migrations = false)]
async fn an_attempt_is_released_after_its_failure_and_a_refusal_holds_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup.fail_times("alice", Some(setup.alice), 5).await;

    assert!(matches!(
        setup.refusal("alice").await,
        Some(LockoutError::Locked { .. })
    ));
    assert!(
        matches!(
            setup.refusal("alice").await,
            Some(LockoutError::Locked { .. })
        ),
        "a locked refusal doesn't leave the name in flight"
    );
}

// --- The audit entry and the log ---------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn each_lock_records_security_lockout_in_the_failing_attempts_commit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup.fail_times("alice", Some(setup.alice), 4).await;
    assert!(
        setup.lockout_entries().await.is_empty(),
        "none below the threshold"
    );

    setup.fail("alice", Some(setup.alice)).await;
    setup.clock.advance(secs(60));
    setup.fail("alice", Some(setup.alice)).await;

    let mut entries = setup.lockout_entries().await;
    entries.sort_by_key(|entry| entry.id);
    assert_eq!(entries.len(), 2, "{entries:?}");
    for entry in &entries {
        assert_eq!(entry.outcome, AuditOutcome::Failure);
        assert_eq!(entry.actor, None);
        assert_eq!(entry.ip, Some(CLIENT.parse::<IpAddr>().unwrap()));
        assert_eq!(
            entry.target,
            Some(RecordedTarget::Known(AuditTarget::User(setup.alice)))
        );
    }
    assert_eq!(
        entries[0].metadata.entries(),
        [
            ("failures".to_owned(), RecordedValue::Int(5)),
            (
                "locked_until".to_owned(),
                RecordedValue::Text("2027-01-15T08:01:00.000Z".to_owned())
            ),
        ]
    );
    assert_eq!(
        entries[1].metadata.entries(),
        [
            ("failures".to_owned(), RecordedValue::Int(6)),
            (
                "locked_until".to_owned(),
                RecordedValue::Text("2027-01-15T08:03:00.000Z".to_owned())
            ),
        ]
    );
}

#[sqlx::test(migrations = false)]
async fn an_unknown_names_lock_has_no_target(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;

    setup.fail_times("nobody", None, 5).await;

    let entries = setup.lockout_entries().await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].target, None);
    assert_eq!(
        entries[0].metadata.entries()[0],
        ("failures".to_owned(), RecordedValue::Int(5))
    );
}

/// The server's log layer at `debug`, into a buffer, as this thread's
/// default; the tests run on tokio's current-thread runtime.
fn capture() -> (LogBuffer, tracing::subscriber::DefaultGuard) {
    let buffer = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: "debug".parse().unwrap(),
    };
    let subscriber =
        tracing_subscriber::registry().with(layer_with(&config, &NoRules, false, buffer.clone()));
    let guard = tracing::subscriber::set_default(subscriber);
    (buffer, guard)
}

#[sqlx::test(migrations = false)]
async fn a_lock_logs_one_info_line_with_the_users_id(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup.fail_times("alice", Some(setup.alice), 4).await;
    let (logs, guard) = capture();

    setup.fail("alice", Some(setup.alice)).await;

    drop(guard);
    let lines = logs
        .lines_with("a username is locked after failed logins")
        .unwrap();
    assert_eq!(lines.len(), 1, "{}", logs.text());
    assert_eq!(lines[0]["level"], "INFO");
    assert_eq!(lines[0]["user_id"], setup.alice.to_string());
    assert_eq!(lines[0]["failures"], 5);
    assert_eq!(lines[0]["locked_until"], "2027-01-15 08:01:00 UTC");
}

// --- Refusals, the purge and the HTTP answer ------------------------------------------

#[sqlx::test(migrations = false)]
async fn refusals_are_counted_and_taken_once(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup.fail_times("alice", Some(setup.alice), 5).await;
    let _held = setup.service.begin(&name("bob")).await.unwrap();
    for _ in 0..3 {
        let _ = setup.refusal("alice").await;
    }
    let _ = setup.refusal("bob").await;

    assert_eq!(
        setup.service.take_refusals(),
        LockoutRefusals {
            locked: 3,
            in_flight: 1,
        }
    );
    assert_eq!(setup.service.take_refusals(), LockoutRefusals::default());
}

#[sqlx::test(migrations = false)]
async fn the_purge_deletes_every_expired_streak_in_batches_and_keeps_the_others(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let stale = 2 * PURGE_BATCH + 500;
    let mut tx = setup.db.write().await.unwrap();
    for n in 0..stale {
        let key = keys().key(&name(&format!("stale{n}")));
        tx.login_attempts()
            .upsert(
                &key,
                &LoginAttempt {
                    failures: 1,
                    last_failure_at: start(),
                    locked_until: None,
                },
            )
            .await
            .unwrap();
    }
    tx.commit_unaudited(UnauditedWrite::LoginAttemptsPurge)
        .await
        .unwrap();
    setup.clock.advance(Duration::from_hours(24));
    setup.fail("alice", Some(setup.alice)).await;

    let deleted = setup
        .service
        .purge_stale(&CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(deleted, u64::from(stale));
    assert_eq!(
        setup.row("alice").await.unwrap().failures,
        1,
        "a fresh streak stays"
    );
    assert_eq!(setup.row("stale7").await, None);
}

#[sqlx::test(migrations = false)]
async fn the_purge_keeps_a_streak_under_a_day_old_or_still_locked(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup.fail_times("alice", Some(setup.alice), 5).await;
    setup.fail("bob", None).await;
    setup
        .clock
        .advance(Duration::from_hours(24) - Duration::from_millis(1));

    assert_eq!(
        setup
            .service
            .purge_stale(&CancellationToken::new())
            .await
            .unwrap(),
        0
    );

    assert!(setup.row("alice").await.is_some());
    assert!(setup.row("bob").await.is_some());
    setup.clock.advance(Duration::from_millis(1));
    assert_eq!(
        setup
            .service
            .purge_stale(&CancellationToken::new())
            .await
            .unwrap(),
        2
    );
}

#[sqlx::test(migrations = false)]
async fn a_cancelled_purge_deletes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup.fail("bob", None).await;
    setup.clock.advance(Duration::from_hours(25));
    let stop = CancellationToken::new();
    stop.cancel();

    assert_eq!(setup.service.purge_stale(&stop).await.unwrap(), 0);

    assert!(setup.row("bob").await.is_some());
}

#[test]
fn a_refused_attempt_answers_429_and_a_failure_like_any_store_failure() {
    let locked = ApiError::from(LockoutError::Locked {
        retry_after: secs(60),
    });
    let in_flight = ApiError::from(LockoutError::InFlight);
    let busy = ApiError::from(LockoutError::Store(StoreError::Busy));

    assert!(matches!(locked, ApiError::RateLimited { retry_after } if retry_after == secs(60)));
    assert!(matches!(in_flight, ApiError::RateLimited { retry_after } if retry_after == secs(1)));
    assert!(matches!(busy, ApiError::Busy { .. }));
}
