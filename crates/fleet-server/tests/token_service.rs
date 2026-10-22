//! Tests for `TokenService` (Plan.md P7.3, §7.2, ADR-0006, ADR-0016): a
//! prepared session's tokens, rotation, and P7.12's service-level refresh
//! tests: reuse revokes the family and records it in the same commit, a
//! session ends after 7 idle days and 30 days at most, and a disabled user
//! can't rotate.
//!
//! A real database from `#[sqlx::test]`, fleet-testkit's `ManualClock` and
//! `SeededRandom`, and the real `Sha256Tokens`. Time isn't paused: sqlx's
//! worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::fmt::Write as _;
use core::time::Duration;
use std::net::IpAddr;
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, TimeZone as _, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget, RecordedValue};
use fleet_core::auth::{SessionPolicy, SessionTimes};
use fleet_core::authz::{Actor, Role};
use fleet_core::id::{SessionId, UserId};
use fleet_core::system::{Clock as _, RandomError};
use fleet_core::value::Username;
use fleet_server::app::auth::{
    IssueError, IssuedTokens, RefreshError, SocketCheckError, SocketSession, TokenService,
};
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditRecord, NewAuditEntry};
use fleet_server::ports::sessions::{RefreshLookup, SessionWithUser};
use fleet_server::ports::store::Store as _;
use fleet_server::ports::tokens::{MintTokenError, TokenHash, TokenKind, Tokens as _};
use fleet_server::ports::users::{NewUser, PasswordHash};
use fleet_startup::config::{LogConfig, LogFormat};
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::log_buffer::LogBuffer;
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::{ExposeSecret as _, SecretString};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tracing_subscriber::layer::SubscriberExt as _;

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// The client's address: a documentation address (RFC 5737).
const CLIENT: [u8; 4] = [192, 0, 2, 10];
const MINUTE: Duration = Duration::from_mins(1);
const DAY: Duration = Duration::from_hours(24);

struct Setup {
    service: TokenService,
    clock: ManualClock,
    random: SeededRandom,
    tokens: Sha256Tokens,
    db: Database,
    user: UserId,
}

fn start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2027, 1, 15, 8, 0, 0).unwrap()
}

async fn setup(options: SqliteConnectOptions) -> Setup {
    let db = Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap();
    let clock = ManualClock::new(start());
    let random = SeededRandom::new(17);
    let tokens = Sha256Tokens::new(Arc::new(random.clone()));
    let service = TokenService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(db.clone()),
        Arc::new(tokens.clone()),
        SessionPolicy::default(),
    );
    let user = UserId::new_v7(start(), [1; 10]).unwrap();
    let mut tx = db.write().await.unwrap();
    tx.users()
        .insert(&NewUser {
            id: user,
            username: Username::try_from("alice").unwrap(),
            password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
            role: Role::Member,
            created_at: start(),
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    Setup {
        service,
        clock,
        random,
        tokens,
        db,
        user,
    }
}

/// The entry a test commits its setup with: a test-only name, built here and
/// never listed in `audit_actions!`.
fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        start(),
        AuditAction::try_from("test.session_create").unwrap(),
        AuditOutcome::Success,
    )
}

fn ip() -> IpAddr {
    IpAddr::from(CLIENT)
}

impl Setup {
    /// Prepares and inserts a session for the user, the way D's login will.
    async fn login(&self) -> IssuedTokens {
        let prepared = self.service.prepare(self.user).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.sessions().insert(&prepared.new_session).await.unwrap();
        tx.commit(setup_entry()).await.unwrap();
        prepared.tokens
    }

    async fn refresh(&self, token: &SecretString) -> Result<IssuedTokens, RefreshError> {
        self.service.refresh(token, ip()).await
    }

    fn hash(&self, kind: TokenKind, token: &SecretString) -> TokenHash {
        self.tokens
            .hash_presented(kind, token.expose_secret())
            .unwrap()
    }

    /// The session an access token belongs to.
    async fn by_access(&self, token: &SecretString) -> Option<SessionWithUser> {
        let hash = self.hash(TokenKind::Access, token);
        self.db.sessions().find_by_access_hash(&hash).await.unwrap()
    }

