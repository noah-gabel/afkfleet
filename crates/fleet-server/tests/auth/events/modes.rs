//! Modes and role changes on the socket (ADR-0020, S33, S15, R11, R36): a
//! `mode` frame equals what that viewer's `GET /modes` lists for it, a mode
//! the viewer stops seeing is `mode_removed` for them only, a hidden mode
//! leaves no trace, another user's role change resyncs the other Admins,
//! and every hint comes after its commit.

use fleet_api_types::events::SocketClose;
use fleet_core::authz::{ModeVisibility, Role, UserRef};
use fleet_core::id::ModeId;
use fleet_server::ports::live_hints::{LiveHints as _, ModeEvent, ModeSeen};
use fleet_server::ports::store::Store as _;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::super::support::{Hint, Person, Setup, USERS, setup};
use super::{
    PATIENCE, Served, Socket, closed, commit_anything, next_event_after_pings, ready, said,
};

const MODES: &str = "/api/v1/modes";

/// An argument planted in a shared mode: only its editors read it (S15).
const CANARY: &str = "hunter2-mode-canary";

/// A mode that sends `/warp <CANARY>` at start.
fn planted() -> String {
    json!({ "steps": [
        { "action": { "type": "send_chat", "message": format!("/warp {CANARY}") }, "schedule": { "type": "at_start" }, "probability": 100 },
    ] })
    .to_string()
}

/// The Owner, an Admin who made a shared mode and a private one, another
/// Admin and a Member.
struct Watchers {
    the_owner: Person,
    creator: Person,
    other_admin: Person,
    member: Person,
    shared: ModeId,
    private: ModeId,
}

impl Watchers {
    fn all(&self) -> [(&'static str, &Person); 4] {
        [
            ("the_owner", &self.the_owner),
            ("creator", &self.creator),
            ("other_admin", &self.other_admin),
            ("member", &self.member),
        ]
    }

    /// The shared mode as the write read it before it changed.
    fn shared_seen(&self, visibility: ModeVisibility) -> ModeSeen {
        ModeSeen {
            owner: Some(UserRef {
                id: self.creator.id,
                role: Role::Admin,
            }),
            visibility,
        }
    }
}

async fn watchers(setup: &Setup) -> Watchers {
    let the_owner = setup.person(6, "theowner", Role::Owner).await;
    let creator = setup.person(7, "creator", Role::Admin).await;
    let other_admin = setup.person(8, "otheradmin", Role::Admin).await;
    let member = setup.person(2, "member", Role::Member).await;
    let shared = setup
        .add_mode(1, creator.id, "warp", "shared", &planted())
        .await;
    let private = setup
        .add_mode(2, creator.id, "mine", "private", &planted())
        .await;
    Watchers {
        the_owner,
        creator,
        other_admin,
        member,
        shared,
        private,
    }
}

async fn sockets(setup: &Setup, served: &Served, watchers: &Watchers) -> Vec<Socket> {
    let mut sockets = Vec::new();
    for (_, person) in watchers.all() {
        sockets.push(ready(setup, served, &person.token).await);
    }
    sockets
}

/// The mode `id` as `person`'s `GET /modes` lists it, if it does.
async fn listed(setup: &Setup, id: ModeId, person: &Person) -> Option<Value> {
    let seen = setup
        .get(&format!("{MODES}?limit=100"), Some(&person.token))
        .await;
    assert_eq!(seen.status, 200, "{}", seen.text);
    seen.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mode| mode["id"] == json!(id.to_string()))
        .cloned()
}

fn mode_frame(id: ModeId, mode: &Value) -> Value {
    json!({ "type": "mode", "mode_id": id.to_string(), "mode": mode })
}

fn removed(id: ModeId) -> Value {
    json!({ "type": "mode_removed", "mode_id": id.to_string() })
}

/// Sets the mode `id`'s visibility, as an edit would.
async fn set_visibility(setup: &Setup, id: ModeId, visibility: &str) {
    let text = id.to_string();
    sqlx::query!(
        "UPDATE modes SET visibility = ? WHERE id = ?",
        visibility,
        text
    )
    .execute(setup.db.write_pool())
    .await
    .unwrap();
}

