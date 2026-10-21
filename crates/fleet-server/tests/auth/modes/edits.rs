//! Editing a mode over HTTP (Plan.md P11.4; ADR-0020, S16–S18, S21, R3):
//! `PUT /api/v1/modes/{id}` and its step-up twin, with the rights both ways,
//! the pointer, the no-op, the version, names, the bots that use the mode,
//! the re-checks in the transaction, the audit entry and the hint.

use std::collections::BTreeMap;

use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget};
use fleet_core::authz::{GrantLevel, ModeVisibility, Role};
use fleet_core::id::{BotId, ModeId};
use fleet_core::mode::{ModeDefinition, ModeName, ModePreset};
use fleet_server::app::modes::{ModeEdit, ModeRoute, ModeService, UpdateModeError};
use fleet_server::app::users::ActingUser;
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::ports::reconcile::Change;
use fleet_server::ports::store::Store as _;
use fleet_testkit::log_capture;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::{MODES, metadata, stepped_up, swinging, with_spawn};
use crate::support::{
    Meanwhile, Options, Person, Seen, Setup, T0, at, redacted, setup, setup_entry, setup_with,
};

/// A mode that jumps every 250 ms.
fn jumping() -> String {
    json!({ "steps": [
        { "action": { "type": "jump" }, "schedule": { "type": "every", "interval_ms": 250, "jitter_ms": 0 }, "probability": 100 },
    ] })
    .to_string()
}

/// A mode that sends `message` at start.
fn sending(message: &str) -> String {
    json!({ "steps": [
        { "action": { "type": "send_chat", "message": message }, "schedule": { "type": "at_start" }, "probability": 100 },
    ] })
    .to_string()
}

/// The steps of the mode JSON `definition`.
fn steps(definition: &str) -> Value {
    serde_json::from_str::<Value>(definition).unwrap()["steps"].clone()
}

/// An edit's body: the mode named `name` with `visibility` and the steps of
/// `definition`, at `version`.
fn body(version: u32, name: &str, visibility: &str, definition: &str) -> Value {
    json!({ "version": version, "name": name, "visibility": visibility, "steps": steps(definition) })
}

/// The edit route of `id`.
fn path(id: ModeId) -> String {
    format!("{MODES}/{id}")
}

/// Its step-up twin.
fn twin(id: ModeId) -> String {
    format!("{MODES}/{id}/with-commands")
}

/// `seen`'s status, headers and body text, its request ID replaced: what
/// two answers that must not tell anything apart share byte for byte.
fn without_request_id(seen: &Seen) -> (u16, BTreeMap<String, String>, String) {
    let id = seen.body["error"]["request_id"].as_str().unwrap();
    (
        seen.status,
        seen.headers.clone(),
        seen.text.replace(id, "<request id>"),
    )
}

impl Setup {
    /// `PUT /modes/{id}` with `body` as `by`.
    async fn edit(&self, by: &Person, id: ModeId, body: Value) -> Seen {
        self.request(
            axum::http::Method::PUT,
            &path(id),
            Some(&by.token),
            Some(body),
        )
        .await
    }

    /// `PUT /modes/{id}/with-commands` with `body` as `by`.
    async fn edit_with_commands(&self, by: &Person, id: ModeId, body: Value) -> Seen {
        self.request(
            axum::http::Method::PUT,
            &twin(id),
            Some(&by.token),
            Some(body),
        )
        .await
    }

    /// The mode `id` as stored now.
    async fn stored(&self, id: ModeId) -> Option<fleet_server::ports::modes::StoredMode> {
        self.db.modes().find(id).await.unwrap()
    }

    /// The `mode.update` entries.
    async fn updates(&self) -> usize {
        self.audit()
            .await
            .iter()
            .filter(|entry| entry.action == AuditAction::MODE_UPDATE)
            .count()
    }

    /// Switches `bot` to `mode`, as a user's call would.
    async fn run_mode(&self, bot: BotId, mode: ModeId) {
        let mut tx = self.db.write().await.unwrap();
        assert!(tx.bots().set_mode(bot, mode).await.unwrap());
        tx.commit(setup_entry()).await.unwrap();
    }
}

#[sqlx::test(migrations = false)]
async fn editing_a_mode_needs_a_session(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let id = ModePreset::Afk.id();

    for route in [path(id), twin(id)] {
        let seen = setup
            .request(
                axum::http::Method::PUT,
                &route,
                None,
                Some(body(1, "Mine", "private", &swinging())),
            )
            .await;
        assert_eq!(seen.status, 401, "{route}: {}", seen.text);
    }
}

