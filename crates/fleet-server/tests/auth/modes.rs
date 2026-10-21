//! The mode routes over HTTP (Plan.md P11.4; ADR-0020, S12–S16, S19):
//! `GET /api/v1/modes` and what each viewer sees of a mode; `POST
//! /api/v1/modes` and its step-up twin, with the pointer, the cap, names,
//! the re-checks and the audit entry.

use std::collections::BTreeMap;

use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget, RecordedValue};
use fleet_core::authz::{CommandAllowlist, GrantLevel, ModeVisibility, Role};
use fleet_core::id::{ModeId, UserId};
use fleet_core::mode::{ModeDefinition, ModeName, ModePreset};
use fleet_server::app::modes::{CreateModeError, ModeRoute, ModeService};
use fleet_server::app::users::ActingUser;
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_testkit::log_capture;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::support::{Meanwhile, Options, Person, Seen, Setup, at, redacted, setup, setup_with};

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

// --- Creating (S12, S13, S16, S19) ----------------------------------------------

/// The step-up twin of `POST /modes`.
const WITH_COMMANDS: &str = "/api/v1/modes/with-commands";

/// A new private mode named `name` that swings every 20 s.
fn swinging_body(name: &str) -> Value {
    json!({ "name": name, "visibility": "private", "steps": [
        { "action": { "type": "swing_arm" }, "schedule": { "type": "every", "interval_ms": 20_000, "jitter_ms": 0 }, "probability": 100 },
    ] })
}

/// A new mode named `name` with `visibility` that sends `command` at
/// start.
fn commanding_body(name: &str, visibility: &str, command: &str) -> Value {
    json!({ "name": name, "visibility": visibility, "steps": [
        { "action": { "type": "send_chat", "message": command }, "schedule": { "type": "at_start" }, "probability": 100 },
    ] })
}

/// `role`'s person `n`, stepped up now.
async fn stepped_up(setup: &Setup, n: u8, name: &str, role: Role) -> Person {
    let person = setup.person(n, name, role).await;
    setup.stamp(person.session).await;
    person
}

/// The setup with `/spawn` on the allowlist.
async fn with_spawn(options: SqliteConnectOptions) -> Setup {
    setup_with(
        options,
        Options {
            allowlist: CommandAllowlist::try_new(&["/spawn"]).unwrap(),
            ..Options::default()
        },
    )
    .await
}

impl Setup {
    /// `POST /modes` with `body` as `by`.
    async fn create(&self, by: &Person, body: Value) -> Seen {
        self.post(MODES, Some(&by.token), Some(body)).await
    }

    /// `POST /modes/with-commands` with `body` as `by`.
    async fn create_with_commands(&self, by: &Person, body: Value) -> Seen {
        self.post(WITH_COMMANDS, Some(&by.token), Some(body)).await
    }

    /// How many modes the users own: every mode but the built-in ones.
    async fn users_modes(&self) -> i64 {
        sqlx::query_scalar!(
            r#"SELECT count(*) AS "count!: i64" FROM modes WHERE owner_user_id IS NOT NULL"#
        )
        .fetch_one(self.db.read_pool())
        .await
        .unwrap()
    }

    /// The `mode.create` entries.
    async fn creations(&self) -> usize {
        self.audit()
            .await
            .iter()
            .filter(|entry| entry.action == AuditAction::MODE_CREATE)
            .count()
    }
}

