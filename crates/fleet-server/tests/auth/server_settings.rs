//! The servers' settings over HTTP (Plan.md P11.9; ADR-0020, U54,
//! S74–S78): `GET` and `POST /api/v1/server-settings`, `PUT` and `DELETE
//! /api/v1/server-settings/{id}`. Admins and the Owner only, every write
//! after a step-up; the server keyed; the texts refused at their entry,
//! never repaired; the cap, a taken server, the version, the no-op, the
//! re-checks in the transaction, the audit entries and the hint.

use std::collections::BTreeMap;

use fleet_api_types::server_settings::SERVER_SETTINGS_PAGE_MAX;
use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget};
use fleet_core::authz::Role;
use fleet_core::chat::ChatFormats;
use fleet_core::disconnect::ConflictTexts;
use fleet_core::id::ServerSettingsId;
use fleet_core::server_settings::MAX_SERVER_SETTINGS;
use fleet_core::value::{ServerAddress, ServerKey};
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::ports::reconcile::Change;
use fleet_server::ports::server_settings::{NewServerSettings, StoredServerSettings};
use fleet_server::ports::store::Store as _;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::support::{
    Meanwhile, Options, Person, Seen, Setup, T0, at, redacted, setup, setup_entry, setup_with,
};

/// The collection's route.
const SETTINGS: &str = "/api/v1/server-settings";

/// A settings row's route.
fn path(id: ServerSettingsId) -> String {
    format!("{SETTINGS}/{id}")
}

/// fleet-client's `MAX_RESPONSE_BYTES`: fleet-server doesn't depend on
/// fleet-client, so it's written out, as modes' test has it.
const CLIENT_CAP: usize = 1024 * 1024;

/// The longest server key `n` makes: a host of 253 characters, four labels
/// of 63, 63, 63 and 61, and a five-digit port.
fn longest_server(n: u8) -> String {
    format!(
        "{n:02}{}.{}.{}.{}:65535",
        "a".repeat(61),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(61)
    )
}

/// 16 distinct texts of 1024 characters each, every character `width` bytes
/// in UTF-8: CJK ideographs for 3, emoji for 4, each text its own one.
fn widest_texts(width: usize, offset: u32) -> Vec<String> {
    let first = if width == 3 { 0x4E00 } else { 0x1_F600 };
    (0..16)
        .map(|n| {
            let character = char::from_u32(first + offset + n).unwrap();
            assert_eq!(character.len_utf8(), width);
            character.to_string().repeat(1024)
        })
        .collect()
}

/// A create's body.
fn create_body(server: &str, texts: &[&str]) -> Value {
    json!({ "server": server, "conflict_texts": texts })
}

/// An edit's body, at `version`.
fn edit_body(version: u32, server: &str, texts: &[&str]) -> Value {
    json!({ "version": version, "server": server, "conflict_texts": texts })
}

fn key(address: &str) -> ServerKey {
    ServerKey::from(&address.parse::<ServerAddress>().unwrap())
}

/// `role`'s person `n`, stepped up now.
async fn stepped_up(setup: &Setup, n: u8, name: &str, role: Role) -> Person {
    let person = setup.person(n, name, role).await;
    setup.stamp(person.session).await;
    person
}

/// An entry's metadata as text values by key.
fn metadata(entry: &fleet_server::ports::audit::AuditRecord) -> BTreeMap<String, String> {
    use fleet_core::audit::RecordedValue;
    entry
        .metadata
        .entries()
        .iter()
        .map(|(key, value)| {
            let value = match value {
                RecordedValue::Text(text) | RecordedValue::Unrecognized(text) => text.clone(),
                RecordedValue::Int(int) => int.to_string(),
                RecordedValue::Bool(flag) => flag.to_string(),
            };
            (key.clone(), value)
        })
        .collect()
}

impl Setup {
    /// `POST /server-settings` with `body` as `by`.
    async fn create_settings(&self, by: &Person, body: Value) -> Seen {
        self.post(SETTINGS, Some(&by.token), Some(body)).await
    }

    /// `PUT /server-settings/{id}` with `body` as `by`.
    async fn edit_settings(&self, by: &Person, id: ServerSettingsId, body: Value) -> Seen {
        self.request(
            axum::http::Method::PUT,
            &path(id),
            Some(&by.token),
            Some(body),
        )
        .await
    }

