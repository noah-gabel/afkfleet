//! Tests for opening the database: the file, the pragmas of both pools, the
//! migrations and the errors (Plan.md P6.3, ADR-0015).
//!
//! Time isn't paused here: sqlx runs each SQLite connection on a worker
//! thread, so the runtime looks idle while it waits for one, and paused time
//! would jump ahead and fire the pools' timeouts early. The one test that
//! waits for a timeout lowers it to 1 s.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::path::{Path, PathBuf};

use chrono::DateTime;
use fleet_core::audit::{AuditAction, AuditMetadata, AuditOutcome, AuditTarget, RecordedMetadata};
use fleet_core::authz::Role;
use fleet_core::id::UserId;
use fleet_core::value::Username;
use fleet_server::config::DatabaseConfig;
use fleet_server::infra::sqlite::{
    CloseTimedOut, Database, DatabaseOptions, MIGRATOR, OpenError, PoolRole, READ_CONNECTIONS,
    connect_options, open, open_migrated,
};
use fleet_server::ports::audit::{AuditLimit, AuditPage, NewAuditEntry};
use fleet_server::ports::store::Store;
use fleet_server::ports::users::{NewUser, PasswordHash};
use secrecy::SecretString;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};
use tempfile::TempDir;

/// The acquire timeout of the tests that wait for it. It also bounds opening
/// the database (the first connections and the migrations), so it leaves room
/// for a slow CI runner: 50 ms once timed out `open()` itself on Windows.
const SHORT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(1);

fn config_in(dir: &TempDir) -> DatabaseConfig {
    DatabaseConfig {
        path: dir.path().join("afkfleet.db"),
    }
}

async fn open_in(dir: &TempDir) -> Database {
    open(&config_in(dir), DatabaseOptions::default())
        .await
        .unwrap()
}

/// The path of the `-wal` or `-shm` file next to `db`.
fn sibling(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The pragmas one connection of `pool` runs with. sqlx's macros describe the
/// integer pragmas as nullable; `journal_mode` they can't type at all.
#[derive(Debug, PartialEq, Eq)]
struct Pragmas {
    journal_mode: String,
    synchronous: Option<i64>,
    foreign_keys: Option<i64>,
    busy_timeout: Option<i64>,
    trusted_schema: Option<i64>,
    query_only: Option<i64>,
}

async fn pragmas(pool: &SqlitePool) -> Pragmas {
    let mut conn = pool.acquire().await.unwrap();
    Pragmas {
        // sqlx's macros describe `journal_mode`'s column as NULL, and a PRAGMA
        // can't alias it to a type override, so this one read is an unchecked
        // query: a fixed literal, never formatted, with no input (ADR-0015).
        journal_mode: sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&mut *conn)
            .await
            .unwrap(),
        synchronous: sqlx::query_scalar!("PRAGMA synchronous")
            .fetch_one(&mut *conn)
            .await
            .unwrap(),
        foreign_keys: sqlx::query_scalar!("PRAGMA foreign_keys")
            .fetch_one(&mut *conn)
            .await
            .unwrap(),
        busy_timeout: sqlx::query_scalar!("PRAGMA busy_timeout")
            .fetch_one(&mut *conn)
            .await
            .unwrap(),
        trusted_schema: sqlx::query_scalar!("PRAGMA trusted_schema")
            .fetch_one(&mut *conn)
            .await
            .unwrap(),
        query_only: sqlx::query_scalar!("PRAGMA query_only")
            .fetch_one(&mut *conn)
            .await
            .unwrap(),
    }
}

async fn applied_migrations(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar!(r#"SELECT COUNT(*) AS "count!: i64" FROM _sqlx_migrations WHERE success"#)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn has_table(pool: &SqlitePool, name: &str) -> bool {
    sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!: i64" FROM sqlite_schema WHERE type = 'table' AND name = ?"#,
        name
    )
    .fetch_one(pool)
    .await
    .unwrap()
        == 1
}

async fn create_probe_table(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query!("CREATE TABLE probe (x INTEGER)")
        .execute(pool)
        .await
        .map(|_| ())
}