/// An entry's metadata as text values by key.
fn metadata(entry: &fleet_server::ports::audit::AuditRecord) -> BTreeMap<String, String> {
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

#[sqlx::test(migrations = false)]
async fn creating_a_mode_needs_a_session(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;

    for path in [MODES, WITH_COMMANDS] {
        let seen = setup.post(path, None, Some(swinging_body("Mine"))).await;
        assert_eq!(seen.status, 401, "{path}: {}", seen.text);
    }
}

/// A created mode answers 201 as its creator sees it, at version 1, and
/// is listed from then on.
#[sqlx::test(migrations = false)]
async fn a_created_mode_is_201_as_its_creator_sees_it_and_listed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = with_spawn(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let body = json!({ "name": " Grüße, „Farm“ #2! ", "visibility": "private", "steps": [
        { "action": { "type": "look", "yaw": -90.5, "pitch": 12.0 }, "schedule": { "type": "at_start" }, "probability": 100 },
        { "action": { "type": "select_hotbar_slot", "slot": 8 }, "schedule": { "type": "at_start" }, "probability": 100 },
        { "action": { "type": "send_chat", "message": "/spawn home" }, "schedule": { "type": "every", "interval_ms": 60_000, "jitter_ms": 5_000 }, "probability": 50 },
    ] });

    let seen = setup.create(&member, body).await;

    assert_eq!(seen.status, 201, "{}", seen.text);
    let id = seen.body["id"].as_str().unwrap().to_owned();
    insta::assert_json_snapshot!(
        "mode_created",
        redacted(&seen.body, &[(id.clone(), "the mode")])
    );
    let listed = setup.modes(&member, "").await;
    assert_eq!(listed.body["items"][0], seen.body, "listed as answered");
}

/// Each rule a field breaks is one 422 at that field's own path, and
/// nothing is written: a sample of the mapping, which fleet-api-types
/// tests in full.
#[sqlx::test(migrations = false)]
async fn a_rule_a_field_breaks_is_one_422_at_its_path_and_nothing_is_written(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let chat = |message: &str, interval: u32| json!({ "action": { "type": "send_chat", "message": message }, "schedule": { "type": "every", "interval_ms": interval, "jitter_ms": 0 }, "probability": 100 });
    let cases = [
        (
            json!({ "name": "", "visibility": "private", "steps": [] }),
            "name",
            "must not be empty",
        ),
        (
            json!({ "name": "AFK", "visibility": "private", "steps": [] }),
            "name",
            "is a built-in mode's name",
        ),
        (
            json!({ "name": "Farm 🐄", "visibility": "private", "steps": [] }),
            "name",
            "must hold only letters, digits, plain spaces and punctuation, without emoji or other symbols",
        ),
        (
            json!({ "name": "Fine", "visibility": "private", "steps": [chat("hi", 30_000), chat("a\nb", 30_000)] }),
            "steps[1].action.message",
            "must be one line, without control characters, formatting codes or text-direction marks",
        ),
        (
            json!({ "name": "Fine", "visibility": "private", "steps": [chat("hi", 29_999)] }),
            "steps[0].schedule.interval_ms",
            "must be at least 30 seconds for chat",
        ),
        (
            json!({ "name": "Fine", "visibility": "private", "steps": [{ "action": { "type": "select_hotbar_slot", "slot": 9 }, "schedule": { "type": "at_start" }, "probability": 100 }] }),
            "steps[0].action.slot",
            "must be a hotbar slot from 0 to 8 (the keys 1 to 9)",
        ),
        (
            json!({ "name": "Fine", "visibility": "private", "steps": [{ "action": { "type": "look", "yaw": 0.0 }, "schedule": { "type": "at_start" }, "probability": 100 }] }),
            "steps[0].action.pitch",
            "is required for this type",
        ),
        (
            json!({ "name": "Fine", "visibility": "private", "steps": vec![chat("hi", 30_000); 33] }),
            "steps",
            "must hold at most 32 steps",
        ),
    ];

    for (body, path, message) in cases {
        let seen = setup.create(&member, body).await;
        assert_eq!(seen.status, 422, "{path}: {}", seen.text);
        assert_eq!(
            seen.body["error"]["fields"],
            json!([{ "path": path, "message": message }]),
            "{path}"
        );
    }
    assert_eq!(setup.users_modes().await, 0);
    assert_eq!(setup.creations().await, 0);
}

/// A body that doesn't parse is the bare 400, whatever text it holds:
/// neither the answer nor any log line repeats it (S12).
#[sqlx::test(migrations = false)]
async fn a_body_that_doesnt_parse_is_400_and_never_repeats_what_it_read(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    log_capture::install().unwrap();
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let step = |action: Value| json!({ "name": "Fine", "visibility": "private", "steps": [{ "action": action, "schedule": { "type": "at_start" }, "probability": 100 }] });
    let bodies = [
        step(json!({ "type": format!("say {CANARY}") })),
        step(json!({ "type": "select_hotbar_slot", "slot": format!("slot {CANARY}") })),
        step(json!({ "type": "jump", CANARY: true })),
        json!({ "name": "Fine", "visibility": format!("shared {CANARY}"), "steps": [] }),
        json!({ "name": "Fine", "visibility": "private", "steps": [], "owner": CANARY }),
    ];

    for body in bodies {
        let seen = setup.create(&member, body).await;
        assert_eq!(seen.status, 400, "{}", seen.text);
        assert_eq!(seen.code(), "bad_request");
        assert!(!seen.text.contains(CANARY), "{}", seen.text);
    }
    assert_eq!(setup.users_modes().await, 0);
    log_capture::check_absent(CANARY).unwrap();
}

#[sqlx::test(migrations = false)]
async fn a_member_may_not_create_a_shared_mode(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = stepped_up(&setup, 2, "member", Role::Member).await;
    let mut body = swinging_body("Mine");
    body["visibility"] = json!("shared");

    let plain = setup.create(&member, body.clone()).await;
    let twin = setup.create_with_commands(&member, body).await;

    assert_eq!(
        (plain.status, plain.code()),
        (403, "forbidden"),
        "{}",
        plain.text
    );
    assert_eq!(
        (twin.status, twin.code()),
        (403, "forbidden"),
        "{}",
        twin.text
    );
    assert_eq!(setup.users_modes().await, 0);
}

/// A mode whose steps send a command off the allowlist is pointed to the
/// step-up route and nothing is written; an allowlisted command passes the
/// plain route (S16).
#[sqlx::test(migrations = false)]
async fn a_command_off_the_allowlist_is_pointed_to_the_step_up_route(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = with_spawn(options).await;
    let member = setup.person(2, "member", Role::Member).await;

    let pointed = setup
        .create(&member, commanding_body("Ops", "private", "/op AfkBot2"))
        .await;
    assert_eq!(setup.users_modes().await, 0, "nothing written");
    let listed = setup
        .create(
            &member,
            commanding_body("Spawner", "private", "/spawn home"),
        )
        .await;

    assert_eq!(pointed.status, 403, "{}", pointed.text);
    assert_eq!(pointed.code(), "mode_needs_step_up");
    insta::assert_json_snapshot!("mode_needs_step_up", pointed.body_without_request_id());
    assert_eq!(listed.status, 201, "{}", listed.text);
}

/// The pointer comes after `forbidden`: only a caller the twin would take
/// sees it.
#[sqlx::test(migrations = false)]
async fn the_pointer_comes_after_forbidden(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = with_spawn(options).await;
    let member = setup.person(2, "member", Role::Member).await;

    let seen = setup
        .create(&member, commanding_body("Ops", "shared", "/op AfkBot2"))
        .await;

    assert_eq!(
        (seen.status, seen.code()),
        (403, "forbidden"),
        "{}",
        seen.text
    );
}

/// The twin needs a fresh step-up, and then takes any valid mode (S16).
#[sqlx::test(migrations = false)]
async fn the_step_up_route_needs_a_fresh_step_up_and_then_takes_any_mode(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = with_spawn(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    let body = commanding_body("Ops", "private", "/op AfkBot2");

    let without = setup.create_with_commands(&member, body.clone()).await;
    setup.stamp(member.session).await;
    let with = setup.create_with_commands(&member, body).await;
    let swinging = setup
        .create_with_commands(&member, swinging_body("Swinging"))
        .await;

    assert_eq!(without.code(), "step_up_required", "{}", without.text);
    assert_eq!(with.status, 201, "{}", with.text);
    assert_eq!(swinging.status, 201, "{}", swinging.text);
    assert_eq!(setup.users_modes().await, 2);
}

/// The audit entry names the route, the visibility and whether the steps
/// send commands, never the name or a step's text (S16; ADR-0016).
#[sqlx::test(migrations = false)]
async fn a_create_is_recorded_without_the_name_or_a_steps_text(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = with_spawn(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    let cases = [
        (
            MODES,
            swinging_body("Quiet hunter2"),
            "private",
            "none",
            "plain",
        ),
        (
            MODES,
            commanding_body("Spawn hunter2", "shared", "/spawn hunter2-secret"),
            "shared",
            "allowlisted",
            "plain",
        ),
        (
            WITH_COMMANDS,
            commanding_body("Ops hunter2", "private", "/op hunter2-secret"),
            "private",
            "unlisted",
            "with_commands",
        ),
    ];

    for (path, body, visibility, commands, route) in cases {
        let seen = setup.post(path, Some(&admin.token), Some(body)).await;
        assert_eq!(seen.status, 201, "{}", seen.text);
        let entry = setup.last(&AuditAction::MODE_CREATE).await;
        assert_eq!(entry.outcome, AuditOutcome::Success);
        assert_eq!(entry.actor, Some(admin.id));
        assert!(entry.ip.is_some());
        assert_eq!(
            entry.target,
            Some(RecordedTarget::Known(AuditTarget::Mode(
                seen.body["id"].as_str().unwrap().parse().unwrap()
            )))
        );
        assert_eq!(
            metadata(&entry),
            BTreeMap::from([
                ("visibility".to_owned(), visibility.to_owned()),
                ("commands".to_owned(), commands.to_owned()),
                ("route".to_owned(), route.to_owned()),
            ])
        );
        assert!(!format!("{entry:?}").contains("hunter2"), "{entry:?}");
    }
}

/// An owner holds at most 50 modes, private and shared together; the 51st
/// is 409 `conflict` and nothing is written (S19).
#[sqlx::test(migrations = false)]
async fn an_owner_holds_at_most_fifty_modes(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    for n in 0..49_u8 {
        setup
            .add_mode(n, admin.id, &format!("Mode {n}"), "private", &swinging())
            .await;
    }
    setup
        .add_mode(49, admin.id, "Shared one", "shared", &swinging())
        .await;

    let seen = setup.create(&admin, swinging_body("Fifty-first")).await;

    assert_eq!(
        (seen.status, seen.code()),
        (409, "conflict"),
        "{}",
        seen.text
    );
    assert_eq!(setup.users_modes().await, 50);
    assert_eq!(setup.creations().await, 0);
}

/// A name another of the owner's modes has, or for a shared mode a shared
/// one has, ASCII case ignored, is 422 on `name` (S13).
#[sqlx::test(migrations = false)]
async fn a_taken_name_is_422_on_name(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let other_admin = setup.person(3, "otheradmin", Role::Admin).await;
    setup
        .add_mode(1, admin.id, "Fishing", "private", &swinging())
        .await;
    setup
        .add_mode(2, other_admin.id, "Shared one", "shared", &swinging())
        .await;
    let mut shared = swinging_body("SHARED ONE");
    shared["visibility"] = json!("shared");

    for (body, case) in [
        (swinging_body("fishing"), "the owner's own"),
        (shared, "a shared one's"),
    ] {
        let seen = setup.create(&admin, body).await;
        assert_eq!(seen.status, 422, "{case}: {}", seen.text);
        assert_eq!(
            seen.body["error"]["fields"],
            json!([{ "path": "name", "message": "is already another mode's name" }]),
            "{case}"
        );
    }
    assert_eq!(setup.users_modes().await, 2);
}

/// A name only a mode the caller can't see has is free: the check never
/// probes another user's private modes (S13).
#[sqlx::test(migrations = false)]
async fn a_name_used_only_by_a_mode_the_caller_cannot_see_is_accepted(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let the_owner = setup.person(2, "theowner", Role::Owner).await;
    let member = setup.person(3, "member", Role::Member).await;
    let admin = setup.person(4, "admin", Role::Admin).await;
    setup
        .add_mode(1, the_owner.id, "Secret", "private", &swinging())
        .await;
    let mut shared = swinging_body("secret");
    shared["visibility"] = json!("shared");

    let members = setup.create(&member, swinging_body("SECRET")).await;
    let admins = setup.create(&admin, shared).await;

    assert_eq!(members.status, 201, "{}", members.text);
    assert_eq!(admins.status, 201, "{}", admins.text);
}

/// The transaction asks `authorize()` again: an Admin demoted right
/// before it may no longer create a shared mode, and nothing is written.
#[sqlx::test(migrations = false)]
async fn a_role_lowered_before_the_transaction_is_403_and_writes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let mut body = swinging_body("Shared one");
    body["visibility"] = json!("shared");
    setup
        .hooked
        .queue(Meanwhile::SetRole(admin.id, Role::Member));

    let seen = setup.create(&admin, body).await;

    assert_eq!(
        (seen.status, seen.code()),
        (403, "forbidden"),
        "{}",
        seen.text
    );
    assert_eq!(setup.users_modes().await, 0);
    assert_eq!(setup.creations().await, 0);
}

/// A session revoked right before the transaction is 401, and nothing is
/// written.
#[sqlx::test(migrations = false)]
async fn a_session_revoked_before_the_transaction_is_401_and_writes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = setup.person(2, "member", Role::Member).await;
    setup.hooked.queue(Meanwhile::RevokeSession(member.session));

    let seen = setup.create(&member, swinging_body("Mine")).await;

    assert_eq!(seen.status, 401, "{}", seen.text);
    assert_eq!(setup.users_modes().await, 0);
}

#[sqlx::test(migrations = false)]
async fn creating_spends_the_users_request_limit(
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

    for (n, path) in [MODES, WITH_COMMANDS].into_iter().enumerate() {
        let body = swinging_body(&format!("Mode {n}"));
        let first = setup
            .post(path, Some(&member.token), Some(body.clone()))
            .await;
        let second = setup.post(path, Some(&member.token), Some(body)).await;
        assert_ne!(first.status, 429, "{path}: {}", first.text);
        assert_eq!(second.status, 429, "{path}");
        assert_eq!(second.headers["retry-after"], "1", "{path}");
        setup.clock.advance(core::time::Duration::from_secs(1));
    }
}

/// The service enforces the route rule itself, before anything is read: a
/// mode with a command off the allowlist through the plain route is
/// `WrongRoute`, even for a session that doesn't exist (S16, as S10).
#[sqlx::test(migrations = false)]
async fn a_mode_its_route_doesnt_take_is_refused_before_anything_is_read(
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
            id: fleet_core::id::UserId::new_v7(at(super::support::T0), [0xAB; 10]).unwrap(),
            role: Role::Owner,
        },
        session_id: fleet_core::id::SessionId::new_v7(at(super::support::T0), [0xAC; 10]).unwrap(),
        ip: "192.0.2.10".parse().unwrap(),
    };
    let definition: ModeDefinition = serde_json::from_value(json!({ "steps": [
        { "action": { "type": "send_chat", "message": "/op AfkBot2" }, "schedule": { "type": "at_start" }, "probability": 100 },
    ] }))
    .unwrap();

    let result = service
        .create(
            &nobody,
            ModeRoute::Plain,
            ModeName::try_from("Ops").unwrap(),
            ModeVisibility::Private,
            definition,
        )
        .await;

    assert!(
        matches!(
            result,
            Err(CreateModeError::WrongRoute {
                route: ModeRoute::Plain
            })
        ),
        "{result:?}"
    );
}