#[sqlx::test(migrations = false)]
async fn editing_spends_the_users_request_limit(
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
    let member = stepped_up(&setup, 2, "member", Role::Member).await;
    let id = setup
        .add_mode(1, member.id, "Mine", "private", &swinging())
        .await;

    for route in [path(id), twin(id)] {
        let unchanged = body(1, "Mine", "private", &swinging());
        let first = setup
            .request(
                axum::http::Method::PUT,
                &route,
                Some(&member.token),
                Some(unchanged.clone()),
            )
            .await;
        let second = setup
            .request(
                axum::http::Method::PUT,
                &route,
                Some(&member.token),
                Some(unchanged),
            )
            .await;
        assert_ne!(first.status, 429, "{route}: {}", first.text);
        assert_eq!(second.status, 429, "{route}");
        assert_eq!(second.headers["retry-after"], "1", "{route}");
        setup.clock.advance(core::time::Duration::from_secs(1));
    }
}

/// The twin needs a fresh step-up, and changes nothing without one; then it
/// takes any steps, a command off the allowlist included (S16).
#[sqlx::test(migrations = false)]
async fn the_twin_needs_a_fresh_step_up_and_then_takes_any_steps(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let id = setup
        .add_mode(1, member.id, "Mine", "private", &swinging())
        .await;
    let ops = body(1, "Mine", "private", &sending("/op AfkBot2"));

    let refused = setup.edit_with_commands(&member, id, ops.clone()).await;
    let before = setup.stored(id).await;
    setup.stamp(member.session).await;
    let taken = setup.edit_with_commands(&member, id, ops).await;

    assert_eq!(
        (refused.status, refused.code()),
        (403, "step_up_required"),
        "{}",
        refused.text
    );
    assert_eq!(before.unwrap().version.get(), 1);
    assert_eq!(taken.status, 200, "{}", taken.text);
    assert_eq!(taken.body["version"], 2);
}

/// A malformed ID, an unknown one and another member's private mode are
/// one 404, nothing told apart.
#[sqlx::test(migrations = false)]
async fn a_malformed_unknown_or_unseen_mode_is_the_same_404(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let other = setup.person(3, "other", Role::Member).await;
    let hidden = setup
        .add_mode(1, other.id, "Theirs", "private", &swinging())
        .await;
    let unknown = ModeId::new_v7(at(T0), [0xEE; 10]).unwrap();
    let edit = body(1, "Mine", "private", &swinging());

    let malformed = setup
        .request(
            axum::http::Method::PUT,
            &format!("{MODES}/not-a-mode"),
            Some(&member.token),
            Some(edit.clone()),
        )
        .await;
    let unknown = setup.edit(&member, unknown, edit.clone()).await;
    let unseen = setup.edit(&member, hidden, edit).await;

    for seen in [&malformed, &unknown, &unseen] {
        assert_eq!(seen.status, 404, "{}", seen.text);
    }
    assert_eq!(without_request_id(&malformed), without_request_id(&unknown));
    assert_eq!(without_request_id(&unknown), without_request_id(&unseen));
    assert_eq!(setup.stored(hidden).await.unwrap().name.as_str(), "Theirs");
}

/// A body that doesn't parse is the bare 400, a response's step sent back
/// among them (R5); a rule it breaks is one 422 at its path, a version
/// below 1 garde's own (R2). Nothing is written.
#[sqlx::test(migrations = false)]
async fn a_body_that_doesnt_parse_is_400_and_a_broken_rule_422(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let id = setup
        .add_mode(1, member.id, "Mine", "private", &swinging())
        .await;
    let mut unknown_field = body(1, "Mine", "private", &swinging());
    unknown_field["owner"] = json!("someone");
    let mut no_version = body(1, "Mine", "private", &swinging());
    no_version.as_object_mut().unwrap().remove("version");
    let mut answered = body(1, "Mine", "private", &swinging());
    answered["steps"][0]["action"]["arguments_hidden"] = json!(false);
    let before = setup.stored(id).await;

    for shape in [unknown_field, no_version, answered] {
        let seen = setup.edit(&member, id, shape).await;
        assert_eq!(seen.status, 400, "{}", seen.text);
    }
    for (edit, field, message) in [
        (
            body(0, "Mine", "private", &swinging()),
            "version",
            "lower than 1",
        ),
        (
            body(1, "", "private", &swinging()),
            "name",
            "must not be empty",
        ),
        (
            body(1, "AFK", "private", &swinging()),
            "name",
            "is a built-in mode's name",
        ),
    ] {
        let seen = setup.edit(&member, id, edit).await;
        assert_eq!(seen.status, 422, "{field}: {}", seen.text);
        assert_eq!(
            seen.body["error"]["fields"],
            json!([{ "path": field, "message": message }])
        );
    }
    assert_eq!(setup.stored(id).await, before);
    assert_eq!(setup.updates().await, 0);
}

