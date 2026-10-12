//! `GET /me` (Plan.md P7.6, ADR-0016): the user's profile, their 2FA and
//! recovery codes, and the wrong 2FA codes typed since their previous
//! login.

use core::num::NonZeroU32;
use core::time::Duration;

use fleet_server::config::{Rate, RateLimitConfig};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use totp_rs::Totp;

use super::support::{ME, Options, PASSWORD, Setup, redacted, setup, setup_with};

/// A server whose login bucket takes a few full logins in a row.
async fn roomy(options: SqliteConnectOptions) -> Setup {
    setup_with(
        options,
        Options {
            limits: RateLimitConfig {
                login: Rate::per_minute(NonZeroU32::new(60).unwrap()),
                ..RateLimitConfig::default()
            },
            ..Options::default()
        },
    )
    .await
}

/// A login whose code step gets a wrong code; the clock stays.
async fn wrong_code(setup: &Setup, app: &Totp) {
    let first = setup.login("alice", PASSWORD).await;
    assert_eq!(first.status, 200, "{}", first.text);
    let seen = setup
        .code_step(&first.secret("token"), &setup.code(app, 10))
        .await;
    assert_eq!(seen.status, 401, "{}", seen.text);
}

/// The clock's time as the API writes it.
fn now_text(setup: &Setup) -> String {
    use fleet_core::system::Clock as _;
    fleet_api_types::Timestamp::try_from(setup.clock.now())
        .unwrap()
        .to_string()
}

async fn me(setup: &Setup, token: &SecretString) -> serde_json::Value {
    let seen = setup.get(ME, Some(token)).await;
    assert_eq!(seen.status, 200, "{}", seen.text);
    seen.body
}

#[sqlx::test(migrations = false)]
async fn the_profile_shows_who_the_user_is_and_their_2fa(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup.enroll_alice().await;
    let (_, token) = setup.session().await;

    let body = me(&setup, &token).await;

    insta::assert_json_snapshot!("me", redacted(&body, &[(setup.alice.to_string(), "alice")]));
}

#[sqlx::test(migrations = false)]
async fn the_wrong_codes_since_the_previous_login_are_counted(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = roomy(options).await;
    let (app, _) = setup.enroll_alice().await;
    setup.full_login(&app).await;
    setup.clock.advance(Duration::from_secs(60));
    wrong_code(&setup, &app).await;
    let away = now_text(&setup);
    setup.clock.advance(Duration::from_secs(60));
    let second = setup.full_login(&app).await.secret("access_token");

    let after_second = me(&setup, &second).await;
    setup.clock.advance(Duration::from_secs(60));
    wrong_code(&setup, &app).await;
    let while_in = now_text(&setup);
    let still_second = me(&setup, &second).await;
    setup.clock.advance(Duration::from_secs(60));
    let third = setup.full_login(&app).await.secret("access_token");
    let after_third = me(&setup, &third).await;

    assert_eq!(
        after_second["failed_code_attempts"],
        json!({ "count": 1, "last_at": away })
    );
    assert_eq!(
        still_second["failed_code_attempts"],
        json!({ "count": 2, "last_at": while_in })
    );
    assert_eq!(
        after_third["failed_code_attempts"],
        json!({ "count": 1, "last_at": while_in })
    );
}

#[sqlx::test(migrations = false)]
async fn a_first_login_counts_every_wrong_code_before_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = roomy(options).await;
    let (app, _) = setup.enroll_alice().await;
    wrong_code(&setup, &app).await;
    let typo = now_text(&setup);
    setup.clock.advance(Duration::from_secs(60));

    let token = setup.full_login(&app).await.secret("access_token");

    assert_eq!(
        me(&setup, &token).await["failed_code_attempts"],
        json!({ "count": 1, "last_at": typo })
    );
}

#[sqlx::test(migrations = false)]
async fn a_used_recovery_code_is_counted_down(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (_, codes) = setup.enroll_alice().await;
    let first = setup.login("alice", PASSWORD).await;
    let tokens = setup
        .code_step(&first.secret("token"), codes[0].expose_secret())
        .await;
    assert_eq!(tokens.status, 200, "{}", tokens.text);

    let body = me(&setup, &tokens.secret("access_token")).await;

    assert_eq!(body["recovery_codes_left"], 9);
}

#[sqlx::test(migrations = false)]
async fn the_profile_spends_the_users_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
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
    let (_, token) = setup.session().await;
    assert_eq!(setup.get(ME, Some(&token)).await.status, 200);

    let seen = setup.get(ME, Some(&token)).await;

    assert_eq!(seen.status, 429, "{}", seen.text);
}
