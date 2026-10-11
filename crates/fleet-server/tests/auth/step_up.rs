//! `POST /api/v1/auth/step-up`, and the re-checks of its commit.

use core::num::NonZeroU32;
use core::time::Duration;

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditMetadata, AuditOutcome, RecordedMetadata};
use fleet_core::id::SessionId;
use fleet_core::system::Clock as _;
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::ports::store::Store as _;
use secrecy::SecretString;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::support::{
    Meanwhile, Options, PASSWORD, RESET, STEP_UP, Seen, Setup, setup, setup_with,
};

async fn step_up(setup: &Setup, access: &SecretString, password: &str) -> Seen {
    setup
        .post(STEP_UP, Some(access), Some(json!({ "password": password })))
        .await
}

/// The session's stored stamp.
async fn stamp(setup: &Setup, session: SessionId) -> Option<DateTime<Utc>> {
    let mut tx = setup.db.write().await.unwrap();
    tx.sessions()
        .find(session)
        .await
        .unwrap()
        .unwrap()
        .session
        .stepped_up_at
}

fn reason(reason: &str) -> RecordedMetadata {
    RecordedMetadata::from(&AuditMetadata::new().text("reason", reason).unwrap())
}

#[sqlx::test(migrations = false)]
async fn the_right_password_allows_step_up_actions_for_five_minutes(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup.enroll_alice().await;
    let (session, access) = setup.session().await;

    let seen = step_up(&setup, &access, PASSWORD).await;

    assert_eq!(seen.status, 204, "{}", seen.text);
    assert_eq!(stamp(&setup, session).await, Some(setup.clock.now()));
    assert_eq!(setup.post(RESET, Some(&access), None).await.status, 200);
    setup.clock.advance(Duration::from_secs(301));
    let late = setup.post(RESET, Some(&access), None).await;
    assert_eq!(late.code(), "step_up_required");
    let entry = setup.last(&AuditAction::AUTH_STEP_UP).await;
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(setup.alice));
}

#[sqlx::test(migrations = false)]
async fn a_wrong_password_is_403_counted_and_never_refreshes_a_stamp(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (session, access) = setup.session().await;
    assert_eq!(step_up(&setup, &access, PASSWORD).await.status, 204);
    let stamped = stamp(&setup, session).await;
    setup.clock.advance(Duration::from_mins(4));

    let seen = step_up(&setup, &access, "wrong horse battery staple").await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(seen.code(), "forbidden");
    assert_eq!(stamp(&setup, session).await, stamped);
    assert_eq!(setup.streak().await, Some(1));
    let entry = setup.last(&AuditAction::AUTH_STEP_UP).await;
    assert_eq!(entry.outcome, AuditOutcome::Failure);
    assert_eq!(entry.metadata, reason("wrong_password"));
}

#[sqlx::test(migrations = false)]
async fn wrong_step_up_passwords_lock_the_name_for_login_too(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        Options {
            limits: RateLimitConfig {
                login: Rate::per_minute(NonZeroU32::new(60).unwrap()),
                ..RateLimitConfig::default()
            },
            ..Options::default()
        },
    )
    .await;
    let (_, access) = setup.session().await;
    for _ in 0..5 {
        assert_eq!(
            step_up(&setup, &access, "wrong horse battery staple")
                .await
                .status,
            403
        );
    }

    let locked_step_up = step_up(&setup, &access, PASSWORD).await;
    let locked_login = setup.login("alice", PASSWORD).await;

    assert_eq!(locked_step_up.status, 429, "{}", locked_step_up.text);
    assert_eq!(locked_login.status, 429, "{}", locked_login.text);
}

#[sqlx::test(migrations = false)]
async fn a_right_step_up_clears_the_streak(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (_, access) = setup.session().await;
    step_up(&setup, &access, "wrong horse battery staple").await;

    assert_eq!(step_up(&setup, &access, PASSWORD).await.status, 204);

    assert_eq!(setup.streak().await, None);
}

#[sqlx::test(migrations = false)]
async fn a_session_revoked_before_the_commit_is_401_and_unstamped(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (session, access) = setup.session().await;
    setup.hooked.queue(Meanwhile::RevokeSession(session));

    let seen = step_up(&setup, &access, PASSWORD).await;

    assert_eq!(seen.status, 401, "{}", seen.text);
    assert_eq!(stamp(&setup, session).await, None);
}

#[sqlx::test(migrations = false)]
async fn a_password_changed_before_the_commit_is_403_uncounted_and_unstamped(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (session, access) = setup.session().await;
    setup.hooked.queue(Meanwhile::ChangePassword);

    let seen = step_up(&setup, &access, PASSWORD).await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(stamp(&setup, session).await, None);
    assert_eq!(setup.streak().await, None);
    assert_eq!(
        setup.last(&AuditAction::AUTH_STEP_UP).await.metadata,
        reason("password_changed")
    );
}

#[sqlx::test(migrations = false)]
async fn a_user_disabled_before_the_commit_is_401_and_unstamped(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (session, access) = setup.session().await;
    setup.hooked.queue(Meanwhile::Disable);

    let seen = step_up(&setup, &access, PASSWORD).await;

    assert_eq!(seen.status, 401, "{}", seen.text);
    assert_eq!(stamp(&setup, session).await, None);
}

#[sqlx::test(migrations = false)]
async fn a_password_over_1024_characters_is_422(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (_, access) = setup.session().await;

    let seen = step_up(&setup, &access, &"p".repeat(1025)).await;

    assert_eq!(seen.status, 422, "{}", seen.text);
    assert_eq!(setup.streak().await, None);
}

#[sqlx::test(migrations = false)]
async fn step_up_spends_the_login_bucket(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup_with(
        options,
        Options {
            limits: RateLimitConfig {
                login: Rate::per_minute(NonZeroU32::MIN),
                ..RateLimitConfig::default()
            },
            ..Options::default()
        },
    )
    .await;
    let (_, access) = setup.session().await;
    assert_eq!(step_up(&setup, &access, PASSWORD).await.status, 204);

    let seen = step_up(&setup, &access, PASSWORD).await;

    assert_eq!(seen.status, 429, "{}", seen.text);
}
