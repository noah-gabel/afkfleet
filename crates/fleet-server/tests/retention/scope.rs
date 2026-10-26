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

/// What a source file left open: a literal or a comment, with the line it
/// starts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unterminated {
    String { line: usize },
    RawString { line: usize },
    Comment { line: usize },
}

/// `text` with its comments, string literals and char literals blanked out,
/// newlines kept, so what's left is code and its lines still count; or what
/// a literal or a comment left open, with the line it starts on.
fn code_of(text: &str) -> Result<String, Unterminated> {
    let mut reader = Reader {
        chars: text.chars().collect(),
        at: 0,
        line: 1,
        code: String::with_capacity(text.len()),
    };
    while let Some(next) = reader.peek() {
        let line = reader.line;
        if reader.starts_with(&['/', '/']) {
            while reader.peek().is_some_and(|c| c != '\n') {
                reader.blank();
            }
        } else if reader.starts_with(&['/', '*']) {
            let mut depth = 0_usize;
            loop {
                if reader.starts_with(&['/', '*']) {
                    depth += 1;
                    reader.blank_n(2);
                } else if reader.starts_with(&['*', '/']) {
                    depth -= 1;
                    reader.blank_n(2);
                    if depth == 0 {
                        break;
                    }
                } else if reader.peek().is_some() {
                    reader.blank();
                } else {
                    return Err(Unterminated::Comment { line });
                }
            }
        } else if let Some((open, hashes)) = reader.raw_string() {
            let close: Vec<char> = std::iter::once('"')
                .chain(std::iter::repeat_n('#', hashes))
                .collect();
            reader.blank_n(open);
            while !reader.starts_with(&close) {
                if reader.peek().is_none() {
                    return Err(Unterminated::RawString { line });
                }
                reader.blank();
            }
            reader.blank_n(close.len());
        } else if next == '"' {
            reader.blank();
            loop {
                match reader.peek() {
                    None => return Err(Unterminated::String { line }),
                    Some('"') => break,
                    Some('\\') => {
                        reader.blank();
                        if reader.peek().is_none() {
                            return Err(Unterminated::String { line });
                        }
                        reader.blank();
                    }
                    Some(_) => reader.blank(),
                }
            }
            reader.blank();
        } else if let Some(len) = char_literal_len(reader.rest()).filter(|_| next == '\'') {
            reader.blank_n(len);
        } else {
            reader.keep();
        }
    }
    Ok(reader.code)
}

/// The code reader's place in a text, and what it kept so far.
struct Reader {
    chars: Vec<char>,
    at: usize,
    line: usize,
    code: String,
}

impl Reader {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn rest(&self) -> &[char] {
        self.chars.get(self.at..).unwrap_or_default()
    }

    fn starts_with(&self, prefix: &[char]) -> bool {
        self.rest().starts_with(prefix)
    }

    /// Takes the next char as code.
    fn keep(&mut self) {
        if let Some(c) = self.peek() {
            self.code.push(c);
            self.step(c);
        }
    }

    /// Takes the next char blanked: a space, or the newline it is.
    fn blank(&mut self) {
        if let Some(c) = self.peek() {
            self.code.push(if c == '\n' { '\n' } else { ' ' });
            self.step(c);
        }
    }

    fn blank_n(&mut self, n: usize) {
        for _ in 0..n {
            self.blank();
        }
    }

    fn step(&mut self, c: char) {
        self.at += 1;
        if c == '\n' {
            self.line += 1;
        }
    }

    /// The raw string opening here (`r"`, `r#"`, `br"`, `br#"`, `cr"`,
    /// `cr#"`, …): how long the opening is, and its hashes.
    fn raw_string(&self) -> Option<(usize, usize)> {
        let before = self.at.checked_sub(1).and_then(|at| self.chars.get(at));
        if before.is_some_and(|c| is_ident(*c)) {
            return None;
        }
        let rest = self.rest();
        let prefix = usize::from(matches!(rest.first(), Some('b' | 'c')));
        if rest.get(prefix) != Some(&'r') {
            return None;
        }
        let hashes = rest
            .iter()
            .skip(prefix + 1)
            .take_while(|c| **c == '#')
            .count();
        let quote = prefix + 1 + hashes;
        (rest.get(quote) == Some(&'"')).then_some((quote + 1, hashes))
    }
}