    /// `DELETE /server-settings/{id}` as `by`.
    async fn delete_settings(&self, by: &Person, id: ServerSettingsId) -> Seen {
        self.delete(&path(id), Some(&by.token)).await
    }

    /// `GET /server-settings` with `query` as `by`.
    async fn list_settings(&self, by: &Person, query: &str) -> Seen {
        self.get(&format!("{SETTINGS}{query}"), Some(&by.token))
            .await
    }

    /// The settings `id` as stored now.
    async fn stored_settings(&self, id: ServerSettingsId) -> Option<StoredServerSettings> {
        self.db.server_settings().find(id).await.unwrap()
    }

    /// How many server-settings entries the log holds.
    async fn settings_entries(&self) -> usize {
        self.audit()
            .await
            .iter()
            .filter(|entry| {
                [
                    AuditAction::SERVER_SETTINGS_CREATE,
                    AuditAction::SERVER_SETTINGS_UPDATE,
                    AuditAction::SERVER_SETTINGS_DELETE,
                ]
                .contains(&entry.action)
            })
            .count()
    }

    /// Stores settings numbered `n` for `server` with one text, as a write
    /// would, without an entry of its own kind: made an hour before now,
    /// `n` milliseconds apart, so their IDs sort by `n`.
    async fn add_settings(&self, n: u8, server: &str) -> ServerSettingsId {
        self.add_settings_with(n, server, &["Already connected"])
            .await
    }