    /// What a refresh token finds, in a transaction that's dropped.
    async fn by_refresh(&self, token: &SecretString) -> Option<RefreshLookup> {
        let hash = self.hash(TokenKind::Refresh, token);
        let mut tx = self.db.write().await.unwrap();
        tx.sessions().find_by_refresh_hash(&hash).await.unwrap()
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

    /// The entries other than the setup's.
    async fn recorded(&self) -> Vec<AuditRecord> {
        self.entries()
            .await
            .into_iter()
            .filter(|entry| entry.action.as_str() != "test.session_create")
            .collect()
    }

    async fn disable_user(&self) {
        let id = self.user.to_string();
        sqlx::query!("UPDATE users SET disabled = 1 WHERE id = ?", id)
            .execute(self.db.write_pool())
            .await
            .unwrap();
    }
}

fn is_current(found: Option<&RefreshLookup>) -> bool {
    matches!(found, Some(RefreshLookup::Current(_)))
}

// --- Preparing a session ------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_prepared_session_holds_the_hashes_of_the_tokens_it_hands_out(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;

    let tokens = setup.login().await;

    let found = setup.by_access(&tokens.access_token).await.unwrap();
    assert_eq!(found.session.id, tokens.session_id);
    assert_eq!(found.session.user_id, setup.user);
    assert_eq!(found.session.created_at, start());
    assert!(is_current(
        setup.by_refresh(&tokens.refresh_token).await.as_ref()
    ));
    assert!(tokens.access_token.expose_secret().starts_with("afk_at_"));
    assert!(tokens.refresh_token.expose_secret().starts_with("afk_rt_"));
}

#[sqlx::test(migrations = false)]
async fn a_prepared_session_names_its_expiries(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;

    let tokens = setup.login().await;

    assert_eq!(tokens.access_expires_at, start() + 15 * MINUTE);
    assert_eq!(tokens.refresh_expires_at, start() + 7 * DAY);
}

#[sqlx::test(migrations = false)]
async fn a_failing_random_source_prepares_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup.random.fail_next(RandomError::Unavailable);

    let prepared = setup.service.prepare(setup.user);

    assert!(
        matches!(
            prepared,
            Err(IssueError::Token(MintTokenError::Random(
                RandomError::Unavailable
            )))
        ),
        "{prepared:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_clock_outside_an_ids_range_prepares_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup
        .clock
        .set(Utc.with_ymd_and_hms(1969, 12, 31, 0, 0, 0).unwrap());

    let prepared = setup.service.prepare(setup.user);

    assert!(matches!(prepared, Err(IssueError::Id(_))), "{prepared:?}");
}

// --- Rotation ------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_refresh_replaces_both_tokens_in_the_same_session(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let first = setup.login().await;
    setup.clock.advance(20 * MINUTE);

    let second = setup.refresh(&first.refresh_token).await.unwrap();

    assert_eq!(second.session_id, first.session_id);
    assert_ne!(
        second.access_token.expose_secret(),
        first.access_token.expose_secret()
    );
    assert!(
        setup.by_access(&first.access_token).await.is_none(),
        "the old access token is gone"
    );
    let found = setup.by_access(&second.access_token).await.unwrap();
    assert_eq!(found.session.refreshed_at, setup.clock.now());
    assert_eq!(found.session.created_at, start());
    assert!(is_current(
        setup.by_refresh(&second.refresh_token).await.as_ref()
    ));
    assert!(matches!(
        setup.by_refresh(&first.refresh_token).await,
        Some(RefreshLookup::Rotated { .. })
    ));
    assert_eq!(second.access_expires_at, setup.clock.now() + 15 * MINUTE);
    assert_eq!(second.refresh_expires_at, setup.clock.now() + 7 * DAY);
}

#[sqlx::test(migrations = false)]
async fn a_rotation_records_no_audit_entry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let first = setup.login().await;

    setup.refresh(&first.refresh_token).await.unwrap();

    assert_eq!(setup.recorded().await, []);
}

// --- Reuse (P7.12) -----------------------------------------------------------------

