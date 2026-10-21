//! Tests for the modes' table and repository (Plan.md P11.1, P11.4,
//! Appendix D; ADR-0019, S38; ADR-0020, S13): the migration seeds the
//! built-in modes `afk` and `farm` with the fixed IDs fleet-core's
//! `ModePreset::id` names, as exactly the JSON `serde_json` writes for each
//! preset, which reads back through the validated deserializer as the preset;
//! a stored mode or name that isn't valid reads as corrupt without its text;
//! a name is unique without ASCII case per owner and among shared modes; and
//! the table refuses what its CHECKs forbid.
//!
//! Each test gets a fresh database from `#[sqlx::test]` and opens it with
//! `Database::connect`, the production settings. Time isn't paused: sqlx's
//! worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::num::NonZeroU32;

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::authz::{
    Actor, ModeVisibility, Permission, ResourceContext, Role, UserRef, authorize,
};
use fleet_core::id::{ModeId, UserId};
use fleet_core::mode::{ModeDefinition, ModeName, ModePreset};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::modes::{ListedMode, ModeChange, NewMode, StoredMode, WriteModeError};
use fleet_server::ports::page::{Page, PageLimit};
use fleet_server::ports::store::{Store, StoreError};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// When the seeds were made: the time in their IDs.
const SEEDED: &str = "2026-10-16T00:00:00.000Z";
/// A user's ID, the owner of the rows the CHECK tests insert.
const OWNER: &str = "018bcfe5-6800-7bab-abab-abababababab";
/// Another user's ID.
const OTHER_OWNER: &str = "018bcfe5-6800-7bcb-abcb-cbcbcbcbcbcb";
/// A mode's ID the seeds don't use.
const OTHER: &str = "018bcfe5-6800-7cdc-8dcd-cdcdcdcdcdcd";
/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// A mode's JSON that is valid.
const VALID: &str = r#"{"steps":[{"action":{"type":"swing_arm"},"schedule":{"type":"every","interval_ms":20000,"jitter_ms":20000},"probability":100}]}"#;

async fn database(options: SqliteConnectOptions) -> Database {
    Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap()
}

fn seeded() -> DateTime<Utc> {
    SEEDED.parse().unwrap()
}

/// Adds the user `OWNER`, through the write pool.
async fn add_owner(pool: &SqlitePool) {
    sqlx::query!(
        "INSERT INTO users (id, username, password_hash, role, disabled, created_at, password_changed_at)
         VALUES (?, 'owner', ?, 'owner', 0, ?, ?)",
        OWNER,
        PHC,
        SEEDED,
        SEEDED
    )
    .execute(pool)
    .await
    .unwrap();
}

/// Adds `OWNER` and the user `OTHER_OWNER`, through the write pool.
async fn add_owners(pool: &SqlitePool) {
    add_owner(pool).await;
    sqlx::query!(
        "INSERT INTO users (id, username, password_hash, role, disabled, created_at, password_changed_at)
         VALUES (?, 'other', ?, 'admin', 0, ?, ?)",
        OTHER_OWNER,
        PHC,
        SEEDED,
        SEEDED
    )
    .execute(pool)
    .await
    .unwrap();
}

/// A row for `modes`, changed by the test before it's inserted.
#[derive(Debug, Clone)]
struct Row {
    id: &'static str,
    owner: Option<&'static str>,
    name: &'static str,
    visibility: &'static str,
    json: &'static str,
    version: i64,
    created_at: &'static str,
    updated_at: &'static str,
}

impl Row {
    /// A valid row: a private mode of `OWNER`.
    const fn valid() -> Self {
        Self {
            id: OTHER,
            owner: Some(OWNER),
            name: "Mine",
            visibility: "private",
            json: VALID,
            version: 1,
            created_at: SEEDED,
            updated_at: SEEDED,
        }
    }

    /// Inserts the row through the write pool.
    async fn insert(&self, pool: &SqlitePool) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "INSERT INTO modes (id, owner_user_id, name, visibility, definition_json, version, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            self.id,
            self.owner,
            self.name,
            self.visibility,
            self.json,
            self.version,
            self.created_at,
            self.updated_at
        )
        .execute(pool)
        .await
        .map(drop)
    }
}

#[sqlx::test(migrations = false)]
async fn the_seeded_modes_read_back_through_the_validated_deserializer_as_the_presets(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;

    for preset in ModePreset::ALL {
        assert_eq!(
            db.modes().find(preset.id()).await.unwrap(),
            Some(StoredMode {
                id: preset.id(),
                owner: None,
                name: ModeName::try_from(preset.name()).unwrap(),
                visibility: ModeVisibility::Shared,
                definition: preset.definition(),
                version: NonZeroU32::MIN,
                created_at: seeded(),
                updated_at: seeded(),
            }),
            "{preset}"
        );
    }
}

