//! `POST /me/password` (Plan.md P7.6, §7.2, ADR-0016): a new password after
//! stepping up, which ends every other session of the user and this one's
//! step-up; a weak, invalid or unchanged one is 422; and the commit
//! re-checks the session, the user and that the password verified at
//! step-up is still the current one.

use core::num::NonZeroU32;
use core::time::Duration;

use fleet_core::audit::{
    AuditAction, AuditMetadata, AuditOutcome, AuditTarget, RecordedMetadata, RecordedTarget,
};
use fleet_core::system::Clock as _;
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::ports::store::Store as _;
use secrecy::SecretString;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::support::{
    LOGOUT, ME_PASSWORD, Meanwhile, Options, PASSWORD, Seen, Setup, setup, setup_with,
};

/// A strong new password.
const NEW_PASSWORD: &str = "violet staircase humming quietly";

async fn change(setup: &Setup, token: &SecretString, password: &str) -> Seen {
    setup
        .post(
            ME_PASSWORD,
            Some(token),
            Some(json!({ "new_password": password })),
        )
        .await
}

async fn changes(setup: &Setup) -> usize {
    setup
        .audit()
        .await
        .iter()
        .filter(|entry| entry.action == AuditAction::USER_CHANGE_PASSWORD)
        .count()
}

#[sqlx::test(migrations = false)]
async fn a_stepped_up_user_changes_their_password_and_every_other_session_ends(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (current, token) = setup.session().await;
    let (other, other_token) = setup.session().await;
    setup.stamp(current).await;
    setup.clock.advance(Duration::from_secs(1));

    let seen = change(&setup, &token, NEW_PASSWORD).await;

    assert_eq!(seen.status, 204, "{}", seen.text);
    assert!(setup.live(current).await);
    assert!(!setup.live(other).await);
    assert_eq!(
        setup.post(LOGOUT, Some(&other_token), None).await.status,
        401
    );
    assert_eq!(setup.login("alice", PASSWORD).await.status, 401);
    assert_eq!(setup.login("alice", NEW_PASSWORD).await.status, 200);
    let user = setup.db.users().get(setup.alice).await.unwrap().unwrap();
    assert_eq!(user.password_changed_at, setup.clock.now());
    let entry = setup.last(&AuditAction::USER_CHANGE_PASSWORD).await;
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(setup.alice));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::User(setup.alice)))
    );
    assert_eq!(
        entry.metadata,
        RecordedMetadata::from(&AuditMetadata::new().int("sessions_revoked", 1).unwrap())
    );
    // This session's step-up ended with the old password.
    let again = change(&setup, &token, "another violet staircase hums").await;
    assert_eq!(again.status, 403, "{}", again.text);
    assert_eq!(again.code(), "step_up_required");
}

#[sqlx::test(migrations = false)]
async fn a_change_needs_a_fresh_step_up(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let (session, token) = setup.session().await;
    let never = change(&setup, &token, NEW_PASSWORD).await;
    setup.stamp(session).await;
    setup
        .clock
        .advance(Duration::from_secs(300) + Duration::from_millis(1));

    let stale = change(&setup, &token, NEW_PASSWORD).await;

    for seen in [never, stale] {
        assert_eq!(seen.status, 403, "{}", seen.text);
        assert_eq!(seen.code(), "step_up_required");
    }
    assert_eq!(changes(&setup).await, 0);
}

#[sqlx::test(migrations = false)]
async fn the_current_password_is_refused(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let (session, token) = setup.session().await;
    let (other, _) = setup.session().await;
    setup.stamp(session).await;

    let seen = change(&setup, &token, PASSWORD).await;

    assert_eq!(seen.status, 422, "{}", seen.text);
    assert_eq!(
        seen.body["error"]["fields"],
        json!([{ "path": "new_password", "message": "is the current password" }])
    );
    assert!(setup.live(other).await);
    assert_eq!(changes(&setup).await, 0);
}

#[sqlx::test(migrations = false)]
async fn a_weak_new_password_is_refused_with_its_hints(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (session, token) = setup.session().await;
    setup.stamp(session).await;
    let before = setup.alice_hash().await;

    let seen = change(&setup, &token, "password1234").await;

    assert_eq!(seen.status, 422, "{}", seen.text);
    let fields = seen.body["error"]["fields"].as_array().unwrap();
    assert_eq!(
        fields[0],
        json!({ "path": "new_password", "message": "is too weak" })
    );
    assert!(fields.len() >= 2, "{fields:?}");
    assert!(fields.iter().all(|field| field["path"] == "new_password"));
    assert_eq!(setup.alice_hash().await, before);
}

#[sqlx::test(migrations = false)]
async fn a_new_password_breaking_its_rules_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (session, token) = setup.session().await;
    setup.stamp(session).await;

    let seen = change(&setup, &token, "x".repeat(129).as_str()).await;

    assert_eq!(seen.status, 422, "{}", seen.text);
    assert_eq!(
        seen.body["error"]["fields"],
        json!([{ "path": "new_password", "message": "a password has at most 128 characters" }])
    );
}

#[sqlx::test(migrations = false)]
async fn a_session_ended_or_a_user_disabled_meanwhile_gets_401(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (session, token) = setup.session().await;
    setup.stamp(session).await;
    setup.hooked.queue(Meanwhile::RevokeSession(session));
    let ended = change(&setup, &token, NEW_PASSWORD).await;
    let (second, second_token) = setup.session().await;
    setup.stamp(second).await;
    setup.hooked.queue(Meanwhile::Disable);
    let disabled = change(&setup, &second_token, NEW_PASSWORD).await;

    assert_eq!(ended.status, 401, "{}", ended.text);
    assert_eq!(disabled.status, 401, "{}", disabled.text);
    assert_eq!(changes(&setup).await, 0);
}

#[sqlx::test(migrations = false)]
async fn a_password_changed_since_the_step_up_needs_a_new_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (session, token) = setup.session().await;
    setup.stamp(session).await;
    setup.clock.advance(Duration::from_secs(1));
    let before = setup.alice_hash().await;
    setup.hooked.queue(Meanwhile::ChangePassword);

    let seen = change(&setup, &token, NEW_PASSWORD).await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(seen.code(), "step_up_required");
    assert_eq!(setup.alice_hash().await, before);
    assert_eq!(changes(&setup).await, 0);
}

#[sqlx::test(migrations = false)]
async fn a_change_spends_the_login_bucket(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
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
    let (session, token) = setup.session().await;
    setup.stamp(session).await;
    assert_eq!(change(&setup, &token, NEW_PASSWORD).await.status, 204);
    setup.stamp(session).await;

    let seen = change(&setup, &token, "another violet staircase hums").await;

    assert_eq!(seen.status, 429, "{}", seen.text);
    assert_eq!(changes(&setup).await, 1);
}
