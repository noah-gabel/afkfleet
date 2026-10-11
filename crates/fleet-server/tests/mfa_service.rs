//! The 2FA service (Plan.md P7.7, P7.12, §7.2, ADR-0016): enroll makes a
//! pending secret, confirm checks it and stores it with 10 recovery codes
//! in one audited commit, a replacement swaps everything and revokes the
//! user's other sessions, and the code check accepts each TOTP step and each
//! recovery code once (P7.12's TOTP replay).
//!
//! Codes are computed from the otpauth URI the service hands out, through
//! totp-rs, never from `SeededRandom`'s bytes. Each test gets a fresh
//! database from `#[sqlx::test]`; time is a `ManualClock`, never paused
//! (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::audit::{
    AuditAction, AuditMetadata, AuditOutcome, AuditTarget, RecordedMetadata, RecordedTarget,
};
use fleet_core::auth::{RecoveryCode, SecondFactor, SessionPolicy, current_step};
use fleet_core::authz::Role;
use fleet_core::id::{SessionId, UserId};
use fleet_core::value::Username;
use fleet_server::app::auth::TokenService;
use fleet_server::app::challenges::{ChallengeUser, Challenges, SetupGrant, SetupPurpose};
use fleet_server::app::mfa::{
    CheckError, CodeCheck, ConfirmError, MfaPorts, MfaService, ReplaceError,
};
use fleet_server::infra::crypto::recovery_codes::Sha256RecoveryCodes;
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::infra::crypto::totp::TotpRs;
use fleet_server::infra::crypto::vault::{VaultKeys, XChaChaVault};
use fleet_server::infra::qr::SvgQr;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditRecord, NewAuditEntry};
use fleet_server::ports::mfa::UserMfa;
use fleet_server::ports::recovery_codes::RecoveryCodes as _;
use fleet_server::ports::store::Store as _;
use fleet_server::ports::users::{NewUser, PasswordHash};
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::{ExposeSecret as _, SecretBox, SecretString};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use totp_rs::Totp;

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// When every test starts: in the middle of a TOTP step.
const T0: i64 = 1_800_000_015_000;

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

fn ip() -> IpAddr {
    "192.0.2.10".parse().unwrap()
}

/// A vault over one in-memory key, `byte` repeated.
fn vault(byte: u8, random: &SeededRandom) -> XChaChaVault {
    let keys = VaultKeys::from_keys(
        BTreeMap::from([(1, SecretBox::new(Box::new([byte; 32])))]),
        1,
    )
    .unwrap();
    XChaChaVault::new(keys, Arc::new(random.clone()))
}

struct Setup {
    db: Database,
    clock: ManualClock,
    random: SeededRandom,
    service: MfaService,
    challenges: Challenges,
    tokens: TokenService,
    user: UserId,
}

/// The service over `db` with a vault on key `vault_byte`.
fn service(
    db: &Database,
    clock: &ManualClock,
    random: &SeededRandom,
    vault_byte: u8,
) -> MfaService {
    let challenges = Challenges::new(
        Arc::new(clock.clone()),
        Arc::new(Sha256Tokens::new(Arc::new(random.clone()))),
    );
    MfaService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(db.clone()),
        Arc::new(vault(vault_byte, random)),
        MfaPorts {
            totp: Arc::new(TotpRs::new()),
            recovery: Arc::new(Sha256RecoveryCodes::new()),
            qr: Arc::new(SvgQr::new()),
        },
        challenges,
    )
}

fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from("test.mfa_setup").unwrap(),
        AuditOutcome::Success,
    )
}