// --- Opening ---------------------------------------------------------------

#[tokio::test]
async fn open_creates_the_file_and_applies_every_migration() {
    let dir = TempDir::new().unwrap();

    let db = open_in(&dir).await;

    assert!(config_in(&dir).path.is_file());
    assert!(has_table(db.read_pool(), "_sqlx_migrations").await);
    let expected = i64::try_from(MIGRATOR.iter().count()).unwrap();
    assert_eq!(applied_migrations(db.read_pool()).await, expected);
    db.close().await;
}

#[tokio::test]
async fn connect_never_creates_a_missing_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("missing.db");

    let result = Database::connect(
        SqliteConnectOptions::new().filename(&path),
        DatabaseOptions::default(),
    )
    .await;

    assert!(matches!(result, Err(OpenError::Connect(_))), "{result:?}");
    assert!(!path.exists());
}

#[tokio::test]
async fn a_missing_folder_is_refused_and_nothing_is_created() {
    let dir = TempDir::new().unwrap();
    let folder = dir.path().join("typo");
    let config = DatabaseConfig {
        path: folder.join("afkfleet.db"),
    };

    let result = open(&config, DatabaseOptions::default()).await;

    assert!(
        matches!(&result, Err(OpenError::MissingFolder { folder: missing }) if *missing == folder),
        "{result:?}"
    );
    assert!(!folder.exists());
}

#[tokio::test]
async fn reopening_keeps_the_data_and_migrates_nothing_twice() {
    let dir = TempDir::new().unwrap();
    let db = open_in(&dir).await;
    create_probe_table(db.write_pool()).await.unwrap();
    db.close().await;

    let db = open_in(&dir).await;

    assert!(has_table(db.read_pool(), "probe").await);
    let expected = i64::try_from(MIGRATOR.iter().count()).unwrap();
    assert_eq!(applied_migrations(db.read_pool()).await, expected);
    db.close().await;
}

#[tokio::test]
async fn a_schema_from_a_newer_server_is_refused() {
    let dir = TempDir::new().unwrap();
    let db = open_in(&dir).await;
    sqlx::query!(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (?, 'from a newer server', 1, x'00', 0)",
        9_999_i64
    )
    .execute(db.write_pool())
    .await
    .unwrap();
    db.close().await;

    let result = open(&config_in(&dir), DatabaseOptions::default()).await;

    assert!(
        matches!(result, Err(OpenError::NewerSchema { version: 9_999 })),
        "{result:?}"
    );
}

#[tokio::test]
async fn closing_checkpoints_and_removes_the_wal() {
    let dir = TempDir::new().unwrap();
    let db = open_in(&dir).await;
    create_probe_table(db.write_pool()).await.unwrap();

    db.close().await;

    let path = config_in(&dir).path;
    assert!(!sibling(&path, "-wal").exists());
    assert!(!sibling(&path, "-shm").exists());
}

// --- Opening without migrating (the vault commands, P9.2) -------------------------------

async fn open_migrated_in(dir: &TempDir) -> Result<Database, OpenError> {
    open_migrated(&config_in(dir), DatabaseOptions::default()).await
}

/// The bytes of the database file and whether a `-wal` or `-shm` sits next to
/// it.
fn on_disk(path: &Path) -> (Vec<u8>, bool, bool) {
    (
        std::fs::read(path).unwrap(),
        sibling(path, "-wal").exists(),
        sibling(path, "-shm").exists(),
    )
}

#[tokio::test]
async fn open_migrated_never_creates_a_missing_file() {
    let dir = TempDir::new().unwrap();
    let path = config_in(&dir).path;

    let result = open_migrated_in(&dir).await;

    assert!(
        matches!(&result, Err(OpenError::MissingFile { file }) if *file == path),
        "{result:?}"
    );
    assert!(!path.exists());
}

#[tokio::test]
async fn open_migrated_refuses_a_missing_folder() {
    let dir = TempDir::new().unwrap();
    let folder = dir.path().join("typo");
    let config = DatabaseConfig {
        path: folder.join("afkfleet.db"),
    };

    let result = open_migrated(&config, DatabaseOptions::default()).await;

    assert!(
        matches!(&result, Err(OpenError::MissingFolder { folder: missing }) if *missing == folder),
        "{result:?}"
    );
    assert!(!folder.exists());
}

