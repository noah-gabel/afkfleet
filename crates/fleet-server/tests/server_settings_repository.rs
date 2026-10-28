//! Tests for the servers' settings table and repository (Plan.md P11.9,
//! Appendix D; ADR-0020, S76–S78): a stored row reads back as it was
//! given; a server's key is unique and in canonical form; an edit writes
//! only at the version it read; a row whose server or texts don't read back
//! exactly is corrupt, without its text; and the table refuses what its
//! CHECKs forbid.
//!
//! Each test gets a fresh database from `#[sqlx::test]` and opens it with
//! `Database::connect`, the production settings. Time isn't paused: sqlx's
//! worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::num::NonZeroU32;

use chrono::{DateTime, Duration, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::disconnect::ConflictTexts;
use fleet_core::id::ServerSettingsId;
use fleet_core::value::{ServerAddress, ServerKey};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::page::{Page, PageLimit};
use fleet_server::ports::server_settings::{
    NewServerSettings, ServerSettingsChange, StoredServerSettings, WriteServerSettingsError,
};
use fleet_server::ports::store::{Store, StoreError};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// When the first row was made.
const MADE: &str = "2027-01-15T08:00:00.000Z";
/// A row's ID for the tests that insert by hand.
const ID: &str = "018bcfe5-6800-7cdc-8dcd-cdcdcdcd0101";

async fn database(options: SqliteConnectOptions) -> Database {
    Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap()
}

fn made() -> DateTime<Utc> {
    MADE.parse().unwrap()
}

fn key(address: &str) -> ServerKey {
    ServerKey::from(&address.parse::<ServerAddress>().unwrap())
}

fn texts(entries: &[&str]) -> ConflictTexts {
    ConflictTexts::try_new(entries).unwrap()
}

fn page(before: Option<ServerSettingsId>, limit: u32) -> Page<ServerSettingsId> {
    Page {
        before,
        limit: PageLimit::new(limit).unwrap(),
    }
}

/// The entry a test commits its writes with. A test-only name, never
/// listed in `audit_actions!`.
fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        made(),
        AuditAction::try_from("test.server_settings_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// New settings numbered `n` for `server`, made `n` minutes after the
/// first, so their IDs sort by `n`.
fn new_settings(n: u8, server: &str, entries: &[&str]) -> NewServerSettings {
    let created = made() + Duration::minutes(i64::from(n));
    NewServerSettings {
        id: ServerSettingsId::new_v7(created, [n; 10]).unwrap(),
        server: key(server),
        conflict_texts: texts(entries),
        created_at: created,
    }
}

/// Stores `settings` in a transaction of its own.
async fn insert(
    db: &Database,
    settings: &NewServerSettings,
) -> Result<(), WriteServerSettingsError> {
    let mut tx = db.write().await.unwrap();
    tx.server_settings().insert(settings).await?;
    tx.commit(setup_entry()).await.unwrap();
    Ok(())
}

/// A row for `server_settings`, changed by the test before it's inserted.
#[derive(Debug, Clone)]
struct Row {
    id: &'static str,
    server: &'static str,
    conflict_texts: &'static str,
    version: i64,
    created_at: &'static str,
    updated_at: &'static str,
}

impl Row {
    const fn valid() -> Self {
        Self {
            id: ID,
            server: "mc.example.com:25565",
            conflict_texts: r#"["Already connected","You logged in from another location"]"#,
            version: 1,
            created_at: MADE,
            updated_at: MADE,
        }
    }

    /// Inserts the row through the write pool.
    async fn insert(&self, pool: &SqlitePool) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "INSERT INTO server_settings (id, server, conflict_texts, version, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?)",
            self.id,
            self.server,
            self.conflict_texts,
            self.version,
            self.created_at,
            self.updated_at
        )
        .execute(pool)
        .await
        .map(drop)
    }
}

/// A stored row reads back as it was given, at version 1, through the
/// reads and through a write transaction alike; its server is stored as
/// its key's text and its texts sorted, as the set iterates.
#[sqlx::test(migrations = false)]
async fn inserted_settings_read_back_at_version_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let settings = new_settings(
        1,
        "MC.example.com",
        &["Zz: line\nbreak", "Already connected"],
    );

    insert(&db, &settings).await.unwrap();

    let expected = StoredServerSettings {
        id: settings.id,
        server: key("mc.example.com:25565"),
        conflict_texts: settings.conflict_texts.clone(),
        version: NonZeroU32::MIN,
        created_at: settings.created_at,
        updated_at: settings.created_at,
    };
    assert_eq!(
        db.server_settings().find(settings.id).await.unwrap(),
        Some(expected.clone())
    );
    let mut tx = db.write().await.unwrap();
    assert_eq!(
        tx.server_settings().find(settings.id).await.unwrap(),
        Some(expected)
    );
    drop(tx);
    let id = settings.id.to_string();
    let row = sqlx::query!(
        r#"SELECT server AS "server!", conflict_texts AS "texts!" FROM server_settings WHERE id = ?"#,
        id
    )
    .fetch_one(db.read_pool())
    .await
    .unwrap();
    assert_eq!(row.server, "mc.example.com:25565");
    assert_eq!(row.texts, r#"["Already connected","Zz: line\nbreak"]"#);
}