#[sqlx::test(migrations = false)]
async fn each_viewers_mode_frame_equals_their_modes_entry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watchers = watchers(&setup).await;
    let served = Served::start(&setup).await;
    let mut sockets = sockets(&setup, &served, &watchers).await;
    // Who edits the shared mode, and so reads its argument.
    let editors = [true, true, false, false];

    setup.events.mode(ModeEvent {
        id: watchers.shared,
        was: None,
    });

    for (((name, person), socket), edits) in
        watchers.all().into_iter().zip(&mut sockets).zip(editors)
    {
        let frame = next_event_after_pings(socket).await;
        let entry = listed(&setup, watchers.shared, person).await.unwrap();
        assert_eq!(frame, mode_frame(watchers.shared, &entry), "{name}");
        assert_eq!(frame.to_string().contains(CANARY), edits, "{name}: {frame}");
    }
}

#[sqlx::test(migrations = false)]
async fn a_hidden_modes_event_sends_no_frame(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watchers = watchers(&setup).await;
    let served = Served::start(&setup).await;
    let mut sockets = sockets(&setup, &served, &watchers).await;
    // Who sees the creator's private mode: the Owner and the creator.
    let sees = [true, true, false, false];

    setup.events.mode(ModeEvent {
        id: watchers.private,
        was: None,
    });
    setup.events.mode(ModeEvent {
        id: watchers.shared,
        was: None,
    });

    for (((name, _), socket), sees) in watchers.all().into_iter().zip(&mut sockets).zip(sees) {
        let first = next_event_after_pings(socket).await;
        let expected = if sees {
            watchers.private
        } else {
            watchers.shared
        };
        assert_eq!(first["mode_id"], json!(expected.to_string()), "{name}");
    }
}

#[sqlx::test(migrations = false)]
async fn a_mode_made_private_is_mode_removed_for_those_who_lose_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watchers = watchers(&setup).await;
    let served = Served::start(&setup).await;
    let mut sockets = sockets(&setup, &served, &watchers).await;

    set_visibility(&setup, watchers.shared, "private").await;
    setup.events.mode(ModeEvent {
        id: watchers.shared,
        was: Some(watchers.shared_seen(ModeVisibility::Shared)),
    });

    for (((name, person), socket), keeps) in watchers
        .all()
        .into_iter()
        .zip(&mut sockets)
        .zip([true, true, false, false])
    {
        let frame = next_event_after_pings(socket).await;
        let expected = match listed(&setup, watchers.shared, person).await {
            Some(entry) => mode_frame(watchers.shared, &entry),
            None => removed(watchers.shared),
        };
        assert_eq!(frame["type"] == json!("mode"), keeps, "{name}");
        assert_eq!(frame, expected, "{name}");
    }
}

#[sqlx::test(migrations = false)]
async fn a_deleted_mode_is_mode_removed_for_its_viewers_only(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watchers = watchers(&setup).await;
    let served = Served::start(&setup).await;
    let mut sockets = sockets(&setup, &served, &watchers).await;
    let text = watchers.private.to_string();

    sqlx::query!("DELETE FROM modes WHERE id = ?", text)
        .execute(setup.db.write_pool())
        .await
        .unwrap();
    setup.events.mode(ModeEvent {
        id: watchers.private,
        was: Some(ModeSeen {
            visibility: ModeVisibility::Private,
            ..watchers.shared_seen(ModeVisibility::Private)
        }),
    });
    setup.events.mode(ModeEvent {
        id: watchers.shared,
        was: None,
    });

    for (((name, _), socket), saw) in watchers
        .all()
        .into_iter()
        .zip(&mut sockets)
        .zip([true, true, false, false])
    {
        let first = next_event_after_pings(socket).await;
        if saw {
            assert_eq!(first, removed(watchers.private), "{name}");
        } else {
            assert_eq!(
                first["mode_id"],
                json!(watchers.shared.to_string()),
                "{name}"
            );
        }
    }
}

