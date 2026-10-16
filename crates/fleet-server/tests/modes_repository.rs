//! Tests for the modes' table and repository (Plan.md P11.1, Appendix D;
//! ADR-0019, S38): the migration seeds the built-in modes `afk` and `farm`
//! with the fixed IDs fleet-core's `ModePreset::id` names, as exactly the
//! JSON `serde_json` writes for each preset, which reads back through the
//! validated deserializer as the preset; a stored mode that isn't valid reads
//! as corrupt without its text; and the table refuses what its CHECKs forbid.
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
use fleet_core::mode::ModePreset;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::modes::StoredMode;
use fleet_server::ports::store::{Store, StoreError};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// When the seeds were made: the time in their IDs.
const SEEDED: &str = "2026-10-16T00:00:00.000Z";
/// A user's ID, the owner of the rows the CHECK tests insert.
const OWNER: &str = "018bcfe5-6800-7bab-abab-abababababab";
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

/// A row for `modes`, changed by the test before it's inserted.
#[derive(Debug, Clone)]
struct Row {
    id: &'static str,
    owner: Option<&'static str>,
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
             VALUES (?, ?, 'mine', ?, ?, ?, ?, ?)",
            self.id,
            self.owner,
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
            visibility: visibility.as_str(),
            ..Row::valid()
        };
        assert!(row.insert(pool).await.is_ok(), "{visibility:?} was refused");
    }
}
