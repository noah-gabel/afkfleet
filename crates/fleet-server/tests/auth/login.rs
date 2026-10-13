//! `POST /api/v1/auth/login` and `/auth/login/mfa`.

use core::num::NonZeroU32;
use core::time::Duration;

use argon2::password_hash::PasswordHasher as _;
use argon2::{Algorithm, Argon2, Params, Version};
use fleet_core::audit::{
    AuditAction, AuditMetadata, AuditOutcome, AuditTarget, RecordedMetadata, RecordedTarget,
    RecordedValue,
};
use fleet_core::id::LoginStepId;
use fleet_core::system::{MintError, RandomError};
use fleet_core::value::Username;
use fleet_server::app::login::LoginError;
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::ports::store::Store as _;
use fleet_server::ports::users::PasswordHash;
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::support::{
    CONFIRM, ENROLL, LOGIN_MFA, LOGOUT, Meanwhile, Options, PASSWORD, RESET, Rehash, STEP_UP,
    Setup, setup, setup_with,
};

fn nonzero(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap()
}

/// Limits with room for many logins from one address, for the lockout's
/// tests.
fn roomy() -> Options {
    Options {
        limits: RateLimitConfig {
            login: Rate::per_minute(nonzero(60)),
            ..RateLimitConfig::default()
        },
        ..Options::default()
    }
}

/// Alice's password hashed with t = 3: a stale hash, today's being t = 2.
fn stale_hash() -> PasswordHash {
    let made = Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(19_456, 3, 1, Some(32)).unwrap(),
    )
    .hash_password_with_salt(PASSWORD.as_bytes(), &[9; 16])
    .unwrap();
    PasswordHash::new(SecretString::from(made.to_string())).unwrap()
}

fn text_metadata(entries: &[(&str, &str)]) -> RecordedMetadata {
    let mut metadata = AuditMetadata::new();
    for (key, value) in entries {
        metadata = metadata.text(key, value).unwrap();
    }
    RecordedMetadata::from(&metadata)
}

// --- Both steps ------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_user_with_2fa_gets_an_mfa_token_then_the_sessions_tokens(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (app, _) = setup.enroll_alice().await;

    let first = setup.login("Alice", PASSWORD).await;
    let second = setup
        .code_step(&first.secret("token"), &setup.code(&app, 0))
        .await;

    assert_eq!(first.status, 200, "{}", first.text);
    assert_eq!(first.body["status"], "mfa_required");
    assert!(first.body["token"].as_str().unwrap().starts_with("afk_mt_"));
    assert_eq!(first.body["expires_in"], 300);
    assert_eq!(first.headers["cache-control"], "no-store");
    assert_eq!(second.status, 200, "{}", second.text);
    assert!(
        second.body["access_token"]
            .as_str()
            .unwrap()
            .starts_with("afk_at_")
    );
    assert!(
        second.body["refresh_token"]
            .as_str()
            .unwrap()
            .starts_with("afk_rt_")
    );
    assert_eq!(second.body["access_expires_in"], 900);
    assert_eq!(second.body["refresh_expires_in"], 604_800);
    assert_eq!(second.headers["cache-control"], "no-store");
    let entry = setup.last(&AuditAction::AUTH_LOGIN).await;
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(setup.alice));
    assert_eq!(entry.ip, Some("192.0.2.10".parse().unwrap()));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::User(setup.alice)))
    );
    let shown = format!("{:?}", entry.metadata);
    assert!(
        shown.contains("session_id") && shown.contains("totp"),
        "{shown}"
    );
    assert!(!shown.contains("rehashed"), "{shown}");
    let access = second.secret("access_token");
    assert_eq!(setup.post(LOGOUT, Some(&access), None).await.status, 204);
}

/// A code step's metadata without its `login_step_id`, which must be there
/// and name a login step. Its value comes from the seeded source, so no test
/// pins it.
fn without_login_step(metadata: &RecordedMetadata) -> RecordedMetadata {
    let step = login_step_of(metadata).expect("a code step names its login step");
    assert!(step.parse::<LoginStepId>().is_ok(), "{step}");
    let mut rest = RecordedMetadata::new();
    for (key, value) in metadata.entries() {
        match value {
            RecordedValue::Text(_) if key == "login_step_id" => {}
            RecordedValue::Text(text) => rest.push_text(key, text),
            other => panic!("{key}: {other:?}"),
        }
    }
    rest
}

