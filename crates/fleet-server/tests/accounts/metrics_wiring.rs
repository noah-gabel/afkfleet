//! serve's wiring of the security counters (Plan.md P12.4; ADR-0022, S12):
//! `app_with` hands the server's own registry to every service that counts.
//! Each test makes its event through serve's router and reads the app's
//! own render; taking that service's `with_metrics` out of `cli/serve.rs`
//! turns its test red. The control plane's are `tests/serve/metrics.rs`'.

use core::num::NonZeroU32;

use axum::http::Method;
use fleet_core::authz::Role;
use fleet_server::config::{AccountsConfig, Rate, RateLimitConfig};
use secrecy::ExposeSecret as _;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::support::http::HttpSetup;

/// Logins loose enough that the lockout refuses first; two refreshes a
/// minute.
fn limits() -> RateLimitConfig {
    RateLimitConfig {
        login: Rate::per_minute(NonZeroU32::new(100).unwrap()),
        refresh: Rate::per_minute(NonZeroU32::new(2).unwrap()),
        ..RateLimitConfig::default()
    }
}

/// A reused refresh token, refused through the real route, counts in the
/// app's metrics (ADR-0016's note); the refresh route's limit counts its
/// refusal under `limit="refresh"`.
#[sqlx::test(migrations = false)]
async fn a_reused_refresh_token_and_the_refresh_limit_count_through_serves_router(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::with(options, &limits(), AccountsConfig::default()).await;
    let person = setup.person(1, Role::Member).await;
    let body = json!({ "refresh_token": person.refresh.expose_secret() });

    let rotated = setup
        .request(
            Method::POST,
            "/api/v1/auth/refresh",
            None,
            Some(body.clone()),
        )
        .await;
    let reused = setup
        .request(
            Method::POST,
            "/api/v1/auth/refresh",
            None,
            Some(body.clone()),
        )
        .await;
    let limited = setup
        .request(Method::POST, "/api/v1/auth/refresh", None, Some(body))
        .await;

    assert_eq!(
        [rotated.status, reused.status, limited.status],
        [200, 401, 429]
    );
    assert_eq!(
        setup
            .metric("afkfleet_refresh_token_reuses_total")
            .as_deref(),
        Some("1")
    );
    assert_eq!(
        setup
            .metric("afkfleet_rate_limit_refusals_total{limit=\"refresh\"}")
            .as_deref(),
        Some("1")
    );
}

/// Five wrong passwords lock the name; the next login is refused by the
/// lockout and counts under `reason="locked"`.
#[sqlx::test(migrations = false)]
async fn a_locked_name_counts_through_serves_router(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::with(options, &limits(), AccountsConfig::default()).await;
    setup
        .add_user_with_password(1, "user1", "a sound passphrase of the right length")
        .await;
    let wrong = json!({ "username": "user1", "password": "not the password at all" });

    let mut statuses = Vec::new();
    for _ in 0..6 {
        let seen = setup
            .request(
                Method::POST,
                "/api/v1/auth/login",
                None,
                Some(wrong.clone()),
            )
            .await;
        statuses.push(seen.status);
    }

    assert_eq!(statuses, [401, 401, 401, 401, 401, 429], "{statuses:?}");
    assert_eq!(
        setup
            .metric("afkfleet_login_lockout_refusals_total{reason=\"locked\"}")
            .as_deref(),
        Some("1")
    );
}
