//! The mode routes over HTTP (Plan.md P11.4; ADR-0020, S12–S16, S19):
//! `GET /api/v1/modes` and what each viewer sees of a mode.

use std::collections::BTreeMap;

use fleet_core::authz::{GrantLevel, Role};
use fleet_core::id::{ModeId, UserId};
use fleet_core::mode::ModePreset;
use fleet_server::config::{Rate, RateLimitConfig};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::support::{Options, Person, Seen, Setup, at, redacted, setup, setup_with};

/// The modes' list.
const MODES: &str = "/api/v1/modes";

/// An argument planted in a shared mode's commands, which only the mode's
/// editors may read (S15).
const CANARY: &str = "hunter2-secret";

/// A mode that sends `/login <CANARY>` at start and `/msg Steve <CANARY>`
/// every 30 s.
fn planted() -> String {
    json!({ "steps": [
        { "action": { "type": "send_chat", "message": format!("/login {CANARY}") }, "schedule": { "type": "at_start" }, "probability": 100 },
        { "action": { "type": "send_chat", "message": format!("/msg Steve {CANARY}") }, "schedule": { "type": "every", "interval_ms": 30_000, "jitter_ms": 0 }, "probability": 100 },
    ] })
    .to_string()
}

/// A mode that swings every 20 s.
fn swinging() -> String {
    json!({ "steps": [
        { "action": { "type": "swing_arm" }, "schedule": { "type": "every", "interval_ms": 20_000, "jitter_ms": 0 }, "probability": 100 },
    ] })
    .to_string()
}

impl Setup {
    /// Mode `n` of `owner`, named `name`, with `visibility` and the mode JSON
    /// `definition`: a later `n` is a newer mode.
    async fn add_mode(
        &self,
        n: u8,
        owner: UserId,
        name: &str,
        visibility: &str,
        definition: &str,
    ) -> ModeId {
        let created = at(super::support::T0 + i64::from(n) * 60_000);
        let id = ModeId::new_v7(created, [n; 10]).unwrap();
        let text = id.to_string();
        let owner = owner.to_string();
        sqlx::query!(
            "INSERT INTO modes (id, owner_user_id, name, visibility, definition_json, version, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, 1, '2027-01-15T08:00:00.000Z', '2027-01-15T08:00:00.000Z')",
            text,
            owner,
            name,
            visibility,
            definition
        )
        .execute(self.db.write_pool())
        .await
        .unwrap();
        id
    }

    /// `GET /modes` with `query` (`?limit=…`) as `by`.
    async fn modes(&self, by: &Person, query: &str) -> Seen {
        self.get(&format!("{MODES}{query}"), Some(&by.token)).await
    }

    /// Sets `user`'s role, as a promotion or a demotion would.
    async fn set_role(&self, user: UserId, role: Role) {
        let id = user.to_string();
        let role = role.as_str();
        sqlx::query!("UPDATE users SET role = ? WHERE id = ?", role, id)
            .execute(self.db.write_pool())
            .await
            .unwrap();
    }
}

/// The mode of `page` named `name`.
fn named<'a>(page: &'a Value, name: &str) -> &'a Value {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mode| mode["name"] == name)
        .unwrap_or_else(|| panic!("no mode named {name:?} in {page}"))
}

/// The names on `page`, in order.
fn names(page: &Value) -> Vec<&str> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|mode| mode["name"].as_str().unwrap())
        .collect()
}

#[sqlx::test(migrations = false)]
async fn the_modes_need_a_session(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;

    let seen = setup.get(MODES, None).await;

    assert_eq!(seen.status, 401, "{}", seen.text);
}

