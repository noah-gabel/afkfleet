//! Tests for `UserService::create_owner` (Plan.md P7.5, ADR-0016): the
//! Owner and its `user.create` entry commit together, a second Owner or a
//! taken username is refused, and a weak password stores nothing. Real
//! argon2 at §7.2's baseline, a real database from `#[sqlx::test]`,
//! fleet-testkit's `ManualClock` and `SeededRandom`.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::sync::Arc;

use chrono::DateTime;
use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget, RecordedValue};
use fleet_core::auth::NewPassword;
use fleet_core::authz::Role;
use fleet_core::system::Clock as _;
use fleet_core::value::Username;
use fleet_server::app::users::{CreateOwnerError, UserService};
use fleet_server::config::Argon2Params;
use fleet_server::infra::crypto::passwords::Argon2Passwords;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditRecord};
use fleet_server::ports::passwords::{HashError, Passwords as _};
use fleet_server::ports::store::Store as _;
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::SecretString;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

const STRONG: &str = "correct horse battery staple";

struct Setup {
    service: UserService,
    passwords: Arc<Argon2Passwords>,
    clock: ManualClock,
    db: Database,
}

async fn setup(options: SqliteConnectOptions) -> Setup {
    let db = Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap();
    let clock = ManualClock::new(DateTime::from_timestamp(1_800_000_000, 0).unwrap());
    let random = SeededRandom::new(7);
    let passwords = Arc::new(
        Argon2Passwords::new(Argon2Params::default(), Arc::new(random.clone()))
            .await
            .unwrap(),
    );
    let service = UserService::new(
        Arc::new(clock.clone()),
        Arc::new(random),
        Arc::new(db.clone()),
        Arc::clone(&passwords) as _,
    );
    Setup {
        service,
        passwords,
        clock,
        db,
    }
}

fn username(name: &str) -> Username {
    Username::try_from(name).unwrap()
}

fn password(text: &str) -> NewPassword {
    NewPassword::try_from(SecretString::from(text)).unwrap()
}

async fn entries(db: &Database) -> Vec<AuditRecord> {
    db.audit()
        .list(AuditPage {
            before: None,
            limit: AuditLimit::new(100).unwrap(),
        })
        .await
        .unwrap()
}

#[sqlx::test(migrations = false)]
async fn the_owner_is_created_with_a_password_that_verifies(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;

    let id = setup
        .service
        .create_owner(&username("alice"), &password(STRONG))
        .await
        .unwrap();

    let user = setup.db.users().get(id).await.unwrap().unwrap();
    assert_eq!(user.username, username("alice"));
    assert_eq!(user.role, Role::Owner);
    assert!(!user.disabled);
    assert_eq!(user.created_at, setup.clock.now());
    assert!(
        setup
            .passwords
            .verify(&SecretString::from(STRONG), &user.password_hash)
            .await
            .unwrap()
    );
}

#[sqlx::test(migrations = false)]
async fn creating_the_owner_records_one_user_create_entry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;

    let id = setup
        .service
        .create_owner(&username("alice"), &password(STRONG))
        .await
        .unwrap();

    let entries = entries(&setup.db).await;
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.action, AuditAction::USER_CREATE);
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.at, setup.clock.now());
    assert_eq!(entry.actor, None);
    assert_eq!(entry.ip, None);
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::User(id)))
    );
    assert_eq!(
        entry.metadata.entries(),
        [
            ("role".to_owned(), RecordedValue::Text("owner".to_owned())),
            ("via".to_owned(), RecordedValue::Text("cli".to_owned())),
        ]
    );
}

#[sqlx::test(migrations = false)]
async fn a_second_owner_is_refused_and_records_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup
        .service
        .create_owner(&username("alice"), &password(STRONG))
        .await
        .unwrap();

    let result = setup
        .service
        .create_owner(&username("bob"), &password(STRONG))
        .await;

    assert!(
        matches!(result, Err(CreateOwnerError::OwnerExists)),
        "{result:?}"
    );
    assert_eq!(entries(&setup.db).await.len(), 1);
    assert!(
        setup
            .db
            .users()
            .find_by_username(&username("bob"))
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrations = false)]
async fn a_taken_username_is_refused(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    // A Member named alice, as group E's registration will make them.
    let hash = setup
        .passwords
        .hash(&password(STRONG), &username("alice"))
        .await
        .unwrap();
    let mut tx = setup.db.write().await.unwrap();
    tx.users()
        .insert(&fleet_server::ports::users::NewUser {
            id: fleet_core::system::mint(setup.clock.now(), &SeededRandom::new(99)).unwrap(),
            username: username("alice"),
            password_hash: hash,
            role: Role::Member,
            created_at: setup.clock.now(),
        })
        .await
        .unwrap();
    tx.commit(fleet_server::ports::audit::NewAuditEntry::new(
        setup.clock.now(),
        AuditAction::USER_CREATE,
        AuditOutcome::Success,
    ))
    .await
    .unwrap();

    let result = setup
        .service
        .create_owner(&username("alice"), &password(STRONG))
        .await;

    assert!(
        matches!(result, Err(CreateOwnerError::UsernameTaken)),
        "{result:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_weak_password_stores_nothing(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;

    let result = setup
        .service
        .create_owner(&username("alice"), &password("passwordpassword"))
        .await;

    assert!(
        matches!(
            result,
            Err(CreateOwnerError::Password(HashError::Weak { .. }))
        ),
        "{result:?}"
    );
    assert!(!setup.service.owner_exists().await.unwrap());
    assert_eq!(entries(&setup.db).await.len(), 0);
}

#[sqlx::test(migrations = false)]
async fn owner_exists_answers_false_then_true(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    assert!(!setup.service.owner_exists().await.unwrap());

    setup
        .service
        .create_owner(&username("alice"), &password(STRONG))
        .await
        .unwrap();

    assert!(setup.service.owner_exists().await.unwrap());
}
