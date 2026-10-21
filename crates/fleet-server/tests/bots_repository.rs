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
use fleet_core::authz::{Actor, GrantLevel, Role};
use fleet_core::bot::{DesiredRunState, FailReason, LastReport, LastState, PauseReason};
use fleet_core::disconnect::ConflictKind;
use fleet_core::id::{AccountId, AgentId, BotId, ModeId, UserId};
use fleet_core::mode::ModePreset;
use fleet_core::value::{AgentName, ServerAddress, Username};
use fleet_server::infra::sqlite::{Database, DatabaseOptions, MIGRATOR, PoolRole, connect_options};
use fleet_server::ports::accounts::{AccountCredential, DeletedAccount, McAccount};
use fleet_server::ports::agents::NewAgent;
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::bots::{Bot, ListedBot, NewBot};
use fleet_server::ports::grants::AccountGrant;
use fleet_server::ports::page::{Page, PageLimit};
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

// --- The bots' endpoints (P11.2; ADR-0019, S48, S54, S55) ----------------------------------

/// A user `n` with `role`, named `user<n>`.
async fn add_user_with(db: &Database, n: u8, role: Role) -> UserId {
    let id = UserId::new_v7(at(T0), [n; 10]).unwrap();
    let user = NewUser {
        id,
        username: Username::try_from(format!("user{n}").as_str()).unwrap(),
        password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
        role,
        created_at: at(T0),
    };
    let mut tx = db.write().await.unwrap();
    tx.users().insert(&user).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    id
}

/// Account `n` of `owner` with its bot `n`; the bot's ID.
async fn add_account_with_bot(db: &Database, owner: UserId, n: u8) -> (AccountId, BotId) {
    let account = add_account(db, owner, n).await;
    let bot = new_bot(account, n);
    insert(db, &bot).await.unwrap();
    (account, bot.id)
}