    /// As [`Setup::add_settings`], with the conflict texts `texts`.
    async fn add_settings_with(
        &self,
        n: u8,
        server: &str,
        texts: &[impl AsRef<str>],
    ) -> ServerSettingsId {
        let created = at(T0 - 3_600_000 + i64::from(n));
        let id = ServerSettingsId::new_v7(created, [n; 10]).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.server_settings()
            .insert(&NewServerSettings {
                id,
                server: key(server),
                conflict_texts: ConflictTexts::try_new(texts).unwrap(),
                chat_formats: ChatFormats::default(),
                created_at: created,
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        id
    }
}

/// The ID a created row was answered with.
fn created_id(seen: &Seen) -> ServerSettingsId {
    seen.body["id"].as_str().unwrap().parse().unwrap()
}

#[sqlx::test(migrations = false)]
async fn the_server_settings_need_a_session(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let id = ServerSettingsId::new_v7(at(T0), [0xEE; 10]).unwrap();

    let seen = [
        setup.get(SETTINGS, None).await,
        setup
            .post(
                SETTINGS,
                None,
                Some(create_body("mc.example.com", &["One"])),
            )
            .await,
        setup
            .request(
                axum::http::Method::PUT,
                &path(id),
                None,
                Some(edit_body(1, "mc.example.com", &["One"])),
            )
            .await,
        setup.delete(&path(id), None).await,
    ];

    for seen in &seen {
        assert_eq!(seen.status, 401, "{}", seen.text);
    }
}

/// A Member gets 403 on every route, for a row that exists and one that
/// doesn't alike: `authorize()` runs before any row is read (S75).
#[sqlx::test(migrations = false)]
async fn a_member_is_refused_every_route_whatever_the_id(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = stepped_up(&setup, 2, "member", Role::Member).await;
    let existing = setup.add_settings(1, "mc.example.com").await;
    let unknown = ServerSettingsId::new_v7(at(T0), [0xEE; 10]).unwrap();
    let before = setup.stored_settings(existing).await;

    let mut seen = vec![
        setup.list_settings(&member, "").await,
        setup
            .create_settings(&member, create_body("play.example.com", &["One"]))
            .await,
    ];
    for id in [existing, unknown] {
        seen.push(
            setup
                .edit_settings(&member, id, edit_body(1, "mc.example.com", &["Two"]))
                .await,
        );
        seen.push(setup.delete_settings(&member, id).await);
    }

    for seen in &seen {
        assert_eq!(
            (seen.status, seen.code()),
            (403, "forbidden"),
            "{}",
            seen.text
        );
    }
    assert_eq!(setup.stored_settings(existing).await, before);
    assert_eq!(setup.settings_entries().await, 0);
}

/// Every write needs a fresh step-up and changes nothing without one;
/// reading doesn't.
#[sqlx::test(migrations = false)]
async fn writes_need_a_fresh_step_up_and_reads_dont(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let id = setup.add_settings(1, "mc.example.com").await;
    let before = setup.stored_settings(id).await;

    let read = setup.list_settings(&admin, "").await;
    let refused = [
        setup
            .create_settings(&admin, create_body("play.example.com", &["One"]))
            .await,
        setup
            .edit_settings(&admin, id, edit_body(1, "mc.example.com", &["Two"]))
            .await,
        setup.delete_settings(&admin, id).await,
    ];

    assert_eq!(read.status, 200, "{}", read.text);
    for seen in &refused {
        assert_eq!(
            (seen.status, seen.code()),
            (403, "step_up_required"),
            "{}",
            seen.text
        );
    }
    assert_eq!(setup.stored_settings(id).await, before);
    assert_eq!(setup.settings_entries().await, 0);
    assert_eq!(setup.hints.take(), []);
}

/// An Admin creates a server's settings, the server answered as its key and
/// each text as its lines; reads them in the list; edits them; deletes them.
/// Each write is one entry and one hint, after its commit.
#[sqlx::test(migrations = false)]
async fn an_admin_creates_lists_edits_and_deletes_a_servers_settings(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;

    let created = setup
        .create_settings(
            &admin,
            create_body(
                "MC.example.com",
                &["You logged in\nfrom another location", "Already connected"],
            ),
        )
        .await;
    assert_eq!(created.status, 201, "{}", created.text);
    let id = created_id(&created);
    insta::assert_json_snapshot!(
        "server_settings_created",
        redacted(&created.body, &[(id.to_string(), "the settings")])
    );
    assert_eq!(setup.hints.take(), [Change::Store]);

    let listed = setup.list_settings(&admin, "").await;
    assert_eq!(listed.status, 200, "{}", listed.text);
    assert_eq!(listed.body["items"], json!([created.body]));

    setup.clock.advance(core::time::Duration::from_secs(60));
    let edited = setup
        .edit_settings(
            &admin,
            id,
            edit_body(1, "mc.example.com:25570", &["Kicked: duplicate"]),
        )
        .await;
    assert_eq!(edited.status, 200, "{}", edited.text);
    insta::assert_json_snapshot!(
        "server_settings_edited",
        redacted(&edited.body, &[(id.to_string(), "the settings")])
    );
    assert_eq!(setup.hints.take(), [Change::Store]);

    let deleted = setup.delete_settings(&admin, id).await;
    assert_eq!(deleted.status, 204, "{}", deleted.text);
    assert_eq!(setup.stored_settings(id).await, None);
    assert_eq!(setup.hints.take(), [Change::Store]);
    assert_eq!(
        setup.list_settings(&admin, "").await.body["items"],
        json!([])
    );
    assert_eq!(setup.settings_entries().await, 3);
}

/// The Owner writes them as an Admin does.
#[sqlx::test(migrations = false)]
async fn the_owner_writes_them_too(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let owner = stepped_up(&setup, 2, "theowner", Role::Owner).await;

    let created = setup
        .create_settings(&owner, create_body("192.0.2.10", &["Already connected"]))
        .await;

    assert_eq!(created.status, 201, "{}", created.text);
    assert_eq!(created.body["server"], "192.0.2.10:25565");
}

/// The list is newest first, a page at a time.
#[sqlx::test(migrations = false)]
async fn the_list_pages_newest_first(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let older = setup.add_settings(1, "one.example.com").await;
    let newer = setup.add_settings(2, "two.example.com").await;

    let first = setup.list_settings(&admin, "?limit=1").await;
    let second = setup
        .list_settings(&admin, &format!("?limit=1&before={newer}"))
        .await;

    assert_eq!(first.body["items"][0]["id"], newer.to_string());
    assert_eq!(first.body["next"], newer.to_string());
    assert_eq!(second.body["items"][0]["id"], older.to_string());
    assert_eq!(second.body.get("next"), None);
}

/// A page holds at most 10 rows, and 10 by default, so a page of rows at
/// their caps fits a client's answer cap (PR #79's review).
#[sqlx::test(migrations = false)]
async fn a_limit_past_ten_is_422_and_a_page_holds_ten_by_default(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    for n in 0..11 {
        setup.add_settings(n, &format!("s{n}.example.com")).await;
    }

    let past = setup.list_settings(&admin, "?limit=11").await;
    let first = setup.list_settings(&admin, "").await;

    assert_eq!(past.status, 422, "{}", past.text);
    assert_eq!(
        past.body["error"]["fields"],
        json!([{ "path": "limit", "message": "greater than 10" }])
    );
    assert_eq!(first.body["items"].as_array().unwrap().len(), 10);
    let next = first.body["next"].as_str().unwrap();
    let second = setup
        .list_settings(&admin, &format!("?before={next}"))
        .await;
    assert_eq!(second.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(second.body.get("next"), None);
}

/// The longest page, rows at every cap (the longest server, 16 texts of
/// 1024 four-byte characters), fits fleet-client's 1 MiB answer cap.
#[sqlx::test(migrations = false)]
async fn the_longest_page_fits_a_clients_answer_cap(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let rows = u8::try_from(SERVER_SETTINGS_PAGE_MAX).unwrap();
    for n in 0..rows {
        setup
            .add_settings_with(n, &longest_server(n), &widest_texts(4, 0))
            .await;
    }

    let seen = setup
        .list_settings(&admin, &format!("?limit={SERVER_SETTINGS_PAGE_MAX}"))
        .await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    assert_eq!(
        seen.body["items"].as_array().unwrap().len(),
        usize::from(rows)
    );
    assert!(
        seen.text.len() > usize::from(rows) * 16 * 1024 * 4,
        "the rows are at their caps: {} bytes",
        seen.text.len()
    );
    assert!(seen.text.len() < CLIENT_CAP, "{} bytes", seen.text.len());
}

/// A malformed or unknown ID is 404 for an Admin, an edit's and a
/// delete's alike.
#[sqlx::test(migrations = false)]
async fn a_malformed_or_unknown_id_is_404(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    let unknown = ServerSettingsId::new_v7(at(T0), [0xEE; 10]).unwrap();
    let edit = edit_body(1, "mc.example.com", &["One"]);

    let seen = [
        setup
            .request(
                axum::http::Method::PUT,
                &format!("{SETTINGS}/not-an-id"),
                Some(&admin.token),
                Some(edit.clone()),
            )
            .await,
        setup
            .delete(&format!("{SETTINGS}/not-an-id"), Some(&admin.token))
            .await,
        setup.edit_settings(&admin, unknown, edit).await,
        setup.delete_settings(&admin, unknown).await,
    ];

    for seen in &seen {
        assert_eq!(seen.status, 404, "{}", seen.text);
    }
    assert_eq!(setup.settings_entries().await, 0);
}

/// A body that doesn't parse is the bare 400; a rule it breaks is one 422
/// at its path, never repaired (S6, S77). Nothing is written.
#[sqlx::test(migrations = false)]
async fn a_body_that_doesnt_parse_is_400_and_a_broken_rule_422(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    let id = setup.add_settings(1, "mc.example.com").await;
    let before = setup.stored_settings(id).await;
    let mut unknown_field = create_body("play.example.com", &["One"]);
    unknown_field["chat_formats"] = json!([]);
    let mut no_version = edit_body(1, "mc.example.com", &["One"]);
    no_version.as_object_mut().unwrap().remove("version");
    let seventeen: Vec<String> = (0..17).map(|n| format!("Text {n}")).collect();
    let seventeen: Vec<&str> = seventeen.iter().map(String::as_str).collect();
    let long = "x".repeat(1025);

    let shapes = [
        setup.create_settings(&admin, unknown_field).await,
        setup
            .create_settings(
                &admin,
                json!({ "server": "mc.example.com", "conflict_texts": [1] }),
            )
            .await,
        setup.edit_settings(&admin, id, no_version).await,
    ];
    for seen in &shapes {
        assert_eq!(seen.status, 400, "{}", seen.text);
    }
    for (body, field, message) in [
        (
            create_body("tcp://mc.example.com", &["One"]),
            "server",
            "must be only a host and an optional port",
        ),
        (
            create_body("play.example.com", &[]),
            "conflict_texts",
            "must hold at least one text",
        ),
        (
            create_body("play.example.com", &seventeen),
            "conflict_texts",
            "must hold at most 16 texts",
        ),
        (
            create_body("play.example.com", &["One", ""]),
            "conflict_texts[1]",
            "must not be empty",
        ),
        (
            create_body("play.example.com", &[long.as_str()]),
            "conflict_texts[0]",
            "must be at most 1024 characters",
        ),
        (
            create_body("play.example.com", &["One", "Two\u{202e}"]),
            "conflict_texts[1]",
            "can't hold control characters other than line breaks, formatting codes, text-direction marks or invisible characters",
        ),
    ] {
        let seen = setup.create_settings(&admin, body).await;
        assert_eq!(seen.status, 422, "{field}: {}", seen.text);
        assert_eq!(
            seen.body["error"]["fields"],
            json!([{ "path": field, "message": message }])
        );
    }
    let low = setup
        .edit_settings(&admin, id, edit_body(0, "mc.example.com", &["One"]))
        .await;
    assert_eq!(low.status, 422, "{}", low.text);
    assert_eq!(low.body["error"]["fields"][0]["path"], "version");
    assert_eq!(setup.stored_settings(id).await, before);
    assert_eq!(setup.settings_entries().await, 0);
}

/// A server that has settings already is 422 on `server`, however its
/// address is written: a create's, and an edit moving another row there.
#[sqlx::test(migrations = false)]
async fn a_server_that_has_settings_already_is_422_on_server(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    setup.add_settings(1, "mc.example.com").await;
    let other = setup.add_settings(2, "play.example.com").await;
    let before = setup.stored_settings(other).await;

    let created = setup
        .create_settings(&admin, create_body("MC.Example.com:25565", &["Two"]))
        .await;
    let moved = setup
        .edit_settings(&admin, other, edit_body(1, "mc.example.com", &["Two"]))
        .await;

    for seen in [&created, &moved] {
        assert_eq!(seen.status, 422, "{}", seen.text);
        assert_eq!(
            seen.body["error"]["fields"],
            json!([{ "path": "server", "message": "already has settings" }])
        );
    }
    assert_eq!(setup.stored_settings(other).await, before);
    assert_eq!(setup.settings_entries().await, 0);
}

/// The 101st server is 409 `conflict`, and nothing is written (S78).
#[sqlx::test(migrations = false)]
async fn the_hundred_and_first_server_is_409_and_nothing_is_written(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    for n in 0..u8::try_from(MAX_SERVER_SETTINGS).unwrap() {
        setup.add_settings(n, &format!("s{n}.example.com")).await;
    }

    let refused = setup
        .create_settings(&admin, create_body("one-more.example.com", &["One"]))
        .await;

    assert_eq!(
        (refused.status, refused.code()),
        (409, "conflict"),
        "{}",
        refused.text
    );
    assert_eq!(setup.settings_entries().await, 0);
    assert_eq!(setup.hints.take(), []);
}

/// An edit at a stale version is 409 `server_settings_changed`, and the row
/// is unchanged (S77).
#[sqlx::test(migrations = false)]
async fn an_edit_at_a_stale_version_is_409(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    let id = setup.add_settings(1, "mc.example.com").await;
    let first = setup
        .edit_settings(&admin, id, edit_body(1, "mc.example.com", &["Two"]))
        .await;
    assert_eq!(first.status, 200, "{}", first.text);
    let after_first = setup.stored_settings(id).await;
    setup.hints.take();

    let stale = setup
        .edit_settings(&admin, id, edit_body(1, "mc.example.com", &["Three"]))
        .await;

    assert_eq!(
        (stale.status, stale.code()),
        (409, "server_settings_changed"),
        "{}",
        stale.text
    );
    assert_eq!(setup.stored_settings(id).await, after_first);
    assert_eq!(setup.hints.take(), []);
}

/// An edit asking for what's stored, its server written another way and
/// its texts in another order with a duplicate, answers the row as it is:
/// no new version, no entry, no hint, even at a stale version (S21's order).
#[sqlx::test(migrations = false)]
async fn an_edit_that_changes_nothing_writes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    let created = setup
        .create_settings(&admin, create_body("mc.example.com", &["One", "Two"]))
        .await;
    let id = created_id(&created);
    setup.hints.take();
    let entries = setup.settings_entries().await;

    let same = setup
        .edit_settings(
            &admin,
            id,
            edit_body(7, "MC.example.com:25565", &["Two", "One", "Two"]),
        )
        .await;

    assert_eq!(same.status, 200, "{}", same.text);
    assert_eq!(same.body, created.body);
    assert_eq!(setup.settings_entries().await, entries);
    assert_eq!(setup.hints.take(), []);
}

/// The answer's lines, joined by line breaks and sent back, are the texts
/// exactly: a no-op (S77).
#[sqlx::test(migrations = false)]
async fn the_answers_lines_sent_back_change_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    let created = setup
        .create_settings(
            &admin,
            create_body(
                "mc.example.com",
                &["a\n\nb", "\nleading", "trailing\n", "plain"],
            ),
        )
        .await;
    let id = created_id(&created);
    let texts: Vec<String> = created.body["conflict_texts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|text| {
            text["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|line| line.as_str().unwrap())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect();
    let texts: Vec<&str> = texts.iter().map(String::as_str).collect();

    let back = setup
        .edit_settings(&admin, id, edit_body(1, "mc.example.com", &texts))
        .await;

    assert_eq!(back.status, 200, "{}", back.text);
    assert_eq!(back.body["version"], 1);
}

/// Each write records one entry with the server's host and port and the
/// counts, never a text; an edit that moves the server records the one
/// before too (S78).
#[sqlx::test(migrations = false)]
async fn each_write_is_recorded_without_a_text(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    let secret = "a kick text no entry may hold";

    let created = setup
        .create_settings(&admin, create_body("mc.example.com", &[secret, "Two"]))
        .await;
    let id = created_id(&created);
    setup
        .edit_settings(&admin, id, edit_body(1, "[2001:db8::1]:25570", &[secret]))
        .await;
    setup
        .edit_settings(&admin, id, edit_body(2, "[2001:db8::1]:25570", &["Other"]))
        .await;
    setup.delete_settings(&admin, id).await;

    let target = Some(RecordedTarget::Known(AuditTarget::ServerSettings(id)));
    let create = setup.last(&AuditAction::SERVER_SETTINGS_CREATE).await;
    let updates: Vec<_> = setup
        .audit()
        .await
        .into_iter()
        .filter(|entry| entry.action == AuditAction::SERVER_SETTINGS_UPDATE)
        .collect();
    let delete = setup.last(&AuditAction::SERVER_SETTINGS_DELETE).await;
    for entry in [&create, &updates[0], &updates[1], &delete] {
        assert_eq!(entry.outcome, AuditOutcome::Success);
        assert_eq!(entry.actor, Some(admin.id));
        assert!(entry.ip.is_some());
        assert_eq!(entry.target, target);
        assert!(
            metadata(entry)
                .values()
                .all(|value| !value.contains("kick text")),
            "{entry:?}"
        );
    }
    let map = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    };
    assert_eq!(
        metadata(&create),
        map(&[
            ("server_host", "mc.example.com"),
            ("server_port", "25565"),
            ("texts", "2")
        ])
    );
    // Newest first: the second edit, then the first, which moved the server.
    assert_eq!(
        metadata(&updates[1]),
        map(&[
            ("version", "2"),
            ("server_host", "2001:db8::1"),
            ("server_port", "25570"),
            ("previous_server_host", "mc.example.com"),
            ("previous_server_port", "25565"),
            ("texts", "1"),
            ("previous_texts", "2"),
            ("texts_changed", "true"),
        ])
    );
    assert_eq!(
        metadata(&updates[0]),
        map(&[
            ("version", "3"),
            ("server_host", "2001:db8::1"),
            ("server_port", "25570"),
            ("texts", "1"),
            ("previous_texts", "1"),
            ("texts_changed", "true"),
        ])
    );
    assert_eq!(
        metadata(&delete),
        map(&[
            ("version", "3"),
            ("server_host", "2001:db8::1"),
            ("server_port", "25570"),
            ("texts", "1"),
        ])
    );
}

/// The writes re-check in their transaction: an Admin demoted meanwhile is
/// 403, a session revoked meanwhile 401, and nothing is written.
#[sqlx::test(migrations = false)]
async fn the_writes_recheck_the_actor_in_their_transaction(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let id = setup.add_settings(1, "mc.example.com").await;
    let before = setup.stored_settings(id).await;

    let mut seen = Vec::new();
    for write in 0..3_u8 {
        // An Admin of their own for each write, demoted right before it.
        let admin = stepped_up(&setup, 2 + write, &format!("admin{write}"), Role::Admin).await;
        setup
            .hooked
            .queue(Meanwhile::SetRole(admin.id, Role::Member));
        seen.push(match write {
            0 => {
                setup
                    .create_settings(&admin, create_body("play.example.com", &["One"]))
                    .await
            }
            1 => {
                setup
                    .edit_settings(&admin, id, edit_body(1, "mc.example.com", &["Two"]))
                    .await
            }
            _ => setup.delete_settings(&admin, id).await,
        });
    }
    let admin = stepped_up(&setup, 9, "admin", Role::Admin).await;
    for seen in &seen {
        assert_eq!(
            (seen.status, seen.code()),
            (403, "forbidden"),
            "{}",
            seen.text
        );
    }
    setup.hooked.queue(Meanwhile::RevokeSession(admin.session));
    let gone = setup
        .edit_settings(&admin, id, edit_body(1, "mc.example.com", &["Two"]))
        .await;

    assert_eq!(gone.status, 401, "{}", gone.text);
    assert_eq!(setup.stored_settings(id).await, before);
    assert_eq!(setup.settings_entries().await, 0);
}

/// A guarded write that matches no row is a 500, never a success, and
/// nothing is recorded.
#[sqlx::test(migrations = false)]
async fn a_lost_write_is_500_never_a_success(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    let id = setup.add_settings(1, "mc.example.com").await;

    setup.hooked.lose_next_mode_write();
    let edit = setup
        .edit_settings(&admin, id, edit_body(1, "mc.example.com", &["Two"]))
        .await;
    setup.hooked.lose_next_mode_write();
    let delete = setup.delete_settings(&admin, id).await;

    assert_eq!(edit.status, 500, "{}", edit.text);
    assert_eq!(delete.status, 500, "{}", delete.text);
    assert_eq!(setup.settings_entries().await, 0);
    assert_eq!(setup.hints.take(), []);
}

#[sqlx::test(migrations = false)]
async fn every_route_spends_the_users_request_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let one = Rate::per_second(core::num::NonZeroU32::MIN, core::num::NonZeroU32::MIN);
    let setup = setup_with(
        options,
        Options {
            limits: RateLimitConfig {
                per_user: one,
                ..RateLimitConfig::default()
            },
            ..Options::default()
        },
    )
    .await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    let id = setup.add_settings(1, "mc.example.com").await;

    for route in 0..4 {
        let call = || async {
            match route {
                0 => setup.list_settings(&admin, "").await,
                1 => {
                    setup
                        .create_settings(&admin, create_body("mc.example.com", &["One"]))
                        .await
                }
                2 => {
                    setup
                        .edit_settings(
                            &admin,
                            id,
                            edit_body(9, "mc.example.com", &["Already connected"]),
                        )
                        .await
                }
                _ => {
                    setup
                        .delete_settings(
                            &admin,
                            ServerSettingsId::new_v7(at(T0), [0xEE; 10]).unwrap(),
                        )
                        .await
                }
            }
        };
        let first = call().await;
        let second = call().await;
        assert_ne!(first.status, 429, "{route}: {}", first.text);
        assert_eq!(second.status, 429, "{route}");
        assert_eq!(second.headers["retry-after"], "1", "{route}");
        setup.clock.advance(core::time::Duration::from_secs(1));
    }
}

/// `authorize()` runs before any row is read, in the service too (S75): an
/// Admin demoted right before the write is 403 for an unknown ID and for a
/// server that has settings already, never 404 or 422.
#[sqlx::test(migrations = false)]
async fn a_demoted_admin_is_403_before_any_row_is_read(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup.add_settings(1, "mc.example.com").await;
    let other = setup.add_settings(2, "play.example.com").await;
    let unknown = ServerSettingsId::new_v7(at(T0), [0xEE; 10]).unwrap();

    let mut seen = Vec::new();
    for write in 0..4_u8 {
        // An Admin of their own for each write, demoted right before it.
        let admin = stepped_up(&setup, 3 + write, &format!("admin{write}"), Role::Admin).await;
        setup
            .hooked
            .queue(Meanwhile::SetRole(admin.id, Role::Member));
        seen.push(match write {
            0 => {
                setup
                    .edit_settings(&admin, unknown, edit_body(1, "mc.example.com", &["Two"]))
                    .await
            }
            1 => setup.delete_settings(&admin, unknown).await,
            2 => {
                setup
                    .create_settings(&admin, create_body("mc.example.com", &["One"]))
                    .await
            }
            _ => {
                setup
                    .edit_settings(&admin, other, edit_body(1, "mc.example.com", &["One"]))
                    .await
            }
        });
    }

    for (write, seen) in seen.iter().enumerate() {
        assert_eq!(
            (seen.status, seen.code()),
            (403, "forbidden"),
            "write {write}: {}",
            seen.text
        );
    }
    assert_eq!(setup.settings_entries().await, 0);
}

/// The largest valid request, the longest server and 16 texts of 1024
/// three-byte characters, fits the default 64 KiB body limit: a create's
/// and an edit's.
#[sqlx::test(migrations = false)]
async fn the_largest_valid_request_fits_the_body_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    let server = longest_server(0);
    let create = json!({ "server": server, "conflict_texts": widest_texts(3, 0) });
    let edit = json!({ "version": 1, "server": server, "conflict_texts": widest_texts(3, 16) });