async fn setup(options: SqliteConnectOptions) -> Setup {
    let db = Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap();
    let clock = ManualClock::new(at(T0));
    let random = SeededRandom::new(61);
    let service = service(&db, &clock, &random, 0x42);
    let challenges = service.challenges().clone();
    let tokens = TokenService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(db.clone()),
        Arc::new(Sha256Tokens::new(Arc::new(random.clone()))),
        SessionPolicy::default(),
    );
    let user = UserId::new_v7(at(T0), [1; 10]).unwrap();
    let mut tx = db.write().await.unwrap();
    tx.users()
        .insert(&NewUser {
            id: user,
            username: Username::try_from("alice").unwrap(),
            password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
            role: Role::Member,
            created_at: at(T0 - 1_000),
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    Setup {
        db,
        clock,
        random,
        service,
        challenges,
        tokens,
        user,
    }
}

impl Setup {
    fn challenge_user(&self) -> ChallengeUser {
        ChallengeUser {
            id: self.user,
            role: Role::Member,
            password_changed_at: at(T0 - 1_000),
        }
    }

    /// A live setup token's grant for `purpose`.
    fn grant(&self, purpose: SetupPurpose) -> SetupGrant {
        let issued = self
            .challenges
            .issue_setup(self.challenge_user(), purpose)
            .unwrap();
        self.challenges.setup(issued.token.expose_secret()).unwrap()
    }

    /// Enrolls `grant` and returns the authenticator app's view of it.
    async fn enroll(&self, grant: &SetupGrant) -> Totp {
        let enrollment = self.service.enroll(grant).await.unwrap();
        Totp::from_url(enrollment.otpauth_uri.expose_secret()).unwrap()
    }

    /// The app's code at the clock's time, plus `steps` steps.
    fn code(&self, app: &Totp, steps: i64) -> String {
        let now = u64::try_from(self.clock.now_secs() + steps * 30).unwrap();
        app.generate(now).to_string()
    }

    /// A first enrollment, confirmed: the app and the recovery codes.
    async fn enrolled(&self) -> (Totp, Vec<SecretString>) {
        let grant = self.grant(SetupPurpose::FirstEnrollment);
        let app = self.enroll(&grant).await;
        let codes = self
            .service
            .confirm(&grant, &totp(&self.code(&app, 0)), ip())
            .await
            .unwrap();
        (app, codes)
    }

    async fn stored(&self) -> Option<UserMfa> {
        self.db.mfa().find(self.user).await.unwrap()
    }

    async fn check(&self, typed: &str) -> CodeCheck {
        self.service
            .check_code(self.user, &SecondFactor::parse(typed).unwrap())
            .await
            .unwrap()
    }

    /// Accepts `check` as a login's commit would.
    async fn accept(&self, check: &CodeCheck) -> bool {
        let mut tx = self.db.write().await.unwrap();
        let accepted = match check {
            CodeCheck::Totp { step, confirmed_at } => tx
                .mfa()
                .advance_step(self.user, *step, *confirmed_at)
                .await
                .unwrap(),
            CodeCheck::Recovery { hash } => tx
                .mfa()
                .use_code(self.user, hash, self.clock_now())
                .await
                .unwrap(),
            CodeCheck::Wrong => false,
        };
        tx.commit(setup_entry()).await.unwrap();
        accepted
    }

    fn clock_now(&self) -> DateTime<Utc> {
        fleet_core::system::Clock::now(&self.clock)
    }

    /// A live session of the user.
    async fn session(&self) -> SessionId {
        let prepared = self.tokens.prepare(self.user).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.sessions().insert(&prepared.new_session).await.unwrap();
        tx.commit(setup_entry()).await.unwrap();
        prepared.new_session.id
    }

    async fn audit(&self) -> Vec<AuditRecord> {
        self.db
            .audit()
            .list(AuditPage {
                before: None,
                limit: AuditLimit::new(100).unwrap(),
            })
            .await
            .unwrap()
    }
}

/// The clock's time in whole seconds.
trait Secs {
    fn now_secs(&self) -> i64;
}

impl Secs for ManualClock {
    fn now_secs(&self) -> i64 {
        fleet_core::system::Clock::now(self).timestamp()
    }
}

fn totp(typed: &str) -> fleet_core::auth::TotpCode {
    fleet_core::auth::TotpCode::parse(typed).unwrap()
}

fn step(clock: &ManualClock) -> u64 {
    current_step(fleet_core::system::Clock::now(clock)).unwrap()
}

// --- Enroll ------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn enroll_answers_a_uri_a_qr_code_and_the_secret_for_the_user(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let grant = setup.grant(SetupPurpose::FirstEnrollment);

    let enrollment = setup.service.enroll(&grant).await.unwrap();

    let uri = enrollment.otpauth_uri.expose_secret();
    let secret = enrollment.secret.expose_secret();
    assert_eq!(secret.len(), 32);
    assert_eq!(
        uri,
        format!("otpauth://totp/afkfleet:alice?secret={secret}&issuer=afkfleet")
    );
    assert!(
        enrollment
            .qr_svg
            .expose_secret()
            .starts_with("data:image/svg+xml;base64,")
    );
    assert!(setup.stored().await.is_none(), "nothing is stored yet");
}