/// Every built-in mode is read-only for everyone, the Owner included: 403.
#[sqlx::test(migrations = false)]
async fn a_built_in_mode_is_403_for_everyone(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let people = [
        stepped_up(&setup, 2, "theowner", Role::Owner).await,
        stepped_up(&setup, 3, "admin", Role::Admin).await,
        stepped_up(&setup, 4, "member", Role::Member).await,
    ];

    for preset in ModePreset::ALL {
        let id = preset.id();
        let before = setup.stored(id).await;
        for person in &people {
            for (n, seen) in [
                setup
                    .edit(person, id, body(1, "My mode", "shared", &swinging()))
                    .await,
                setup
                    .edit_with_commands(person, id, body(1, "My mode", "shared", &swinging()))
                    .await,
            ]
            .into_iter()
            .enumerate()
            {
                assert_eq!(
                    (seen.status, seen.code()),
                    (403, "forbidden"),
                    "{preset} {n}: {}",
                    seen.text
                );
            }
        }
        assert_eq!(setup.stored(id).await, before, "{preset}");
    }
}

/// The edit needs the right to edit the mode both as it is and as it will
/// be: an Admin can't share a Member's private mode, and a Member can't
/// share theirs (ADR-0010).
#[sqlx::test(migrations = false)]
async fn editing_needs_the_right_both_as_the_mode_is_and_as_it_will_be(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "theowner", Role::Owner).await;
    let admin = setup.person(3, "admin", Role::Admin).await;
    let other_admin = setup.person(4, "admin2", Role::Admin).await;
    let member = setup.person(5, "member", Role::Member).await;
    let cases = [
        (&member, other_admin.id, "shared", "shared", 403),
        (&admin, other_admin.id, "shared", "shared", 403),
        (&admin, member.id, "private", "private", 200),
        (&admin, member.id, "private", "shared", 403),
        (&member, member.id, "private", "shared", 403),
        (&admin, admin.id, "shared", "private", 200),
        (&owner, member.id, "private", "shared", 200),
        (&owner, other_admin.id, "shared", "private", 200),
    ];

    for (n, (by, mode_owner, before, after, status)) in (1_u8..).zip(cases) {
        let name = format!("Mode {n}");
        let id = setup
            .add_mode(n, mode_owner, &name, before, &swinging())
            .await;
        let seen = setup.edit(by, id, body(1, &name, after, &jumping())).await;
        assert_eq!(seen.status, status, "case {n}: {}", seen.text);
        let version = setup.stored(id).await.unwrap().version.get();
        assert_eq!(version, if status == 200 { 2 } else { 1 }, "case {n}");
    }
}

/// The transaction asks for the right after the change too: an Admin
/// demoted to Member right before it, sharing their own private mode, may
/// still edit it as it is but not as it would be, so it's 403 and nothing
/// is written (PR #64's review).
#[sqlx::test(migrations = false)]
async fn an_admin_demoted_right_before_sharing_their_private_mode_is_403(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let id = setup
        .add_mode(1, admin.id, "Mine", "private", &swinging())
        .await;
    let before = setup.stored(id).await;
    setup
        .hooked
        .queue(Meanwhile::SetRole(admin.id, Role::Member));

    let seen = setup
        .edit(&admin, id, body(1, "Mine", "shared", &swinging()))
        .await;

    assert_eq!(
        (seen.status, seen.code()),
        (403, "forbidden"),
        "{}",
        seen.text
    );
    assert_eq!(setup.stored(id).await, before);
    assert_eq!(setup.updates().await, 0);
}