    let created = setup.create_settings(&admin, create.clone()).await;

    assert!(
        create.to_string().len() > 48 * 1024,
        "{}",
        create.to_string().len()
    );
    assert!(
        edit.to_string().len() > 48 * 1024,
        "{}",
        edit.to_string().len()
    );
    assert_eq!(created.status, 201, "{}", created.text);
    let edited = setup
        .edit_settings(&admin, created_id(&created), edit)
        .await;
    assert_eq!(edited.status, 200, "{}", edited.text);
    assert_eq!(edited.body["version"], 2);
}

/// 16 texts of 1024 four-byte characters are valid but don't fit the
/// default 64 KiB body limit: 413, and nothing is written (accepted at PR
/// #79's review; an operator may raise `[http] max_body_bytes`).
#[sqlx::test(migrations = false)]
async fn sixteen_texts_of_four_byte_characters_are_413(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    let body = json!({ "server": "mc.example.com", "conflict_texts": widest_texts(4, 0) });

    let seen = setup.create_settings(&admin, body).await;
    let listed = setup.list_settings(&admin, "").await;

    assert_eq!(
        (seen.status, seen.code()),
        (413, "payload_too_large"),
        "{}",
        seen.text
    );
    assert_eq!(listed.body["items"], json!([]));
    assert_eq!(setup.settings_entries().await, 0);
}

