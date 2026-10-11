//! `POST /api/v1/auth/refresh` (P7.12's expired sessions and refresh reuse).

use core::num::NonZeroU32;
use core::time::Duration;

use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::ports::store::Store as _;
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::support::{LOGOUT, Options, REFRESH, Seen, Setup, setup, setup_with};

async fn refresh(setup: &Setup, token: &SecretString) -> Seen {
    setup
        .post(
            REFRESH,
            None,
            Some(json!({ "refresh_token": token.expose_secret() })),
        )
        .await
}

/// Whether `access` still opens a session: logging out answers 204 once.
async fn works(setup: &Setup, access: &SecretString) -> bool {
    setup.post(LOGOUT, Some(access), None).await.status == 204
}

#[sqlx::test(migrations = false)]
async fn a_refresh_rotates_both_tokens(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let (app, _) = setup.enroll_alice().await;
    let first = setup.full_login(&app).await;

    let second = refresh(&setup, &first.secret("refresh_token")).await;

    assert_eq!(second.status, 200, "{}", second.text);
    assert_eq!(second.body["access_expires_in"], 900);
    assert_eq!(second.headers["cache-control"], "no-store");
    assert_ne!(second.body["access_token"], first.body["access_token"]);
    assert!(!works(&setup, &first.secret("access_token")).await);
    assert!(works(&setup, &second.secret("access_token")).await);
}

#[sqlx::test(migrations = false)]
async fn reusing_a_replaced_refresh_token_revokes_the_whole_session(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (app, _) = setup.enroll_alice().await;
    let first = setup.full_login(&app).await;
    let old = first.secret("refresh_token");
    let second = refresh(&setup, &old).await;

    let reused = refresh(&setup, &old).await;
    let newest = refresh(&setup, &second.secret("refresh_token")).await;

    assert_eq!(reused.status, 401, "{}", reused.text);
    assert_eq!(reused.code(), "unauthorized");
    assert_eq!(reused.headers["www-authenticate"], "Bearer");
    assert_eq!(newest.status, 401, "the whole family is revoked");
    assert!(!works(&setup, &second.secret("access_token")).await);
    let entry = setup.last(&AuditAction::SECURITY_REFRESH_TOKEN_REUSE).await;
    assert_eq!(entry.outcome, AuditOutcome::Failure);
    assert_eq!(entry.ip, Some("192.0.2.10".parse().unwrap()));
}

#[sqlx::test(migrations = false)]
async fn a_session_idle_for_seven_days_cant_refresh(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (app, _) = setup.enroll_alice().await;
    let tokens = setup.full_login(&app).await;
    setup.clock.advance(Duration::from_hours(7 * 24));

    let seen = refresh(&setup, &tokens.secret("refresh_token")).await;

    assert_eq!(seen.status, 401, "{}", seen.text);
}

#[sqlx::test(migrations = false)]
async fn a_session_past_thirty_days_cant_refresh_however_often_it_did(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (app, _) = setup.enroll_alice().await;
    let mut token = setup.full_login(&app).await.secret("refresh_token");
    for _ in 0..4 {
        setup.clock.advance(Duration::from_hours(6 * 24));
        let seen = refresh(&setup, &token).await;
        assert_eq!(seen.status, 200, "{}", seen.text);
        token = seen.secret("refresh_token");
    }
    // Day 30 since the login, counting the 30 s `full_login` moved on: the
    // absolute end, though the last refresh was only 6 days ago.
    setup.clock.advance(Duration::from_hours(6 * 24));

    let seen = refresh(&setup, &token).await;

    assert_eq!(seen.status, 401, "{}", seen.text);
}

#[sqlx::test(migrations = false)]
async fn an_unknown_or_malformed_token_is_401_without_an_entry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let before = setup.audit().await.len();

    for token in [
        "afk_rt_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "afk_at_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "not a token",
        "",
    ] {
        let seen = refresh(&setup, &SecretString::from(token)).await;

        assert_eq!(seen.status, 401, "{token}: {}", seen.text);
        assert_eq!(seen.code(), "unauthorized");
    }
    assert_eq!(setup.audit().await.len(), before);
}

#[sqlx::test(migrations = false)]
async fn an_unknown_token_is_refused_without_the_write_connection(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let held = setup.db.write().await.unwrap();

    let seen = tokio::time::timeout(
        Duration::from_secs(2),
        refresh(
            &setup,
            &SecretString::from("afk_rt_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        ),
    )
    .await
    .expect("an unknown token never waits for the write connection");

    assert_eq!(seen.status, 401, "{}", seen.text);
    drop(held);
}

#[sqlx::test(migrations = false)]
async fn a_token_field_over_64_characters_is_422(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;

    let seen = refresh(&setup, &SecretString::from("afk_rt_".repeat(10))).await;

    assert_eq!(seen.status, 422, "{}", seen.text);
    assert_eq!(seen.body["error"]["fields"][0]["path"], "refresh_token");
}

#[sqlx::test(migrations = false)]
async fn refreshes_spend_their_own_bucket(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup_with(
        options,
        Options {
            limits: RateLimitConfig {
                refresh: Rate::per_minute(NonZeroU32::new(2).unwrap()),
                ..RateLimitConfig::default()
            },
            ..Options::default()
        },
    )
    .await;
    let unknown = SecretString::from("afk_rt_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
    refresh(&setup, &unknown).await;
    refresh(&setup, &unknown).await;

    let third = refresh(&setup, &unknown).await;

    assert_eq!(third.status, 429, "{}", third.text);
}
