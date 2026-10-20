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
use fleet_core::authz::ModeVisibility;
use fleet_core::mode::{ModeName, ModePreset};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::modes::StoredMode;
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