/// A grant of `level` on `account` to `user`, by `by`.
async fn add_grant(db: &Database, account: AccountId, user: UserId, level: GrantLevel, by: UserId) {
    let mut tx = db.write().await.unwrap();
    tx.grants()
        .insert(&AccountGrant {
            account,
            user,
            level,
            granted_by: by,
            granted_at: at(T0 + 1000),
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
}

/// Enabled agent `n`, with `bot` assigned to it.
async fn assign_to_agent(db: &Database, bot: BotId, n: u8) -> AgentId {
    let agent = AgentId::new_v7(at(T0), [n; 10]).unwrap();
    let mut tx = db.write().await.unwrap();
    tx.agents()
        .insert(&NewAgent {
            id: agent,
            name: AgentName::try_from(format!("agent-{n}").as_str()).unwrap(),
            created_at: at(T0),
        })
        .await
        .unwrap();
    assert!(tx.bots().assign(bot, agent).await.unwrap());
    tx.commit(setup_entry()).await.unwrap();
    agent
}

fn page(limit: u32) -> Page<BotId> {
    Page {
        before: None,
        limit: PageLimit::new(limit).unwrap(),
    }
}

#[sqlx::test(migrations = false)]
async fn a_viewer_sees_a_bot_with_its_account_and_their_own_grant_on_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user_with(&db, 1, Role::Member).await;
    let friend = add_user_with(&db, 2, Role::Member).await;
    let (account, bot) = add_account_with_bot(&db, owner, 1).await;
    add_grant(&db, account, friend, GrantLevel::Control, owner).await;

    let seen = db.bots().listed(bot, friend).await.unwrap().unwrap();
    let own = db.bots().listed(bot, owner).await.unwrap().unwrap();

    assert_eq!(seen.record, db.bots().record(bot).await.unwrap().unwrap());
    assert_eq!(
        seen.account,
        db.accounts()
            .listed(account, friend)
            .await
            .unwrap()
            .unwrap()
    );
    assert_eq!(seen.account.grant, Some(GrantLevel::Control));
    assert_eq!(own.account.grant, None);
    assert_eq!(own.account.owner.id, owner);
    assert_eq!(
        db.bots()
            .listed(new_bot(account, 9).id, owner)
            .await
            .unwrap(),
        None
    );
}

/// Every role, owner role and grant: the bots a viewer sees are exactly the
/// bots of the accounts `list_visible` lists for them, in bot ID order.
#[sqlx::test(migrations = false)]
async fn the_visible_bots_are_exactly_the_bots_of_the_visible_accounts(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let the_owner = add_user_with(&db, 1, Role::Owner).await;
    let admin = add_user_with(&db, 2, Role::Admin).await;
    let other_admin = add_user_with(&db, 3, Role::Admin).await;
    let member = add_user_with(&db, 4, Role::Member).await;
    let other_member = add_user_with(&db, 5, Role::Member).await;
    let people = [the_owner, admin, other_admin, member, other_member];
    let roles = [
        Role::Owner,
        Role::Admin,
        Role::Admin,
        Role::Member,
        Role::Member,
    ];
    let mut n = 10;
    for owner in people {
        for _ in 0..2 {
            let (account, _) = add_account_with_bot(&db, owner, n).await;
            // The first account of every user is shared with the next user.
            if n % 2 == 0 {
                let next = people[(people.iter().position(|p| *p == owner).unwrap() + 1) % 5];
                add_grant(&db, account, next, GrantLevel::View, owner).await;
            }
            n += 1;
        }
    }

    for (viewer, role) in people.into_iter().zip(roles) {
        let actor = Actor { id: viewer, role };
        let accounts = db
            .accounts()
            .list_visible(
                actor,
                Page {
                    before: None,
                    limit: PageLimit::new(100).unwrap(),
                },
            )
            .await
            .unwrap();
        let bots = db.bots().visible(actor, page(100)).await.unwrap();

        let mut expected: Vec<AccountId> = accounts.iter().map(|account| account.id).collect();
        expected.sort();
        let mut seen: Vec<AccountId> = bots.iter().map(|bot| bot.account.id).collect();
        seen.sort();
        assert_eq!(seen, expected, "{role:?}");
        assert!(!seen.is_empty(), "{role:?}");
        for listed in &bots {
            assert_eq!(listed.record.bot.account, listed.account.id);
            assert_eq!(
                Some(listed.account.clone()),
                db.accounts()
                    .listed(listed.account.id, viewer)
                    .await
                    .unwrap()
            );
        }
        let ids: Vec<BotId> = bots.iter().map(|bot| bot.record.bot.id).collect();
        let mut descending = ids.clone();
        descending.sort_by(|a, b| b.cmp(a));
        assert_eq!(ids, descending, "newest first");
    }
}

#[sqlx::test(migrations = false)]
async fn the_visible_bots_page_before_a_cursor_with_one_more_than_the_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user_with(&db, 1, Role::Member).await;
    let mut bots = Vec::new();
    for n in 1..=4 {
        bots.push(add_account_with_bot(&db, owner, n).await.1);
    }
    let actor = Actor {
        id: owner,
        role: Role::Member,
    };

    let first = db.bots().visible(actor, page(2)).await.unwrap();
    let rest = db
        .bots()
        .visible(
            actor,
            Page {
                before: Some(bots[2]),
                limit: PageLimit::new(2).unwrap(),
            },
        )
        .await
        .unwrap();

    let ids = |page: &[ListedBot]| page.iter().map(|bot| bot.record.bot.id).collect::<Vec<_>>();
    assert_eq!(ids(&first), [bots[3], bots[2], bots[1]]);
    assert_eq!(ids(&rest), [bots[1], bots[0]]);
}

#[sqlx::test(migrations = false)]
async fn setting_the_server_writes_its_written_form_and_says_whether_it_changed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, _, bot) = with_bot(options).await;
    let first: ServerAddress = "MC.Example.com:25566".parse().unwrap();
    let other: ServerAddress = "192.0.2.10".parse().unwrap();

    let mut tx = db.write().await.unwrap();
    assert!(tx.bots().set_server(bot.id, &first).await.unwrap());
    assert!(
        !tx.bots().set_server(bot.id, &first).await.unwrap(),
        "the same address"
    );
    tx.commit(setup_entry()).await.unwrap();
    let stored = db.bots().find(bot.id).await.unwrap().unwrap();
    let mut tx = db.write().await.unwrap();
    assert!(tx.bots().set_server(bot.id, &other).await.unwrap());
    let unknown = new_bot(bot.account, 9).id;
    assert!(
        !tx.bots().set_server(unknown, &first).await.unwrap(),
        "no such bot"
    );
    tx.commit(setup_entry()).await.unwrap();

    assert_eq!(stored.server, Some(first));
    let written: Option<String> = sqlx::query_scalar!("SELECT server_address FROM bots")
        .fetch_one(db.write_pool())
        .await
        .unwrap();
    assert_eq!(written.as_deref(), Some("192.0.2.10"));
    assert_eq!(
        db.bots().find(bot.id).await.unwrap().unwrap().server,
        Some(other)
    );
}

