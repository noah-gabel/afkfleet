//! Tests for what the accounts' repository lists for the endpoints (Plan.md
//! P9.6, ADR-0010, ADR-0018): an account as one viewer sees it, the pages
//! of what a viewer may see and review, which must be exactly what
//! `authorize()` allows, deleting an account with its grants, and storing a
//! new link only over the envelope it replaces.
//!
//! Each test gets a fresh database from `#[sqlx::test]` and opens it with
//! `Database::connect`, the production settings. Time isn't paused: sqlx's
//! worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use fleet_core::account::{AccountKind, AccountStatus, RefusalReason, awaits_review};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::authz::{Actor, GrantLevel, Permission, ResourceContext, Role, UserRef, authorize};
use fleet_core::id::{AccountId, UserId};
use fleet_core::value::{McUsername, Username};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::accounts::{AccountCredential, ListedAccount, McAccount};
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::grants::AccountGrant;
use fleet_server::ports::page::{Page, PageLimit};
use fleet_server::ports::store::{Store, StoreError};
use fleet_server::ports::users::{NewUser, PasswordHash};
use secrecy::SecretString;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// The time everything starts at, in milliseconds.
const T0: i64 = 1_800_000_000_000;
/// The length of a version 1 envelope around a 16-byte token.
const ENVELOPE_LEN: usize = 61;

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
        AuditAction::try_from("test.account_lists_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// Adds `user{n}` with `role`, and returns them as an actor.
async fn add_user(db: &Database, n: u8, role: Role) -> Actor {
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
    Actor { id, role }
}

/// A Minecraft profile's UUID, distinct for each `n`.
fn profile(n: u8) -> String {
    format!("069a79f4-44e9-4726-a5be-fca90e38aa{n:02x}")
}

/// Account `n` of `owner`: a Microsoft one in `status`, or an offline one
/// for `None`. Accounts with a higher `n` are newer.
fn account(owner: UserId, n: u8, status: Option<AccountStatus>) -> McAccount {
    let credential = status.map_or(AccountCredential::Offline, |status| {
        AccountCredential::Microsoft {
            uuid: profile(n).parse().unwrap(),
            refresh_envelope: vec![n; ENVELOPE_LEN],
            status,
        }
    });
    McAccount {
        id: AccountId::new_v7(at(T0 + i64::from(n)), [n; 10]).unwrap(),
        owner,
        name: format!("AfkBot{n}").parse().unwrap(),
        created_at: at(T0 + i64::from(n)),
        credential,
    }
}

async fn insert(db: &Database, account: &McAccount) {
    let mut tx = db.write().await.unwrap();
    tx.accounts().insert(account).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
}

async fn grant(db: &Database, account: AccountId, user: UserId, level: GrantLevel, by: UserId) {
    let mut tx = db.write().await.unwrap();
    tx.grants()
        .insert(&AccountGrant {
            account,
            user,
            level,
            granted_by: by,
            granted_at: at(T0),
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
}

fn page(before: Option<AccountId>, limit: u32) -> Page<AccountId> {
    Page {
        before,
        limit: PageLimit::new(limit).unwrap(),
    }
}

fn ids(listed: &[ListedAccount]) -> Vec<AccountId> {
    listed.iter().map(|account| account.id).collect()
}

/// The Owner, two Admins and two Members, each with accounts in several
/// statuses, and grants across them.
struct Cast {
    db: Database,
    users: Vec<Actor>,
    /// Every account, with its owner's role.
    accounts: Vec<(McAccount, Role)>,
    /// Every grant: account, grantee, level.
    grants: Vec<(AccountId, UserId, GrantLevel)>,
}

async fn cast(options: SqliteConnectOptions) -> Cast {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let admin = add_user(&db, 2, Role::Admin).await;
    let other_admin = add_user(&db, 3, Role::Admin).await;
    let member = add_user(&db, 4, Role::Member).await;
    let other_member = add_user(&db, 5, Role::Member).await;
    let users = vec![owner, admin, other_admin, member, other_member];
    let statuses = [
        Some(AccountStatus::Pending),
        Some(AccountStatus::Active),
        Some(AccountStatus::Refused(RefusalReason::NotOwned)),
        Some(AccountStatus::ReauthRequired),
        None,
    ];
    let mut accounts = Vec::new();
    let mut n = 10;
    for user in &users {
        for status in statuses {
            // The Owner's own links are never pending (ADR-0018).
            if user.role == Role::Owner && status == Some(AccountStatus::Pending) {
                continue;
            }
            let account = account(user.id, n, status);
            insert(&db, &account).await;
            accounts.push((account, user.role));
            n += 1;
        }
    }
    let of = |user: &Actor, index: usize| {
        accounts
            .iter()
            .filter(|(account, _)| account.owner == user.id)
            .nth(index)
            .unwrap()
            .0
            .id
    };
    let grants = vec![
        (of(&admin, 0), member.id, GrantLevel::View),
        (of(&owner, 0), admin.id, GrantLevel::Control),
        (of(&other_admin, 0), admin.id, GrantLevel::Manage),
        (of(&member, 0), other_member.id, GrantLevel::Manage),
        (of(&other_member, 1), member.id, GrantLevel::View),
        (of(&admin, 1), other_admin.id, GrantLevel::View),
    ];
    for &(account, user, level) in &grants {
        let by = accounts
            .iter()
            .find(|(a, _)| a.id == account)
            .unwrap()
            .0
            .owner;
        grant(&db, account, user, level, by).await;
    }
    Cast {
        db,
        users,
        accounts,
        grants,
    }
}

impl Cast {
    fn grant_of(&self, account: AccountId, user: UserId) -> Option<GrantLevel> {
        self.grants
            .iter()
            .find(|&&(a, u, _)| a == account && u == user)
            .map(|&(_, _, level)| level)
    }

    /// The accounts `viewer` may do `permission` on, newest first, by
    /// `authorize()` itself.
    fn allowed(&self, viewer: &Actor, permission: Permission) -> Vec<AccountId> {
        let mut allowed: Vec<AccountId> = self
            .accounts
            .iter()
            .filter(|(account, role)| {
                let context = ResourceContext::Account {
                    owner: UserRef {
                        id: account.owner,
                        role: *role,
                    },
                    grant: self.grant_of(account.id, viewer.id),
                };
                authorize(viewer, permission, &context).is_ok()
            })
            .map(|(account, _)| account.id)
            .collect();
        allowed.sort_unstable_by(|a, b| b.cmp(a));
        allowed
    }

    fn status_of(&self, id: AccountId) -> AccountStatus {
        self.accounts
            .iter()
            .find(|(account, _)| account.id == id)
            .unwrap()
            .0
            .status()
    }
}

// --- What a viewer sees -------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn each_viewer_lists_exactly_what_authorize_lets_them_see(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let c = cast(options).await;

    for viewer in &c.users {
        let listed =
            c.db.accounts()
                .list_visible(*viewer, page(None, 100))
                .await
                .unwrap();

        assert_eq!(
            ids(&listed),
            c.allowed(viewer, Permission::ViewBot),
            "{viewer:?}"
        );
        for account in &listed {
            assert_eq!(
                account.grant,
                c.grant_of(account.id, viewer.id),
                "{viewer:?}"
            );
        }
    }
}

#[sqlx::test(migrations = false)]
async fn each_viewer_reviews_exactly_the_pending_links_authorize_lets_them_review(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let c = cast(options).await;
    let mut reviewed = BTreeSet::new();

    for viewer in &c.users {
        let expected: Vec<AccountId> = c
            .allowed(viewer, Permission::ReviewLink)
            .into_iter()
            .filter(|&id| awaits_review(c.status_of(id)))
            .collect();

        let listed =
            c.db.accounts()
                .awaiting_review(*viewer, page(None, 100))
                .await
                .unwrap();

        assert_eq!(ids(&listed), expected, "{viewer:?}");
        reviewed.extend(ids(&listed));
    }
    // Every pending link has someone to review it: the Owner at least.
    let pending: BTreeSet<AccountId> = c
        .accounts
        .iter()
        .filter(|(account, _)| account.status() == AccountStatus::Pending)
        .map(|(account, _)| account.id)
        .collect();
    assert_eq!(reviewed, pending);
}

#[sqlx::test(migrations = false)]
async fn a_listed_account_carries_its_owner_and_the_viewers_grant(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let admin = add_user(&db, 1, Role::Admin).await;
    let member = add_user(&db, 2, Role::Member).await;
    let refused = account(
        admin.id,
        1,
        Some(AccountStatus::Refused(RefusalReason::XboxBanned)),
    );
    let offline = account(admin.id, 2, None);
    insert(&db, &refused).await;
    insert(&db, &offline).await;
    grant(&db, refused.id, member.id, GrantLevel::Control, admin.id).await;
    let expected = ListedAccount {
        id: refused.id,
        owner: UserRef {
            id: admin.id,
            role: Role::Admin,
        },
        owner_name: Username::try_from("user1").unwrap(),
        name: refused.name.clone(),
        uuid: Some(profile(1).parse().unwrap()),
        kind: AccountKind::Microsoft,
        status: AccountStatus::Refused(RefusalReason::XboxBanned),
        created_at: refused.created_at,
        grant: Some(GrantLevel::Control),
    };

    let read = db.accounts().listed(refused.id, member.id).await.unwrap();
    let in_tx = {
        let mut tx = db.write().await.unwrap();
        tx.accounts().listed(refused.id, member.id).await.unwrap()
    };
    let by_owner = db.accounts().listed(refused.id, admin.id).await.unwrap();
    let offline_read = db.accounts().listed(offline.id, member.id).await.unwrap();

    assert_eq!(read.as_ref(), Some(&expected));
    assert_eq!(in_tx.as_ref(), Some(&expected));
    assert_eq!(
        by_owner,
        Some(ListedAccount {
            grant: None,
            ..expected
        })
    );
    let offline_read = offline_read.unwrap();
    assert_eq!(offline_read.kind, AccountKind::Offline);
    assert_eq!(offline_read.uuid, None);
    assert_eq!(offline_read.status, AccountStatus::Active);
    assert_eq!(offline_read.grant, None);
}

#[sqlx::test(migrations = false)]
async fn an_unknown_account_isnt_listed(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let viewer = add_user(&db, 1, Role::Owner).await;
    insert(&db, &account(viewer.id, 1, Some(AccountStatus::Active))).await;
    let unknown = AccountId::new_v7(at(T0), [99; 10]).unwrap();

    let read = db.accounts().listed(unknown, viewer.id).await.unwrap();

    assert_eq!(read, None);
}

#[sqlx::test(migrations = false)]
async fn the_lists_page_newest_first(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let member = add_user(&db, 2, Role::Member).await;
    let accounts: Vec<McAccount> = (1..=4)
        .map(|n| account(member.id, n, Some(AccountStatus::Pending)))
        .collect();
    for account in &accounts {
        insert(&db, account).await;
    }
    let [a1, a2, a3, a4] = [0, 1, 2, 3].map(|i| accounts[i].id);

    for list in ["visible", "review"] {
        let read = |before| {
            let db = &db;
            async move {
                let page = page(before, 2);
                if list == "visible" {
                    db.accounts().list_visible(owner, page).await.unwrap()
                } else {
                    db.accounts().awaiting_review(owner, page).await.unwrap()
                }
            }
        };

        // A repository fetches one row more than the page holds.
        assert_eq!(ids(&read(None).await), [a4, a3, a2], "{list}");
        assert_eq!(ids(&read(Some(a2)).await), [a1], "{list}");
    }
}

#[sqlx::test(migrations = false)]
async fn a_role_change_moves_an_account_between_the_lists(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let admin = add_user(&db, 2, Role::Admin).await;
    let member = add_user(&db, 3, Role::Member).await;
    let pending = account(member.id, 1, Some(AccountStatus::Pending));
    insert(&db, &pending).await;
    let lists = |viewer: Actor| {
        let db = &db;
        async move {
            (
                ids(&db
                    .accounts()
                    .list_visible(viewer, page(None, 50))
                    .await
                    .unwrap()),
                ids(&db
                    .accounts()
                    .awaiting_review(viewer, page(None, 50))
                    .await
                    .unwrap()),
            )
        }
    };
    assert_eq!(lists(admin).await, (vec![pending.id], vec![pending.id]));
    let mut tx = db.write().await.unwrap();
    tx.users().set_role(member.id, Role::Admin).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert_eq!(lists(admin).await, (vec![], vec![]));
    assert_eq!(lists(owner).await, (vec![pending.id], vec![pending.id]));
}

#[sqlx::test(migrations = false)]
async fn a_stored_status_that_doesnt_read_back_is_corrupt(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let stored = account(owner.id, 1, Some(AccountStatus::Active));
    insert(&db, &stored).await;
    let id = stored.id.to_string();
    sqlx::query!("UPDATE mc_accounts SET status = 'frozen' WHERE id = ?", id)
        .execute(db.write_pool())
        .await
        .unwrap();

    let listed = db.accounts().listed(stored.id, owner.id).await;
    let visible = db.accounts().list_visible(owner, page(None, 50)).await;

    for read in [listed.map(drop), visible.map(drop)] {
        assert!(
            matches!(
                read,
                Err(StoreError::Corrupt {
                    table: "mc_accounts",
                    column: "status",
                    ..
                })
            ),
            "{read:?}"
        );
    }
}

// --- Deleting ---------------------------------------------------------------------------

async fn grant_rows(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar!(r#"SELECT count(*) AS "count!: i64" FROM account_grants"#)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = false)]
async fn deleting_an_account_takes_its_grants_and_nothing_else(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let member = add_user(&db, 2, Role::Member).await;
    let gone = account(owner.id, 1, Some(AccountStatus::Active));
    let kept = account(owner.id, 2, None);
    insert(&db, &gone).await;
    insert(&db, &kept).await;
    grant(&db, gone.id, member.id, GrantLevel::View, owner.id).await;
    grant(&db, kept.id, member.id, GrantLevel::View, owner.id).await;
    let mut tx = db.write().await.unwrap();

    let deleted = tx.accounts().delete(gone.id).await.unwrap();
    let again = tx.accounts().delete(gone.id).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert!(deleted);
    assert!(!again);
    assert_eq!(db.accounts().find(gone.id).await.unwrap(), None);
    assert_eq!(db.accounts().find(kept.id).await.unwrap(), Some(kept));
    assert_eq!(grant_rows(db.read_pool()).await, 1);
}

// --- Linking again ----------------------------------------------------------------------

async fn relink(db: &Database, id: AccountId, from: &[u8], to: &[u8], name: &str) -> bool {
    let mut tx = db.write().await.unwrap();
    let name: McUsername = name.parse().unwrap();
    let done = tx.accounts().relink(id, from, to, &name).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    done
}

#[sqlx::test(migrations = false)]
async fn a_new_link_replaces_the_envelope_activates_and_renames_an_approved_account(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Admin).await;
    let statuses = [
        AccountStatus::Active,
        AccountStatus::ReauthRequired,
        AccountStatus::Refused(RefusalReason::ProfileMismatch),
    ];
    for (n, status) in (1..).zip(statuses) {
        let stored = account(owner.id, n, Some(status));
        insert(&db, &stored).await;
        let new = vec![0xEE; ENVELOPE_LEN];

        let done = relink(&db, stored.id, &[n; ENVELOPE_LEN], &new, "Renamed_1").await;

        assert!(done, "{status:?}");
        let read = db.accounts().find(stored.id).await.unwrap().unwrap();
        assert_eq!(read.name.as_str(), "Renamed_1", "{status:?}");
        assert_eq!(
            read.credential,
            AccountCredential::Microsoft {
                uuid: profile(n).parse().unwrap(),
                refresh_envelope: new,
                status: AccountStatus::Active,
            },
            "{status:?}"
        );
    }
}

#[sqlx::test(migrations = false)]
async fn a_new_link_changes_nothing_over_another_envelope_or_on_a_pending_or_offline_account(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Member).await;
    let active = account(owner.id, 1, Some(AccountStatus::Active));
    let pending = account(owner.id, 2, Some(AccountStatus::Pending));
    let offline = account(owner.id, 3, None);
    for stored in [&active, &pending, &offline] {
        insert(&db, stored).await;
    }
    let new = [0xEE; ENVELOPE_LEN];

    let over_another = relink(&db, active.id, &[9; ENVELOPE_LEN], &new, "AfkBot1").await;
    let on_pending = relink(&db, pending.id, &[2; ENVELOPE_LEN], &new, "AfkBot2").await;
    let on_offline = relink(&db, offline.id, &[3; ENVELOPE_LEN], &new, "AfkBot3").await;

    assert!(!over_another);
    assert!(!on_pending);
    assert!(!on_offline);
    for stored in [active, pending, offline] {
        assert_eq!(db.accounts().find(stored.id).await.unwrap(), Some(stored));
    }
}
