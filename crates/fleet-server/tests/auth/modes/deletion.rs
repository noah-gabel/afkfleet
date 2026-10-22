//! Deleting a mode over HTTP (Plan.md P11.4; ADR-0020, S16, S17):
//! `DELETE /api/v1/modes/{id}`, a step-up route, with the right to delete,
//! the bots that use the mode, the re-checks in the transaction and the
//! audit entry.

use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget};
use fleet_core::authz::Role;
use fleet_core::id::ModeId;
use fleet_core::mode::ModePreset;
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::ports::store::Store as _;
use fleet_testkit::log_capture;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::{MODES, metadata, stepped_up, swinging, swinging_body};
use crate::support::{Meanwhile, Options, Person, Seen, Setup, T0, at, setup, setup_with};

/// The mode `id`'s route.
fn path(id: ModeId) -> String {
    format!("{MODES}/{id}")
}

impl Setup {
    /// `DELETE /modes/{id}` as `by`.
    async fn delete_mode(&self, by: &Person, id: ModeId) -> Seen {
        self.delete(&path(id), Some(&by.token)).await
    }

    /// Whether the mode `id` is still stored.
    async fn kept(&self, id: ModeId) -> bool {
        self.db.modes().find(id).await.unwrap().is_some()
    }

    /// The `mode.delete` entries.
    async fn deletions(&self) -> usize {
        self.audit()
            .await
            .iter()
            .filter(|entry| entry.action == AuditAction::MODE_DELETE)
            .count()
    }

    /// Switches `bot` to `mode`, as a user's call would.
    async fn runs(&self, bot: fleet_core::id::BotId, mode: ModeId) {
        let mut tx = self.db.write().await.unwrap();
        assert!(tx.bots().set_mode(bot, mode).await.unwrap());
        tx.commit(crate::support::setup_entry()).await.unwrap();
    }
}

/// `seen`'s status, headers and body text, its request ID replaced.
fn without_request_id(seen: &Seen) -> (u16, std::collections::BTreeMap<String, String>, String) {
    let id = seen.body["error"]["request_id"].as_str().unwrap();
    (
        seen.status,
        seen.headers.clone(),
        seen.text.replace(id, "<request id>"),
    )
}

#[sqlx::test(migrations = false)]
async fn deleting_a_mode_needs_a_session(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;

    let seen = setup.delete(&path(ModePreset::Afk.id()), None).await;

    assert_eq!(seen.status, 401, "{}", seen.text);
}

#[sqlx::test(migrations = false)]
async fn deleting_spends_the_users_request_limit(
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
    let first = setup
        .add_mode(1, member.id, "One", "private", &swinging())
        .await;
    let second = setup
        .add_mode(2, member.id, "Two", "private", &swinging())
        .await;

    let deleted = setup.delete_mode(&member, first).await;
    let limited = setup.delete_mode(&member, second).await;

    assert_eq!(deleted.status, 204, "{}", deleted.text);
    assert_eq!(limited.status, 429, "{}", limited.text);
    assert_eq!(limited.headers["retry-after"], "1");
    assert!(setup.kept(second).await);
}

/// Deleting needs the password entered again: without a fresh step-up it's
/// 403 and deletes nothing (S16).
#[sqlx::test(migrations = false)]
async fn deleting_needs_a_fresh_step_up_and_deletes_nothing_without_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let id = setup
        .add_mode(1, member.id, "Mine", "private", &swinging())
        .await;

    let refused = setup.delete_mode(&member, id).await;
    let kept = setup.kept(id).await;
    setup.stamp(member.session).await;
    let deleted = setup.delete_mode(&member, id).await;

    assert_eq!(
        (refused.status, refused.code()),
        (403, "step_up_required"),
        "{}",
        refused.text
    );
    assert!(kept);
    assert_eq!(deleted.status, 204, "{}", deleted.text);
    assert!(!setup.kept(id).await);
}