/// The `login_step_id` an entry records, if any.
fn login_step_of(metadata: &RecordedMetadata) -> Option<String> {
    metadata
        .entries()
        .iter()
        .find_map(|(key, value)| match value {
            RecordedValue::Text(text) if key == "login_step_id" => Some(text.clone()),
            _ => None,
        })
}

#[sqlx::test(migrations = false)]
async fn a_code_steps_failure_and_success_name_the_same_login_step(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (app, _) = setup.enroll_alice().await;
    let other = setup.login("alice", PASSWORD).await;
    let abandoned = setup
        .code_step(&other.secret("token"), &setup.code(&app, 10))
        .await;
    let first = setup.login("alice", PASSWORD).await;
    let token = first.secret("token");

    let typo = setup.code_step(&token, &setup.code(&app, 10)).await;
    let right = setup.code_step(&token, &setup.code(&app, 0)).await;

    assert_eq!(abandoned.status, 401, "{}", abandoned.text);
    assert_eq!(typo.status, 401, "{}", typo.text);
    assert_eq!(right.status, 200, "{}", right.text);
    let steps: Vec<(AuditOutcome, Option<String>)> = setup
        .audit()
        .await
        .into_iter()
        .filter(|entry| entry.action == AuditAction::AUTH_LOGIN)
        .map(|entry| (entry.outcome, login_step_of(&entry.metadata)))
        .collect();
    let [
        (AuditOutcome::Success, Some(success)),
        (AuditOutcome::Failure, Some(failure)),
        (AuditOutcome::Failure, Some(earlier)),
    ] = steps.as_slice()
    else {
        panic!("{steps:?}");
    };
    assert_eq!(failure, success);
    assert_ne!(earlier, success);
    assert!(success.parse::<LoginStepId>().is_ok());
}

#[sqlx::test(migrations = false)]
async fn a_login_step_whose_id_cant_be_minted_fails_before_any_token(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup.enroll_alice().await;
    setup.random.fail_next(RandomError::Unavailable);

    let refused = setup
        .login
        .password_step(
            &Username::try_from("alice").unwrap(),
            &SecretString::from(PASSWORD),
            "192.0.2.10".parse().unwrap(),
        )
        .await;

    assert!(
        matches!(
            refused,
            Err(LoginError::StepId(MintError::Random(
                RandomError::Unavailable
            )))
        ),
        "{refused:?}"
    );
    let seen = setup.login("alice", PASSWORD).await;
    assert_eq!(seen.status, 200, "{}", seen.text);
}

#[sqlx::test(migrations = false)]
async fn a_recovery_code_logs_in_once(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let (_, codes) = setup.enroll_alice().await;
    let code = codes[2].expose_secret().to_uppercase();

    let first = setup.login("alice", PASSWORD).await;
    let used = setup.code_step(&first.secret("token"), &code).await;
    let success = setup.last(&AuditAction::AUTH_LOGIN).await;
    let again_token = setup.login("alice", PASSWORD).await;
    let again = setup.code_step(&again_token.secret("token"), &code).await;

    assert_eq!(used.status, 200, "{}", used.text);
    assert_eq!(success.outcome, AuditOutcome::Success);
    assert!(
        format!("{:?}", success.metadata).contains("recovery_code"),
        "{:?}",
        success.metadata
    );
    assert_eq!(again.status, 401, "{}", again.text);
    assert_eq!(again.code(), "invalid_credentials");
}

#[sqlx::test(migrations = false)]
async fn a_user_without_2fa_gets_a_setup_token_and_never_real_tokens(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;

    let first = setup.login("alice", PASSWORD).await;
    let token = first.secret("token");
    let as_mfa_token = setup.code_step(&token, "123456").await;
    let enroll = setup.post(ENROLL, Some(&token), None).await;

    assert_eq!(first.status, 200, "{}", first.text);
    assert_eq!(first.body["status"], "mfa_setup_required");
    assert!(token.expose_secret().starts_with("afk_st_"));
    assert_eq!(first.body["expires_in"], 600);
    assert!(first.body.get("access_token").is_none());
    assert_eq!(as_mfa_token.status, 401);
    assert_eq!(enroll.status, 200, "{}", enroll.text);
}

