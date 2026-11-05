//! What a delete leaves in the database's files (ADR-0020, S44): with
//! `secure_delete` on, chat past its age or its bot's bound and old audit
//! entries leave no trace of their text in the file, its `-wal` or its
//! `-shm` once the database closed, freed pages included.

use core::time::Duration;
use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::fixtures::{add_bot, add_user, database, days, entries_noting, lines_saying, millis};
use super::service::{now, policy, service};
use super::watched::WatchedStore;

/// Whether `text` is in `file`, its `-wal` or its `-shm`.
fn holds(file: &Path, text: &str) -> bool {
    let mut read = 0;
    let mut found = false;
    for suffix in ["", "-wal", "-shm"] {
        let mut path = file.to_owned().into_os_string();
        path.push(suffix);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        read += 1;
        found |= bytes
            .windows(text.len())
            .any(|window| window == text.as_bytes());
    }
    assert!(read > 0, "no file of the database to read");
    found
}

#[sqlx::test(migrations = false)]
async fn expired_chat_leaves_no_text_in_the_database_files(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let file = options.get_filename().to_owned();
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let bot = add_bot(&db, user, 1).await;
    lines_saying(&db, bot, 3000, now() - days(15), "canary-expired-chat").await;
    let held = holds(&file, "canary-expired-chat");

    let run = service(
        &WatchedStore::new(db.clone()),
        now(),
        policy(),
        Duration::ZERO,
    )
    .run_once(&tokio_util::sync::CancellationToken::new())
    .await;
    db.close().await;

    assert!(held, "the text was written");
    assert_eq!(run.deleted.chat_by_age, 3000);
    assert!(!holds(&file, "canary-expired-chat"));
}

#[sqlx::test(migrations = false)]
async fn chat_past_the_size_bound_leaves_no_text_in_the_database_files(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let file = options.get_filename().to_owned();
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let bot = add_bot(&db, user, 1).await;
    let recent = now() - days(1);
    lines_saying(&db, bot, 3000, recent, "canary-chat-past-the-bound").await;
    lines_saying(&db, bot, 100, recent + millis(1), "kept").await;
    let held = holds(&file, "canary-chat-past-the-bound");

    let run = service(
        &WatchedStore::new(db.clone()),
        now(),
        policy(),
        Duration::ZERO,
    )
    .run_once(&tokio_util::sync::CancellationToken::new())
    .await;
    db.close().await;

    assert!(held, "the text was written");
    assert_eq!(run.deleted.chat_by_size, 3000);
    assert!(!holds(&file, "canary-chat-past-the-bound"));
}

#[sqlx::test(migrations = false)]
async fn expired_audit_entries_leave_no_text_in_the_database_files(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let file = options.get_filename().to_owned();
    let db = database(options).await;
    entries_noting(&db, 3000, now() - days(91), "canary-expired-entry").await;
    let held = holds(&file, "canary-expired-entry");

    let run = service(
        &WatchedStore::new(db.clone()),
        now(),
        policy(),
        Duration::ZERO,
    )
    .run_once(&tokio_util::sync::CancellationToken::new())
    .await;
    db.close().await;

    assert!(held, "the text was written");
    assert_eq!(run.deleted.audit_entries, 3000);
    assert!(!holds(&file, "canary-expired-entry"));
}

/// The size of `file`'s `-wal`, in bytes.
fn wal_bytes(file: &Path) -> u64 {
    let mut path = file.to_owned().into_os_string();
    path.push("-wal");
    std::fs::metadata(&path).unwrap().len()
}

/// Whether `text` is in `file`'s `-wal` alone.
fn wal_holds(file: &Path, text: &str) -> bool {
    let mut path = file.to_owned().into_os_string();
    path.push("-wal");
    std::fs::read(&path)
        .unwrap()
        .windows(text.len())
        .any(|window| window == text.as_bytes())
}

/// ADR-0022, S7: a read snapshot (a backup's `VACUUM INTO`) keeps every
/// frame in the WAL meanwhile, so it grows by megabytes. Once the snapshot
/// ends, a checkpoint and the next commit cut the file back
/// (`journal_size_limit = 0`), and the deleted chat's old frames go with it
/// while the database is still open.
#[sqlx::test(migrations = false)]
async fn a_wal_that_grew_under_a_snapshot_is_cut_back_without_the_deleted_text(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let file = options.get_filename().to_owned();
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let bot = add_bot(&db, user, 1).await;
    let canary = format!("canary-wal-chat {}", "x".repeat(900));
    let mut snapshot = db.read_pool().begin().await.unwrap();
    sqlx::query_scalar!(r#"SELECT COUNT(*) AS "count!: i64" FROM chat_messages"#)
        .fetch_one(&mut *snapshot)
        .await
        .unwrap();
    lines_saying(&db, bot, 3000, now() - days(15), &canary).await;
    let run = service(
        &WatchedStore::new(db.clone()),
        now(),
        policy(),
        Duration::ZERO,
    )
    .run_once(&tokio_util::sync::CancellationToken::new())
    .await;
    let grown = wal_bytes(&file);
    let held = wal_holds(&file, "canary-wal-chat");

    snapshot.rollback().await.unwrap();
    lines_saying(&db, bot, 1, now(), "after the snapshot").await;
    lines_saying(&db, bot, 1, now(), "and once more").await;
    let cut = wal_bytes(&file);
    let still_held = wal_holds(&file, "canary-wal-chat");
    db.close().await;

    assert_eq!(run.deleted.chat_by_age, 3000);
    assert!(held, "the text was written to the WAL");
    assert!(
        grown > 4 << 20,
        "the WAL grew under the snapshot: {grown} bytes"
    );
    assert!(cut < 256 << 10, "the WAL was cut back: {cut} bytes");
    assert!(!still_held, "the WAL kept the deleted text");
}
