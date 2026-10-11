//! `POST /api/v1/auth/logout`.

use core::num::NonZeroU32;

use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget};
use fleet_server::config::{Rate, RateLimitConfig};
use secrecy::ExposeSecret as _;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::support::{LOGOUT, Options, REFRESH, setup, setup_with};

#[sqlx::test(migrations = false)]
async fn logout_ends_the_session_and_both_its_tokens(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (app, _) = setup.enroll_alice().await;
    let tokens = setup.full_login(&app).await;
    let access = tokens.secret("access_token");

    let seen = setup.post(LOGOUT, Some(&access), None).await;

    assert_eq!(seen.status, 204, "{}", seen.text);
    assert_eq!(setup.post(LOGOUT, Some(&access), None).await.status, 401);
    let refresh = setup
        .post(
            REFRESH,
            None,
            Some(json!({ "refresh_token": tokens.secret("refresh_token").expose_secret() })),
        )
        .await;
    assert_eq!(refresh.status, 401);
    let entry = setup.last(&AuditAction::AUTH_LOGOUT).await;
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(setup.alice));
    assert!(matches!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Session(_)))
    ));
}

#[sqlx::test(migrations = false)]
async fn logout_ends_only_its_own_session(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let (_, mine) = setup.session().await;
    let (_, other) = setup.session().await;

    assert_eq!(setup.post(LOGOUT, Some(&mine), None).await.status, 204);

    assert_eq!(setup.post(LOGOUT, Some(&other), None).await.status, 204);
}

#[sqlx::test(migrations = false)]
async fn logout_spends_the_users_limit(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup_with(
        options,
        Options {
            limits: RateLimitConfig {
                per_user: Rate::per_second(NonZeroU32::MIN, NonZeroU32::MIN),
                ..RateLimitConfig::default()
            },
            ..Options::default()
        },
    )
    .await;
    let (_, first) = setup.session().await;
    let (_, second) = setup.session().await;
    assert_eq!(setup.post(LOGOUT, Some(&first), None).await.status, 204);

    let seen = setup.post(LOGOUT, Some(&second), None).await;

    assert_eq!(seen.status, 429, "{}", seen.text);
}