#[sqlx::test(migrations = false)]
async fn the_setup_token_is_refused_by_the_public_auth_routes_too(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let token = setup.login("alice", PASSWORD).await.secret("token");

    let refresh = setup
        .post(
            super::support::REFRESH,
            None,
            Some(json!({ "refresh_token": token.expose_secret() })),
        )
        .await;
    let code_step = setup.code_step(&token, "123456").await;

    assert_eq!(refresh.status, 401, "{}", refresh.text);
    assert_eq!(code_step.status, 401, "{}", code_step.text);
}

#[sqlx::test(migrations = false)]
async fn during_a_replacement_login_still_asks_for_the_old_authenticator(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (old_app, _) = setup.enroll_alice().await;
    let tokens = setup.full_login(&old_app).await;
    let access = tokens.secret("access_token");
    assert_eq!(
        setup
            .post(
                STEP_UP,
                Some(&access),
                Some(json!({ "password": PASSWORD }))
            )
            .await
            .status,
        204
    );
    let reset = setup.post(RESET, Some(&access), None).await;
    let replace_token = reset.secret("setup_token");
    assert_eq!(
        setup.post(ENROLL, Some(&replace_token), None).await.status,
        200
    );

    let first = setup.login("alice", PASSWORD).await;
    let second = setup
        .code_step(&first.secret("token"), &setup.code(&old_app, 0))
        .await;

    assert_eq!(first.body["status"], "mfa_required");
    assert_eq!(second.status, 200, "{}", second.text);
}

// --- Enumeration (P7.12) -----------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn an_unknown_name_a_wrong_password_and_a_disabled_user_answer_alike(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let unknown = setup.login("bob", PASSWORD).await;
    let wrong = setup.login("alice", "wrong horse battery staple").await;
    sqlx::query!("UPDATE users SET disabled = 1")
        .execute(setup.db.write_pool())
        .await
        .unwrap();
    let disabled = setup.login("alice", PASSWORD).await;

    for seen in [&unknown, &wrong, &disabled] {
        assert_eq!(seen.status, 401, "{}", seen.text);
        assert_eq!(seen.code(), "invalid_credentials");
        assert_eq!(seen.headers, unknown.headers);
        assert_eq!(
            seen.body_without_request_id(),
            unknown.body_without_request_id()
        );
    }
    let failures: Vec<_> = setup
        .audit()
        .await
        .into_iter()
        .filter(|entry| entry.action == AuditAction::AUTH_LOGIN)
        .collect();
    let [disabled, wrong, unknown] = failures.as_slice() else {
        panic!("three refusals: {failures:?}");
    };
    assert_eq!(unknown.target, None, "an unknown name is never stored");
    assert_eq!(
        wrong.target,
        Some(RecordedTarget::Known(AuditTarget::User(setup.alice)))
    );
    assert_eq!(unknown.actor, None);
    assert_eq!(
        unknown.metadata,
        text_metadata(&[("step", "password"), ("reason", "wrong_password")])
    );
    assert_eq!(
        disabled.metadata,
        text_metadata(&[("step", "password"), ("reason", "disabled")])
    );
}

#[sqlx::test(migrations = false)]
async fn the_typed_name_is_stored_nowhere(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;

    let seen = setup.login("hunter2.secret", PASSWORD).await;

    assert_eq!(seen.status, 401);
    let entries = format!("{:?}", setup.audit().await);
    assert!(!entries.contains("hunter2"), "{entries}");
    let rows: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!: i64" FROM audit_log WHERE metadata_json LIKE '%hunter2%'"#
    )
    .fetch_one(setup.db.write_pool())
    .await
    .unwrap();
    assert_eq!(rows, 0);
}

// --- The lockout through login (P7.12) ---------------------------------------------