#[sqlx::test(migrations = false)]
async fn unknown_settings_are_none(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;

    assert_eq!(
        db.server_settings()
            .find(ID.parse().unwrap())
            .await
            .unwrap(),
        None
    );
}

/// The list is newest first by ID, one more row than the page, and starts
/// after its cursor.
#[sqlx::test(migrations = false)]
async fn the_settings_page_newest_first_before_their_cursor(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let rows = [
        new_settings(1, "one.example.com", &["One"]),
        new_settings(2, "two.example.com", &["Two"]),
        new_settings(3, "three.example.com", &["Three"]),
    ];
    for row in &rows {
        insert(&db, row).await.unwrap();
    }
    let ids = |listed: Vec<StoredServerSettings>| -> Vec<ServerSettingsId> {
        listed.into_iter().map(|settings| settings.id).collect()
    };

    let first = db.server_settings().page(page(None, 1)).await.unwrap();
    let after = db
        .server_settings()
        .page(page(Some(rows[2].id), 5))
        .await
        .unwrap();

    assert_eq!(ids(first), [rows[2].id, rows[1].id]);
    assert_eq!(ids(after), [rows[1].id, rows[0].id]);
}

/// A second row for a server the unique key already holds is
/// `ServerTaken`, never a bare database error, and nothing is stored.
#[sqlx::test(migrations = false)]
async fn a_server_the_unique_key_refuses_is_taken_and_nothing_is_stored(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    insert(&db, &new_settings(1, "mc.example.com", &["One"]))
        .await
        .unwrap();
    let again = new_settings(2, "mc.example.com:25565", &["Two"]);

    let refused = insert(&db, &again).await;

    assert!(
        matches!(refused, Err(WriteServerSettingsError::ServerTaken)),
        "{refused:?}"
    );
    assert_eq!(db.server_settings().find(again.id).await.unwrap(), None);
}

/// The count is every row; a server is taken by any row but the one being
/// edited.
#[sqlx::test(migrations = false)]
async fn servers_are_counted_and_taken_by_any_row_but_the_one_being_edited(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let one = new_settings(1, "one.example.com", &["One"]);
    insert(&db, &one).await.unwrap();
    insert(&db, &new_settings(2, "two.example.com", &["Two"]))
        .await
        .unwrap();

    let mut tx = db.write().await.unwrap();
    let count = tx.server_settings().count().await.unwrap();
    let taken = tx
        .server_settings()
        .server_taken(&key("one.example.com"), None)
        .await
        .unwrap();
    let taken_by_itself = tx
        .server_settings()
        .server_taken(&key("one.example.com"), Some(one.id))
        .await
        .unwrap();
    let taken_by_another = tx
        .server_settings()
        .server_taken(&key("two.example.com"), Some(one.id))
        .await
        .unwrap();
    let free = tx
        .server_settings()
        .server_taken(&key("one.example.com:25566"), None)
        .await
        .unwrap();
    drop(tx);

    assert_eq!(count, 2);
    assert_eq!(
        (taken, taken_by_itself, taken_by_another, free),
        (true, false, true, false)
    );
}

fn change(
    of: &NewServerSettings,
    expected: u32,
    server: &str,
    entries: &[&str],
) -> ServerSettingsChange {
    ServerSettingsChange {
        id: of.id,
        expected: NonZeroU32::new(expected).unwrap(),
        version: NonZeroU32::new(expected + 1).unwrap(),
        server: key(server),
        conflict_texts: texts(entries),
        updated_at: of.created_at + Duration::hours(1),
    }
}

/// An edit writes only while the row has the version it expects; what it
/// wrote reads back, `created_at` unchanged (ADR-0020, S77).
#[sqlx::test(migrations = false)]
async fn an_update_writes_only_at_the_expected_version(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let settings = new_settings(1, "mc.example.com", &["One"]);
    insert(&db, &settings).await.unwrap();
    let stale = change(&settings, 2, "stale.example.com", &["Stale"]);
    let fresh = change(&settings, 1, "play.example.com:25570", &["Two", "One"]);

    let mut tx = db.write().await.unwrap();
    let wrote_stale = tx.server_settings().update(&stale).await.unwrap();
    let wrote_fresh = tx.server_settings().update(&fresh).await.unwrap();
    let wrote_again = tx.server_settings().update(&fresh).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert_eq!(
        (wrote_stale, wrote_fresh, wrote_again),
        (false, true, false)
    );
    assert_eq!(
        db.server_settings().find(settings.id).await.unwrap(),
        Some(StoredServerSettings {
            id: settings.id,
            server: key("play.example.com:25570"),
            conflict_texts: texts(&["One", "Two"]),
            version: NonZeroU32::new(2).unwrap(),
            created_at: settings.created_at,
            updated_at: fresh.updated_at,
        })
    );
}