#[sqlx::test(migrations = false)]
async fn the_seeds_are_named_as_the_presets_in_serde_jsons_compact_form(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;

    for preset in ModePreset::ALL {
        let id = preset.id().to_string();
        let row = sqlx::query!(
            r#"SELECT name AS "name!", definition_json AS "json!" FROM modes WHERE id = ?"#,
            id
        )
        .fetch_one(db.read_pool())
        .await
        .unwrap();
        assert_eq!(row.name, preset.name());
        assert_eq!(
            row.json,
            serde_json::to_string(&preset.definition()).unwrap()
        );
    }
    let built_in = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!: i64" FROM modes WHERE owner_user_id IS NULL"#
    )
    .fetch_one(db.read_pool())
    .await
    .unwrap();
    assert_eq!(usize::try_from(built_in).unwrap(), ModePreset::ALL.len());
}

#[sqlx::test(migrations = false)]
async fn an_unknown_mode_is_none(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;

    assert_eq!(db.modes().find(OTHER.parse().unwrap()).await.unwrap(), None);
}

#[sqlx::test(migrations = false)]
async fn a_users_mode_reads_back_with_its_owner_visibility_and_version(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    add_owner(db.write_pool()).await;
    Row {
        visibility: "shared",
        version: 3,
        updated_at: "2026-10-17T00:00:00.000Z",
        ..Row::valid()
    }
    .insert(db.write_pool())
    .await
    .unwrap();

    let found = db.modes().find(OTHER.parse().unwrap()).await.unwrap();

    assert_eq!(
        found,
        Some(StoredMode {
            id: OTHER.parse().unwrap(),
            owner: Some(OWNER.parse().unwrap()),
            name: ModeName::try_from("Mine").unwrap(),
            visibility: ModeVisibility::Shared,
            definition: serde_json::from_str(VALID).unwrap(),
            version: NonZeroU32::new(3).unwrap(),
            created_at: seeded(),
            updated_at: "2026-10-17T00:00:00.000Z".parse().unwrap(),
        })
    );
}