#[sqlx::test(migrations = false)]
async fn the_fifth_wrong_password_locks_the_name_and_the_right_one_waits(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(options, roomy()).await;
    let (app, _) = setup.enroll_alice().await;
    for _ in 0..5 {
        assert_eq!(
            setup
                .login("alice", "wrong horse battery staple")
                .await
                .status,
            401
        );
    }

    let locked = setup.login("alice", PASSWORD).await;
    setup.clock.advance(Duration::from_secs(60));
    let after = setup.login("alice", PASSWORD).await;
    let streak_after_the_password = setup.streak().await;
    let tokens = setup
        .code_step(&after.secret("token"), &setup.code(&app, 0))
        .await;

    assert_eq!(locked.status, 429, "{}", locked.text);
    assert_eq!(locked.headers["retry-after"], "60");
    assert_eq!(after.status, 200, "{}", after.text);
    assert_eq!(
        streak_after_the_password,
        Some(5),
        "the password step clears nothing"
    );
    assert_eq!(tokens.status, 200, "{}", tokens.text);
    assert_eq!(
        setup.streak().await,
        None,
        "a completed login clears the streak"
    );
}

#[sqlx::test(migrations = false)]
async fn wrong_codes_count_toward_the_lockout(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(options, roomy()).await;
    let (app, _) = setup.enroll_alice().await;
    let wrong = setup.code(&app, 5);
    let first = setup.login("alice", PASSWORD).await.secret("token");
    for _ in 0..3 {
        assert_eq!(setup.code_step(&first, &wrong).await.status, 401);
    }
    let second = setup.login("alice", PASSWORD).await.secret("token");
    assert_eq!(setup.code_step(&second, &wrong).await.status, 401);

    let fifth = setup.code_step(&second, &wrong).await;
    let locked = setup.login("alice", PASSWORD).await;

    assert_eq!(fifth.status, 401);
    assert_eq!(setup.streak().await, Some(5));
    assert_eq!(locked.status, 429, "{}", locked.text);
    let entry = setup.last(&AuditAction::AUTH_LOGIN).await;
    assert_eq!(
        without_login_step(&entry.metadata),
        text_metadata(&[("step", "code"), ("reason", "wrong_code")])
    );
}

#[sqlx::test(migrations = false)]
async fn a_second_attempt_while_one_is_in_flight_is_429(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let held = setup
        .lockout
        .begin(&Username::try_from("alice").unwrap())
        .await
        .unwrap();

    let seen = setup.login("alice", PASSWORD).await;

    assert_eq!(seen.status, 429, "{}", seen.text);
    assert_eq!(seen.headers["retry-after"], "1");
    drop(held);
}

// --- The mfa token ---------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn the_third_wrong_code_uses_the_mfa_token_up(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (app, _) = setup.enroll_alice().await;
    let token = setup.login("alice", PASSWORD).await.secret("token");
    for _ in 0..3 {
        let wrong = setup.code_step(&token, &setup.code(&app, 5)).await;
        assert_eq!(wrong.code(), "invalid_credentials");
    }

    let right = setup.code_step(&token, &setup.code(&app, 0)).await;

    assert_eq!(right.status, 401, "{}", right.text);
}

#[sqlx::test(migrations = false)]
async fn an_mfa_token_expires_after_five_minutes(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (app, _) = setup.enroll_alice().await;
    let token = setup.login("alice", PASSWORD).await.secret("token");
    setup.clock.advance(Duration::from_mins(5));

    let seen = setup.code_step(&token, &setup.code(&app, 0)).await;

    assert_eq!(seen.status, 401, "{}", seen.text);
    assert_eq!(seen.code(), "invalid_credentials");
}

#[sqlx::test(migrations = false)]
async fn a_malformed_code_is_422_and_spends_no_attempt(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (app, _) = setup.enroll_alice().await;
    let token = setup.login("alice", PASSWORD).await.secret("token");
    for _ in 0..3 {
        let malformed = setup.code_step(&token, "12345").await;
        assert_eq!(malformed.status, 422, "{}", malformed.text);
        assert_eq!(malformed.body["error"]["fields"][0]["path"], "code");
    }

    let right = setup.code_step(&token, &setup.code(&app, 0)).await;

    assert_eq!(right.status, 200, "{}", right.text);
    assert_eq!(setup.streak().await, None);
}

