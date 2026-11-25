//! A step mode converted to a script over HTTP (Plan.md P15.27; ADR-0046),
//! and the stopgap `enter_minecart` the conversion replaced, removed in the
//! release after 0.2.1 (ADR-0034; ADR-0046, C5): a request with it is a body
//! of the wrong shape, as any action this server doesn't know, and a
//! converted chat step keeps its arguments below the right to edit, as its
//! steps did.

use axum::http::Method;
use fleet_core::audit::AuditAction;
use fleet_core::authz::Role;
use fleet_core::id::ModeId;
use fleet_core::mode::ModeDraft;
use fleet_core::script::ScriptDraft;
use fleet_server::ports::store::Store as _;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::{CANARY, MODES, named, planted, swinging};
use crate::support::{Person, Seen, Setup, setup};

impl Setup {
    /// `PUT /modes/{id}` with `body` as `by`.
    async fn edit_mode(&self, by: &Person, id: ModeId, body: Value) -> Seen {
        self.request(
            Method::PUT,
            &format!("{MODES}/{id}"),
            Some(&by.token),
            Some(body),
        )
        .await
    }

    /// How many entries of `action` the audit log holds.
    async fn audited(&self, action: &AuditAction) -> usize {
        self.audit()
            .await
            .iter()
            .filter(|entry| entry.action == *action)
            .count()
    }

    /// How many modes the users own.
    async fn modes_owned(&self) -> i64 {
        sqlx::query_scalar!(
            r#"SELECT count(*) AS "count!: i64" FROM modes WHERE owner_user_id IS NOT NULL"#
        )
        .fetch_one(self.db.read_pool())
        .await
        .unwrap()
    }
}

/// A step mode that jumps at start and gets into the nearest minecart with
/// the removed action.
fn with_enter_minecart() -> Value {
    json!({ "steps": [
        { "action": { "type": "jump" }, "schedule": { "type": "at_start" }, "probability": 100 },
        { "action": { "type": "enter_minecart" }, "schedule": { "type": "at_start" }, "probability": 100 },
    ] })
}

/// A request's body with `name`, `visibility` and the steps of `mode`.
fn body(name: &str, visibility: &str, mode: &Value) -> Value {
    json!({ "name": name, "visibility": visibility, "steps": mode["steps"].clone() })
}

#[sqlx::test(migrations = false)]
async fn a_new_mode_with_the_removed_enter_minecart_is_400_and_nothing_is_stored(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let admin = setup.person(3, "admin", Role::Admin).await;

    for (by, visibility) in [(&member, "private"), (&admin, "shared")] {
        let seen = setup
            .post(
                MODES,
                Some(&by.token),
                Some(body("Back in", visibility, &with_enter_minecart())),
            )
            .await;

        assert_eq!(seen.status, 400, "{}", seen.text);
    }
    assert_eq!(setup.modes_owned().await, 0);
    assert_eq!(setup.audited(&AuditAction::MODE_CREATE).await, 0);
}

#[sqlx::test(migrations = false)]
async fn an_edit_that_brings_in_the_removed_enter_minecart_is_400_and_changes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let id = setup
        .add_mode(1, member.id, "Swing", "private", &swinging())
        .await;
    let mut edit = body("Swing", "private", &with_enter_minecart());
    edit["version"] = json!(1);

    let seen = setup.edit_mode(&member, id, edit).await;

    assert_eq!(seen.status, 400, "{}", seen.text);
    let stored = setup.db.modes().find(id).await.unwrap().unwrap();
    assert_eq!(stored.version.get(), 1);
    assert_eq!(setup.audited(&AuditAction::MODE_UPDATE).await, 0);
}

/// "Convert to script" on a shared step mode whose chat steps hold a
/// password: the script names its messages by key alone, and below the
/// right to edit the mode, each declared command is its `/` and name, as
/// the chat steps were (S15).
#[sqlx::test(migrations = false)]
async fn a_converted_chat_step_keeps_its_arguments_below_the_right_to_edit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let member = setup.person(3, "member", Role::Member).await;
    let id = setup
        .add_mode(1, admin.id, "Logs in", "shared", &planted())
        .await;
    let steps: ModeDraft = serde_json::from_str(&planted()).unwrap();
    let converted = fleet_script::convert(&steps.validate().unwrap()).unwrap();
    let script = ScriptDraft::from(converted.script().clone());
    let request = json!({ "version": 1, "name": "Logs in", "visibility": "shared", "steps": [],
        "script": {
            "source": script.source,
            "leash": script.leash,
            "tool_guard": script.tool_guard,
            "block_guard": script.block_guard,
            "messages": script.messages,
            "patterns": [],
        } });
    assert!(!script.source.contains(CANARY), "{}", script.source);

    // Its commands are off the allowlist, as the steps' were: the twin.
    let saved = setup
        .request(
            Method::PUT,
            &format!("{MODES}/{id}/with-commands"),
            Some(&admin.token),
            Some(request),
        )
        .await;
    let members = setup.get(MODES, Some(&member.token)).await;
    let admins = setup.get(MODES, Some(&admin.token)).await;

    assert_eq!(saved.status, 200, "{}", saved.text);
    assert!(!members.text.contains(CANARY), "{}", members.text);
    let seen = &named(&members.body, "Logs in")["script"]["messages"];
    assert_eq!(
        *seen,
        json!([
            { "key": "join_1", "text": "/login", "values": [], "arguments_hidden": true },
            { "key": "online_1", "text": "/msg", "values": [], "arguments_hidden": true },
        ])
    );
    let whole = &named(&admins.body, "Logs in")["script"]["messages"];
    assert_eq!(whole[0]["text"], format!("/login {CANARY}"));
    assert_eq!(whole[1]["text"], format!("/msg Steve {CANARY}"));
}