#[tokio::test]
async fn an_empty_file_is_pending_from_the_first_migration_and_stays_empty() {
    let dir = TempDir::new().unwrap();
    let path = config_in(&dir).path;
    std::fs::write(&path, b"").unwrap();

    let result = open_migrated_in(&dir).await;

    assert!(
        matches!(result, Err(OpenError::Pending { version: 1 })),
        "{result:?}"
    );
    assert_eq!(on_disk(&path), (Vec::new(), false, false));
}

#[tokio::test]
async fn a_database_behind_this_server_is_pending_and_left_as_it_was() {
    let dir = TempDir::new().unwrap();
    let path = config_in(&dir).path;
    std::fs::write(&path, b"").unwrap();
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect_options(
            SqliteConnectOptions::new().filename(&path),
            PoolRole::Write,
            &DatabaseOptions::default(),
        ))
        .await
        .unwrap();
    let last = MIGRATOR
        .iter()
        .map(|migration| migration.version)
        .max()
        .unwrap();
    MIGRATOR.run_to(last - 1, &pool).await.unwrap();
    pool.close().await;
    let before = on_disk(&path);

    let result = open_migrated_in(&dir).await;

    assert!(
        matches!(result, Err(OpenError::Pending { version }) if version == last),
        "{result:?}"
    );
    assert_eq!(on_disk(&path), before);
}

/// A change to `_sqlx_migrations` of a migrated database.
#[derive(Debug, Clone, Copy)]
enum Tampered {
    /// Migration 1's checksum changes.
    Edited,
    /// A migration this server doesn't know appears.
    Newer,
    /// Migration 2 is marked as failed halfway.
    Failed,
}

/// What `open_migrated` answers once a migrated database's
/// `_sqlx_migrations` was changed by `tampered`.
async fn open_migrated_after(tampered: Tampered) -> Result<Database, OpenError> {
    let dir = TempDir::new().unwrap();
    let db = open_in(&dir).await;
    match tampered {
        Tampered::Edited => {
            sqlx::query!("UPDATE _sqlx_migrations SET checksum = x'00' WHERE version = 1")
                .execute(db.write_pool())
                .await
                .unwrap();
        }
        Tampered::Newer => {
            sqlx::query!(
                "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (?, 'from a newer server', 1, x'00', 0)",
                9_999_i64
            )
            .execute(db.write_pool())
            .await
            .unwrap();
        }
        Tampered::Failed => {
            sqlx::query!("UPDATE _sqlx_migrations SET success = 0 WHERE version = 2")
                .execute(db.write_pool())
                .await
                .unwrap();
        }
    }
    db.close().await;
    open_migrated_in(&dir).await
}

#[tokio::test]
async fn open_migrated_refuses_an_edited_migration() {
    let result = open_migrated_after(Tampered::Edited).await;

    assert!(
        matches!(result, Err(OpenError::EditedMigration { version: 1 })),
        "{result:?}"
    );
}

#[tokio::test]
async fn open_migrated_refuses_a_schema_from_a_newer_server() {
    let result = open_migrated_after(Tampered::Newer).await;

    assert!(
        matches!(result, Err(OpenError::NewerSchema { version: 9_999 })),
        "{result:?}"
    );
}

#[tokio::test]
async fn open_migrated_refuses_a_migration_that_failed_halfway() {
    let result = open_migrated_after(Tampered::Failed).await;

    assert!(
        matches!(
            result,
            Err(OpenError::Migrate(sqlx::migrate::MigrateError::Dirty(2)))
        ),
        "{result:?}"
    );
}

#[tokio::test]
async fn a_current_database_opens_and_its_lazy_write_pool_still_writes() {
    let dir = TempDir::new().unwrap();
    open_in(&dir).await.close().await;

    let db = open_migrated_in(&dir).await.unwrap();
    create_probe_table(db.write_pool()).await.unwrap();

    assert!(has_table(db.read_pool(), "probe").await);
    db.close().await;
}