#[sqlx::test(migrations = false)]
async fn each_enroll_makes_a_new_secret(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let grant = setup.grant(SetupPurpose::FirstEnrollment);

    let first = setup.service.enroll(&grant).await.unwrap();
    let second = setup.service.enroll(&grant).await.unwrap();

    assert_ne!(first.secret.expose_secret(), second.secret.expose_secret());
}

// --- Confirm -----------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn confirm_stores_the_authenticator_and_ten_recovery_codes_in_one_audited_commit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;

    let (_, codes) = setup.enrolled().await;

    let stored = setup.stored().await.unwrap();
    assert_eq!(stored.last_used_step, step(&setup.clock));
    assert_eq!(stored.confirmed_at, at(T0));
    assert_eq!(stored.secret_envelope.len(), 65);
    assert_eq!(codes.len(), 10);
    for code in &codes {
        let shown = code.expose_secret();
        assert_eq!(shown.len(), 24, "{shown}");
        assert_eq!(shown.matches('-').count(), 4, "{shown}");
        let hash = Sha256RecoveryCodes.hash(&RecoveryCode::parse(shown).unwrap());
        assert!(
            setup
                .db
                .mfa()
                .find_unused_code(setup.user, &hash)
                .await
                .unwrap()
                .is_some()
        );
    }
    let entries = setup.audit().await;
    let entry = &entries[0];
    assert_eq!(entry.action, AuditAction::MFA_ENROLL);
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(setup.user));
    assert_eq!(entry.ip, Some(ip()));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::User(setup.user)))
    );
}

/// Confirms with a code `steps` steps away and returns the stored step's
/// distance from the current one.
async fn confirm_steps_away(options: SqliteConnectOptions, steps: i64) -> i64 {
    let setup = setup(options).await;
    let grant = setup.grant(SetupPurpose::FirstEnrollment);
    let app = setup.enroll(&grant).await;

    setup
        .service
        .confirm(&grant, &totp(&setup.code(&app, steps)), ip())
        .await
        .unwrap();

    let stored = setup.stored().await.unwrap().last_used_step;
    i64::try_from(stored).unwrap() - i64::try_from(step(&setup.clock)).unwrap()
}

#[sqlx::test(migrations = false)]
async fn confirm_accepts_a_code_of_the_step_before(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    assert_eq!(confirm_steps_away(options, -1).await, -1);
}

#[sqlx::test(migrations = false)]
async fn confirm_accepts_a_code_of_the_step_after(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    assert_eq!(confirm_steps_away(options, 1).await, 1);
}

#[sqlx::test(migrations = false)]
async fn a_wrong_code_at_confirm_is_refused_and_the_token_stays(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let grant = setup.grant(SetupPurpose::FirstEnrollment);
    let app = setup.enroll(&grant).await;

    let two_steps_late = setup
        .service
        .confirm(&grant, &totp(&setup.code(&app, 2)), ip())
        .await;
    let right = setup
        .service
        .confirm(&grant, &totp(&setup.code(&app, 0)), ip())
        .await;

    assert!(
        matches!(two_steps_late, Err(ConfirmError::WrongCode)),
        "{two_steps_late:?}"
    );
    assert!(right.is_ok(), "{right:?}");
}