#[sqlx::test(migrations = false)]
async fn a_role_change_resyncs_the_other_admins(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watchers = watchers(&setup).await;
    let served = Served::start(&setup).await;
    let mut sockets = sockets(&setup, &served, &watchers).await;
    setup.stamp(watchers.the_owner.session).await;

    let seen = setup
        .patch(
            &format!("{USERS}/{}", watchers.member.id),
            Some(&watchers.the_owner.token),
            json!({ "role": "admin" }),
        )
        .await;
    assert_eq!(seen.status, 200, "{}", seen.text);
    setup.events.mode(ModeEvent {
        id: watchers.shared,
        was: None,
    });

    let [owner, creator, other_admin, member] = &mut sockets[..] else {
        panic!("four sockets");
    };
    // The resync comes within its gap, before or after the mode's frame.
    for socket in [creator, other_admin] {
        let mut kinds = [
            next_event_after_pings(socket).await["type"].clone(),
            next_event_after_pings(socket).await["type"].clone(),
        ];
        kinds.sort_by_key(ToString::to_string);
        assert_eq!(kinds, [json!("mode"), json!("resync")]);
    }
    // The Admins' resyncs came; the Owner's next frames are the modes'
    // alone.
    setup.events.mode(ModeEvent {
        id: watchers.private,
        was: None,
    });
    for mode in [watchers.shared, watchers.private] {
        assert_eq!(
            next_event_after_pings(owner).await["mode_id"],
            json!(mode.to_string()),
            "the Owner sees what they saw, and no resync"
        );
    }
    assert_eq!(closed(member).await, said(SocketClose::Unauthenticated));
}

#[sqlx::test(migrations = false)]
async fn a_mode_read_that_fails_closes_the_socket_with_1013(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watchers = watchers(&setup).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &watchers.member.token).await;

    setup.hooked.fail_mode_reads();
    setup.events.mode(ModeEvent {
        id: watchers.shared,
        was: None,
    });

    assert_eq!(closed(&mut socket).await, said(SocketClose::Busy));
}

// --- The services' hints (R36): after their commit, never for a no-op -----

/// The commit count now.
fn count(setup: &Setup) -> u64 {
    *setup.db.commits().borrow()
}

#[sqlx::test(migrations = false)]
async fn each_mode_write_hints_the_sockets_after_its_commit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let body = |name: &str| {
        json!({ "name": name, "visibility": "private", "steps": [
            { "action": { "type": "swing_arm" }, "schedule": { "type": "every", "interval_ms": 20_000, "jitter_ms": 0 }, "probability": 100 },
        ] })
    };
    let member_seen = ModeSeen {
        owner: Some(UserRef {
            id: member.id,
            role: Role::Member,
        }),
        visibility: ModeVisibility::Private,
    };

    let created = setup
        .post(MODES, Some(&member.token), Some(body("swing")))
        .await;
    assert_eq!(created.status, 201, "{}", created.text);
    let id: ModeId = created.body["id"].as_str().unwrap().parse().unwrap();
    let after_create = (count(&setup), setup.heard.take());
    let mut edit = body("swing two");
    edit["version"] = json!(1);
    let edited = setup
        .request(
            axum::http::Method::PUT,
            &format!("{MODES}/{id}"),
            Some(&member.token),
            Some(edit.clone()),
        )
        .await;
    assert_eq!(edited.status, 200, "{}", edited.text);
    let after_edit = (count(&setup), setup.heard.take());
    edit["version"] = json!(2);
    let unchanged = setup
        .request(
            axum::http::Method::PUT,
            &format!("{MODES}/{id}"),
            Some(&member.token),
            Some(edit),
        )
        .await;
    assert_eq!(unchanged.status, 200, "{}", unchanged.text);
    let after_no_op = setup.heard.take();
    setup.stamp(member.session).await;
    let deleted = setup
        .delete(&format!("{MODES}/{id}"), Some(&member.token))
        .await;
    assert_eq!(deleted.status, 204, "{}", deleted.text);
    let after_delete = (count(&setup), setup.heard.take());

    assert_eq!(
        after_create,
        (
            after_create.0,
            vec![(after_create.0, Hint::Mode(ModeEvent { id, was: None }))]
        )
    );
    assert_eq!(
        after_edit.1,
        vec![(
            after_edit.0,
            Hint::Mode(ModeEvent {
                id,
                was: Some(member_seen)
            })
        )]
    );
    assert_eq!(after_no_op, Vec::new(), "a no-op hints nothing");
    assert_eq!(
        after_delete.1,
        vec![(
            after_delete.0,
            Hint::Mode(ModeEvent {
                id,
                was: Some(member_seen)
            })
        )]
    );
}