/// P7.12: reusing a refresh token revokes the family, and the entry is
/// recorded in the same commit.
#[sqlx::test(migrations = false)]
async fn reusing_a_replaced_refresh_token_revokes_the_session_and_records_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let first = setup.login().await;
    setup.clock.advance(20 * MINUTE);
    let replaced_at = setup.clock.now();
    let second = setup.refresh(&first.refresh_token).await.unwrap();
    setup.clock.advance(5 * MINUTE);

    let reused = setup.refresh(&first.refresh_token).await;

    assert!(matches!(reused, Err(RefreshError::Rejected)), "{reused:?}");
    let session = setup.by_access(&second.access_token).await.unwrap();
    assert_eq!(session.session.revoked_at, Some(setup.clock.now()));
    let entries = setup.recorded().await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    let entry = &entries[0];
    assert_eq!(entry.action, AuditAction::SECURITY_REFRESH_TOKEN_REUSE);
    assert_eq!(entry.outcome, AuditOutcome::Failure);
    assert_eq!(entry.actor, None);
    assert_eq!(entry.ip, Some(ip()));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::User(setup.user)))
    );
    assert_eq!(
        entry.metadata.entries(),
        [
            (
                "rotated_at".to_owned(),
                RecordedValue::Text(replaced_at.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
            ),
            (
                "session_id".to_owned(),
                RecordedValue::Text(first.session_id.to_string())
            ),
        ]
    );
}

#[sqlx::test(migrations = false)]
async fn after_a_reuse_the_current_refresh_token_is_refused_too(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let first = setup.login().await;
    let second = setup.refresh(&first.refresh_token).await.unwrap();
    assert!(setup.refresh(&first.refresh_token).await.is_err());

    let current = setup.refresh(&second.refresh_token).await;

    assert!(
        matches!(current, Err(RefreshError::Rejected)),
        "{current:?}"
    );
    assert_eq!(setup.recorded().await.len(), 1, "no second entry");
}

#[sqlx::test(migrations = false)]
async fn reuse_in_a_revoked_session_records_nothing_more(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let first = setup.login().await;
    setup.refresh(&first.refresh_token).await.unwrap();
    assert!(setup.refresh(&first.refresh_token).await.is_err());

    let again = setup.refresh(&first.refresh_token).await;

    assert!(matches!(again, Err(RefreshError::Rejected)), "{again:?}");
    assert_eq!(setup.recorded().await.len(), 1);
}

#[sqlx::test(migrations = false)]
async fn reuse_in_an_expired_session_is_a_plain_rejection(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let first = setup.login().await;
    let second = setup.refresh(&first.refresh_token).await.unwrap();
    setup.clock.advance(7 * DAY);

    let reused = setup.refresh(&first.refresh_token).await;

    assert!(matches!(reused, Err(RefreshError::Rejected)), "{reused:?}");
    assert_eq!(setup.recorded().await, []);
    let session = setup.by_access(&second.access_token).await.unwrap();
    assert_eq!(session.session.revoked_at, None, "nothing to revoke");
}

#[sqlx::test(migrations = false)]
async fn two_refreshes_with_the_same_token_give_one_rotation_and_one_reuse(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let first = setup.login().await;

    let (a, b) = tokio::join!(
        setup.refresh(&first.refresh_token),
        setup.refresh(&first.refresh_token)
    );

    let (rotated, refused) = match (a, b) {
        (Ok(rotated), Err(refused)) | (Err(refused), Ok(rotated)) => (rotated, refused),
        other => panic!("not one rotation and one refusal: {other:?}"),
    };
    assert!(matches!(refused, RefreshError::Rejected));
    let session = setup.by_access(&rotated.access_token).await.unwrap();
    assert!(session.session.revoked_at.is_some(), "the second was reuse");
    assert_eq!(setup.recorded().await.len(), 1);
}

// --- Expiry (P7.12) ------------------------------------------------------------------

/// P7.12: a session idle for 7 days can't be refreshed.
#[sqlx::test(migrations = false)]
async fn a_session_idle_for_7_days_cant_be_refreshed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let first = setup.login().await;
    setup.clock.advance(7 * DAY);

    let refused = setup.refresh(&first.refresh_token).await;

    assert!(
        matches!(refused, Err(RefreshError::Rejected)),
        "{refused:?}"
    );
    assert!(is_current(
        setup.by_refresh(&first.refresh_token).await.as_ref()
    ));
}

