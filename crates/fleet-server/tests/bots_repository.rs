//! Tests for the bots' table and repository (Plan.md P11.1, P10.5, Appendix
//! D; ADR-0019, S35, S38, S40): a new bot reads back with no server, stopped,
//! unassigned and with no state reported, in the built-in `afk` mode; an
//! account has one bot, which goes with it; the last state reads back through
//! fleet-core's names; a stored value that doesn't read back is corrupt; the
//! table refuses what its CHECKs and foreign keys forbid; and the migration
//! gives every account that existed before it a bot of its own.
//!
//! Each test gets a fresh database from `#[sqlx::test]`. Time isn't paused:
//! sqlx's worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use chrono::{DateTime, Utc};
use fleet_core::account::AccountStatus;
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::authz::Role;
use fleet_core::bot::{DesiredRunState, FailReason, LastReport, LastState, PauseReason};
use fleet_core::disconnect::ConflictKind;
use fleet_core::id::{AccountId, BotId, UserId};
use fleet_core::mode::ModePreset;
use fleet_core::value::Username;
use fleet_server::infra::sqlite::{Database, DatabaseOptions, MIGRATOR, PoolRole, connect_options};
use fleet_server::ports::accounts::{AccountCredential, DeletedAccount, McAccount};
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::bots::{Bot, NewBot};
use fleet_server::ports::store::{Store, StoreError};
use fleet_server::ports::users::{NewUser, PasswordHash};
use secrecy::SecretString;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// The time everything starts at, in milliseconds.
const T0: i64 = 1_800_000_000_000;
/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// A Minecraft profile's UUID (a well-known example, not a real account's).
const PROFILE: &str = "069a79f4-44e9-4726-a5be-fca90e38aaf5";
/// The migration before the bots' one, which backfills them.
const BEFORE_BOTS: i64 = 13;

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

async fn database(options: SqliteConnectOptions) -> Database {
    Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap()
}

/// The entry a test commits its setup with. A test-only name, never listed
/// in `audit_actions!`.
fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from("test.bots_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// Adds user `n` and returns its ID.
async fn add_user(db: &Database, n: u8) -> UserId {
    let id = UserId::new_v7(at(T0), [n; 10]).unwrap();
    let user = NewUser {
        id,
        username: Username::try_from(format!("user{n}").as_str()).unwrap(),
        password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
        role: Role::Member,
        created_at: at(T0),
    };
    let mut tx = db.write().await.unwrap();
    tx.users().insert(&user).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    id
}

/// Adds an offline account `n` of `owner`, without a bot, and returns it.
async fn add_account(db: &Database, owner: UserId, n: u8) -> AccountId {
    let account = McAccount {
        id: AccountId::new_v7(at(T0 + i64::from(n)), [n; 10]).unwrap(),
        owner,
        name: format!("AfkBot{n}").parse().unwrap(),
        created_at: at(T0 + i64::from(n)),
        credential: AccountCredential::Offline,
    };
    let mut tx = db.write().await.unwrap();
    tx.accounts().insert(&account).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    account.id
}

/// Bot `n` of `account`, in the `afk` mode.
fn new_bot(account: AccountId, n: u8) -> NewBot {
    NewBot {
        id: BotId::new_v7(at(T0 + i64::from(n)), [n; 10]).unwrap(),
        account,
        mode: ModePreset::Afk.id(),
    }
}

/// Inserts `bot` in its own audited transaction.
async fn insert(db: &Database, bot: &NewBot) -> Result<(), StoreError> {
    let mut tx = db.write().await?;
    tx.bots().insert(bot).await?;
    tx.commit(setup_entry()).await?;
    Ok(())
}

/// A database with one user, account 1 and its bot 1.
async fn with_bot(options: SqliteConnectOptions) -> (Database, AccountId, NewBot) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let account = add_account(&db, owner, 1).await;
    let bot = new_bot(account, 1);
    insert(&db, &bot).await.unwrap();
    (db, account, bot)
}

/// Sets `bot`'s raw state columns through the write pool.
async fn set_state(
    pool: &SqlitePool,
    bot: BotId,
    state: Option<&str>,
    reason: Option<&str>,
    at: Option<&str>,
) -> Result<(), sqlx::Error> {
    let id = bot.to_string();
    sqlx::query!(
        "UPDATE bots SET last_state = ?, last_state_reason = ?, last_state_at = ? WHERE id = ?",
        state,
        reason,
        at,
        id
    )
    .execute(pool)
    .await
    .map(drop)
}

#[sqlx::test(migrations = false)]
async fn a_new_bot_reads_back_stopped_unassigned_in_afk_on_the_read_pool_and_in_a_transaction(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, account, bot) = with_bot(options).await;
    let expected = Bot {
        id: bot.id,
        account,
        server: None,
        mode: ModePreset::Afk.id(),
        desired: DesiredRunState::Stopped,
        agent: None,
        last: None,
    };

    let read = db.bots().find(bot.id).await.unwrap();
    let by_account = db.bots().of_account(account).await.unwrap();
    let mut tx = db.write().await.unwrap();
    let in_tx = tx.bots().find(bot.id).await.unwrap();
    drop(tx);

    assert_eq!(read, Some(expected.clone()));
    assert_eq!(by_account, Some(expected.clone()));
    assert_eq!(in_tx, Some(expected));
}