#[sqlx::test(migrations = false)]
async fn a_query_that_doesnt_parse_is_400_and_one_out_of_range_422(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;

    for query in ["?before=not-an-id", "?limit=many", "?colour=red"] {
        let seen = setup.modes(&member, query).await;
        assert_eq!(seen.status, 400, "{query}: {}", seen.text);
    }
    for query in ["?limit=0", "?limit=101"] {
        let seen = setup.modes(&member, query).await;
        assert_eq!(seen.status, 422, "{query}: {}", seen.text);
        assert_eq!(seen.body["error"]["fields"][0]["path"], "limit", "{query}");
    }
}

#[sqlx::test(migrations = false)]
async fn the_list_spends_the_users_request_limit(
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
    let member = setup.person(2, "member", Role::Member).await;

    let first = setup.modes(&member, "").await;
    let second = setup.modes(&member, "").await;

    assert_eq!(first.status, 200, "{}", first.text);
    assert_eq!(second.status, 429, "{}", second.text);
    assert_eq!(second.headers["retry-after"], "1");
}

#[sqlx::test(migrations = false)]
async fn the_list_pages_newest_first_and_ends_without_next(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    for n in 1..=3 {
        setup
            .add_mode(n, member.id, &format!("Mode {n}"), "private", &swinging())
            .await;
    }

    let first = setup.modes(&member, "?limit=2").await;
    let next = first.body["next"].as_str().unwrap().to_owned();
    let second = setup
        .modes(&member, &format!("?limit=2&before={next}"))
        .await;
    let last = setup
        .modes(
            &member,
            &format!("?before={}", second.body["next"].as_str().unwrap()),
        )
        .await;
    let all = setup.modes(&member, "").await;

    assert_eq!(names(&first.body), ["Mode 3", "Mode 2"]);
    assert_eq!(names(&second.body), ["Mode 1", "farm"]);
    assert_eq!(names(&last.body), ["afk"]);
    assert_eq!(last.body.get("next"), None);
    assert_eq!(
        names(&all.body),
        ["Mode 3", "Mode 2", "Mode 1", "farm", "afk"]
    );
    assert_eq!(all.body.get("next"), None);
}

/// Users of every kind, and the modes they see: the built-in ones, an
/// Admin's planted shared mode and a private one, a Member's private one,
/// the shared mode of a creator demoted since, the private one of a creator
/// promoted since, and the Owner's.
struct Viewers {
    the_owner: Person,
    admin: Person,
    other_admin: Person,
    member: Person,
    demoted: Person,
    promoted: Person,
}