/// An on-respawn step is stored and read back as itself; a schedule type
/// this version doesn't know is `Corrupt`, as any row that doesn't read
/// back, never read as another schedule (ADR-0020, S25).
#[sqlx::test(migrations = false)]
async fn an_on_respawn_step_reads_back_and_an_unknown_schedule_is_corrupt(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    add_owner(db.write_pool()).await;
    let unknown_id = "018bcfe5-6800-7eee-8eee-eeeeeeeeeeee";
    Row {
        json: r#"{"steps":[{"action":{"type":"jump"},"schedule":{"type":"on_respawn"},"probability":100}]}"#,
        ..Row::valid()
    }
    .insert(db.write_pool())
    .await
    .unwrap();
    Row {
        id: unknown_id,
        name: "Unknown",
        json: r#"{"steps":[{"action":{"type":"jump"},"schedule":{"type":"on_teleport"},"probability":100}]}"#,
        ..Row::valid()
    }
    .insert(db.write_pool())
    .await
    .unwrap();

    let read = db.modes().find(OTHER.parse().unwrap()).await.unwrap();
    let unknown = db.modes().find(unknown_id.parse().unwrap()).await;

    let steps = read.unwrap().definition.steps().to_vec();
    assert_eq!(
        steps.iter().map(|step| step.schedule).collect::<Vec<_>>(),
        [fleet_core::mode::Schedule::OnRespawn]
    );
    assert!(
        matches!(
            unknown,
            Err(StoreError::Corrupt {
                table: "modes",
                column: "definition_json",
                ..
            })
        ),
        "{unknown:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_stored_mode_that_isnt_valid_is_corrupt_and_the_error_never_holds_its_json(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    add_owner(db.write_pool()).await;
    Row {
        json: r#"{"steps":[{"action":{"type":"say-secret-x"}}]}"#,
        ..Row::valid()
    }
    .insert(db.write_pool())
    .await
    .unwrap();

    let read = db.modes().find(OTHER.parse().unwrap()).await;

    match read {
        Err(error @ StoreError::Corrupt { table, column, .. }) => {
            assert_eq!((table, column), ("modes", "definition_json"));
            assert!(!error.to_string().contains("secret"), "{error}");
            assert!(!format!("{error:?}").contains("secret"), "{error:?}");
        }
        other => panic!("{other:?}"),
    }
}

#[sqlx::test(migrations = false)]
async fn the_table_refuses_what_its_checks_forbid_and_takes_every_visibility(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let pool = db.write_pool();
    add_owner(pool).await;
    let refused = [
        (
            "a private built-in",
            Row {
                owner: None,
                ..Row::valid()
            },
        ),
        (
            "an unknown visibility",
            Row {
                visibility: "public",
                ..Row::valid()
            },
        ),
        (
            "JSON that isn't JSON",
            Row {
                json: "{steps",
                ..Row::valid()
            },
        ),
        (
            "JSON that isn't an object",
            Row {
                json: "[]",
                ..Row::valid()
            },
        ),
        (
            "version 0",
            Row {
                version: 0,
                ..Row::valid()
            },
        ),
        (
            "a change before the creation",
            Row {
                updated_at: "2026-10-15T00:00:00.000Z",
                ..Row::valid()
            },
        ),
        (
            "an ID in upper case",
            Row {
                id: "018BCFE5-6800-7CDC-8DCD-CDCDCDCDCDCD",
                ..Row::valid()
            },
        ),
        (
            "a time without milliseconds",
            Row {
                created_at: "2026-10-16T00:00:00Z",
                ..Row::valid()
            },
        ),
        (
            "an owner that doesn't exist",
            Row {
                owner: Some("018bcfe5-6800-7eee-8eee-eeeeeeeeeeee"),
                ..Row::valid()
            },
        ),
    ];

    for (case, row) in refused {
        assert!(row.insert(pool).await.is_err(), "{case} was stored");
    }
    for (n, visibility) in [ModeVisibility::Private, ModeVisibility::Shared]
        .into_iter()
        .enumerate()
    {
        let id = [
            "018bcfe5-6800-7aaa-8aaa-aaaaaaaaaaa1",
            "018bcfe5-6800-7aaa-8aaa-aaaaaaaaaaa2",
        ]
        .get(n)
        .copied()
        .unwrap();
        let row = Row {
            id,
            name: ["Private one", "Shared one"].get(n).copied().unwrap(),
            visibility: visibility.as_str(),
            ..Row::valid()
        };
        assert!(row.insert(pool).await.is_ok(), "{visibility:?} was refused");
    }
}

/// A stored name the constructor refuses or changes reads as corrupt,
/// naming its column, never the name.
#[sqlx::test(migrations = false)]
async fn a_stored_name_that_doesnt_read_back_is_corrupt_without_its_text(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    add_owner(db.write_pool()).await;

    for (n, name) in ["secret🐄", " secret untrimmed", ""]
        .into_iter()
        .enumerate()
    {
        let id = [
            "018bcfe5-6800-7aaa-8aaa-aaaaaaaaaab1",
            "018bcfe5-6800-7aaa-8aaa-aaaaaaaaaab2",
            "018bcfe5-6800-7aaa-8aaa-aaaaaaaaaab3",
        ]
        .get(n)
        .copied()
        .unwrap();
        Row {
            id,
            name,
            ..Row::valid()
        }
        .insert(db.write_pool())
        .await
        .unwrap();

        let read = db.modes().find(id.parse().unwrap()).await;

        match read {
            Err(error @ StoreError::Corrupt { table, column, .. }) => {
                assert_eq!((table, column), ("modes", "name"), "{name:?}");
                assert!(!format!("{error:?}").contains("secret"), "{error:?}");
            }
            other => panic!("{name:?}: {other:?}"),
        }
    }
}

/// One owner's modes, private and shared together, never share a name,
/// ASCII case ignored; another owner may use it (S13).
#[sqlx::test(migrations = false)]
async fn a_name_is_unique_per_owner_without_ascii_case(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let pool = db.write_pool();
    add_owners(pool).await;
    Row {
        name: "Fishing",
        ..Row::valid()
    }
    .insert(pool)
    .await
    .unwrap();

    let same_owner = [
        ("the same name", "Fishing", "private"),
        ("another ASCII case", "fISHING", "private"),
        ("the same name, shared", "Fishing", "shared"),
    ];
    for (n, (case, name, visibility)) in same_owner.into_iter().enumerate() {
        let id = [
            "018bcfe5-6800-7aaa-8aaa-aaaaaaaaaac1",
            "018bcfe5-6800-7aaa-8aaa-aaaaaaaaaac2",
            "018bcfe5-6800-7aaa-8aaa-aaaaaaaaaac3",
        ]
        .get(n)
        .copied()
        .unwrap();
        let row = Row {
            id,
            name,
            visibility,
            ..Row::valid()
        };
        assert!(row.insert(pool).await.is_err(), "{case} was stored");
    }
    let others = Row {
        id: "018bcfe5-6800-7aaa-8aaa-aaaaaaaaaac4",
        owner: Some(OTHER_OWNER),
        name: "fishing",
        ..Row::valid()
    };
    assert!(
        others.insert(pool).await.is_ok(),
        "another owner's was refused"
    );
    let non_ascii_case = Row {
        id: "018bcfe5-6800-7aaa-8aaa-aaaaaaaaaac5",
        name: "FİSHING",
        ..Row::valid()
    };
    assert!(
        non_ascii_case.insert(pool).await.is_ok(),
        "only ASCII case is ignored, as lower() and same_name do"
    );
}

/// No two shared modes share a name, ASCII case ignored, the built-in ones
/// included; a private mode may take a shared one's name (S13).
#[sqlx::test(migrations = false)]
async fn a_shared_name_is_unique_among_shared_modes_the_built_in_ones_included(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let pool = db.write_pool();
    add_owners(pool).await;
    Row {
        name: "Fishing",
        visibility: "shared",
        ..Row::valid()
    }
    .insert(pool)
    .await
    .unwrap();

    let refused = [
        ("another owner's shared mode of that name", "FISHING"),
        ("a built-in mode's name", "AFK"),
    ];
    for (n, (case, name)) in refused.into_iter().enumerate() {
        let id = [
            "018bcfe5-6800-7aaa-8aaa-aaaaaaaaaad1",
            "018bcfe5-6800-7aaa-8aaa-aaaaaaaaaad2",
        ]
        .get(n)
        .copied()
        .unwrap();
        let row = Row {
            id,
            owner: Some(OTHER_OWNER),
            name,
            visibility: "shared",
            ..Row::valid()
        };
        assert!(row.insert(pool).await.is_err(), "{case} was stored");
    }
    let private = Row {
        id: "018bcfe5-6800-7aaa-8aaa-aaaaaaaaaad3",
        owner: Some(OTHER_OWNER),
        name: "fishing",
        ..Row::valid()
    };
    assert!(
        private.insert(pool).await.is_ok(),
        "a private namesake was refused"
    );
}

/// A user of `role`, named `name`, with an ID from `n`.
async fn add_user(pool: &SqlitePool, n: u8, name: &str, role: Role) -> UserId {
    let id = UserId::new_v7(seeded(), [n; 10]).unwrap();
    let text = id.to_string();
    let role = role.as_str();
    sqlx::query!(
        "INSERT INTO users (id, username, password_hash, role, disabled, created_at, password_changed_at)
         VALUES (?, ?, ?, ?, 0, ?, ?)",
        text,
        name,
        PHC,
        role,
        SEEDED,
        SEEDED
    )
    .execute(pool)
    .await
    .unwrap();
    id
}

/// Mode `n` of `owner`, named `name`, with `visibility`: a later `n` is a
/// newer mode.
async fn add_mode(pool: &SqlitePool, n: u8, owner: UserId, name: &str, visibility: &str) -> ModeId {
    let created = seeded() + chrono::Duration::minutes(i64::from(n));
    let id = ModeId::new_v7(created, [n; 10]).unwrap();
    let text = id.to_string();
    let owner = owner.to_string();
    sqlx::query!(
        "INSERT INTO modes (id, owner_user_id, name, visibility, definition_json, version, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, 1, ?, ?)",
        text,
        owner,
        name,
        visibility,
        VALID,
        SEEDED,
        SEEDED
    )
    .execute(pool)
    .await
    .unwrap();
    id
}

fn page(before: Option<ModeId>, limit: u32) -> Page<ModeId> {
    Page {
        before,
        limit: PageLimit::new(limit).unwrap(),
    }
}

/// Users of every role and modes of every kind: each user's private mode,
/// an Admin's and the Owner's shared ones, and a shared mode of a Member
/// (an Admin demoted since).
struct Cast {
    owner: Actor,
    admin: Actor,
    other_admin: Actor,
    member: Actor,
    other_member: Actor,
}

async fn cast(pool: &SqlitePool) -> Cast {
    let actor = |id, role| Actor { id, role };
    let owner = actor(
        add_user(pool, 11, "theowner", Role::Owner).await,
        Role::Owner,
    );
    let admin = actor(add_user(pool, 12, "admin", Role::Admin).await, Role::Admin);
    let other_admin = actor(add_user(pool, 13, "admin2", Role::Admin).await, Role::Admin);
    let member = actor(
        add_user(pool, 14, "member", Role::Member).await,
        Role::Member,
    );
    let other_member = actor(
        add_user(pool, 15, "member2", Role::Member).await,
        Role::Member,
    );
    let modes = [
        (owner.id, "Owner private", "private"),
        (owner.id, "Owner shared", "shared"),
        (admin.id, "Admin private", "private"),
        (admin.id, "Admin shared", "shared"),
        (other_admin.id, "Admin2 private", "private"),
        (member.id, "Member private", "private"),
        (other_member.id, "Member2 private", "private"),
        (other_member.id, "Demoted shared", "shared"),
    ];
    for (n, (owner, name, visibility)) in (1_u8..).zip(modes) {
        add_mode(pool, n, owner, name, visibility).await;
    }
    Cast {
        owner,
        admin,
        other_admin,
        member,
        other_member,
    }
}

/// Every mode in the table, as `authorize()` sees it.
async fn every_mode(db: &Database, everyone: Actor) -> Vec<ListedMode> {
    db.modes().visible(everyone, page(None, 100)).await.unwrap()
}

/// The visible modes are exactly those `authorize(ViewMode)` lets each viewer
/// see, the built-in ones included, newest first (ADR-0010).
#[sqlx::test(migrations = false)]
async fn the_visible_modes_are_exactly_those_authorize_lets_the_viewer_see(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let cast = cast(db.write_pool()).await;
    let all = every_mode(&db, cast.owner).await;
    assert_eq!(all.len(), 10, "the Owner sees every mode");

    for viewer in [
        cast.owner,
        cast.admin,
        cast.other_admin,
        cast.member,
        cast.other_member,
    ] {
        let seen: Vec<ModeId> = db
            .modes()
            .visible(viewer, page(None, 100))
            .await
            .unwrap()
            .iter()
            .map(|listed| listed.mode.id)
            .collect();
        let allowed: Vec<ModeId> = all
            .iter()
            .filter(|listed| authorize(&viewer, Permission::ViewMode, &listed.context()).is_ok())
            .map(|listed| listed.mode.id)
            .collect();

        assert_eq!(seen, allowed, "{viewer:?}");
        assert!(seen.is_sorted_by(|a, b| a > b), "newest first: {viewer:?}");
    }
}

/// A sanity check on the rule itself, so the test above can't pass with
/// both sides wrong: what a Member and an Admin see, by name.
#[sqlx::test(migrations = false)]
async fn a_member_sees_the_shared_and_their_own_and_an_admin_also_members_private_ones(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let cast = cast(db.write_pool()).await;
    let names = |listed: Vec<ListedMode>| -> Vec<String> {
        listed
            .into_iter()
            .map(|listed| listed.mode.name.as_str().to_owned())
            .collect()
    };

    let members = names(
        db.modes()
            .visible(cast.member, page(None, 100))
            .await
            .unwrap(),
    );
    let admins = names(
        db.modes()
            .visible(cast.admin, page(None, 100))
            .await
            .unwrap(),
    );

    assert_eq!(
        members,
        [
            "Demoted shared",
            "Member private",
            "Admin shared",
            "Owner shared",
            "farm",
            "afk"
        ]
    );
    assert_eq!(
        admins,
        [
            "Demoted shared",
            "Member2 private",
            "Member private",
            "Admin shared",
            "Admin private",
            "Owner shared",
            "farm",
            "afk",
        ]
    );
}

/// Each listed mode carries its owner's current role and username, and a
/// built-in one none.
#[sqlx::test(migrations = false)]
async fn a_listed_mode_carries_its_owners_current_role_and_name(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let cast = cast(db.write_pool()).await;
    let promoted = cast.other_member.id.to_string();
    sqlx::query!("UPDATE users SET role = 'admin' WHERE id = ?", promoted)
        .execute(db.write_pool())
        .await
        .unwrap();

    let all = every_mode(&db, cast.owner).await;

    let owners: Vec<(String, Option<(UserRef, String)>)> = all
        .iter()
        .map(|listed| {
            (
                listed.mode.name.as_str().to_owned(),
                listed
                    .owner
                    .as_ref()
                    .map(|owner| (owner.user, owner.username.as_str().to_owned())),
            )
        })
        .collect();
    let of = |actor: Actor, role: Role, name: &str| {
        Some((UserRef { id: actor.id, role }, name.to_owned()))
    };
    assert_eq!(
        owners,
        [
            (
                "Demoted shared".to_owned(),
                of(cast.other_member, Role::Admin, "member2")
            ),
            (
                "Member2 private".to_owned(),
                of(cast.other_member, Role::Admin, "member2")
            ),
            (
                "Member private".to_owned(),
                of(cast.member, Role::Member, "member")
            ),
            (
                "Admin2 private".to_owned(),
                of(cast.other_admin, Role::Admin, "admin2")
            ),
            (
                "Admin shared".to_owned(),
                of(cast.admin, Role::Admin, "admin")
            ),
            (
                "Admin private".to_owned(),
                of(cast.admin, Role::Admin, "admin")
            ),
            (
                "Owner shared".to_owned(),
                of(cast.owner, Role::Owner, "theowner")
            ),
            (
                "Owner private".to_owned(),
                of(cast.owner, Role::Owner, "theowner")
            ),
            ("farm".to_owned(), None),
            ("afk".to_owned(), None),
        ]
    );
    let built_in = all.last().unwrap();
    assert_eq!(
        built_in.context(),
        ResourceContext::Mode {
            owner: None,
            visibility: ModeVisibility::Shared
        }
    );
}

/// Pages are newest first, `before` exclusive, one more row than the page
/// holds, so the caller can tell a next page follows.
#[sqlx::test(migrations = false)]
async fn the_modes_page_newest_first_before_their_cursor(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let cast = cast(db.write_pool()).await;
    let all: Vec<ModeId> = every_mode(&db, cast.owner)
        .await
        .iter()
        .map(|listed| listed.mode.id)
        .collect();

    let first: Vec<ModeId> = db
        .modes()
        .visible(cast.owner, page(None, 3))
        .await
        .unwrap()
        .iter()
        .map(|listed| listed.mode.id)
        .collect();
    let after = *all.get(2).unwrap();
    let second: Vec<ModeId> = db
        .modes()
        .visible(cast.owner, page(Some(after), 3))
        .await
        .unwrap()
        .iter()
        .map(|listed| listed.mode.id)
        .collect();

    assert_eq!(first, all.get(..4).unwrap(), "a page and one row more");
    assert_eq!(second, all.get(3..7).unwrap());
}

/// The entry a test commits its writes with. A test-only name, never
/// listed in `audit_actions!`.
fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        seeded(),
        AuditAction::try_from("test.modes_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// A new mode of `owner` numbered `n`, named `name`, with `visibility`.
fn new_mode(n: u8, owner: UserId, name: &str, visibility: ModeVisibility) -> NewMode {
    let created = seeded() + chrono::Duration::minutes(i64::from(n));
    NewMode {
        id: ModeId::new_v7(created, [n; 10]).unwrap(),
        owner,
        name: ModeName::try_from(name).unwrap(),
        visibility,
        definition: serde_json::from_str::<ModeDefinition>(VALID).unwrap(),
        created_at: created,
    }
}

/// Stores `mode` in a transaction of its own.
async fn insert(db: &Database, mode: &NewMode) -> Result<(), WriteModeError> {
    let mut tx = db.write().await.unwrap();
    tx.modes().insert(mode).await?;
    tx.commit(setup_entry()).await.unwrap();
    Ok(())
}

/// A stored mode reads back as it was given, at version 1, its definition
/// in `serde_json`'s compact form, as the seeds are.
#[sqlx::test(migrations = false)]
async fn an_inserted_mode_reads_back_at_version_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(db.write_pool(), 21, "fisher", Role::Member).await;
    let mode = new_mode(1, owner, "Fishing trip", ModeVisibility::Shared);

    insert(&db, &mode).await.unwrap();

    let found = db.modes().find(mode.id).await.unwrap();
    assert_eq!(
        found,
        Some(StoredMode {
            id: mode.id,
            owner: Some(owner),
            name: mode.name.clone(),
            visibility: ModeVisibility::Shared,
            definition: mode.definition.clone(),
            version: NonZeroU32::MIN,
            created_at: mode.created_at,
            updated_at: mode.created_at,
        })
    );
    let id = mode.id.to_string();
    let json = sqlx::query_scalar!(
        r#"SELECT definition_json AS "json!" FROM modes WHERE id = ?"#,
        id
    )
    .fetch_one(db.read_pool())
    .await
    .unwrap();
    assert_eq!(json, serde_json::to_string(&mode.definition).unwrap());
}

/// A name the unique indexes refuse is `NameTaken`, and nothing is stored.
#[sqlx::test(migrations = false)]
async fn a_name_the_indexes_refuse_is_taken_and_nothing_is_stored(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(db.write_pool(), 21, "fisher", Role::Member).await;
    let other = add_user(db.write_pool(), 22, "other", Role::Admin).await;
    insert(&db, &new_mode(1, owner, "Fishing", ModeVisibility::Private))
        .await
        .unwrap();
    insert(&db, &new_mode(2, other, "Shared", ModeVisibility::Shared))
        .await
        .unwrap();

    for (case, mode) in [
        (
            "the owner's own",
            new_mode(3, owner, "FISHING", ModeVisibility::Private),
        ),
        (
            "a shared one's",
            new_mode(4, owner, "shared", ModeVisibility::Shared),
        ),
        (
            "a built-in one's",
            new_mode(5, owner, "Afk", ModeVisibility::Shared),
        ),
    ] {
        let result = insert(&db, &mode).await;
        assert!(
            matches!(result, Err(WriteModeError::NameTaken)),
            "{case}: {result:?}"
        );
        assert_eq!(db.modes().find(mode.id).await.unwrap(), None, "{case}");
    }
}

/// An owner's modes are counted, private and shared together; nobody
/// else's, and never the built-in ones (S19).
#[sqlx::test(migrations = false)]
async fn an_owners_modes_are_counted_private_and_shared_together(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(db.write_pool(), 21, "fisher", Role::Admin).await;
    let other = add_user(db.write_pool(), 22, "other", Role::Member).await;
    insert(&db, &new_mode(1, owner, "One", ModeVisibility::Private))
        .await
        .unwrap();
    insert(&db, &new_mode(2, owner, "Two", ModeVisibility::Shared))
        .await
        .unwrap();
    insert(&db, &new_mode(3, other, "Three", ModeVisibility::Private))
        .await
        .unwrap();

    let mut tx = db.write().await.unwrap();
    let counts = (
        tx.modes().count_owned_by(owner).await.unwrap(),
        tx.modes().count_owned_by(other).await.unwrap(),
        tx.modes()
            .count_owned_by(UserId::new_v7(seeded(), [99; 10]).unwrap())
            .await
            .unwrap(),
    );

    assert_eq!(counts, (2, 1, 0));
}

/// A name is taken by another of the owner's modes, and for a shared mode
/// by any shared one, ASCII case ignored; a private mode may take a shared
/// one's name, and another owner's private name is free (S13).
#[sqlx::test(migrations = false)]
async fn a_name_is_taken_as_the_indexes_would_refuse_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(db.write_pool(), 21, "fisher", Role::Admin).await;
    let other = add_user(db.write_pool(), 22, "other", Role::Admin).await;
    insert(&db, &new_mode(1, owner, "Fishing", ModeVisibility::Private))
        .await
        .unwrap();
    insert(
        &db,
        &new_mode(2, other, "Shared one", ModeVisibility::Shared),
    )
    .await
    .unwrap();
    insert(&db, &new_mode(3, other, "Secret", ModeVisibility::Private))
        .await
        .unwrap();
    let name = |text: &str| ModeName::try_from(text).unwrap();

    let mut tx = db.write().await.unwrap();
    let mut taken = Vec::new();
    for (text, shared) in [
        ("fishing", false),
        ("FISHING", true),
        ("shared one", true),
        ("shared one", false),
        ("AFK", true),
        ("afk", false),
        ("secret", false),
        ("secret", true),
        ("Fresh", true),
    ] {
        taken.push(
            tx.modes()
                .name_taken(owner, &name(text), shared, None)
                .await
                .unwrap(),
        );
    }

    assert_eq!(
        taken,
        [true, true, true, false, true, false, false, false, false],
        "the built-in names on a private mode are the service's to refuse (R4)"
    );
}