#[sqlx::test(migrations = false)]
async fn a_session_refreshed_just_before_its_idle_end_lives_on(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let first = setup.login().await;
    setup
        .clock
        .advance((7 * DAY).checked_sub(Duration::from_millis(1)).unwrap());

    assert!(setup.refresh(&first.refresh_token).await.is_ok());
}

/// P7.12: a session ends 30 days after login, however often it's refreshed.
#[sqlx::test(migrations = false)]
async fn a_session_ends_30_days_after_login_however_often_it_is_refreshed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let mut tokens = setup.login().await;
    for _ in 0..4 {
        setup.clock.advance(6 * DAY);
        tokens = setup.refresh(&tokens.refresh_token).await.unwrap();
    }
    assert_eq!(
        tokens.refresh_expires_at,
        start() + 30 * DAY,
        "refreshed on day 24, it still ends on day 30"
    );
    setup.clock.set(start() + 30 * DAY);

    let refused = setup.refresh(&tokens.refresh_token).await;

    assert!(
        matches!(refused, Err(RefreshError::Rejected)),
        "{refused:?}"
    );
}

// --- Disabled users and bad tokens ------------------------------------------------

#[sqlx::test(migrations = false)]
async fn the_current_token_of_a_disabled_user_doesnt_rotate(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let first = setup.login().await;
    setup.disable_user().await;

    let refused = setup.refresh(&first.refresh_token).await;

    assert!(
        matches!(refused, Err(RefreshError::Rejected)),
        "{refused:?}"
    );
    assert!(is_current(
        setup.by_refresh(&first.refresh_token).await.as_ref()
    ));
    assert_eq!(setup.recorded().await, []);
}

#[sqlx::test(migrations = false)]
async fn a_replaced_token_of_a_disabled_user_is_still_reuse(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let first = setup.login().await;
    let second = setup.refresh(&first.refresh_token).await.unwrap();
    setup.disable_user().await;

    assert!(setup.refresh(&first.refresh_token).await.is_err());

    let session = setup.by_access(&second.access_token).await.unwrap();
    assert!(session.session.revoked_at.is_some());
    assert_eq!(setup.recorded().await.len(), 1);
}

#[sqlx::test(migrations = false)]
async fn malformed_unknown_and_wrong_kind_tokens_are_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let tokens = setup.login().await;
    let unknown = format!("afk_rt_{}", "A".repeat(43));

    for presented in [
        SecretString::from("nonsense"),
        SecretString::from(unknown.as_str()),
        SecretString::from(tokens.access_token.expose_secret()),
    ] {
        let refused = setup.refresh(&presented).await;

        assert!(
            matches!(refused, Err(RefreshError::Rejected)),
            "{refused:?}"
        );
    }
    assert!(is_current(
        setup.by_refresh(&tokens.refresh_token).await.as_ref()
    ));
    assert_eq!(setup.recorded().await, []);
}

#[sqlx::test(migrations = false)]
async fn a_failing_random_source_refreshes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let first = setup.login().await;
    setup.random.fail_next(RandomError::Os { code: 5 });

    let failed = setup.refresh(&first.refresh_token).await;

    assert!(
        matches!(
            failed,
            Err(RefreshError::Token(MintTokenError::Random(
                RandomError::Os { code: 5 }
            )))
        ),
        "{failed:?}"
    );
    assert!(is_current(
        setup.by_refresh(&first.refresh_token).await.as_ref()
    ));
}

// --- What the log and Debug show --------------------------------------------------

/// Every 8-character window of the random part of `token`, and the token.
fn parts(token: &SecretString) -> Vec<String> {
    let text = token.expose_secret();
    let secret = &text[7..];
    let mut parts: Vec<String> = (0..=secret.len() - 8)
        .map(|start| secret[start..start + 8].to_owned())
        .collect();
    parts.push(text.to_owned());
    parts
}

fn assert_shows_no_part(text: &str, tokens: &[&SecretString]) {
    for token in tokens {
        for part in parts(token) {
            assert!(!text.contains(&part), "{part} in {text}");
        }
    }
}