/// The plain route points any command off the allowlist to the twin,
/// whether it's new, changed or kept, and only after `forbidden`; one on
/// the allowlist passes (S16).
#[sqlx::test(migrations = false)]
async fn the_plain_route_points_any_unlisted_command_to_the_twin_after_forbidden(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = with_spawn(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let admin = setup.person(3, "admin", Role::Admin).await;
    let mine = setup
        .add_mode(1, member.id, "Ops", "private", &sending("/op AfkBot2"))
        .await;
    let theirs = setup
        .add_mode(2, admin.id, "Shared", "shared", &swinging())
        .await;

    for (case, definition) in [
        ("kept", sending("/op AfkBot2")),
        ("changed", sending("/op AfkBot3")),
        ("new", sending("/kill @e")),
    ] {
        let seen = setup
            .edit(&member, mine, body(1, "Ops", "private", &definition))
            .await;
        assert_eq!(
            (seen.status, seen.code()),
            (403, "mode_needs_step_up"),
            "{case}: {}",
            seen.text
        );
    }
    let forbidden = setup
        .edit(
            &member,
            theirs,
            body(1, "Shared", "shared", &sending("/op x")),
        )
        .await;
    let listed = setup
        .edit(
            &member,
            mine,
            body(1, "Ops", "private", &sending("/spawn home")),
        )
        .await;

    assert_eq!(
        (forbidden.status, forbidden.code()),
        (403, "forbidden"),
        "{}",
        forbidden.text
    );
    assert_eq!(listed.status, 200, "{}", listed.text);
    assert_eq!(listed.body["version"], 2);
}

/// A mode answered and sent back untouched is no change, whatever
/// `version` comes with it: a stored zero-width space, which the answer
/// leaves out, doesn't make it one (S21). 200 with the mode as it is: the
/// same version and time, no entry, no hint, the row untouched.
#[sqlx::test(migrations = false)]
async fn an_untouched_save_is_200_and_changes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let id = setup
        .add_mode(
            1,
            member.id,
            "Greeter",
            "private",
            &sending("he\u{200B}llo there"),
        )
        .await;
    let before = setup.stored(id).await;
    let listed = setup.modes(&member, "").await;
    let answer = listed.body["items"][0].clone();
    assert_eq!(answer["steps"][0]["action"]["message"], "hello there");
    let mut sent_back = answer["steps"].clone();
    for step in sent_back.as_array_mut().unwrap() {
        step["action"]
            .as_object_mut()
            .unwrap()
            .remove("arguments_hidden");
    }
    setup.hints.take();

    for version in [1, 7] {
        let seen = setup
            .edit(
                &member,
                id,
                json!({ "version": version, "name": answer["name"], "visibility": answer["visibility"], "steps": sent_back }),
            )
            .await;
        assert_eq!(seen.status, 200, "{version}: {}", seen.text);
        assert_eq!(seen.body, answer, "{version}");
    }
    assert_eq!(setup.stored(id).await, before);
    assert_eq!(setup.updates().await, 0);
    assert_eq!(setup.hints.take(), []);
}

/// The no-op comes before S17's check (R3): saving untouched a mode that a
/// bot the editor can't set it on uses is 200 with nothing written, while
/// a real edit is 409 (PR #64's review).
#[sqlx::test(migrations = false)]
async fn an_untouched_save_is_200_even_when_a_bot_the_editor_cant_set_it_on_uses_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let other = setup.person(3, "admin2", Role::Admin).await;
    let id = setup
        .add_mode(
            1,
            admin.id,
            "Greeter",
            "shared",
            &sending("he\u{200B}llo there"),
        )
        .await;
    let (_, hidden) = setup.bot_of(other.id, 9).await;
    setup.run_mode(hidden, id).await;
    let listed = setup.modes(&admin, "").await;
    let answer = super::named(&listed.body, "Greeter").clone();
    let mut sent_back = answer["steps"].clone();
    for step in sent_back.as_array_mut().unwrap() {
        step["action"]
            .as_object_mut()
            .unwrap()
            .remove("arguments_hidden");
    }
    let before = setup.stored(id).await;
    setup.hints.take();

    let untouched = setup
        .edit(
            &admin,
            id,
            json!({ "version": 1, "name": "Greeter", "visibility": "shared", "steps": sent_back }),
        )
        .await;
    let changed = setup
        .edit(&admin, id, body(1, "Greeter", "shared", &sending("hello")))
        .await;

    assert_eq!(untouched.status, 200, "{}", untouched.text);
    assert_eq!(untouched.body, answer);
    assert_eq!(
        (changed.status, changed.code()),
        (409, "mode_in_use_by_others"),
        "{}",
        changed.text
    );
    assert_eq!(setup.stored(id).await, before);
    assert_eq!(setup.updates().await, 0);
    assert_eq!(setup.hints.take(), []);
}