// --- Refused before any check ------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_name_that_isnt_a_username_is_422_uncounted_and_unaudited(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;

    let seen = setup.login("hunter2 not a name", PASSWORD).await;

    assert_eq!(seen.status, 422, "{}", seen.text);
    assert_eq!(seen.body["error"]["fields"][0]["path"], "username");
    assert!(!seen.text.contains("hunter2"));
    assert!(
        setup
            .audit()
            .await
            .iter()
            .all(|entry| entry.action != AuditAction::AUTH_LOGIN)
    );
}

#[sqlx::test(migrations = false)]
async fn a_password_over_1024_characters_is_422(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;

    let seen = setup.login("alice", &"p".repeat(1025)).await;

    assert_eq!(seen.status, 422, "{}", seen.text);
    assert_eq!(
        seen.body["error"]["fields"],
        json!([{ "path": "password", "message": "length is greater than 1024" }])
    );
    assert_eq!(setup.streak().await, None);
}

#[sqlx::test(migrations = false)]
async fn the_sixth_login_step_in_a_minute_from_one_address_is_429(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let token = setup.login("alice", PASSWORD).await.secret("token");
    for _ in 0..3 {
        setup.login("bob", PASSWORD).await;
    }
    setup.code_step(&token, "123456").await;

    let sixth = setup.login("alice", PASSWORD).await;

    assert_eq!(sixth.status, 429, "{}", sixth.text);
    assert_eq!(sixth.code(), "rate_limited");
}

// --- Re-hashing --------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_stale_hash_is_rehashed_once_the_code_passed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        Options {
            hash: Some(stale_hash()),
            ..Options::default()
        },
    )
    .await;
    let (app, _) = setup.enroll_alice().await;
    let before = setup.alice_hash().await;

    let first = setup.login("alice", PASSWORD).await;
    let unchanged = setup.alice_hash().await;
    let second = setup
        .code_step(&first.secret("token"), &setup.code(&app, 0))
        .await;

    assert_eq!(second.status, 200, "{}", second.text);
    assert_eq!(
        unchanged, before,
        "nothing is written before the code passed"
    );
    let after = setup.alice_hash().await;
    assert!(after.contains("$m=19456,t=2,p=1$"), "{after}");
    let user = setup.db.users().get(setup.alice).await.unwrap().unwrap();
    assert_eq!(
        user.password_changed_at,
        super::support::at(super::support::T0 - 60_000)
    );
    let shown = format!("{:?}", setup.last(&AuditAction::AUTH_LOGIN).await.metadata);
    assert!(shown.contains("rehashed"), "{shown}");
}

#[sqlx::test(migrations = false)]
async fn a_rehash_the_queue_refuses_leaves_the_old_hash_and_the_login_goes_on(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        Options {
            hash: Some(stale_hash()),
            rehash: Rehash::Busy,
            ..Options::default()
        },
    )
    .await;
    let (app, _) = setup.enroll_alice().await;
    let before = setup.alice_hash().await;

    let tokens = setup.full_login(&app).await;

    assert_eq!(tokens.status, 200);
    assert_eq!(setup.alice_hash().await, before);
    let shown = format!("{:?}", setup.last(&AuditAction::AUTH_LOGIN).await.metadata);
    assert!(!shown.contains("rehashed"), "{shown}");
}

#[sqlx::test(migrations = false)]
async fn a_rehash_that_fails_leaves_the_old_hash_and_the_login_goes_on(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        Options {
            hash: Some(stale_hash()),
            rehash: Rehash::Fails,
            ..Options::default()
        },
    )
    .await;
    let (app, _) = setup.enroll_alice().await;
    let before = setup.alice_hash().await;

    let tokens = setup.full_login(&app).await;

    assert_eq!(tokens.status, 200);
    assert_eq!(setup.alice_hash().await, before);
}