/// An edit to a server another row holds is `ServerTaken`.
#[sqlx::test(migrations = false)]
async fn an_update_to_a_server_another_row_holds_is_taken(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let one = new_settings(1, "one.example.com", &["One"]);
    insert(&db, &one).await.unwrap();
    insert(&db, &new_settings(2, "two.example.com", &["Two"]))
        .await
        .unwrap();

    let mut tx = db.write().await.unwrap();
    let refused = tx
        .server_settings()
        .update(&change(&one, 1, "two.example.com", &["One"]))
        .await;
    drop(tx);

    assert!(
        matches!(refused, Err(WriteServerSettingsError::ServerTaken)),
        "{refused:?}"
    );
}

/// A delete removes the row and says whether it was there.
#[sqlx::test(migrations = false)]
async fn a_delete_removes_the_row_and_says_whether_it_was_there(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let settings = new_settings(1, "mc.example.com", &["One"]);
    insert(&db, &settings).await.unwrap();

    let mut tx = db.write().await.unwrap();
    let deleted = tx.server_settings().delete(settings.id).await.unwrap();
    let again = tx.server_settings().delete(settings.id).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert_eq!((deleted, again), (true, false));
    assert_eq!(db.server_settings().find(settings.id).await.unwrap(), None);
}

/// The CHECKs refuse what a row may never hold; a valid row is taken.
#[sqlx::test(migrations = false)]
async fn the_table_refuses_what_its_checks_forbid(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let refused = [
        Row {
            id: "not-an-id",
            ..Row::valid()
        },
        Row {
            server: "MC.example.com:25565",
            ..Row::valid()
        },
        Row {
            conflict_texts: "not json",
            ..Row::valid()
        },
        Row {
            conflict_texts: r#"{"text":"Already connected"}"#,
            ..Row::valid()
        },
        Row {
            version: 0,
            ..Row::valid()
        },
        Row {
            created_at: "2027-01-15 08:00:00",
            ..Row::valid()
        },
        Row {
            updated_at: "2027-01-14T08:00:00.000Z",
            ..Row::valid()
        },
        // Later than `created_at`, so only its format's GLOB refuses it.
        Row {
            updated_at: "2027-01-16 08:00:00",
            ..Row::valid()
        },
    ];

    for row in &refused {
        assert!(row.insert(db.write_pool()).await.is_err(), "{row:?}");
    }
    Row::valid().insert(db.write_pool()).await.unwrap();
    let second = Row {
        id: "018bcfe5-6800-7cdc-8dcd-cdcdcdcd0102",
        ..Row::valid()
    };
    assert!(
        second.insert(db.write_pool()).await.is_err(),
        "the server is unique"
    );
}

/// A row whose server isn't a key's canonical text, or whose texts aren't
/// exactly what the set writes (sorted, without duplicates, each one the
/// constructor takes), is `Corrupt` naming its column; the error never
/// holds the text.
#[sqlx::test(migrations = false)]
async fn a_row_that_doesnt_read_back_exactly_is_corrupt_without_its_text(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let cases = [
        (
            "server",
            Row {
                server: "mc.example.com",
                ..Row::valid()
            },
        ),
        (
            "server",
            Row {
                server: "not a server:25565",
                ..Row::valid()
            },
        ),
        (
            "conflict_texts",
            Row {
                conflict_texts: r#"["You logged in from another location","Already connected"]"#,
                ..Row::valid()
            },
        ),
        (
            "conflict_texts",
            Row {
                conflict_texts: r#"["Already connected","Already connected"]"#,
                ..Row::valid()
            },
        ),
        (
            "conflict_texts",
            Row {
                conflict_texts: r#"["Already §connected"]"#,
                ..Row::valid()
            },
        ),
        (
            "conflict_texts",
            Row {
                conflict_texts: "[1]",
                ..Row::valid()
            },
        ),
    ];

    for (column, row) in &cases {
        sqlx::query!("DELETE FROM server_settings")
            .execute(db.write_pool())
            .await
            .unwrap();
        row.insert(db.write_pool()).await.unwrap();

        let read = db.server_settings().find(ID.parse().unwrap()).await;
        let listed = db.server_settings().page(page(None, 10)).await;

        match (&read, &listed) {
            (
                Err(StoreError::Corrupt {
                    table: "server_settings",
                    column: read_column,
                    ..
                }),
                Err(StoreError::Corrupt {
                    column: listed_column,
                    ..
                }),
            ) if read_column == column && listed_column == column => {}
            _ => panic!("{row:?} read as {read:?} and {listed:?}"),
        }
        let shown = format!("{:?} {}", read.unwrap_err(), listed.unwrap_err());
        assert!(
            !shown.contains("connected") && !shown.contains("example"),
            "{shown}"
        );
    }
}

/// An empty list reads back as no texts: the routes refuse to write one,
/// but the chat format (H7) will store rows without texts.
#[sqlx::test(migrations = false)]
async fn a_row_without_texts_reads_back_as_an_empty_set(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    Row {
        conflict_texts: "[]",
        ..Row::valid()
    }
    .insert(db.write_pool())
    .await
    .unwrap();

    let read = db
        .server_settings()
        .find(ID.parse().unwrap())
        .await
        .unwrap();

    assert_eq!(
        read.map(|settings| settings.conflict_texts),
        Some(ConflictTexts::default())
    );
}