#[sqlx::test(migrations = false)]
async fn the_reuse_warning_names_the_ids_and_no_token(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let first = setup.login().await;
    let second = setup.refresh(&first.refresh_token).await.unwrap();
    let logs = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: "debug".parse().unwrap(),
    };
    let subscriber =
        tracing_subscriber::registry().with(layer_with(&config, &NoRules, false, logs.clone()));
    let guard = tracing::subscriber::set_default(subscriber);

    let _ = setup.refresh(&first.refresh_token).await;

    drop(guard);
    let lines = logs
        .lines_with("a reused refresh token revoked its session")
        .unwrap();
    assert_eq!(lines.len(), 1, "{}", logs.text());
    assert_eq!(lines[0]["level"], "WARN");
    assert_eq!(lines[0]["user_id"], setup.user.to_string());
    assert_eq!(lines[0]["session_id"], first.session_id.to_string());
    assert_shows_no_part(
        &logs.text(),
        &[
            &first.access_token,
            &first.refresh_token,
            &second.access_token,
            &second.refresh_token,
        ],
    );
}

#[sqlx::test(migrations = false)]
async fn debug_shows_no_part_of_a_token(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let prepared = setup.service.prepare(setup.user).unwrap();
    let tokens = [
        &prepared.tokens.access_token,
        &prepared.tokens.refresh_token,
    ];

    let prepared_debug = format!("{prepared:?}");
    let issued_debug = format!("{:?}", prepared.tokens);

    assert_shows_no_part(&prepared_debug, &tokens);
    assert_shows_no_part(&issued_debug, &tokens);
}

#[sqlx::test(migrations = false)]
async fn a_new_sessions_debug_shows_no_hash_byte(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let prepared = setup.service.prepare(setup.user).unwrap();
    let session = &prepared.new_session;

    let debug = format!("{session:?}");

    for hash in [&session.access_hash, &session.refresh_hash] {
        let bytes = hash.as_bytes();
        let decimal = format!("{bytes:?}");
        let hex = bytes.iter().fold(String::new(), |mut hex, byte| {
            write!(hex, "{byte:02x}").unwrap();
            hex
        });
        let first_four = decimal
            .trim_start_matches('[')
            .split(", ")
            .take(4)
            .collect::<Vec<_>>()
            .join(", ");
        assert!(!debug.contains(&decimal), "{debug}");
        assert!(!debug.contains(&first_four), "{first_four} in {debug}");
        for start in 0..=hex.len() - 8 {
            assert!(!debug.contains(&hex[start..start + 8]), "{debug}");
        }
    }
}

// --- A live events socket's session (P11.5; ADR-0020, S31) -----------------------

impl Setup {
    /// The socket session a fresh login's access token opens, with the
    /// login's tokens.
    async fn socket(&self) -> (SocketSession, IssuedTokens) {
        let tokens = self.login().await;
        let found = self
            .service
            .authenticate(&tokens.access_token)
            .await
            .unwrap();
        let socket = SocketSession {
            actor: found.actor,
            session_id: found.session_id,
            times: found.times,
        };
        (socket, tokens)
    }

    /// The reason `socket`'s session no longer counts, or `None` when it
    /// still does.
    async fn refusal(&self, socket: &SocketSession) -> Option<&'static str> {
        match self.service.check_socket(socket).await {
            Ok(()) => None,
            Err(SocketCheckError::Refused(reason)) => Some(reason),
            Err(other @ SocketCheckError::Store(_)) => panic!("{other}"),
        }
    }
}

#[sqlx::test(migrations = false)]
async fn a_live_sockets_session_still_counts(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (socket, _) = setup.socket().await;
    setup.clock.advance(MINUTE);

    assert_eq!(setup.refusal(&socket).await, None);
}

#[sqlx::test(migrations = false)]
async fn a_revoked_session_no_longer_counts(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (socket, _) = setup.socket().await;
    let mut tx = setup.db.write().await.unwrap();
    tx.sessions()
        .revoke(socket.session_id, setup.clock.now())
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert_eq!(setup.refusal(&socket).await, Some("revoked"));
}