#[sqlx::test(migrations = false)]
async fn switching_the_mode_says_whether_it_changed_and_needs_a_mode_that_exists(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, _, bot) = with_bot(options).await;
    let farm = ModePreset::Farm.id();

    let mut tx = db.write().await.unwrap();
    assert!(tx.bots().set_mode(bot.id, farm).await.unwrap());
    assert!(
        !tx.bots().set_mode(bot.id, farm).await.unwrap(),
        "the same mode"
    );
    let missing = ModeId::new_v7(at(T0), [9; 10]).unwrap();
    assert!(tx.bots().set_mode(bot.id, missing).await.is_err());
    tx.commit(setup_entry()).await.unwrap();

    assert_eq!(db.bots().find(bot.id).await.unwrap().unwrap().mode, farm);
}

#[sqlx::test(migrations = false)]
async fn the_desired_state_changes_only_to_the_other_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, _, bot) = with_bot(options).await;

    let mut tx = db.write().await.unwrap();
    assert!(
        !tx.bots()
            .set_desired(bot.id, DesiredRunState::Stopped)
            .await
            .unwrap()
    );
    assert!(
        tx.bots()
            .set_desired(bot.id, DesiredRunState::Running)
            .await
            .unwrap()
    );
    assert!(
        !tx.bots()
            .set_desired(bot.id, DesiredRunState::Running)
            .await
            .unwrap()
    );
    tx.commit(setup_entry()).await.unwrap();

    assert_eq!(
        db.bots().find(bot.id).await.unwrap().unwrap().desired,
        DesiredRunState::Running
    );
}

/// A Resume or a Reset clears a stored sticky state only while the bot runs
/// nowhere and its state is still the one the call judged (S48).
#[sqlx::test(migrations = false)]
async fn a_sticky_state_is_cleared_only_with_no_agent_and_while_it_is_the_judged_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, _, bot) = with_bot(options).await;
    set_state(
        db.write_pool(),
        bot.id,
        Some("paused"),
        Some("duplicate_login"),
        Some("2027-01-15T08:00:00.000Z"),
    )
    .await
    .unwrap();
    let paused = LastReport {
        state: LastState::Paused(PauseReason::Conflict {
            kind: ConflictKind::DuplicateLogin,
        }),
        at: "2027-01-15T08:00:00Z".parse().unwrap(),
    };
    let failed = LastReport {
        state: LastState::Failed(FailReason::Auth),
        ..paused
    };
    let later = LastReport {
        at: "2027-01-15T08:00:01Z".parse().unwrap(),
        ..paused
    };

    let mut tx = db.write().await.unwrap();
    assert!(
        !tx.bots().clear_sticky(bot.id, &failed).await.unwrap(),
        "another state"
    );
    assert!(
        !tx.bots().clear_sticky(bot.id, &later).await.unwrap(),
        "another time"
    );
    assert!(tx.bots().clear_sticky(bot.id, &paused).await.unwrap());
    assert!(
        !tx.bots().clear_sticky(bot.id, &paused).await.unwrap(),
        "cleared already"
    );
    tx.commit(setup_entry()).await.unwrap();

    assert_eq!(db.bots().find(bot.id).await.unwrap().unwrap().last, None);
}

#[sqlx::test(migrations = false)]
async fn an_assigned_bots_sticky_state_is_never_cleared(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, _, bot) = with_bot(options).await;
    set_state(
        db.write_pool(),
        bot.id,
        Some("failed"),
        Some("auth"),
        Some("2027-01-15T08:00:00.000Z"),
    )
    .await
    .unwrap();
    assign_to_agent(&db, bot.id, 1).await;
    let failed = LastReport {
        state: LastState::Failed(FailReason::Auth),
        at: "2027-01-15T08:00:00Z".parse().unwrap(),
    };

    let mut tx = db.write().await.unwrap();
    assert!(!tx.bots().clear_sticky(bot.id, &failed).await.unwrap());
    tx.commit(setup_entry()).await.unwrap();

    assert_eq!(
        db.bots().find(bot.id).await.unwrap().unwrap().last,
        Some(failed)
    );
}