/// Whether `c` may be part of an identifier.
fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The length of the char literal `rest` starts with (`'x'`, `'\n'`,
/// `'\''`, `'\u{1F600}'`), or `None` for a lifetime or a label.
fn char_literal_len(rest: &[char]) -> Option<usize> {
    match rest.get(1)? {
        '\\' => rest
            .iter()
            .skip(3)
            .position(|c| *c == '\'')
            .map(|at| at + 4),
        _ => (rest.get(2) == Some(&'\'')).then_some(3),
    }
}

/// Each use of the identifier `name` in `code`: its line, and the code on
/// that line before it.
fn uses<'a>(code: &'a str, name: &str) -> Vec<(usize, &'a str)> {
    code.lines()
        .enumerate()
        .flat_map(|(index, line)| {
            line.match_indices(name)
                .filter(move |(at, _)| {
                    let before = line[..*at].chars().next_back();
                    let after = line[at + name.len()..].chars().next();
                    !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
                })
                .map(move |(at, _)| (index + 1, &line[..at]))
        })
        .collect()
}

#[test]
fn the_code_reader_skips_comments_and_literals_and_keeps_lines() {
    let text = concat!(
        "let a = 1; // x_y\n",
        "/* x_y /* x_y */ */ let b = \"x_y \\\" x_y\";\n",
        "let c = r#\"x_y \"x_y\"# ; let d = '\"'; fn e<'a>(f: &'a str) {}\n",
        "let g = br\"x_y\\\"; let h = br#\"x_y \"x_y\"#; let i = cr\"x_y\\\";\n",
        "let j = cr#\"x_y \"x_y\"#; let k = b\"x_y\"; let l = c\"x_y\"; let m = b'\"';\n",
        "x_y(); x_yz(); ax_y();\n",
    );

    let code = code_of(text).unwrap();

    assert_eq!(code.lines().count(), 6);
    assert_eq!(uses(&code, "x_y"), [(6, "")]);
    assert!(code.contains("fn e<'a>(f: &'a str)"), "{code}");
}

#[rstest::rstest]
#[case::string("let a = 1;\nlet b = \"x_y;\n", Unterminated::String { line: 2 })]
#[case::escape_at_the_end("let b = \"x_y\\", Unterminated::String { line: 1 })]
#[case::raw_string("let c = r#\"x_y\"\n;\n", Unterminated::RawString { line: 1 })]
#[case::byte_raw_string("\nlet c = br\"x_y", Unterminated::RawString { line: 2 })]
#[case::comment("/* x_y /* x_y */\n", Unterminated::Comment { line: 1 })]
fn an_unterminated_literal_or_comment_is_named_with_its_line(
    #[case] text: &str,
    #[case] expected: Unterminated,
) {
    assert_eq!(code_of(text), Err(expected));
}

/// The server's own sources name `Store::retention_batch` only where it's
/// declared, where the store implements it and once in the retention
/// service, which calls it: no other service opens the transaction that
/// deletes without an audit entry, by any path (a method call, a fully
/// qualified call, a method reference, a second caller in the service).
#[test]
fn only_the_retention_service_opens_a_retention_batch() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&src, &mut files);

    let mut found: Vec<String> = Vec::new();
    for file in &files {
        let code = code_of(&std::fs::read_to_string(file).unwrap())
            .unwrap_or_else(|unterminated| panic!("{}: {unterminated:?}", file.display()));
        let name = file
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        for (line, before) in uses(&code, "retention_batch") {
            let kind = if before.trim_end().ends_with("fn") {
                "fn"
            } else if before.trim_end().ends_with('.') {
                "call"
            } else {
                "other"
            };
            found.push(format!("{name}:{kind}:{line}"));
        }
    }
    let shape: Vec<String> = found
        .iter()
        .map(|use_| use_.rsplit_once(':').unwrap().0.to_owned())
        .collect();

    assert!(files.len() > 100, "the sources were read: {}", files.len());
    let mut expected = [
        "app/retention.rs:call",
        "infra/sqlite/store.rs:fn",
        "ports/store.rs:fn",
    ];
    expected.sort_unstable();
    let mut shape = shape;
    shape.sort();
    assert_eq!(shape, expected, "every use: {found:?}");
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