#[sqlx::test(migrations = false)]
async fn an_unknown_bot_or_an_account_without_one_is_none(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let account = add_account(&db, owner, 1).await;
    // Another account with a bot, which no lookup may find instead.
    let other = add_account(&db, owner, 2).await;
    insert(&db, &new_bot(other, 2)).await.unwrap();

    assert_eq!(db.bots().find(new_bot(account, 9).id).await.unwrap(), None);
    assert_eq!(db.bots().of_account(account).await.unwrap(), None);
}

#[sqlx::test(migrations = false)]
async fn an_account_has_one_bot_and_a_bot_needs_its_account_and_mode(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, account, _) = with_bot(options).await;
    let unknown_account = AccountId::new_v7(at(T0), [9; 10]).unwrap();

    let second = insert(&db, &new_bot(account, 2)).await;
    let orphan = insert(&db, &new_bot(unknown_account, 3)).await;
    let mode_missing = insert(
        &db,
        &NewBot {
            mode: "018bcfe5-6800-7cdc-8dcd-cdcdcdcdcdcd".parse().unwrap(),
            ..new_bot(add_account(&db, add_user(&db, 2).await, 2).await, 4)
        },
    )
    .await;

    for (case, result) in [
        ("a second bot", second),
        ("an unknown account", orphan),
        ("an unknown mode", mode_missing),
    ] {
        assert!(
            matches!(result, Err(StoreError::Backend(_))),
            "{case}: {result:?}"
        );
    }
}

#[sqlx::test(migrations = false)]
async fn deleting_the_account_deletes_its_bot_and_says_which(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, account, bot) = with_bot(options).await;
    let mut tx = db.write().await.unwrap();

    let deleted = tx.accounts().delete(account, at(T0)).await.unwrap();
    let again = tx.accounts().delete(account, at(T0)).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert_eq!(
        deleted,
        Some(DeletedAccount {
            bot: Some(bot.id),
            agent: None
        })
    );
    assert_eq!(again, None);
    assert_eq!(db.bots().find(bot.id).await.unwrap(), None);
}

#[sqlx::test(migrations = false)]
async fn the_last_state_reads_back_through_fleet_cores_names(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, _, bot) = with_bot(options).await;
    let cases = [
        (
            "paused",
            Some("duplicate_login"),
            LastState::Paused(PauseReason::Conflict {
                kind: ConflictKind::DuplicateLogin,
            }),
        ),
        ("failed", Some("auth"), LastState::Failed(FailReason::Auth)),
        ("online", None, LastState::Online),
    ];

    for (state, reason, expected) in cases {
        set_state(
            db.write_pool(),
            bot.id,
            Some(state),
            reason,
            Some("2027-01-15T08:00:00.123Z"),
        )
        .await
        .unwrap();

        let read = db.bots().find(bot.id).await.unwrap().unwrap();

        assert_eq!(
            read.last,
            Some(LastReport {
                state: expected,
                at: "2027-01-15T08:00:00.123Z".parse().unwrap(),
            }),
            "{state}"
        );
    }
}

#[sqlx::test(migrations = false)]
async fn the_state_columns_go_together(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let (db, _, bot) = with_bot(options).await;
    let pool = db.write_pool();
    let at = Some("2027-01-15T08:00:00.000Z");

    let refused = [
        ("a state without its time", Some("online"), None, None),
        ("a time without a state", None, None, at),
        ("a reason without a state", None, Some("auth"), None),
        (
            "a time in another form",
            Some("online"),
            None,
            Some("2027-01-15T08:00:00Z"),
        ),
    ];

    for (case, state, reason, when) in refused {
        assert!(
            set_state(pool, bot.id, state, reason, when).await.is_err(),
            "{case} was stored"
        );
    }
}

#[sqlx::test(migrations = false)]
async fn a_stored_value_that_doesnt_read_back_is_corrupt_naming_its_column(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, _, bot) = with_bot(options).await;
    let id = bot.id.to_string();
    let pool = db.write_pool();

    sqlx::query!(
        "UPDATE bots SET server_address = 'not a server' WHERE id = ?",
        id
    )
    .execute(pool)
    .await
    .unwrap();
    let server = db.bots().find(bot.id).await;
    sqlx::query!("UPDATE bots SET server_address = NULL WHERE id = ?", id)
        .execute(pool)
        .await
        .unwrap();
    set_state(
        pool,
        bot.id,
        Some("sleeping"),
        None,
        Some("2027-01-15T08:00:00.000Z"),
    )
    .await
    .unwrap();
    let state = db.bots().find(bot.id).await;

    for (read, expected) in [(server, "server_address"), (state, "last_state")] {
        match read {
            Err(StoreError::Corrupt { table, column, .. }) => {
                assert_eq!((table, column), ("bots", expected));
            }
            other => panic!("{expected}: {other:?}"),
        }
    }
}