#[sqlx::test(migrations = false)]
async fn a_rehash_isnt_a_password_change_for_a_replacement_in_flight(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        Options {
            hash: Some(stale_hash()),
            ..Options::default()
        },
    )
    .await;
    let (app, _) = setup.enroll_alice().await;
    let (session, access) = setup.session().await;
    let mut tx = setup.db.write().await.unwrap();
    assert!(
        tx.sessions()
            .stamp_step_up(session, fleet_core::system::Clock::now(&setup.clock))
            .await
            .unwrap()
    );
    tx.commit(super::support::setup_entry()).await.unwrap();
    let replace = setup
        .post(RESET, Some(&access), None)
        .await
        .secret("setup_token");
    let new_app = totp_rs::Totp::from_url(
        setup.post(ENROLL, Some(&replace), None).await.body["otpauth_uri"]
            .as_str()
            .unwrap(),
    )
    .unwrap();

    setup.full_login(&app).await;
    assert!(
        setup.alice_hash().await.contains("t=2"),
        "the login re-hashed"
    );
    let confirmed = setup
        .post(
            CONFIRM,
            Some(&replace),
            Some(json!({ "code": setup.code(&new_app, 0) })),
        )
        .await;

    assert_eq!(confirmed.status, 200, "{}", confirmed.text);
}

// --- The re-checks before the commit -------------------------------------------------

/// Logs Alice in up to the code step with `change` landing before its
/// commit; the code step's answer.
async fn code_step_with(setup: &Setup, change: Meanwhile) -> super::support::Seen {
    let (app, _) = setup.enroll_alice().await;
    let token = setup.login("alice", PASSWORD).await.secret("token");
    setup.hooked.queue(change);
    setup.code_step(&token, &setup.code(&app, 0)).await
}

/// How many sessions exist.
async fn sessions(setup: &Setup) -> i64 {
    sqlx::query_scalar!(r#"SELECT count(*) AS "count!: i64" FROM sessions"#)
        .fetch_one(setup.db.write_pool())
        .await
        .unwrap()
}

#[sqlx::test(migrations = false)]
async fn a_password_changed_between_the_steps_refuses_the_login_and_keeps_the_hash(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        Options {
            hash: Some(stale_hash()),
            ..Options::default()
        },
    )
    .await;
    let before = setup.alice_hash().await;

    let seen = code_step_with(&setup, Meanwhile::ChangePassword).await;

    assert_eq!(seen.status, 401, "{}", seen.text);
    assert_eq!(seen.code(), "invalid_credentials");
    assert_eq!(setup.alice_hash().await, before);
    assert_eq!(sessions(&setup).await, 0);
    assert_eq!(setup.streak().await, None, "not counted");
    assert_eq!(
        without_login_step(&setup.last(&AuditAction::AUTH_LOGIN).await.metadata),
        text_metadata(&[("step", "code"), ("reason", "password_changed")])
    );
}

#[sqlx::test(migrations = false)]
async fn a_user_disabled_between_the_steps_is_refused_and_counted(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;

    let seen = code_step_with(&setup, Meanwhile::Disable).await;

    assert_eq!(seen.status, 401, "{}", seen.text);
    assert_eq!(sessions(&setup).await, 0);
    assert_eq!(setup.streak().await, Some(1));
    assert_eq!(
        without_login_step(&setup.last(&AuditAction::AUTH_LOGIN).await.metadata),
        text_metadata(&[("step", "code"), ("reason", "disabled")])
    );
}

#[sqlx::test(migrations = false)]
async fn a_replacement_between_the_code_check_and_the_commit_refuses_the_login(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;

    let seen = code_step_with(&setup, Meanwhile::ReplaceMfa).await;

    assert_eq!(seen.status, 401, "{}", seen.text);
    assert_eq!(sessions(&setup).await, 0);
    assert_eq!(setup.streak().await, None, "not counted");
    assert_eq!(
        without_login_step(&setup.last(&AuditAction::AUTH_LOGIN).await.metadata),
        text_metadata(&[("step", "code"), ("reason", "mfa_changed")])
    );
}

#[sqlx::test(migrations = false)]
async fn the_code_step_spends_the_login_bucket(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        Options {
            limits: RateLimitConfig {
                login: Rate::per_minute(nonzero(1)),
                ..RateLimitConfig::default()
            },
            ..Options::default()
        },
    )
    .await;
    let token = setup.login("alice", PASSWORD).await.secret("token");

    let seen = setup
        .post(
            LOGIN_MFA,
            None,
            Some(json!({ "mfa_token": token.expose_secret(), "code": "123456" })),
        )
        .await;

    assert_eq!(seen.status, 429, "{}", seen.text);
}