#[sqlx::test(migrations = false)]
async fn a_real_role_change_hints_the_sockets_after_its_commit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let the_owner = setup.person(6, "theowner", Role::Owner).await;
    let member = setup.person(2, "member", Role::Member).await;
    setup.stamp(the_owner.session).await;
    let path = format!("{USERS}/{}", member.id);
    let patch = |role: &str| setup.patch(&path, Some(&the_owner.token), json!({ "role": role }));

    let promoted = patch("admin").await;
    let after_promotion = (count(&setup), setup.heard.take());
    let again = patch("admin").await;
    let after_no_op = setup.heard.take();

    assert_eq!((promoted.status, again.status), (200, 200));
    assert_eq!(
        after_promotion.1,
        vec![(after_promotion.0, Hint::RoleChanged(member.id))]
    );
    assert_eq!(after_no_op, Vec::new(), "the same role hints nothing");
}

/// The shared mode made private while the other Admin's read of it is held,
/// after that read: the read is older than the count, so the mode is read
/// again, and they get `mode_removed`, never the mode (S43).
#[sqlx::test(migrations = false)]
async fn a_mode_made_private_while_its_read_is_held_sends_mode_removed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watchers = watchers(&setup).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &watchers.other_admin.token).await;
    let edited = ModeEvent {
        id: watchers.shared,
        was: Some(watchers.shared_seen(ModeVisibility::Shared)),
    };

    let mut held = setup.hooked.hold_next_mode_read();
    setup.events.mode(edited);
    held.read(PATIENCE).await;
    set_visibility(&setup, watchers.shared, "private").await;
    commit_anything(&setup).await;
    setup.events.mode(edited);
    setup.events.role_changed(watchers.member.id);
    held.release();
    let mut frames = Vec::new();
    while frames.last() != Some(&json!({ "type": "resync" })) {
        frames.push(next_event_after_pings(&mut socket).await);
    }

    assert_eq!(
        frames,
        [
            removed(watchers.shared),
            removed(watchers.shared),
            json!({ "type": "resync" })
        ],
        "each hint answered, and never the mode"
    );
}

/// The shared mode's first event is being read while three more of it and
/// one of the private mode wait: the three are read once, at their first
/// place (R33).
#[sqlx::test(migrations = false)]
async fn waiting_events_of_one_mode_merge_into_one_read(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watchers = watchers(&setup).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &watchers.creator.token).await;
    let edited = ModeEvent {
        id: watchers.shared,
        was: Some(watchers.shared_seen(ModeVisibility::Shared)),
    };
    let reads = setup.hooked.mode_reads();

    let mut held = setup.hooked.hold_next_mode_read();
    setup.events.mode(edited);
    held.read(PATIENCE).await;
    for _ in 0..3 {
        setup.events.mode(edited);
    }
    setup.events.mode(ModeEvent {
        id: watchers.private,
        was: None,
    });
    held.release();
    let mut named = Vec::new();
    for _ in 0..3 {
        named.push(next_event_after_pings(&mut socket).await["mode_id"].clone());
    }

    let (shared, private) = (
        json!(watchers.shared.to_string()),
        json!(watchers.private.to_string()),
    );
    assert_eq!(named, [shared.clone(), shared, private]);
    assert_eq!(
        setup.hooked.mode_reads() - reads,
        3,
        "the held read, the three merged, the other mode"
    );
}