/// A stale `version` is 409 `mode_changed`, and nothing is written or
/// recorded (S18).
#[sqlx::test(migrations = false)]
async fn a_stale_version_is_409_mode_changed_and_writes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let id = setup
        .add_mode(1, member.id, "Mine", "private", &swinging())
        .await;
    let edited = setup
        .edit(&member, id, body(1, "Mine", "private", &jumping()))
        .await;
    assert_eq!(edited.status, 200, "{}", edited.text);
    let before = setup.stored(id).await;

    for version in [1, 3] {
        let seen = setup
            .edit(
                &member,
                id,
                body(version, "Renamed", "private", &swinging()),
            )
            .await;
        assert_eq!(
            (seen.status, seen.code()),
            (409, "mode_changed"),
            "{version}: {}",
            seen.text
        );
        assert_eq!(
            seen.body["error"]["message"],
            "This mode changed since you opened it. Load it again to see the change."
        );
    }
    assert_eq!(setup.stored(id).await, before);
    assert_eq!(setup.updates().await, 1);
}

/// R3's order in the transaction: a stale `version` is `mode_changed`
/// before a taken name is looked at, and a taken name is 422 before the
/// bots that use the mode are (PR #64's review).
#[sqlx::test(migrations = false)]
async fn in_the_transaction_the_version_comes_before_the_name_and_the_name_before_the_bots(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let other = setup.person(3, "admin2", Role::Admin).await;
    let edited = setup
        .add_mode(1, admin.id, "Edited", "private", &swinging())
        .await;
    let used = setup
        .add_mode(2, admin.id, "Used", "shared", &swinging())
        .await;
    setup
        .add_mode(3, admin.id, "Taken", "private", &swinging())
        .await;
    let (_, hidden) = setup.bot_of(other.id, 9).await;
    setup.run_mode(hidden, used).await;
    let first = setup
        .edit(&admin, edited, body(1, "Edited", "private", &jumping()))
        .await;
    assert_eq!(first.status, 200, "{}", first.text);

    let stale_and_taken = setup
        .edit(&admin, edited, body(1, "Taken", "private", &swinging()))
        .await;
    let taken_and_blocked = setup
        .edit(&admin, used, body(1, "Taken", "shared", &jumping()))
        .await;

    assert_eq!(
        (stale_and_taken.status, stale_and_taken.code()),
        (409, "mode_changed"),
        "{}",
        stale_and_taken.text
    );
    assert_eq!(taken_and_blocked.status, 422, "{}", taken_and_blocked.text);
    assert_eq!(
        taken_and_blocked.body["error"]["fields"],
        json!([{ "path": "name", "message": "is already another mode's name" }])
    );
    assert_eq!(setup.updates().await, 1);
}

/// R1's canary for edits: an edit's answer shows its editor the mode as
/// `GET /modes` does, by the same rules: an Admin editing a Member's private
/// mode and the Owner editing an Admin's shared one get its owner and the
/// planted command whole (S14, S15; PR #64's review).
#[sqlx::test(migrations = false)]
async fn an_edit_answers_its_editor_as_the_list_shows_them(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let the_owner = stepped_up(&setup, 2, "theowner", Role::Owner).await;
    let admin = stepped_up(&setup, 3, "admin", Role::Admin).await;
    let member = setup.person(4, "member", Role::Member).await;
    let members = setup
        .add_mode(1, member.id, "Member farm", "private", &super::planted())
        .await;
    let admins = setup
        .add_mode(2, admin.id, "Admin farm", "shared", &super::planted())
        .await;

    for (editor, id, name, visibility, owner) in [
        (&admin, members, "Member farm 2", "private", "member"),
        (&the_owner, admins, "Admin farm 2", "shared", "admin"),
    ] {
        let seen = setup
            .edit_with_commands(editor, id, body(1, name, visibility, &super::planted()))
            .await;
        assert_eq!(seen.status, 200, "{name}: {}", seen.text);
        assert_eq!(seen.body["owner"]["username"], owner, "{name}");
        assert_eq!(
            seen.text.matches(super::CANARY).count(),
            2,
            "{name}: {}",
            seen.text
        );
        let listed = setup.modes(editor, "").await;
        assert_eq!(super::named(&listed.body, name), &seen.body, "{name}");
    }
}

/// A real edit answers the mode at the next version, as its editor sees it
/// and as the list shows it from then on; it's recorded and the reconciler
/// is told.
#[sqlx::test(migrations = false)]
async fn an_edit_is_200_at_the_next_version_recorded_and_hinted(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let id = setup
        .add_mode(1, member.id, "Mine", "private", &swinging())
        .await;
    setup.hints.take();

    let seen = setup
        .edit(
            &member,
            id,
            body(1, " Grüße, „Farm“ #2! ", "private", &jumping()),
        )
        .await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    insta::assert_json_snapshot!(
        "mode_updated",
        redacted(&seen.body, &[(id.to_string(), "the mode")])
    );
    let listed = setup.modes(&member, "").await;
    assert_eq!(listed.body["items"][0], seen.body, "listed as answered");
    assert_eq!(setup.hints.take(), [Change::Store]);
    let entry = setup.last(&AuditAction::MODE_UPDATE).await;
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(member.id));
    assert!(entry.ip.is_some());
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Mode(id)))
    );
}