/// An edit's texts are refused at the same paths as a create's: the list's
/// rules at `conflict_texts`, an entry's at `conflict_texts[i]`. Nothing is
/// written.
#[sqlx::test(migrations = false)]
async fn an_edits_broken_texts_are_422_at_their_paths(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    let id = setup.add_settings(1, "mc.example.com").await;
    let before = setup.stored_settings(id).await;
    let seventeen: Vec<String> = (0..17).map(|n| format!("Text {n}")).collect();
    let seventeen: Vec<&str> = seventeen.iter().map(String::as_str).collect();
    let long = "x".repeat(1025);

    for (texts, field, message) in [
        (vec![], "conflict_texts", "must hold at least one text"),
        (seventeen, "conflict_texts", "must hold at most 16 texts"),
        (vec!["One", ""], "conflict_texts[1]", "must not be empty"),
        (
            vec![long.as_str()],
            "conflict_texts[0]",
            "must be at most 1024 characters",
        ),
    ] {
        let seen = setup
            .edit_settings(&admin, id, edit_body(1, "mc.example.com", &texts))
            .await;

        assert_eq!(seen.status, 422, "{field}: {}", seen.text);
        assert_eq!(
            seen.body["error"]["fields"],
            json!([{ "path": field, "message": message }])
        );
    }
    assert_eq!(setup.stored_settings(id).await, before);
    assert_eq!(setup.settings_entries().await, 0);
}