#[sqlx::test(migrations = false)]
async fn a_disabled_users_session_no_longer_counts(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (socket, _) = setup.socket().await;
    let mut tx = setup.db.write().await.unwrap();
    tx.users().set_disabled(setup.user, true).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert_eq!(setup.refusal(&socket).await, Some("disabled"));
}

#[sqlx::test(migrations = false)]
async fn a_session_whose_user_has_another_role_now_no_longer_counts(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (socket, _) = setup.socket().await;
    let mut tx = setup.db.write().await.unwrap();
    tx.users().set_role(setup.user, Role::Admin).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert_eq!(setup.refusal(&socket).await, Some("role_changed"));
}

#[sqlx::test(migrations = false)]
async fn a_session_that_isnt_there_or_isnt_the_users_no_longer_counts(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (socket, _) = setup.socket().await;
    let unknown = SocketSession {
        session_id: SessionId::new_v7(start(), [9; 10]).unwrap(),
        ..socket
    };
    let someone_else = SocketSession {
        actor: Actor {
            id: UserId::new_v7(start(), [9; 10]).unwrap(),
            ..socket.actor
        },
        ..socket
    };

    assert_eq!(setup.refusal(&unknown).await, Some("gone"));
    assert_eq!(setup.refusal(&someone_else).await, Some("gone"));
}

#[sqlx::test(migrations = false)]
async fn a_refresh_doesnt_extend_the_socket(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (socket, tokens) = setup.socket().await;
    setup.clock.advance(MINUTE * 10);
    let refreshed = setup.refresh(&tokens.refresh_token).await.unwrap();
    setup.clock.advance(MINUTE * 5);

    let refusal = setup.refusal(&socket).await;

    assert_eq!(refusal, Some("expired"), "15 minutes after the connect");
    assert!(
        setup
            .service
            .authenticate(&refreshed.access_token)
            .await
            .is_ok(),
        "the refreshed token still counts"
    );
}

#[sqlx::test(migrations = false)]
async fn a_socket_counts_until_a_millisecond_before_its_access_tokens_expiry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (socket, _) = setup.socket().await;

    setup
        .clock
        .advance(Duration::from_millis(15 * 60 * 1000 - 1));
    let before = setup.refusal(&socket).await;
    setup.clock.advance(Duration::from_millis(1));
    let at_it = setup.refusal(&socket).await;

    assert_eq!(before, None);
    assert_eq!(at_it, Some("expired"));
}

#[sqlx::test(migrations = false)]
async fn a_sockets_deadline_is_its_access_tokens_expiry_capped_at_the_sessions_end(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let fresh = SessionTimes {
        created_at: start(),
        refreshed_at: start(),
    };
    let ending = SessionTimes {
        created_at: start() - TimeDelta::days(30) + TimeDelta::minutes(5),
        refreshed_at: start(),
    };

    assert_eq!(
        setup.service.socket_deadline(fresh),
        start() + TimeDelta::minutes(15)
    );
    assert_eq!(
        setup.service.socket_deadline(ending),
        start() + TimeDelta::minutes(5)
    );
}

#[sqlx::test(migrations = false)]
async fn a_database_that_fails_is_no_refusal(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (socket, _) = setup.socket().await;
    setup.db.close().await;

    let result = setup.service.check_socket(&socket).await;

    assert!(
        matches!(result, Err(SocketCheckError::Store(_))),
        "{result:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_sockets_time_left_runs_down_to_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (socket, _) = setup.socket().await;

    let fresh = setup.service.socket_time_left(socket.times);
    setup.clock.advance(MINUTE * 10);
    let later = setup.service.socket_time_left(socket.times);
    setup.clock.advance(MINUTE * 6);
    let past = setup.service.socket_time_left(socket.times);

    assert_eq!(fresh, MINUTE * 15);
    assert_eq!(later, MINUTE * 5);
    assert_eq!(past, Duration::ZERO);
}

#[sqlx::test(migrations = false)]
async fn a_sockets_commit_count_follows_the_stores(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let mut commits = setup.service.socket_commits();
    commits.borrow_and_update();

    setup.login().await;

    assert!(commits.has_changed().unwrap());
}