/// `mode.update` records the version written, whether the steps changed
/// byte for byte, the visibility (and the one before, when it changed), the
/// commands, the route and how many bots were checked; never the name or a
/// step's text (S21; ADR-0016).
#[sqlx::test(migrations = false)]
async fn an_edit_is_recorded_without_the_name_or_a_steps_text(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    log_capture::install().unwrap();
    let setup = with_spawn(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    let id = setup
        .add_mode(1, admin.id, "Quiet hunter2", "private", &swinging())
        .await;
    let (_, bot) = setup.bot_of(admin.id, 7).await;
    setup.run_mode(bot, id).await;
    let entry = |version: &str, changed: &str, visibility: &str, commands: &str, route: &str| {
        BTreeMap::from([
            ("version".to_owned(), version.to_owned()),
            ("definition_changed".to_owned(), changed.to_owned()),
            ("visibility".to_owned(), visibility.to_owned()),
            ("commands".to_owned(), commands.to_owned()),
            ("route".to_owned(), route.to_owned()),
            ("bots_checked".to_owned(), "1".to_owned()),
        ])
    };
    let mut shared = entry("2", "true", "shared", "allowlisted", "plain");
    shared.insert("previous_visibility".to_owned(), "private".to_owned());
    let cases = [
        (
            path(id),
            body(
                1,
                "Spawn hunter2",
                "shared",
                &sending("/spawn hunter2-secret"),
            ),
            shared,
        ),
        (
            path(id),
            body(
                2,
                "Renamed hunter2",
                "shared",
                &sending("/spawn hunter2-secret"),
            ),
            entry("3", "false", "shared", "allowlisted", "plain"),
        ),
        (
            twin(id),
            body(3, "Ops hunter2", "shared", &sending("/op hunter2-secret")),
            entry("4", "true", "shared", "unlisted", "with_commands"),
        ),
    ];

    for (route, edit, expected) in cases {
        let seen = setup
            .request(
                axum::http::Method::PUT,
                &route,
                Some(&admin.token),
                Some(edit),
            )
            .await;
        assert_eq!(seen.status, 200, "{route}: {}", seen.text);
        let recorded = setup.last(&AuditAction::MODE_UPDATE).await;
        assert_eq!(metadata(&recorded), expected, "{route}");
        assert!(!format!("{recorded:?}").contains("hunter2"), "{recorded:?}");
    }
    log_capture::check_absent("hunter2").unwrap();
}

/// `definition_changed` compares the steps as stored, byte for byte (S21):
/// a rename whose steps differ only by `-0.0` against `0.0`, which read as
/// the same angle, changed them (PR #64's review).
#[sqlx::test(migrations = false)]
async fn definition_changed_compares_the_stored_steps_byte_for_byte(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let looking = |yaw: f64| {
        json!({ "steps": [
            { "action": { "type": "look", "yaw": yaw, "pitch": 0.0 }, "schedule": { "type": "at_start" }, "probability": 100 },
        ] })
        .to_string()
    };
    let id = setup
        .add_mode(1, member.id, "Looker", "private", &looking(-0.0))
        .await;

    let seen = setup
        .edit(&member, id, body(1, "Renamed", "private", &looking(0.0)))
        .await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    let recorded = setup.last(&AuditAction::MODE_UPDATE).await;
    assert_eq!(metadata(&recorded)["definition_changed"], "true");
}

/// A rename is checked against every name but the mode's own: a case
/// change of its own name passes, another of the owner's names is 422, a
/// shared mode's name is 422 for a mode made shared (never a 500) and free
/// for a private one (S13).
#[sqlx::test(migrations = false)]
async fn a_rename_is_checked_against_every_name_but_its_own(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let other = setup.person(3, "admin2", Role::Admin).await;
    let fishing = setup
        .add_mode(1, admin.id, "Fishing", "private", &swinging())
        .await;
    let mining = setup
        .add_mode(2, admin.id, "Mining", "private", &swinging())
        .await;
    let farming = setup
        .add_mode(3, admin.id, "Farming", "private", &swinging())
        .await;
    setup
        .add_mode(4, other.id, "Farming", "shared", &swinging())
        .await;
    setup
        .add_mode(5, other.id, "Herding", "shared", &swinging())
        .await;

    for (case, id, name, visibility, status) in [
        ("its own in capitals", fishing, "FISHING", "private", 200),
        ("another of the owner's", mining, "fishing", "private", 422),
        (
            "a shared one's, made shared",
            farming,
            "Farming",
            "shared",
            422,
        ),
        (
            "a shared one's, staying private",
            mining,
            "Herding",
            "private",
            200,
        ),
    ] {
        let seen = setup
            .edit(&admin, id, body(1, name, visibility, &swinging()))
            .await;
        assert_eq!(seen.status, status, "{case}: {}", seen.text);
        if status == 422 {
            assert_eq!(
                seen.body["error"]["fields"],
                json!([{ "path": "name", "message": "is already another mode's name" }]),
                "{case}"
            );
        }
    }
    assert_eq!(
        setup.stored(fishing).await.unwrap().name,
        ModeName::try_from("FISHING").unwrap()
    );
}

/// An edit goes through only if its editor could set the edited mode on
/// every bot that uses it (S17). A bot they can't see and one they see
/// without the level are one 409, byte for byte, naming none; Control
/// passes steps on the allowlist, and a command off it needs Manage.
#[sqlx::test(migrations = false)]
async fn an_edit_needs_set_bot_mode_on_every_bot_that_uses_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = with_spawn(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    let other = setup.person(3, "admin2", Role::Admin).await;
    let member = setup.person(4, "member", Role::Member).await;
    let id = setup
        .add_mode(1, admin.id, "Shared farm", "shared", &swinging())
        .await;
    let (_, own) = setup.bot_of(admin.id, 1).await;
    let (_, members) = setup.bot_of(member.id, 2).await;
    let (_, hidden) = setup.bot_of(other.id, 3).await;
    let (viewed_account, viewed) = setup.bot_of(other.id, 4).await;
    let (controlled_account, controlled) = setup.bot_of(other.id, 5).await;
    setup
        .share(viewed_account, admin.id, GrantLevel::View, other.id)
        .await;
    setup
        .share(controlled_account, admin.id, GrantLevel::Control, other.id)
        .await;
    for bot in [own, members, hidden] {
        setup.run_mode(bot, id).await;
    }

    let refused_hidden = setup
        .edit(&admin, id, body(1, "Shared farm", "shared", &jumping()))
        .await;
    setup.run_mode(hidden, ModePreset::Afk.id()).await;
    setup.run_mode(viewed, id).await;
    let refused_viewed = setup
        .edit(&admin, id, body(1, "Shared farm", "shared", &jumping()))
        .await;
    setup.run_mode(viewed, ModePreset::Afk.id()).await;
    setup.run_mode(controlled, id).await;
    let unlisted = setup
        .edit_with_commands(
            &admin,
            id,
            body(1, "Shared farm", "shared", &sending("/op x")),
        )
        .await;
    let listed = setup
        .edit(
            &admin,
            id,
            body(1, "Shared farm", "shared", &sending("/spawn")),
        )
        .await;

    assert_eq!(
        (refused_hidden.status, refused_hidden.code()),
        (409, "mode_in_use_by_others"),
        "{}",
        refused_hidden.text
    );
    assert_eq!(
        without_request_id(&refused_hidden),
        without_request_id(&refused_viewed)
    );
    assert_eq!(
        unlisted.code(),
        "mode_in_use_by_others",
        "{}",
        unlisted.text
    );
    assert_eq!(listed.status, 200, "{}", listed.text);
    let recorded = setup.last(&AuditAction::MODE_UPDATE).await;
    assert_eq!(metadata(&recorded)["bots_checked"], "3");
    assert_eq!(setup.updates().await, 1);
}

/// The bots, the mode and the acting user are read again in the edit's
/// transaction: a bot switched to the mode, an edit or a delete, a lowered
/// role or a revoked session that lands right before it is seen there, and
/// nothing is written.
#[sqlx::test(migrations = false)]
async fn a_change_right_before_the_transaction_is_seen_in_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let other = setup.person(3, "admin2", Role::Admin).await;
    let (_, hidden) = setup.bot_of(other.id, 9).await;

    for (n, status, code) in [
        (1_u8, 409, "mode_in_use_by_others"),
        (2, 409, "mode_changed"),
        (3, 404, "not_found"),
        (4, 403, "forbidden"),
        (5, 401, "unauthorized"),
    ] {
        let name = format!("Shared {n}");
        let id = setup
            .add_mode(n, admin.id, &name, "shared", &swinging())
            .await;
        let meanwhile = match n {
            1 => Meanwhile::SetBotMode(hidden, id),
            2 => Meanwhile::EditMode(id),
            3 => Meanwhile::DeleteMode(id),
            4 => Meanwhile::SetRole(admin.id, Role::Member),
            _ => Meanwhile::RevokeSession(admin.session),
        };
        setup.hooked.queue(meanwhile);
        let before = setup.stored(id).await;
        let seen = setup
            .edit(&admin, id, body(1, &name, "shared", &jumping()))
            .await;
        assert_eq!(
            (seen.status, seen.code()),
            (status, code),
            "case {n}: {}",
            seen.text
        );
        let after = setup.stored(id).await;
        if n == 2 {
            assert_eq!(after.unwrap().version.get(), 2, "only the other edit");
        } else if n != 3 {
            assert_eq!(after, before, "case {n}");
        }
        setup.set_role(admin.id, Role::Admin).await;
    }
    assert_eq!(setup.updates().await, 0);
}