#[sqlx::test(migrations = false)]
async fn confirm_before_enroll_has_nothing_to_confirm(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let grant = setup.grant(SetupPurpose::FirstEnrollment);

    let confirmed = setup.service.confirm(&grant, &totp("123456"), ip()).await;

    assert!(
        matches!(confirmed, Err(ConfirmError::NotEnrolled)),
        "{confirmed:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_used_or_expired_setup_token_cant_confirm(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let grant = setup.grant(SetupPurpose::FirstEnrollment);
    let app = setup.enroll(&grant).await;
    let code = setup.code(&app, 0);
    setup
        .service
        .confirm(&grant, &totp(&code), ip())
        .await
        .unwrap();

    let again = setup.service.confirm(&grant, &totp(&code), ip()).await;
    let expired_grant = setup.grant(SetupPurpose::FirstEnrollment);
    let expired_app = setup.enroll(&expired_grant).await;
    let expired_code = setup.code(&expired_app, 0);
    setup.clock.advance(Duration::from_mins(10));
    let expired = setup
        .service
        .confirm(&expired_grant, &totp(&expired_code), ip())
        .await;

    assert!(matches!(again, Err(ConfirmError::Gone)), "{again:?}");
    assert!(matches!(expired, Err(ConfirmError::Gone)), "{expired:?}");
}

/// Confirms after `change` altered the user between enroll and confirm.
async fn confirm_after(setup: &Setup, change: impl AsyncFnOnce(&Database)) {
    let grant = setup.grant(SetupPurpose::FirstEnrollment);
    let app = setup.enroll(&grant).await;
    change(&setup.db).await;

    let confirmed = setup
        .service
        .confirm(&grant, &totp(&setup.code(&app, 0)), ip())
        .await;

    assert!(
        matches!(confirmed, Err(ConfirmError::Gone)),
        "{confirmed:?}"
    );
    assert!(setup.stored().await.is_none());
    assert!(
        setup.challenges.pending(&grant).is_none(),
        "the token is used up"
    );
}

#[sqlx::test(migrations = false)]
async fn a_user_disabled_before_confirm_gets_nothing_written(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;

    confirm_after(&setup, async |db: &Database| {
        sqlx::query!("UPDATE users SET disabled = 1")
            .execute(db.write_pool())
            .await
            .unwrap();
    })
    .await;
}

#[sqlx::test(migrations = false)]
async fn a_password_changed_before_confirm_gets_nothing_written(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;

    confirm_after(&setup, async |db: &Database| {
        sqlx::query!("UPDATE users SET password_changed_at = '2027-01-15T08:00:00.000Z'")
            .execute(db.write_pool())
            .await
            .unwrap();
    })
    .await;
}

#[sqlx::test(migrations = false)]
async fn a_first_confirm_when_2fa_already_exists_writes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (_, _) = setup.enrolled().await;
    let before = setup.stored().await.unwrap();
    let grant = setup.grant(SetupPurpose::FirstEnrollment);
    let app = setup.enroll(&grant).await;
    setup.clock.advance(Duration::from_secs(30));

    let confirmed = setup
        .service
        .confirm(&grant, &totp(&setup.code(&app, 0)), ip())
        .await;

    assert!(
        matches!(confirmed, Err(ConfirmError::AlreadyEnrolled)),
        "{confirmed:?}"
    );
    let after = setup.stored().await.unwrap();
    assert_eq!(after.secret_envelope, before.secret_envelope);
    assert_eq!(after.last_used_step, before.last_used_step);
}

#[sqlx::test(migrations = false)]
async fn two_concurrent_confirms_with_one_token_store_the_winners_codes(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let grant = setup.grant(SetupPurpose::FirstEnrollment);
    let app = setup.enroll(&grant).await;
    let code = totp(&setup.code(&app, 0));

    let (first, second) = tokio::join!(
        setup.service.confirm(&grant, &code, ip()),
        setup.service.confirm(&grant, &code, ip()),
    );

    let (won, lost) = match (first, second) {
        (Ok(codes), Err(error)) | (Err(error), Ok(codes)) => (codes, error),
        other => panic!("exactly one confirm wins: {other:?}"),
    };
    assert!(matches!(lost, ConfirmError::Gone), "{lost:?}");
    for code in &won {
        let hash = Sha256RecoveryCodes.hash(&RecoveryCode::parse(code.expose_secret()).unwrap());
        assert!(
            setup
                .db
                .mfa()
                .find_unused_code(setup.user, &hash)
                .await
                .unwrap()
                .is_some()
        );
    }
}