/// Only the reconciler assigns (S36): no write of a user's ever changes a
/// bot's agent.
#[sqlx::test(migrations = false)]
async fn no_bot_write_of_a_users_changes_its_agent(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, _, bot) = with_bot(options).await;
    let agent = assign_to_agent(&db, bot.id, 1).await;

    let mut tx = db.write().await.unwrap();
    assert!(
        tx.bots()
            .set_server(bot.id, &"mc.example.com".parse().unwrap())
            .await
            .unwrap()
    );
    assert!(
        tx.bots()
            .set_mode(bot.id, ModePreset::Farm.id())
            .await
            .unwrap()
    );
    assert!(
        tx.bots()
            .set_desired(bot.id, DesiredRunState::Running)
            .await
            .unwrap()
    );
    tx.commit(setup_entry()).await.unwrap();

    let stored = db.bots().find(bot.id).await.unwrap().unwrap();
    assert_eq!(stored.agent, Some(agent));
    assert_eq!(stored.desired, DesiredRunState::Running);
}

#[sqlx::test(migrations = false)]
async fn a_transaction_reads_a_mode_as_the_read_pool_does(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let afk = ModePreset::Afk.id();

    let mut tx = db.write().await.unwrap();
    let in_tx = tx.modes().find(afk).await.unwrap();
    let missing = tx
        .modes()
        .find(ModeId::new_v7(at(T0), [9; 10]).unwrap())
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert_eq!(in_tx, db.modes().find(afk).await.unwrap());
    assert!(in_tx.is_some());
    assert_eq!(missing, None);
}

/// The accounts whose bots run a mode, in ID order, read in the
/// transaction: none before a switch, each after it (ADR-0020, S17).
#[sqlx::test(migrations = false)]
async fn a_modes_accounts_are_those_whose_bots_run_it_in_id_order(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let (third, third_bot) = add_account_with_bot(&db, owner, 3).await;
    let (first, first_bot) = add_account_with_bot(&db, owner, 1).await;
    let (second, _) = add_account_with_bot(&db, owner, 2).await;
    let farm = ModePreset::Farm.id();

    let mut tx = db.write().await.unwrap();
    let before = tx.bots().accounts_with_mode(farm).await.unwrap();
    tx.bots().set_mode(third_bot, farm).await.unwrap();
    tx.bots().set_mode(first_bot, farm).await.unwrap();
    let after = tx.bots().accounts_with_mode(farm).await.unwrap();
    let afk = tx
        .bots()
        .accounts_with_mode(ModePreset::Afk.id())
        .await
        .unwrap();

    assert_eq!(before, []);
    assert_eq!(after, [first, third]);
    assert_eq!(afk, [second]);
}

/// A bot's account ID that doesn't read back as an account's is corrupt,
/// naming the column.
#[sqlx::test(migrations = false)]
async fn a_modes_account_that_doesnt_read_back_is_corrupt(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await.to_string();
    // A version 4 ID: the table's checks take any UUID's form.
    let account = "550e8400-e29b-41d4-a716-446655440000";
    let bot = BotId::new_v7(at(T0), [7; 10]).unwrap().to_string();
    let time = "2027-01-15T08:00:00.000Z";
    sqlx::query!(
        "INSERT INTO mc_accounts (id, owner_user_id, kind, mc_uuid, mc_name, refresh_envelope, status, created_at)
         VALUES (?, ?, 'offline', NULL, 'AfkBot7', NULL, 'active', ?)",
        account,
        owner,
        time
    )
    .execute(db.write_pool())
    .await
    .unwrap();
    let farm = ModePreset::Farm.id().to_string();
    sqlx::query!(
        "INSERT INTO bots (id, account_id, mode_id, desired_state) VALUES (?, ?, ?, 'stopped')",
        bot,
        account,
        farm
    )
    .execute(db.write_pool())
    .await
    .unwrap();

    let mut tx = db.write().await.unwrap();
    let read = tx.bots().accounts_with_mode(ModePreset::Farm.id()).await;

    match read {
        Err(StoreError::Corrupt { table, column, .. }) => {
            assert_eq!((table, column), ("bots", "account_id"));
        }
        other => panic!("{other:?}"),
    }
}