/// A mode at the last version a `u32` holds has no next one: the internal
/// error, and nothing is written (S18).
#[sqlx::test(migrations = false)]
async fn a_mode_at_its_last_version_is_the_internal_error_and_writes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let id = setup
        .add_mode(1, member.id, "Mine", "private", &swinging())
        .await;
    let text = id.to_string();
    sqlx::query!("UPDATE modes SET version = 4294967295 WHERE id = ?", text)
        .execute(setup.db.write_pool())
        .await
        .unwrap();
    let before = setup.stored(id).await;

    let seen = setup
        .edit(&member, id, body(u32::MAX, "Mine", "private", &jumping()))
        .await;

    assert_eq!(
        (seen.status, seen.code()),
        (500, "internal"),
        "{}",
        seen.text
    );
    assert_eq!(setup.stored(id).await, before);
    assert_eq!(setup.updates().await, 0);
}

/// A guarded write that matches no row after the transaction's own re-read
/// matched its version is a bug: the internal error, never a 200, and
/// nothing is committed or hinted (the user's note at C2's plan review).
#[sqlx::test(migrations = false)]
async fn an_update_that_matches_no_row_is_the_internal_error_never_a_success(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let id = setup
        .add_mode(1, member.id, "Mine", "private", &swinging())
        .await;
    let before = setup.stored(id).await;
    setup.hints.take();
    setup.hooked.lose_next_mode_write();

    let seen = setup
        .edit(&member, id, body(1, "Renamed", "private", &jumping()))
        .await;

    assert_eq!(
        (seen.status, seen.code()),
        (500, "internal"),
        "{}",
        seen.text
    );
    assert_eq!(setup.stored(id).await, before);
    assert_eq!(setup.updates().await, 0);
    assert_eq!(setup.hints.take(), []);
}