// --- The code check and TOTP replay (P7.12) ----------------------------------------

#[sqlx::test(migrations = false)]
async fn the_confirming_code_cant_log_in_afterwards(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let grant = setup.grant(SetupPurpose::FirstEnrollment);
    let app = setup.enroll(&grant).await;
    let code = setup.code(&app, 0);
    setup
        .service
        .confirm(&grant, &totp(&code), ip())
        .await
        .unwrap();

    assert!(matches!(setup.check(&code).await, CodeCheck::Wrong));
}

#[sqlx::test(migrations = false)]
async fn a_totp_code_is_accepted_once_and_no_older_code_after_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (app, _) = setup.enrolled().await;
    let confirmed = step(&setup.clock);
    setup.clock.advance(Duration::from_secs(60));
    let older = setup.code(&app, -1);
    let current = setup.code(&app, 0);

    let first = setup.check(&current).await;
    assert!(
        matches!(first, CodeCheck::Totp { step, .. } if step == confirmed + 2),
        "{first:?}"
    );
    assert!(setup.accept(&first).await);
    let replayed = setup.check(&current).await;
    let behind = setup.check(&older).await;

    assert!(matches!(replayed, CodeCheck::Wrong), "{replayed:?}");
    assert!(matches!(behind, CodeCheck::Wrong), "{behind:?}");
    assert!(
        !setup.accept(&first).await,
        "the commit refuses a replay too"
    );
}

#[sqlx::test(migrations = false)]
async fn a_code_outside_the_window_or_of_another_secret_is_wrong(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (app, _) = setup.enrolled().await;
    setup.clock.advance(Duration::from_mins(5));

    let late = setup.check(&setup.code(&app, -2)).await;
    let early = setup.check(&setup.code(&app, 2)).await;

    assert!(matches!(late, CodeCheck::Wrong), "{late:?}");
    assert!(matches!(early, CodeCheck::Wrong), "{early:?}");
}

#[sqlx::test(migrations = false)]
async fn a_recovery_code_is_accepted_once(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let (_, codes) = setup.enrolled().await;
    let code = codes[3].expose_secret().to_owned();

    let first = setup.check(&code).await;
    assert!(matches!(first, CodeCheck::Recovery { .. }), "{first:?}");
    assert!(setup.accept(&first).await);
    let again = setup.check(&code).await;

    assert!(matches!(again, CodeCheck::Wrong), "{again:?}");
    assert!(matches!(
        setup.check(codes[4].expose_secret()).await,
        CodeCheck::Recovery { .. }
    ));
}

#[sqlx::test(migrations = false)]
async fn a_user_without_2fa_has_no_right_code(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;

    assert!(matches!(setup.check("123456").await, CodeCheck::Wrong));
    assert!(matches!(
        setup.check("abcd-efgh-ijkl-mnop-qrst").await,
        CodeCheck::Wrong
    ));
}

#[sqlx::test(migrations = false)]
async fn a_secret_the_vault_cant_open_is_the_servers_failure(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (app, _) = setup.enrolled().await;
    setup.clock.advance(Duration::from_secs(30));
    let other_key = service(&setup.db, &setup.clock, &setup.random, 0x43);

    let checked = other_key
        .check_code(
            setup.user,
            &SecondFactor::parse(&setup.code(&app, 0)).unwrap(),
        )
        .await;

    assert!(matches!(checked, Err(CheckError::Vault(_))), "{checked:?}");
}