/// `just dev-rotate-key` reads a `-wal` next to the dev database as a running
/// server (ADR-0018), so `vault status`'s reads must leave none behind:
/// read connections with `query_only` and a write pool that never connected.
#[tokio::test]
async fn a_closed_open_migrated_database_leaves_no_wal_or_shm() {
    let dir = TempDir::new().unwrap();
    let path = config_in(&dir).path;
    open_in(&dir).await.close().await;
    let before = on_disk(&path);
    assert_eq!((before.1, before.2), (false, false));

    let db = open_migrated_in(&dir).await.unwrap();
    db.envelopes().all().await.unwrap();
    db.accounts()
        .find(fleet_core::id::AccountId::new_v7(DateTime::UNIX_EPOCH, [1; 10]).unwrap())
        .await
        .unwrap();
    db.close().await;

    assert_eq!(on_disk(&path), before);
}

#[tokio::test]
async fn an_edited_migration_is_refused() {
    let dir = TempDir::new().unwrap();
    let db = open_in(&dir).await;
    sqlx::query!("UPDATE _sqlx_migrations SET checksum = x'00' WHERE version = 1")
        .execute(db.write_pool())
        .await
        .unwrap();
    db.close().await;

    let result = open(&config_in(&dir), DatabaseOptions::default()).await;

    assert!(
        matches!(result, Err(OpenError::EditedMigration { version: 1 })),
        "{result:?}"
    );
}

/// A user and an audit entry that touch every column with a CHECK, metadata JSON
/// included, through the real `open()`: every CHECK holds up with
/// `trusted_schema=OFF`.
#[tokio::test]
async fn a_full_row_passes_every_check_with_the_production_settings() {
    let dir = TempDir::new().unwrap();
    let db = open_in(&dir).await;
    let created = DateTime::from_timestamp_millis(1_800_000_000_123).unwrap();
    let user = NewUser {
        id: UserId::new_v7(created, [1; 10]).unwrap(),
        username: Username::try_from("afkowner").unwrap(),
        password_hash: PasswordHash::new(SecretString::from(
            "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g",
        ))
        .unwrap(),
        role: Role::Owner,
        created_at: created,
    };
    let metadata = AuditMetadata::new()
        .text("reason", "a \"quoted\" note")
        .unwrap()
        .int("attempts", 2)
        .unwrap()
        .flag("locked", false)
        .unwrap();
    let entry = NewAuditEntry::new(
        created,
        AuditAction::try_from("user.create").unwrap(),
        AuditOutcome::Success,
    )
    .actor(user.id)
    .ip("2001:db8::7".parse().unwrap())
    .target(AuditTarget::User(user.id))
    .metadata(metadata.clone());

    let mut tx = db.write().await.unwrap();
    tx.users().insert(&user).await.unwrap();
    let id = tx.commit(entry).await.unwrap();

    let stored = db.users().get(user.id).await.unwrap().unwrap();
    assert_eq!(stored.role, Role::Owner);
    assert_eq!(stored.password_changed_at, created);
    let page = AuditPage {
        before: None,
        limit: AuditLimit::new(10).unwrap(),
    };
    let records = db.audit().list(page).await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, id);
    assert_eq!(records[0].metadata, RecordedMetadata::from(&metadata));
    db.close().await;
}

// --- Connections -----------------------------------------------------------

#[tokio::test]
async fn the_write_connection_has_the_production_pragmas() {
    let dir = TempDir::new().unwrap();
    let db = open_in(&dir).await;

    let pragmas = pragmas(db.write_pool()).await;

    assert_eq!(
        pragmas,
        Pragmas {
            journal_mode: "wal".to_owned(),
            synchronous: Some(1),
            foreign_keys: Some(1),
            busy_timeout: Some(5_000),
            trusted_schema: Some(0),
            query_only: Some(0),
        }
    );
    db.close().await;
}