/// A malformed ID, an unknown one and another member's private mode are
/// one 404, nothing told apart; nothing is deleted.
#[sqlx::test(migrations = false)]
async fn a_malformed_unknown_or_unseen_mode_is_the_same_404(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = stepped_up(&setup, 2, "member", Role::Member).await;
    let other = setup.person(3, "other", Role::Member).await;
    let hidden = setup
        .add_mode(1, other.id, "Theirs", "private", &swinging())
        .await;

    let malformed = setup
        .delete(&format!("{MODES}/not-a-mode"), Some(&member.token))
        .await;
    let unknown = setup
        .delete_mode(&member, ModeId::new_v7(at(T0), [0xEE; 10]).unwrap())
        .await;
    let unseen = setup.delete_mode(&member, hidden).await;

    for seen in [&malformed, &unknown, &unseen] {
        assert_eq!(seen.status, 404, "{}", seen.text);
    }
    assert_eq!(without_request_id(&malformed), without_request_id(&unknown));
    assert_eq!(without_request_id(&unknown), without_request_id(&unseen));
    assert!(setup.kept(hidden).await);
}

/// A built-in mode is never deleted, by anyone: 403.
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

    for person in &people {
        for preset in ModePreset::ALL {
            let seen = setup.delete_mode(person, preset.id()).await;
            assert_eq!(
                (seen.status, seen.code()),
                (403, "forbidden"),
                "{preset}: {}",
                seen.text
            );
        }
    }
    for preset in ModePreset::ALL {
        assert!(setup.kept(preset.id()).await, "{preset}");
    }
}

/// Deleting needs the right to delete the mode: a shared mode is its
/// creator's while they're an Admin, and the Owner's; a private one follows
/// the account rule (ADR-0010).
#[sqlx::test(migrations = false)]
async fn deleting_needs_the_right_to_delete_the_mode(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = stepped_up(&setup, 2, "theowner", Role::Owner).await;
    let admin = stepped_up(&setup, 3, "admin", Role::Admin).await;
    let other_admin = setup.person(4, "admin2", Role::Admin).await;
    let member = stepped_up(&setup, 5, "member", Role::Member).await;
    let cases = [
        (&member, other_admin.id, "shared", 403),
        (&admin, other_admin.id, "shared", 403),
        (&admin, member.id, "private", 204),
        (&owner, other_admin.id, "shared", 204),
        (&owner, admin.id, "private", 204),
    ];

    for (n, (by, mode_owner, visibility, status)) in (1_u8..).zip(cases) {
        let id = setup
            .add_mode(n, mode_owner, &format!("Mode {n}"), visibility, &swinging())
            .await;
        let seen = setup.delete_mode(by, id).await;
        assert_eq!(seen.status, status, "case {n}: {}", seen.text);
        assert_eq!(setup.kept(id).await, status != 204, "case {n}");
    }
}

/// A mode a bot uses isn't deleted, whoever's bot it is: the deleter's own
/// and one they can't see are one 409, byte for byte (S17).
#[sqlx::test(migrations = false)]
async fn a_mode_a_bot_uses_is_409_whoever_runs_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    let other = setup.person(3, "admin2", Role::Admin).await;
    let id = setup
        .add_mode(1, admin.id, "Shared farm", "shared", &swinging())
        .await;
    let (_, own) = setup.bot_of(admin.id, 1).await;
    let (_, hidden) = setup.bot_of(other.id, 2).await;

    setup.runs(own, id).await;
    let mine = setup.delete_mode(&admin, id).await;
    setup.runs(own, ModePreset::Afk.id()).await;
    setup.runs(hidden, id).await;
    let theirs = setup.delete_mode(&admin, id).await;

    assert_eq!(
        (mine.status, mine.code()),
        (409, "mode_in_use"),
        "{}",
        mine.text
    );
    assert_eq!(
        mine.body["error"]["message"],
        "A bot uses this mode, so it can't be deleted. Switch every bot to another mode first."
    );
    assert_eq!(without_request_id(&mine), without_request_id(&theirs));
    assert!(setup.kept(id).await);
    assert_eq!(setup.deletions().await, 0);
}