/// One mode read by its ID carries what the list carries: its owner's
/// current role and name, none for a built-in mode, on the read pool and
/// in a transaction alike; an unknown ID is none (P11.4).
#[sqlx::test(migrations = false)]
async fn a_mode_read_by_its_id_is_listed_with_its_owner(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let cast = cast(db.write_pool()).await;
    let listed = every_mode(&db, cast.owner).await;
    let admins = listed
        .iter()
        .find(|listed| listed.mode.name.as_str() == "Admin shared")
        .unwrap();
    let afk = ModePreset::Afk.id();
    let unknown = ModeId::new_v7(seeded(), [99; 10]).unwrap();

    let read = db.modes().listed(admins.mode.id).await.unwrap();
    let built_in = db.modes().listed(afk).await.unwrap().unwrap();
    let missing = db.modes().listed(unknown).await.unwrap();
    let mut tx = db.write().await.unwrap();
    let in_tx = tx.modes().listed(admins.mode.id).await.unwrap();

    assert_eq!(read.as_ref(), Some(admins));
    assert_eq!(in_tx, read);
    assert_eq!(built_in.owner, None);
    assert_eq!(built_in.mode.name.as_str(), "afk");
    assert_eq!(missing, None);
}

/// The mode being renamed never takes its own name, in any case; every
/// other mode's still counts, a shared one's for a mode made shared
/// (ADR-0020, S13).
#[sqlx::test(migrations = false)]
async fn a_name_is_never_taken_by_the_mode_being_renamed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(db.write_pool(), 21, "fisher", Role::Admin).await;
    let other = add_user(db.write_pool(), 22, "other", Role::Admin).await;
    let fishing = new_mode(1, owner, "Fishing", ModeVisibility::Private);
    let mining = new_mode(2, owner, "Mining", ModeVisibility::Private);
    let shared = new_mode(3, other, "Farming", ModeVisibility::Shared);
    for mode in [&fishing, &mining, &shared] {
        insert(&db, mode).await.unwrap();
    }
    let name = |text: &str| ModeName::try_from(text).unwrap();

    let mut tx = db.write().await.unwrap();
    let mut taken = Vec::new();
    for (owner, text, shared_now, except) in [
        (owner, "FISHING", false, Some(fishing.id)),
        (owner, "FISHING", true, Some(fishing.id)),
        (owner, "fishing", false, Some(mining.id)),
        (owner, "fishing", false, None),
        (owner, "farming", false, Some(fishing.id)),
        (owner, "farming", true, Some(fishing.id)),
        (other, "FARMING", true, Some(shared.id)),
        (owner, "afk", true, Some(fishing.id)),
    ] {
        taken.push(
            tx.modes()
                .name_taken(owner, &name(text), shared_now, except)
                .await
                .unwrap(),
        );
    }

    assert_eq!(taken, [false, false, true, true, false, true, false, true]);
}