#[sqlx::test(migrations = false)]
async fn a_bot_with_a_server_reads_it_back(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, _, bot) = with_bot(options).await;
    let id = bot.id.to_string();
    sqlx::query!(
        "UPDATE bots SET server_address = 'play.example.com:25566', desired_state = 'running' WHERE id = ?",
        id
    )
    .execute(db.write_pool())
    .await
    .unwrap();

    let read = db.bots().find(bot.id).await.unwrap().unwrap();

    assert_eq!(
        read.server.map(|server| server.to_string()),
        Some("play.example.com:25566".to_owned())
    );
    assert_eq!(read.desired, DesiredRunState::Running);
}

// --- The backfill ---------------------------------------------------------------------

/// Account IDs whose last hex digit covers both halves of its range, and the
/// bot ID the migration gives each: the same ID with that digit's high bit
/// flipped (S35).
const BACKFILLED: [(&str, &str); 4] = [
    (
        "018bcfe5-6800-7bab-abab-ababababab00",
        "018bcfe5-6800-7bab-abab-ababababab08",
    ),
    (
        "018bcfe5-6801-7bab-abab-ababababab07",
        "018bcfe5-6801-7bab-abab-ababababab0f",
    ),
    (
        "018bcfe5-6802-7bab-abab-ababababab08",
        "018bcfe5-6802-7bab-abab-ababababab00",
    ),
    (
        "018bcfe5-6803-7bab-abab-ababababab0f",
        "018bcfe5-6803-7bab-abab-ababababab07",
    ),
];

/// The write pool's settings for `options`, without migrating.
async fn raw_pool(options: SqliteConnectOptions) -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect_options(
            options,
            PoolRole::Write,
            &DatabaseOptions::default(),
        ))
        .await
        .unwrap()
}

#[sqlx::test(migrations = false)]
async fn the_migration_gives_every_existing_account_a_bot_of_its_own(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let pool = raw_pool(options.clone()).await;
    MIGRATOR.run_to(BEFORE_BOTS, &pool).await.unwrap();
    let owner = "018bcfe5-6800-7aaa-8aaa-aaaaaaaaaaaa";
    let time = "2027-01-15T08:00:00.000Z";
    sqlx::query!(
        "INSERT INTO users (id, username, password_hash, role, disabled, created_at, password_changed_at)
         VALUES (?, 'owner', ?, 'owner', 0, ?, ?)",
        owner,
        PHC,
        time,
        time
    )
    .execute(&pool)
    .await
    .unwrap();
    for (n, (account, _)) in BACKFILLED.iter().enumerate() {
        let name = format!("AfkBot{n}");
        let (kind, uuid, envelope, status) = if n == 0 {
            (
                "microsoft",
                Some(PROFILE),
                Some(vec![0xAB_u8; 61]),
                "pending",
            )
        } else {
            ("offline", None, None, "active")
        };
        sqlx::query!(
            "INSERT INTO mc_accounts (id, owner_user_id, kind, mc_uuid, mc_name, refresh_envelope, status, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            account,
            owner,
            kind,
            uuid,
            name,
            envelope,
            status,
            time
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    MIGRATOR.run(&pool).await.unwrap();
    pool.close().await;
    let db = database(options).await;

    for (account, bot) in BACKFILLED {
        let account: AccountId = account.parse().unwrap();
        let expected_bot: BotId = bot.parse().unwrap();
        assert_eq!(
            db.bots().of_account(account).await.unwrap(),
            Some(Bot {
                id: expected_bot,
                account,
                server: None,
                mode: ModePreset::Afk.id(),
                desired: DesiredRunState::Stopped,
                agent: None,
                last: None,
            }),
            "{account}"
        );
        assert_ne!(expected_bot.to_string(), account.to_string());
        // The same 48-bit timestamp: the first 13 characters, hyphen included.
        assert_eq!(
            expected_bot.to_string().get(..13),
            account.to_string().get(..13)
        );
    }
    let bots = sqlx::query_scalar!(r#"SELECT count(*) AS "count!: i64" FROM bots"#)
        .fetch_one(db.read_pool())
        .await
        .unwrap();
    assert_eq!(usize::try_from(bots).unwrap(), BACKFILLED.len());
    // A pending link still has its bot; the status decides only placement.
    let pending: AccountId = BACKFILLED[0].0.parse().unwrap();
    assert_eq!(
        db.accounts().find(pending).await.unwrap().unwrap().status(),
        AccountStatus::Pending
    );
}

#[test]
fn the_backfills_rule_is_the_last_digits_high_bit_flipped() {
    for (account, bot) in BACKFILLED {
        let last = u8::from_str_radix(account.get(35..).unwrap(), 16).unwrap();
        let flipped = format!("{}{:x}", account.get(..35).unwrap(), last ^ 0x8);
        assert_eq!(flipped, bot);
        // Both are version 7.
        assert!(account.parse::<AccountId>().is_ok());
        assert!(bot.parse::<BotId>().is_ok());
    }
}