/// A bot switched to the mode, a lowered role, the mode deleted or a
/// revoked session right before the delete's transaction is seen in it, and
/// nothing is deleted by it.
#[sqlx::test(migrations = false)]
async fn a_change_right_before_the_transaction_is_seen_in_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    let other = setup.person(3, "admin2", Role::Admin).await;
    let (_, hidden) = setup.bot_of(other.id, 9).await;

    for (n, status, code) in [
        (1_u8, 409, "mode_in_use"),
        (2, 403, "forbidden"),
        (3, 404, "not_found"),
        (4, 401, "unauthorized"),
    ] {
        let id = setup
            .add_mode(n, admin.id, &format!("Shared {n}"), "shared", &swinging())
            .await;
        let meanwhile = match n {
            1 => Meanwhile::SetBotMode(hidden, id),
            2 => Meanwhile::SetRole(admin.id, Role::Member),
            3 => Meanwhile::DeleteMode(id),
            _ => Meanwhile::RevokeSession(admin.session),
        };
        setup.hooked.queue(meanwhile);
        let seen = setup.delete_mode(&admin, id).await;
        assert_eq!(
            (seen.status, seen.code()),
            (status, code),
            "case {n}: {}",
            seen.text
        );
        assert_eq!(setup.kept(id).await, n != 3, "case {n}");
        setup.set_role(admin.id, Role::Admin).await;
    }
    assert_eq!(setup.deletions().await, 0);
}

/// A delete answers 204 without a body, records `mode.delete` with the
/// version and the visibility, never the name; the mode leaves the list,
/// its name is free again, and the reconciler isn't told: no bot used it.
#[sqlx::test(migrations = false)]
async fn a_delete_is_204_recorded_without_the_name_and_frees_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    log_capture::install().unwrap();
    let setup = setup(options).await;
    let member = stepped_up(&setup, 2, "member", Role::Member).await;
    let id = setup
        .add_mode(1, member.id, "Fishing hunter2", "private", &swinging())
        .await;
    let renamed = setup
        .request(
            axum::http::Method::PUT,
            &path(id),
            Some(&member.token),
            Some(json!({ "version": 1, "name": "Fishing hunter2", "visibility": "private", "steps": [] })),
        )
        .await;
    assert_eq!(renamed.status, 200, "{}", renamed.text);
    setup.hints.take();

    let seen = setup.delete_mode(&member, id).await;

    assert_eq!(seen.status, 204, "{}", seen.text);
    assert_eq!(seen.text, "");
    let entry = setup.last(&AuditAction::MODE_DELETE).await;
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(member.id));
    assert!(entry.ip.is_some());
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Mode(id)))
    );
    assert_eq!(
        metadata(&entry),
        std::collections::BTreeMap::from([
            ("version".to_owned(), "2".to_owned()),
            ("visibility".to_owned(), "private".to_owned()),
        ])
    );
    assert!(!format!("{entry:?}").contains("hunter2"), "{entry:?}");
    assert_eq!(setup.hints.take(), []);
    let listed = setup.modes(&member, "").await;
    let names: Vec<&Value> = listed.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|mode| &mode["name"])
        .collect();
    assert_eq!(names, [&json!("farm"), &json!("afk")]);
    let again = setup
        .create(&member, swinging_body("fishing HUNTER2"))
        .await;
    assert_eq!(again.status, 201, "{}", again.text);
    log_capture::check_absent("hunter2").unwrap();
}

/// A delete that matches no row after the transaction's own re-read found
/// the mode is a bug: the internal error, never a 204, and nothing is
/// recorded.
#[sqlx::test(migrations = false)]
async fn a_delete_that_matches_no_row_is_the_internal_error_never_a_success(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = stepped_up(&setup, 2, "member", Role::Member).await;
    let id = setup
        .add_mode(1, member.id, "Mine", "private", &swinging())
        .await;
    setup.hooked.lose_next_mode_write();

    let seen = setup.delete_mode(&member, id).await;

    assert_eq!(
        (seen.status, seen.code()),
        (500, "internal"),
        "{}",
        seen.text
    );
    assert!(setup.kept(id).await);
    assert_eq!(setup.deletions().await, 0);
}
