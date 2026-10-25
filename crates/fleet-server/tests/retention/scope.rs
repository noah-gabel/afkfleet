//! The scope of a retention batch (ADR-0020, U20, R57): the one transaction
//! that commits deletes without an audit entry. Only the retention service
//! opens one, and each of its deletes removes rows of its own table and
//! touches nothing else, over a database with rows in every table.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use fleet_core::id::BotId;
use fleet_server::infra::sqlite::Database;
use fleet_server::ports::audit::AuditEntryId;
use fleet_server::ports::chat::ChatMessageId;
use fleet_server::ports::store::Store;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::fixtures::{
    BATCH, T0, add_account_bot, add_agent, add_invite, add_session, add_token, add_user, at, batch,
    database, days, entries, lines, rotations,
};
use crate::tables::{Ids, TABLES, dump, fill};

/// Every `.rs` file under `dir`, recursively.
fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// The server's own sources call `Store::retention_batch` in one place, the
/// retention service: no other service opens the transaction that deletes
/// without an audit entry.
#[test]
fn only_the_retention_service_opens_a_retention_batch() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&src, &mut files);

    let callers: Vec<String> = files
        .iter()
        .filter(|file| {
            std::fs::read_to_string(file)
                .unwrap()
                .contains(".retention_batch(")
        })
        .map(|file| {
            file.strip_prefix(&src)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();

    assert!(files.len() > 100, "the sources were read: {}", files.len());
    assert_eq!(callers, ["app/retention.rs"]);
}

/// One kind of the retention's deletes, in the service's order.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Rotations,
    Sessions,
    ChatByAge,
    ChatBySize,
    Audit,
    Invites,
    Tokens,
}

impl Kind {
    const ALL: [Self; 7] = [
        Self::Rotations,
        Self::Sessions,
        Self::ChatByAge,
        Self::ChatBySize,
        Self::Audit,
        Self::Invites,
        Self::Tokens,
    ];

    /// The table it deletes from.
    const fn table(self) -> &'static str {
        match self {
            Self::Rotations => "rotated_refresh_tokens",
            Self::Sessions => "sessions",
            Self::ChatByAge | Self::ChatBySize => "chat_messages",
            Self::Audit => "audit_log",
            Self::Invites => "invites",
            Self::Tokens => "enrollment_tokens",
        }
    }
}

/// Where the test's batches cut.
#[derive(Debug, Clone, Copy)]
struct Cuts {
    at: DateTime<Utc>,
    bot: BotId,
    chat: ChatMessageId,
    audit: AuditEntryId,
}

/// A database with rows in every table that no cut reaches, and rows of
/// each kind the retention deletes.
async fn filled(options: SqliteConnectOptions) -> (Database, Cuts) {
    let db = database(options).await;
    let cut = at(T0) - days(1);
    // Old entries first: they're the prefix the audit cut finds.
    entries(&db, 3, cut - days(1)).await;
    let owner = add_user(&db, 7).await;
    let friend = add_user(&db, 8).await;
    let one = add_agent(&db, 7).await;
    let two = add_agent(&db, 8).await;
    let (account, held) = add_account_bot(&db, owner, 7).await;
    let (_, other) = add_account_bot(&db, owner, 8).await;
    let ids = Ids {
        owner: owner.to_string(),
        friend: friend.to_string(),
        account: account.to_string(),
        one: one.to_string(),
        two: two.to_string(),
        held: held.to_string(),
    };
    // A row of every other table, dated at T0 or later.
    fill(db.write_pool(), &ids).await;
    let session = add_session(&db, owner, 7, cut - days(1), false).await;
    rotations(&db, session, 3).await;
    lines(&db, held, 3, cut - days(1)).await;
    lines(&db, other, 5, at(T0)).await;
    add_invite(&db, owner, 7, cut - days(1)).await;
    add_token(&db, 7, cut - days(1)).await;
    let chat = db.retention().chat_cut(other, 2).await.unwrap().unwrap();
    let audit = db
        .retention()
        .audit_cut(cut)
        .await
        .unwrap()
        .unwrap()
        .through;
    let cuts = Cuts {
        at: cut,
        bot: other,
        chat,
        audit,
    };
    (db, cuts)
}

/// Whether `after` is `before` with some rows left out and nothing else:
/// every row it holds is one of `before`'s, unchanged and in order.
fn only_removed(before: &[String], after: &[String]) -> bool {
    let mut rest = before.iter();
    after.len() < before.len() && after.iter().all(|row| rest.any(|old| old == row))
}

/// One batch of each delete, in the service's order, over a database with
/// rows in every table: its own table only loses rows, and every other
/// table stays the same byte for byte, so no delete updates a row or
/// deletes from another table.
#[sqlx::test(migrations = false)]
async fn a_retention_batch_deletes_only_rows_of_its_own_table(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, cuts) = filled(options).await;
    let start = dump(db.read_pool()).await;
    assert!(
        TABLES.iter().all(|table| !start[table].is_empty()),
        "every table holds a row to watch: {start:?}"
    );

    let mut changed: BTreeMap<&str, usize> = BTreeMap::new();
    for kind in Kind::ALL {
        let before = dump(db.read_pool()).await;
        let deleted = batch(&db, |writes| {
            Box::pin(async move {
                match kind {
                    Kind::Rotations => writes.delete_expired_rotations(cuts.at, BATCH).await,
                    Kind::Sessions => writes.delete_expired_sessions(cuts.at, BATCH).await,
                    Kind::ChatByAge => writes.delete_chat_received_by(cuts.at, BATCH).await,
                    Kind::ChatBySize => {
                        writes.delete_chat_through(cuts.bot, cuts.chat, BATCH).await
                    }
                    Kind::Audit => writes
                        .delete_audit_through(cuts.audit, BATCH)
                        .await
                        .map(|purged| purged.count),
                    Kind::Invites => writes.delete_unused_invites(cuts.at, BATCH).await,
                    Kind::Tokens => writes.delete_unused_enrollment_tokens(cuts.at, BATCH).await,
                }
                .unwrap()
            })
        })
        .await;
        let after = dump(db.read_pool()).await;

        assert!(deleted > 0, "{kind:?} deleted something");
        for table in TABLES {
            if before[table] != after[table] {
                *changed.entry(table).or_default() += 1;
                assert!(
                    table == kind.table() && only_removed(&before[table], &after[table]),
                    "{kind:?} changed {table}: {:?} to {:?}",
                    before[table],
                    after[table]
                );
            }
        }
    }
    // Each delete changed its own table, and chat twice.
    assert_eq!(
        changed,
        BTreeMap::from([
            ("audit_log", 1),
            ("chat_messages", 2),
            ("enrollment_tokens", 1),
            ("invites", 1),
            ("rotated_refresh_tokens", 1),
            ("sessions", 1),
        ])
    );
}