#[tokio::test]
async fn read_connections_have_the_production_pragmas() {
    let dir = TempDir::new().unwrap();
    let db = open_in(&dir).await;

    let pragmas = pragmas(db.read_pool()).await;

    assert_eq!(
        pragmas,
        Pragmas {
            journal_mode: "wal".to_owned(),
            synchronous: Some(1),
            foreign_keys: Some(1),
            busy_timeout: Some(5_000),
            trusted_schema: Some(0),
            query_only: Some(1),
        }
    );
    db.close().await;
}

#[tokio::test]
async fn writes_have_one_connection_and_reads_four() {
    let dir = TempDir::new().unwrap();
    let db = open_in(&dir).await;

    assert_eq!(db.write_pool().options().get_max_connections(), 1);
    assert_eq!(
        db.read_pool().options().get_max_connections(),
        READ_CONNECTIONS
    );
    assert_eq!(READ_CONNECTIONS, 4);
    db.close().await;
}

#[tokio::test]
async fn the_read_pool_refuses_writes() {
    let dir = TempDir::new().unwrap();
    let db = open_in(&dir).await;

    let error = create_probe_table(db.read_pool()).await.unwrap_err();

    let code = error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .unwrap();
    assert_eq!(code, "8", "SQLITE_READONLY: {error}");
    assert!(!has_table(db.write_pool(), "probe").await);
    db.close().await;
}

#[tokio::test]
async fn the_read_pool_sees_committed_writes() {
    let dir = TempDir::new().unwrap();
    let db = open_in(&dir).await;

    create_probe_table(db.write_pool()).await.unwrap();

    assert!(has_table(db.read_pool(), "probe").await);
    db.close().await;
}

#[tokio::test]
async fn the_production_defaults_are_5_s_and_250_ms() {
    let dir = TempDir::new().unwrap();
    let db = open_in(&dir).await;

    let defaults = DatabaseOptions::default();

    assert_eq!(defaults.acquire_timeout(), Duration::from_secs(5));
    assert_eq!(defaults.slow_statement(), Duration::from_millis(250));
    assert_eq!(
        db.write_pool().options().get_acquire_timeout(),
        Duration::from_secs(5)
    );
    assert_eq!(
        db.read_pool().options().get_acquire_timeout(),
        Duration::from_secs(5)
    );
    db.close().await;
}

#[tokio::test]
async fn a_held_write_connection_times_the_next_writer_out() {
    let dir = TempDir::new().unwrap();
    let options = DatabaseOptions::default().with_acquire_timeout(SHORT_ACQUIRE_TIMEOUT);
    let db = open(&config_in(&dir), options).await.unwrap();
    let held = db.write_pool().acquire().await.unwrap();

    let result = db.write_pool().acquire().await;

    assert!(
        matches!(result, Err(sqlx::Error::PoolTimedOut)),
        "{result:?}"
    );
    drop(held);
    db.close().await;
}

#[tokio::test]
async fn closing_within_a_limit_closes_an_idle_database() {
    let dir = TempDir::new().unwrap();
    let db = open_in(&dir).await;
    create_probe_table(db.write_pool()).await.unwrap();

    let closed = db.close_within(Duration::from_secs(5)).await;

    assert_eq!(closed, Ok(()));
    assert!(db.read_pool().is_closed());
    assert!(db.write_pool().is_closed());
    assert!(!sibling(&config_in(&dir).path, "-wal").exists());
}

#[tokio::test]
async fn closing_within_a_limit_gives_up_while_a_connection_is_checked_out() {
    let dir = TempDir::new().unwrap();
    let db = open_in(&dir).await;
    let held = db.read_pool().acquire().await.unwrap();
    let limit = Duration::from_millis(300);

    let closed = db.close_within(limit).await;

    assert_eq!(closed, Err(CloseTimedOut { limit }));
    assert_eq!(
        closed.unwrap_err().to_string(),
        "the database didn't close within 300 ms"
    );
    assert!(
        db.read_pool().is_closed(),
        "no new connection is handed out"
    );
    drop(held);
    db.close().await;
}

// --- Permissions (Unix) ----------------------------------------------------