/// The service enforces the route rule itself, before anything is read: an
/// edit with a command off the allowlist through the plain route is
/// `WrongRoute`, even for a session and a mode that don't exist (S16).
#[sqlx::test(migrations = false)]
async fn an_edit_its_route_doesnt_take_is_refused_before_anything_is_read(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let service = ModeService::new(
        std::sync::Arc::new(setup.clock.clone()),
        std::sync::Arc::new(fleet_testkit::system::SeededRandom::new(5)),
        std::sync::Arc::new(setup.db.clone()),
    );
    let nobody = ActingUser {
        actor: fleet_core::authz::Actor {
            id: fleet_core::id::UserId::new_v7(at(T0), [0xAB; 10]).unwrap(),
            role: Role::Owner,
        },
        session_id: fleet_core::id::SessionId::new_v7(at(T0), [0xAC; 10]).unwrap(),
        ip: "192.0.2.10".parse().unwrap(),
    };
    let definition: ModeDefinition = serde_json::from_str(&sending("/op AfkBot2")).unwrap();
    let edit = ModeEdit {
        version: core::num::NonZeroU32::MIN,
        name: ModeName::try_from("Ops").unwrap(),
        visibility: ModeVisibility::Private,
        definition,
    };

    let result = service
        .update(
            &nobody,
            ModeRoute::Plain,
            ModeId::new_v7(at(T0), [0xAD; 10]).unwrap(),
            edit,
        )
        .await;

    assert!(
        matches!(
            result,
            Err(UpdateModeError::WrongRoute {
                route: ModeRoute::Plain
            })
        ),
        "{result:?}"
    );
}