// --- Replacing ---------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_replacement_needs_2fa_to_replace(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let session = setup.session().await;

    let started = setup.service.start_replace(setup.user, session).await;

    assert!(
        matches!(started, Err(ReplaceError::NotEnrolled)),
        "{started:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_replacement_swaps_the_secret_and_the_codes_and_revokes_the_other_sessions(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (old_app, old_codes) = setup.enrolled().await;
    let asking = setup.session().await;
    let _other = setup.session().await;
    let _third = setup.session().await;
    let issued = setup
        .service
        .start_replace(setup.user, asking)
        .await
        .unwrap();
    assert_eq!(issued.expires_in, 600);
    let grant = setup
        .challenges
        .setup(issued.token.expose_secret())
        .unwrap();
    assert_eq!(grant.purpose, SetupPurpose::Replace { session_id: asking });
    setup.clock.advance(Duration::from_secs(30));
    let new_app = setup.enroll(&grant).await;

    let new_codes = setup
        .service
        .confirm(&grant, &totp(&setup.code(&new_app, 0)), ip())
        .await
        .unwrap();

    setup.clock.advance(Duration::from_secs(30));
    assert!(matches!(
        setup.check(&setup.code(&old_app, 0)).await,
        CodeCheck::Wrong
    ));
    assert!(matches!(
        setup.check(&setup.code(&new_app, 0)).await,
        CodeCheck::Totp { .. }
    ));
    assert!(matches!(
        setup.check(old_codes[0].expose_secret()).await,
        CodeCheck::Wrong
    ));
    assert!(matches!(
        setup.check(new_codes[0].expose_secret()).await,
        CodeCheck::Recovery { .. }
    ));
    let entries = setup.audit().await;
    let entry = &entries[0];
    assert_eq!(entry.action, AuditAction::MFA_RESET);
    assert_eq!(entry.actor, Some(setup.user));
    assert_eq!(
        entry.metadata,
        RecordedMetadata::from(&AuditMetadata::new().int("sessions_revoked", 2).unwrap())
    );
    let mut tx = setup.db.write().await.unwrap();
    let kept = tx.sessions().find(asking).await.unwrap().unwrap();
    assert_eq!(kept.session.revoked_at, None, "the asking session stays");
}

#[sqlx::test(migrations = false)]
async fn during_a_replacement_and_after_an_abandoned_one_the_old_2fa_works(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (old_app, _) = setup.enrolled().await;
    let asking = setup.session().await;
    let issued = setup
        .service
        .start_replace(setup.user, asking)
        .await
        .unwrap();
    let grant = setup
        .challenges
        .setup(issued.token.expose_secret())
        .unwrap();
    setup.enroll(&grant).await;
    setup.clock.advance(Duration::from_secs(30));

    let during = setup.check(&setup.code(&old_app, 0)).await;
    setup.clock.advance(Duration::from_mins(10));
    let after = setup.check(&setup.code(&old_app, 0)).await;

    assert!(matches!(during, CodeCheck::Totp { .. }), "{during:?}");
    assert!(matches!(after, CodeCheck::Totp { .. }), "{after:?}");
    assert!(
        setup
            .challenges
            .setup(issued.token.expose_secret())
            .is_none()
    );
}

#[sqlx::test(migrations = false)]
async fn a_replacement_whose_session_was_revoked_writes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (old_app, _) = setup.enrolled().await;
    let asking = setup.session().await;
    let issued = setup
        .service
        .start_replace(setup.user, asking)
        .await
        .unwrap();
    let grant = setup
        .challenges
        .setup(issued.token.expose_secret())
        .unwrap();
    setup.clock.advance(Duration::from_secs(30));
    let new_app = setup.enroll(&grant).await;
    let mut tx = setup.db.write().await.unwrap();
    tx.sessions()
        .revoke(asking, setup.clock_now())
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();

    let confirmed = setup
        .service
        .confirm(&grant, &totp(&setup.code(&new_app, 0)), ip())
        .await;

    assert!(
        matches!(confirmed, Err(ConfirmError::Gone)),
        "{confirmed:?}"
    );
    assert!(matches!(
        setup.check(&setup.code(&old_app, 0)).await,
        CodeCheck::Totp { .. }
    ));
}