#[cfg(unix)]
mod permissions {
    use std::fs::{self, Permissions};
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// Whether this user is held to permission bits: it can't write into a
    /// `0500` folder. Root ignores the bits, so under root the two tests that
    /// need a folder the server can't use say so and skip, instead of failing
    /// for a reason that has nothing to do with the server.
    fn permission_bits_apply(dir: &TempDir) -> bool {
        let probe = dir.path().join("probe");
        fs::create_dir(&probe).unwrap();
        fs::set_permissions(&probe, Permissions::from_mode(0o500)).unwrap();
        let applies = fs::write(probe.join("file"), b"").is_err();
        fs::set_permissions(&probe, Permissions::from_mode(0o700)).unwrap();
        applies
    }

    /// The message a test prints when it skips under root.
    fn skip(test: &str) {
        eprintln!(
            "skipped {test}: this user ignores permission bits (running as root?), so a folder the server can't use can't be made"
        );
    }

    #[tokio::test]
    async fn a_new_database_file_is_private() {
        let dir = TempDir::new().unwrap();
        let db = open_in(&dir).await;
        create_probe_table(db.write_pool()).await.unwrap();

        let path = config_in(&dir).path;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&sibling(&path, "-wal")), 0o600);
        db.close().await;
    }

    #[tokio::test]
    async fn an_existing_database_others_can_read_is_refused() {
        let dir = TempDir::new().unwrap();
        let path = config_in(&dir).path;
        fs::write(&path, b"").unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o644)).unwrap();

        let result = open(&config_in(&dir), DatabaseOptions::default()).await;

        let Err(error @ OpenError::Permissions { .. }) = result else {
            panic!("expected a permission error: {result:?}");
        };
        let message = error.to_string();
        assert!(
            message.starts_with(&path.display().to_string()),
            "{message}"
        );
        assert!(message.contains("mode 644"), "{message}");
        assert!(message.contains("chmod 600"), "{message}");
    }

    #[tokio::test]
    async fn a_leftover_wal_others_can_read_is_refused() {
        let dir = TempDir::new().unwrap();
        let db = open_in(&dir).await;
        db.close().await;
        let wal = sibling(&config_in(&dir).path, "-wal");
        fs::write(&wal, b"").unwrap();
        fs::set_permissions(&wal, Permissions::from_mode(0o644)).unwrap();

        let result = open(&config_in(&dir), DatabaseOptions::default()).await;

        assert!(
            matches!(&result, Err(OpenError::Permissions { file, mode: 0o644 }) if *file == wal),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_folder_the_server_cant_write_to_is_a_create_error() {
        let dir = TempDir::new().unwrap();
        if !permission_bits_apply(&dir) {
            skip("a_folder_the_server_cant_write_to_is_a_create_error");
            return;
        }
        let folder = dir.path().join("read-only");
        fs::create_dir(&folder).unwrap();
        fs::set_permissions(&folder, Permissions::from_mode(0o500)).unwrap();
        let config = DatabaseConfig {
            path: folder.join("afkfleet.db"),
        };

        let result = open(&config, DatabaseOptions::default()).await;

        fs::set_permissions(&folder, Permissions::from_mode(0o700)).unwrap();
        assert!(
            matches!(&result, Err(OpenError::Create { file, .. }) if *file == config.path),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_folder_the_server_cant_look_into_is_an_inspect_error() {
        let dir = TempDir::new().unwrap();
        if !permission_bits_apply(&dir) {
            skip("a_folder_the_server_cant_look_into_is_an_inspect_error");
            return;
        }
        let locked = dir.path().join("locked");
        let folder = locked.join("data");
        fs::create_dir_all(&folder).unwrap();
        fs::set_permissions(&locked, Permissions::from_mode(0o000)).unwrap();
        let config = DatabaseConfig {
            path: folder.join("afkfleet.db"),
        };

        let result = open(&config, DatabaseOptions::default()).await;

        fs::set_permissions(&locked, Permissions::from_mode(0o700)).unwrap();
        assert!(
            matches!(&result, Err(OpenError::Inspect { file, .. }) if *file == folder),
            "{result:?}"
        );
    }
}