async fn viewers(setup: &Setup) -> (Viewers, Vec<(String, &'static str)>) {
    let the_owner = setup.person(2, "theowner", Role::Owner).await;
    let admin = setup.person(3, "admin", Role::Admin).await;
    let other_admin = setup.person(4, "otheradmin", Role::Admin).await;
    let member = setup.person(5, "member", Role::Member).await;
    let demoted = setup.person(6, "demoted", Role::Admin).await;
    let promoted = setup.person(7, "promoted", Role::Member).await;
    let modes = [
        (admin.id, "Admin shared", "shared", planted()),
        (admin.id, "Admin private", "private", swinging()),
        (member.id, "Member private", "private", planted()),
        (demoted.id, "Demoted shared", "shared", planted()),
        (promoted.id, "Promoted private", "private", swinging()),
        (the_owner.id, "Owner shared", "shared", swinging()),
        (the_owner.id, "Owner private", "private", swinging()),
    ];
    let mut names = vec![
        (the_owner.id.to_string(), "theowner"),
        (admin.id.to_string(), "admin"),
        (member.id.to_string(), "member"),
        (demoted.id.to_string(), "demoted"),
        (promoted.id.to_string(), "promoted"),
        (ModePreset::Afk.id().to_string(), "afk"),
        (ModePreset::Farm.id().to_string(), "farm"),
    ];
    for (n, (owner, name, visibility, definition)) in (10_u8..).zip(modes) {
        let id = setup
            .add_mode(n, owner, name, visibility, &definition)
            .await;
        names.push((id.to_string(), name));
    }
    setup.set_role(demoted.id, Role::Member).await;
    setup.set_role(promoted.id, Role::Admin).await;
    let viewers = Viewers {
        the_owner,
        admin,
        other_admin,
        member,
        demoted,
        promoted,
    };
    (viewers, names)
}

impl Viewers {
    fn all(&self) -> [(&'static str, &Person); 6] {
        [
            ("the_owner", &self.the_owner),
            ("admin", &self.admin),
            ("other_admin", &self.other_admin),
            ("member", &self.member),
            ("demoted", &self.demoted),
            ("promoted", &self.promoted),
        ]
    }
}

/// Each viewer's page, every field under its rule (S14, S15): whose a mode
/// is for everyone, its owner only for Admins and the Owner, the viewer's
/// rights from `authorize()`, and a chat step's command arguments only for
/// a viewer who may edit the mode.
#[sqlx::test(migrations = false)]
async fn each_viewer_sees_the_modes_under_their_field_rules(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (viewers, ids) = viewers(&setup).await;

    let mut seen = BTreeMap::new();
    for (name, person) in viewers.all() {
        let page = setup.modes(person, "").await;
        assert_eq!(page.status, 200, "{name}: {}", page.text);
        seen.insert(name, page.body);
    }

    insta::assert_json_snapshot!(
        "modes_as_each_viewer_sees_them",
        redacted(&serde_json::to_value(&seen).unwrap(), &ids)
    );
    // The rules the snapshot shows, spelled out.
    assert_eq!(
        names(&seen["member"]),
        [
            "Owner shared",
            "Demoted shared",
            "Member private",
            "Admin shared",
            "farm",
            "afk"
        ]
    );
    let admin_shared = named(&seen["admin"], "Admin shared");
    assert_eq!(admin_shared["ownership"], "mine");
    assert_eq!(admin_shared["editable"], true);
    assert_eq!(admin_shared["owner"]["username"], "admin");
    let as_member = named(&seen["member"], "Admin shared");
    assert_eq!(as_member["ownership"], "someone_else");
    assert_eq!(as_member["editable"], false);
    assert_eq!(as_member.get("owner"), None, "Members see no owner");
    let built_in = named(&seen["member"], "afk");
    assert_eq!(built_in["ownership"], "built_in");
    assert_eq!(
        (&built_in["editable"], &built_in["deletable"]),
        (&json!(false), &json!(false))
    );
    let demoted_own = named(&seen["demoted"], "Demoted shared");
    assert_eq!(demoted_own["ownership"], "mine");
    assert_eq!(
        demoted_own["editable"], false,
        "a demoted creator can't edit it"
    );
    assert!(
        !names(&seen["other_admin"]).contains(&"Promoted private"),
        "a promoted creator's private mode is no longer an Admin's to see"
    );
}

/// The canary (S15): an argument planted in a shared mode's commands never
/// reaches a viewer who may not edit the mode, a Control holder of a bot
/// that runs it included, and reaches the creator and the Owner, until the
/// creator is demoted.
#[sqlx::test(migrations = false)]
async fn a_planted_argument_in_a_mode_never_reaches_a_viewer_who_cant_edit_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let the_owner = setup.person(2, "theowner", Role::Owner).await;
    let admin = setup.person(3, "admin", Role::Admin).await;
    let other_admin = setup.person(4, "otheradmin", Role::Admin).await;
    let member = setup.person(5, "member", Role::Member).await;
    let controller = setup.person(6, "controller", Role::Member).await;
    let mode = setup
        .add_mode(10, admin.id, "Admin shared", "shared", &planted())
        .await;
    let (account, bot) = setup.bot_of(member.id, 20).await;
    setup
        .share(account, controller.id, GrantLevel::Control, member.id)
        .await;
    let (mode_text, bot_text) = (mode.to_string(), bot.to_string());
    sqlx::query!(
        "UPDATE bots SET mode_id = ? WHERE id = ?",
        mode_text,
        bot_text
    )
    .execute(setup.db.write_pool())
    .await
    .unwrap();

    for (name, below) in [
        ("member", &member),
        ("other_admin", &other_admin),
        ("controller", &controller),
    ] {
        let seen = setup.modes(below, "").await;
        assert_eq!(seen.status, 200, "{name}: {}", seen.text);
        assert!(!seen.text.contains(CANARY), "{name}: {}", seen.text);
        let step = &named(&seen.body, "Admin shared")["steps"][0]["action"];
        assert_eq!(step["message"], "/login", "{name}");
        assert_eq!(step["arguments_hidden"], true, "{name}");
    }
    for (name, editor) in [("admin", &admin), ("the_owner", &the_owner)] {
        let seen = setup.modes(editor, "").await;
        assert_eq!(
            seen.text.matches(CANARY).count(),
            2,
            "{name}: {}",
            seen.text
        );
    }

    setup.set_role(admin.id, Role::Member).await;
    let demoted = setup.modes(&admin, "").await;

    assert!(!demoted.text.contains(CANARY), "{}", demoted.text);
}

/// Who made a shared mode is an afkfleet username, which Members never
/// learn (S14, as S11 and U3): no Admin's or Owner's name in any field.
#[sqlx::test(migrations = false)]
async fn no_admins_or_owners_username_reaches_a_member(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let the_owner = setup.person(2, "zz-owner-canary", Role::Owner).await;
    let admin = setup.person(3, "zz-admin-canary", Role::Admin).await;
    let member = setup.person(4, "member", Role::Member).await;
    setup
        .add_mode(10, admin.id, "Admin shared", "shared", &swinging())
        .await;
    setup
        .add_mode(11, the_owner.id, "Owner shared", "shared", &swinging())
        .await;

    let seen = setup.modes(&member, "").await;
    let as_owner = setup.modes(&the_owner, "").await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    assert!(!seen.text.contains("canary"), "{}", seen.text);
    assert!(
        as_owner.text.contains("zz-admin-canary"),
        "{}",
        as_owner.text
    );
}

/// The longest page a client may ask for, 100 modes of 32 steps each with
/// the longest names, messages and numbers, fits fleet-client's 1 MiB cap on
/// an answer's body (`MAX_RESPONSE_BYTES`).
#[sqlx::test(migrations = false)]
async fn the_longest_page_fits_a_clients_answer_cap(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    const CLIENT_CAP: usize = 1024 * 1024;
    let setup = setup(options).await;
    let the_owner = setup.person(2, "theowner", Role::Owner).await;
    let message = format!("/{}", "w".repeat(255));
    let mut steps = vec![
        json!({ "action": { "type": "send_chat", "message": message }, "schedule": { "type": "at_start" }, "probability": 100 }),
        json!({ "action": { "type": "send_chat", "message": message }, "schedule": { "type": "every", "interval_ms": 86_400_000, "jitter_ms": 86_400_000 }, "probability": 100 }),
    ];
    steps.extend((0..30).map(|_| {
        json!({ "action": { "type": "rotate_random", "max_yaw": 179.999_98, "max_pitch": 89.999_99 }, "schedule": { "type": "every", "interval_ms": 86_400_000, "jitter_ms": 86_400_000 }, "probability": 100 })
    }));
    let definition = json!({ "steps": steps }).to_string();
    for n in 0..100_u8 {
        let name = format!("{n:0>3} {}", "Ü".repeat(28));
        setup
            .add_mode(n, the_owner.id, &name, "shared", &definition)
            .await;
    }

    let seen = setup.modes(&the_owner, "?limit=100").await;

    assert_eq!(seen.status, 200);
    assert_eq!(seen.body["items"].as_array().unwrap().len(), 100);
    assert!(seen.text.len() < CLIENT_CAP, "{} bytes", seen.text.len());
}