/// `mode`'s change to `name`, `visibility` and a jumping definition, from
/// `expected` to the next version, at `at`.
fn change(mode: &NewMode, expected: u32, name: &str, visibility: ModeVisibility) -> ModeChange {
    let expected = NonZeroU32::new(expected).unwrap();
    ModeChange {
        id: mode.id,
        expected,
        version: expected.checked_add(1).unwrap(),
        name: ModeName::try_from(name).unwrap(),
        visibility,
        definition: serde_json::from_str(r#"{"steps":[{"action":{"type":"jump"},"schedule":{"type":"every","interval_ms":250,"jitter_ms":0},"probability":100}]}"#).unwrap(),
        updated_at: seeded() + chrono::Duration::hours(1),
    }
}

/// An update writes only while the row has the version it expects, and
/// never over a built-in mode; what it wrote reads back, `created_at` and
/// the owner unchanged (ADR-0020, S18).
#[sqlx::test(migrations = false)]
async fn an_update_writes_only_at_the_expected_version_and_never_a_built_in_mode(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(db.write_pool(), 21, "fisher", Role::Admin).await;
    let mode = new_mode(1, owner, "Fishing", ModeVisibility::Private);
    insert(&db, &mode).await.unwrap();
    let stale = change(&mode, 2, "Stale", ModeVisibility::Private);
    let fresh = change(&mode, 1, "Fishing trip", ModeVisibility::Shared);
    let mut built_in = change(&mode, 1, "Not afk", ModeVisibility::Shared);
    built_in.id = ModePreset::Afk.id();
    let afk_before = db.modes().find(ModePreset::Afk.id()).await.unwrap();

    let mut tx = db.write().await.unwrap();
    let wrote_stale = tx.modes().update(&stale).await.unwrap();
    let wrote_fresh = tx.modes().update(&fresh).await.unwrap();
    let wrote_again = tx.modes().update(&fresh).await.unwrap();
    let wrote_built_in = tx.modes().update(&built_in).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert_eq!(
        (wrote_stale, wrote_fresh, wrote_again, wrote_built_in),
        (false, true, false, false)
    );
    assert_eq!(
        db.modes().find(mode.id).await.unwrap(),
        Some(StoredMode {
            id: mode.id,
            owner: Some(owner),
            name: fresh.name.clone(),
            visibility: ModeVisibility::Shared,
            definition: fresh.definition.clone(),
            version: NonZeroU32::new(2).unwrap(),
            created_at: mode.created_at,
            updated_at: fresh.updated_at,
        })
    );
    assert_eq!(
        db.modes().find(ModePreset::Afk.id()).await.unwrap(),
        afk_before
    );
}

/// An update to a name either unique index refuses is `NameTaken`, never a
/// bare database error: another of the owner's names, or a shared one's
/// for a mode made shared (ADR-0020, S13).
#[sqlx::test(migrations = false)]
async fn an_update_to_a_name_the_indexes_refuse_is_taken(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(db.write_pool(), 21, "fisher", Role::Admin).await;
    let other = add_user(db.write_pool(), 22, "other", Role::Admin).await;
    let fishing = new_mode(1, owner, "Fishing", ModeVisibility::Private);
    let mining = new_mode(2, owner, "Mining", ModeVisibility::Private);
    let farming = new_mode(3, other, "Farming", ModeVisibility::Shared);
    for mode in [&fishing, &mining, &farming] {
        insert(&db, mode).await.unwrap();
    }

    for (case, change) in [
        (
            "the owner's other name",
            change(&mining, 1, "FISHING", ModeVisibility::Private),
        ),
        (
            "a shared name, made shared",
            change(&mining, 1, "farming", ModeVisibility::Shared),
        ),
    ] {
        let mut tx = db.write().await.unwrap();
        let result = tx.modes().update(&change).await;
        assert!(
            matches!(result, Err(WriteModeError::NameTaken)),
            "{case}: {result:?}"
        );
    }
    assert_eq!(
        db.modes()
            .find(mining.id)
            .await
            .unwrap()
            .unwrap()
            .name
            .as_str(),
        "Mining"
    );
}

/// A delete removes a user's mode and says so; a second one, or one of a
/// built-in mode, finds nothing and leaves the table as it was.
#[sqlx::test(migrations = false)]
async fn a_delete_removes_a_users_mode_and_never_a_built_in_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(db.write_pool(), 21, "fisher", Role::Member).await;
    let mode = new_mode(1, owner, "Fishing", ModeVisibility::Private);
    insert(&db, &mode).await.unwrap();
    let afk = ModePreset::Afk.id();

    let mut tx = db.write().await.unwrap();
    let first = tx.modes().delete(mode.id).await.unwrap();
    let again = tx.modes().delete(mode.id).await.unwrap();
    let built_in = tx.modes().delete(afk).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert_eq!((first, again, built_in), (true, false, false));
    assert_eq!(db.modes().find(mode.id).await.unwrap(), None);
    assert!(db.modes().find(afk).await.unwrap().is_some());
}
