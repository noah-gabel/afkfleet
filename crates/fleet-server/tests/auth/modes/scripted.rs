//! A scripted mode over HTTP (Plan.md P15.20, P15.21; ADR-0036, D13 to
//! D15): who may write one, its rules at their fields, the compile at save
//! (off the async workers, never running a hook), the generic answer to a
//! compiler that fails, the audit entry, what a viewer sees of it, and the
//! 409 that keeps it off an agent that doesn't run its script API level,
//! when a mode is set on a bot and when a mode is edited into a script.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

use fleet_core::audit::AuditAction;
use fleet_core::authz::{CommandAllowlist, Role};
use fleet_core::id::{BotId, ModeId};
use fleet_core::mode::ModePreset;
use fleet_core::script::Script;
use fleet_server::app::modes::Compiler;
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::ports::store::Store as _;
use fleet_testkit::log_capture;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::{CANARY, MODES, metadata, named, swinging, swinging_body};
use crate::support::{Options, Person, Seen, Setup, redacted, setup_entry, setup_with};
use fleet_api_types::modes::{MODE_SCRIPT_STAYS, MODE_SIGNS_UNSEEN};

/// A script that sends `home` when the bot joins.
const SOURCE: &str = "fn on_join(first) {\n    chat.send(\"home\");\n}\n";

/// A new scripted mode named `name` with `visibility` and `source`: a
/// command with the canary as its argument, one with a value, and a chat
/// pattern that counts at any time.
fn scripted_body(name: &str, visibility: &str, source: &str) -> Value {
    json!({ "name": name, "visibility": visibility, "steps": [], "script": {
        "source": source,
        "leash": 8,
        "tool_guard": 2,
        "block_guard": ["cobblestone"],
        "messages": [
            { "key": "home", "text": format!("/home {CANARY}"), "values": [] },
            { "key": "pay", "text": "/pay AfkMain {amount}", "values": [
                { "name": "amount", "type": "decimal", "min": 1, "max": 1000, "places": 2 }
            ] }
        ],
        "patterns": [
            { "key": "restart", "when": "any_time", "text": "Restarting in {m} minutes", "captures": [
                { "name": "m", "type": "integer", "min": 0, "max": 60 }
            ] }
        ]
    } })
}

/// The same as an edit's body at `version`.
fn scripted_edit(version: u32, name: &str, visibility: &str, source: &str) -> Value {
    let mut body = scripted_body(name, visibility, source);
    body["version"] = json!(version);
    body
}

/// What a test's compiler saw: how many scripts, and on which threads.
#[derive(Debug, Clone, Default)]
struct Watched {
    calls: Arc<AtomicUsize>,
    threads: Arc<Mutex<Vec<ThreadId>>>,
}

impl Watched {
    /// fleet-script's compiler, counting its calls and their threads.
    fn compiler(&self) -> Compiler {
        let watched = self.clone();
        Compiler::new(move |script: &Script| {
            watched.calls.fetch_add(1, Ordering::SeqCst);
            watched
                .threads
                .lock()
                .unwrap()
                .push(std::thread::current().id());
            fleet_script::compile_lowest(script).map(|leveled| leveled.program.script().clone())
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

/// The setup with both declared commands on the allowlist, so the plain
/// routes take the mode, and `compiler`.
async fn scripted_setup(options: SqliteConnectOptions, compiler: Compiler) -> Setup {
    scripted_setup_with(options, compiler, &RateLimitConfig::default()).await
}

/// The same with `limits`.
async fn scripted_setup_with(
    options: SqliteConnectOptions,
    compiler: Compiler,
    limits: &RateLimitConfig,
) -> Setup {
    setup_with(
        options,
        Options {
            allowlist: CommandAllowlist::try_new(&["/home", "/pay"]).unwrap(),
            compiler,
            limits: *limits,
            ..Options::default()
        },
    )
    .await
}

impl Setup {
    /// `PUT /modes/{id}` with `body` as `by`.
    async fn put_mode(&self, by: &Person, id: ModeId, body: Value) -> Seen {
        self.request(
            axum::http::Method::PUT,
            &format!("{MODES}/{id}"),
            Some(&by.token),
            Some(body),
        )
        .await
    }

    /// The scripted mode `name` of `admin`, created through the API with
    /// `visibility`.
    async fn scripted_mode(&self, admin: &Person, name: &str, visibility: &str) -> ModeId {
        let seen = self
            .post(
                MODES,
                Some(&admin.token),
                Some(scripted_body(name, visibility, SOURCE)),
            )
            .await;
        assert_eq!(seen.status, 201, "{}", seen.text);
        seen.body["id"].as_str().unwrap().parse().unwrap()
    }

    /// The mode `bot` runs, as stored.
    async fn mode_of(&self, bot: BotId) -> ModeId {
        self.db.bots().find(bot).await.unwrap().unwrap().mode
    }

    /// Switches `bot` to `mode` in the database, as a user's call would.
    async fn on_mode(&self, bot: BotId, mode: ModeId) {
        let mut tx = self.db.write().await.unwrap();
        assert!(tx.bots().set_mode(bot, mode).await.unwrap());
        tx.commit(setup_entry()).await.unwrap();
    }

    /// The entries of `action`.
    async fn entries_of(
        &self,
        action: &AuditAction,
    ) -> Vec<fleet_server::ports::audit::AuditRecord> {
        self.audit()
            .await
            .into_iter()
            .filter(|entry| entry.action == *action)
            .collect()
    }

    /// How many modes the users own.
    async fn owned_modes(&self) -> i64 {
        sqlx::query_scalar!(
            r#"SELECT count(*) AS "count!: i64" FROM modes WHERE owner_user_id IS NOT NULL"#
        )
        .fetch_one(self.db.read_pool())
        .await
        .unwrap()
    }
}

// --- Who may write one (ADR-0036, D15) --------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_scripted_mode_needs_a_session_and_is_never_compiled_without_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let watched = Watched::default();
    let setup = scripted_setup(options, watched.compiler()).await;

    let created = setup
        .post(MODES, None, Some(scripted_body("Mine", "private", SOURCE)))
        .await;
    let edited = setup
        .request(
            axum::http::Method::PUT,
            &format!("{MODES}/{}", ModePreset::Afk.id()),
            None,
            Some(scripted_edit(1, "Mine", "private", SOURCE)),
        )
        .await;

    assert_eq!(created.status, 401, "{}", created.text);
    assert_eq!(edited.status, 401, "{}", edited.text);
    assert_eq!(watched.calls(), 0);
}

/// A Member writes no scripted mode, private or shared, on either route:
/// a 403 that no rule and no compiler saw first, so even a script that
/// isn't a program, or breaks a rule, is the 403 (ADR-0036, D15).
#[sqlx::test(migrations = false)]
async fn a_member_may_not_create_a_scripted_mode_and_its_script_is_never_read(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let watched = Watched::default();
    let setup = scripted_setup(options, watched.compiler()).await;
    let member = setup.person(2, "member", Role::Member).await;
    let mut broken_rule = scripted_body("Mine", "private", SOURCE);
    broken_rule["script"]["leash"] = json!(0);
    let bodies = [
        scripted_body("Mine", "private", SOURCE),
        scripted_body("Mine", "shared", SOURCE),
        scripted_body("Mine", "private", "fn on_join(first) {"),
        broken_rule,
    ];

    for body in bodies {
        for path in [MODES, "/api/v1/modes/with-commands"] {
            let seen = setup
                .post(path, Some(&member.token), Some(body.clone()))
                .await;
            assert_eq!(
                (seen.status, seen.code()),
                (403, "forbidden"),
                "{path}: {}",
                seen.text
            );
        }
    }
    assert_eq!(watched.calls(), 0, "never compiled");
    assert_eq!(setup.owned_modes().await, 0);
}

/// A Member's own step mode doesn't become a script: 403, the mode as it
/// was, its script never read (ADR-0036, D15).
#[sqlx::test(migrations = false)]
async fn a_member_may_not_turn_their_step_mode_into_a_script(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let watched = Watched::default();
    let setup = scripted_setup(options, watched.compiler()).await;
    let member = setup.person(2, "member", Role::Member).await;
    let id = setup
        .add_mode(1, member.id, "Mine", "private", &swinging())
        .await;
    let before = setup.db.modes().find(id).await.unwrap();

    let seen = setup
        .put_mode(&member, id, scripted_edit(1, "Mine", "private", SOURCE))
        .await;

    assert_eq!(
        (seen.status, seen.code()),
        (403, "forbidden"),
        "{}",
        seen.text
    );
    assert_eq!(setup.db.modes().find(id).await.unwrap(), before);
    assert_eq!(watched.calls(), 0);
    assert_eq!(setup.entries_of(&AuditAction::MODE_UPDATE).await, []);
}

/// An Admin and the Owner write one, private or shared: 201 with the
/// script as its creator sees it, and an entry that says its body is a
/// script, how many patterns count at any time and its level, never its
/// text (ADR-0036, D15; ADR-0037, C5).
#[sqlx::test(migrations = false)]
async fn an_admins_scripted_mode_is_201_with_its_script_and_audited_by_its_body(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let watched = Watched::default();
    let setup = scripted_setup(options, watched.compiler()).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let owner = setup.person(3, "owner", Role::Owner).await;

    let seen = setup
        .post(
            MODES,
            Some(&admin.token),
            Some(scripted_body("Balance", "shared", SOURCE)),
        )
        .await;
    let owners = setup
        .post(
            MODES,
            Some(&owner.token),
            Some(scripted_body("Mine", "private", SOURCE)),
        )
        .await;
    let steps = setup
        .post(MODES, Some(&admin.token), Some(swinging_body("Swing")))
        .await;

    assert_eq!(seen.status, 201, "{}", seen.text);
    assert_eq!(owners.status, 201, "{}", owners.text);
    assert_eq!(steps.status, 201, "{}", steps.text);
    let id = seen.body["id"].as_str().unwrap().to_owned();
    insta::assert_json_snapshot!(
        "scripted_mode_created",
        redacted(&seen.body, &[(id.clone(), "the mode")])
    );
    assert_eq!(watched.calls(), 2, "one compile a scripted mode");
    let entries = setup.entries_of(&AuditAction::MODE_CREATE).await;
    let bodies: Vec<_> = entries.iter().map(metadata).collect();
    let scripted = bodies
        .iter()
        .find(|entry| entry.get("visibility").map(String::as_str) == Some("shared"))
        .unwrap();
    assert_eq!(scripted["body"], "script");
    assert_eq!(scripted["any_time_patterns"], "1");
    assert_eq!(scripted["script_api"], "1");
    assert_eq!(
        bodies
            .iter()
            .filter(|entry| entry["body"] == "steps")
            .map(|entry| entry.contains_key("script_api"))
            .collect::<Vec<_>>(),
        [false],
        "a step mode's entry says steps and nothing of a script"
    );
    for entry in &bodies {
        for value in entry.values() {
            assert!(
                !value.contains(CANARY) && !value.contains("chat.send"),
                "{entry:?}"
            );
        }
    }
}

/// Below the right to edit a shared scripted mode, a viewer sees each
/// declared command's `/` and name alone (S15): the canary never reaches a
/// Member, and the Admin who wrote it sees it whole.
#[sqlx::test(migrations = false)]
async fn a_member_sees_a_shared_scripts_commands_without_their_arguments(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = scripted_setup(options, Compiler::fleet_script()).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let member = setup.person(3, "member", Role::Member).await;
    setup.scripted_mode(&admin, "Balance", "shared").await;

    let members = setup.modes(&member, "").await;
    let admins = setup.modes(&admin, "").await;

    assert!(!members.text.contains(CANARY), "{}", members.text);
    let seen = &named(&members.body, "Balance")["script"];
    assert_eq!(
        seen["messages"][0],
        json!({ "key": "home", "text": "/home", "values": [], "arguments_hidden": true })
    );
    assert_eq!(seen["any_time_chat"], true);
    let whole = &named(&admins.body, "Balance")["script"]["messages"][0];
    assert_eq!(whole["text"], format!("/home {CANARY}"));
    assert_eq!(whole["arguments_hidden"], false);
}

// --- Its rules and the compile (ADR-0036, D14) ------------------------------------------

/// An app of the last release reads a scripted mode as one without steps,
/// and saving it there sends steps and no script: the server refuses an
/// edit that turns a scripted mode into steps, so its script is never lost,
/// and writes nothing (ADR-0046, C6).
#[sqlx::test(migrations = false)]
async fn an_edit_never_turns_a_scripted_mode_into_steps_as_an_older_app_would_save_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = scripted_setup(options, Compiler::fleet_script()).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let id = setup.scripted_mode(&admin, "Balance", "private").await;
    let before = setup.db.modes().find(id).await.unwrap();
    let as_an_older_app_saves_it =
        json!({ "version": 1, "name": "Balance", "visibility": "private", "steps": [] });
    let with_steps = {
        let mut body = swinging_body("Balance");
        body["version"] = json!(1);
        body
    };

    for body in [as_an_older_app_saves_it, with_steps] {
        let seen = setup.put_mode(&admin, id, body).await;

        assert_eq!(seen.status, 422, "{}", seen.text);
        let fields = seen.body["error"]["fields"].as_array().unwrap();
        assert_eq!(
            (
                fields.len(),
                fields[0]["path"].clone(),
                fields[0]["message"].clone()
            ),
            (1, json!("steps"), json!(MODE_SCRIPT_STAYS)),
            "{}",
            seen.text
        );
    }
    assert_eq!(setup.db.modes().find(id).await.unwrap(), before);
    assert_eq!(setup.entries_of(&AuditAction::MODE_UPDATE).await, []);
}

/// A text that isn't a program is one 422 at `script.source`, with Rhai's
/// message about the caller's own script, its line and its column; nothing
/// is written.
#[sqlx::test(migrations = false)]
async fn a_script_that_isnt_a_program_is_422_at_its_source_with_its_line_and_column(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = scripted_setup(options, Compiler::fleet_script()).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let source = "fn on_join(first) {\n    let x = ;\n}\n";
    let request: fleet_api_types::script::ModeScriptRequest =
        serde_json::from_value(scripted_body("Broken", "private", source)["script"].clone())
            .unwrap();
    let expected = fleet_script::compile(&request.script().unwrap()).unwrap_err();
    let at = expected.at.unwrap();

    let seen = setup
        .post(
            MODES,
            Some(&admin.token),
            Some(scripted_body("Broken", "private", source)),
        )
        .await;

    assert_eq!(seen.status, 422, "{}", seen.text);
    assert_eq!(
        seen.body["error"]["fields"],
        json!([{
            "path": "script.source",
            "message": expected.kind.to_string(),
            "line": at.line,
            "column": at.column,
        }])
    );
    assert_eq!(at.line, 2, "the line of the author's own mistake");
    assert_eq!(setup.owned_modes().await, 0);
    assert_eq!(setup.entries_of(&AuditAction::MODE_CREATE).await, []);
}

/// fleet-core's rules on a script's body are each one 422 at its own field,
/// with the place in a text where one is known, and the compiler never sees
/// a body that breaks one; steps beside a script are 422 at `steps`.
#[sqlx::test(migrations = false)]
async fn a_scripts_rule_is_422_at_its_field_before_any_compile(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let watched = Watched::default();
    let setup = scripted_setup(options, watched.compiler()).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let with = |change: &dyn Fn(&mut Value)| {
        let mut body = scripted_body("Fine", "private", SOURCE);
        change(&mut body);
        body
    };
    let cases = [
        (
            with(&|body| body["script"]["leash"] = json!(0)),
            "script.leash",
            None,
        ),
        (
            with(&|body| body["script"]["messages"][1]["key"] = json!("home")),
            "script.messages[1].key",
            None,
        ),
        (
            with(&|body| {
                body["script"]["source"] = json!("fn on_join(first) {\n  x\u{202e}\n}");
            }),
            "script.source",
            Some((2, 4)),
        ),
        (
            with(&|body| {
                body["steps"] =
                    serde_json::from_str::<Value>(&swinging()).unwrap()["steps"].clone();
            }),
            "steps",
            None,
        ),
    ];

    for (body, path, place) in cases {
        let seen = setup.post(MODES, Some(&admin.token), Some(body)).await;
        assert_eq!(seen.status, 422, "{path}: {}", seen.text);
        let fields = seen.body["error"]["fields"].as_array().unwrap();
        assert_eq!(fields.len(), 1, "{path}: {}", seen.text);
        assert_eq!(fields[0]["path"], path, "{}", seen.text);
        if let Some((line, column)) = place {
            assert_eq!(
                (fields[0]["line"].clone(), fields[0]["column"].clone()),
                (json!(line), json!(column))
            );
        }
    }
    assert_eq!(watched.calls(), 0, "never compiled");
    assert_eq!(setup.owned_modes().await, 0);
}

/// Saving only compiles: a hook that would never end is never run, so the
/// mode is saved at once (ADR-0036, D14).
#[sqlx::test(migrations = false)]
async fn a_script_whose_hook_never_ends_is_saved_since_saving_runs_no_hook(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = scripted_setup(options, Compiler::fleet_script()).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let source = "fn on_join(first) {\n    loop { }\n}\n";

    let seen = tokio::time::timeout(
        core::time::Duration::from_secs(30),
        setup.post(
            MODES,
            Some(&admin.token),
            Some(scripted_body("Forever", "private", source)),
        ),
    )
    .await
    .expect("saving never runs the hook");

    assert_eq!(seen.status, 201, "{}", seen.text);
}

/// The compile runs on a blocking thread, never on the request's own,
/// which runs the async workers' tasks (CLAUDE.md, ADR-0036, D14).
#[sqlx::test(migrations = false)]
async fn a_script_is_compiled_off_the_requests_thread(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let watched = Watched::default();
    let setup = scripted_setup(options, watched.compiler()).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let id = setup.scripted_mode(&admin, "Mine", "private").await;

    let edited = setup
        .put_mode(
            &admin,
            id,
            scripted_edit(1, "Mine", "private", "fn on_join(first) {}\n"),
        )
        .await;

    assert_eq!(edited.status, 200, "{}", edited.text);
    let threads = watched.threads.lock().unwrap().clone();
    assert_eq!(threads.len(), 2, "the create's and the edit's");
    // `#[sqlx::test]` runs its runtime on the test's own thread.
    assert!(
        threads
            .iter()
            .all(|thread| *thread != std::thread::current().id()),
        "{threads:?}"
    );
}

/// A compiler that fails is an internal fault, never the script's: the
/// generic 500 with its request ID, nothing written, nothing of what it
/// said in the answer.
#[sqlx::test(migrations = false)]
async fn a_compiler_that_fails_is_the_generic_500_with_a_request_id_and_writes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    log_capture::install().unwrap();
    let compiler = Compiler::new(|_| panic!("the compiler broke at {CANARY}"));
    let setup = scripted_setup(options, compiler).await;
    let admin = setup.person(2, "admin", Role::Admin).await;

    let seen = setup
        .post(
            MODES,
            Some(&admin.token),
            Some(scripted_body("Mine", "private", SOURCE)),
        )
        .await;

    assert_eq!(
        (seen.status, seen.code()),
        (500, "internal"),
        "{}",
        seen.text
    );
    assert!(
        seen.body["error"]["request_id"].is_string(),
        "{}",
        seen.text
    );
    assert!(!seen.text.contains(CANARY), "{}", seen.text);
    assert!(!seen.text.contains("compiler"), "{}", seen.text);
    assert_eq!(setup.owned_modes().await, 0);
    assert_eq!(setup.entries_of(&AuditAction::MODE_CREATE).await, []);
}

/// Writing a scripted mode spends the user's request limit as any mode
/// does: the next is 429 and its script is never compiled.
#[sqlx::test(migrations = false)]
async fn writing_a_scripted_mode_spends_the_users_request_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let one = Rate::per_second(core::num::NonZeroU32::MIN, core::num::NonZeroU32::MIN);
    let watched = Watched::default();
    let setup = scripted_setup_with(
        options,
        watched.compiler(),
        &RateLimitConfig {
            per_user: one,
            ..RateLimitConfig::default()
        },
    )
    .await;
    let admin = setup.person(2, "admin", Role::Admin).await;

    let first = setup
        .post(
            MODES,
            Some(&admin.token),
            Some(scripted_body("One", "private", SOURCE)),
        )
        .await;
    let second = setup
        .post(
            MODES,
            Some(&admin.token),
            Some(scripted_body("Two", "private", SOURCE)),
        )
        .await;
    setup.clock.advance(core::time::Duration::from_secs(1));
    let id: ModeId = first.body["id"].as_str().unwrap().parse().unwrap();
    let edit = setup
        .put_mode(&admin, id, scripted_edit(1, "One", "private", SOURCE))
        .await;
    let edit_again = setup
        .put_mode(&admin, id, scripted_edit(1, "One", "private", SOURCE))
        .await;

    assert_eq!(first.status, 201, "{}", first.text);
    assert_eq!(second.status, 429, "{}", second.text);
    assert_eq!(second.headers["retry-after"], "1");
    assert_ne!(edit.status, 429, "{}", edit.text);
    assert_eq!(edit_again.status, 429, "{}", edit_again.text);
    assert_eq!(
        watched.calls(),
        2,
        "the two that got through, never a refused one"
    );
}

// --- Never to an agent below its level (ADR-0036, D13) ----------------------------------

/// A scripted mode for a bot whose agent's live stream runs no scripts (a
/// hello without a level) is 409 `agent_too_old`: the bot keeps its mode
/// and nothing is written.
#[sqlx::test(migrations = false)]
async fn a_scripted_mode_for_a_bot_on_an_agent_before_scripts_is_409_and_changes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = scripted_setup(options, Compiler::fleet_script()).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let mode = setup.scripted_mode(&admin, "Balance", "private").await;
    let (_, bot) = setup.bot_of(admin.id, 1).await;
    setup.assigned(bot, 1, None).await;
    let _stream = setup.holding_at(1, 0, &[bot]);
    setup.hints.take();

    let seen = setup.set_mode(bot, mode, &admin).await;

    assert_eq!(
        (seen.status, seen.code()),
        (409, "agent_too_old"),
        "{}",
        seen.text
    );
    assert_eq!(setup.mode_of(bot).await, ModePreset::Afk.id());
    assert_eq!(setup.entries_of(&AuditAction::BOT_SET_MODE).await, []);
    assert_eq!(setup.hints.take(), []);
}

/// A scripted mode goes to a bot whose agent runs its level, and to one
/// whose agent is offline: the reconciler holds that one to its level at
/// its next hello. A step mode goes to any agent, and a Member sets a
/// shared scripted mode on a bot they control (ADR-0036, D13, D15).
#[sqlx::test(migrations = false)]
async fn a_scripted_mode_goes_to_a_bot_on_an_agent_at_its_level_or_offline(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = scripted_setup(options, Compiler::fleet_script()).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let member = setup.person(3, "member", Role::Member).await;
    let mode = setup.scripted_mode(&admin, "Balance", "shared").await;
    let steps = setup
        .add_mode(1, admin.id, "Swing", "shared", &swinging())
        .await;
    let (_, current) = setup.bot_of(admin.id, 1).await;
    let (_, offline) = setup.bot_of(admin.id, 2).await;
    let (_, old) = setup.bot_of(admin.id, 3).await;
    let (_, members) = setup.bot_of(member.id, 4).await;
    setup.assigned(current, 1, None).await;
    setup.assigned(offline, 2, None).await;
    setup.assigned(old, 3, None).await;
    let _current = setup.holding_at(1, 1, &[current]);
    let _old = setup.holding_at(3, 0, &[old]);

    let seen = [
        setup.set_mode(current, mode, &admin).await,
        setup.set_mode(offline, mode, &admin).await,
        setup.set_mode(old, steps, &admin).await,
        setup.set_mode(members, mode, &member).await,
    ];

    for answer in &seen {
        assert_eq!(answer.status, 200, "{}", answer.text);
    }
    assert_eq!(setup.mode_of(current).await, mode);
    assert_eq!(setup.mode_of(offline).await, mode);
    assert_eq!(setup.mode_of(old).await, steps);
    assert_eq!(setup.mode_of(members).await, mode);
}

/// An edit that makes a mode a script's while a bot that uses it is on an
/// agent whose live stream runs no scripts is 409 `agent_too_old`: the
/// mode as it was, no entry, no hint.
#[sqlx::test(migrations = false)]
async fn an_edit_into_a_script_while_a_bot_on_it_is_on_an_agent_before_scripts_is_409(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = scripted_setup(options, Compiler::fleet_script()).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let id = setup
        .add_mode(1, admin.id, "Swing", "private", &swinging())
        .await;
    let (_, bot) = setup.bot_of(admin.id, 1).await;
    setup.on_mode(bot, id).await;
    setup.assigned(bot, 1, None).await;
    let _stream = setup.holding_at(1, 0, &[bot]);
    let before = setup.db.modes().find(id).await.unwrap();
    setup.hints.take();

    let seen = setup
        .put_mode(&admin, id, scripted_edit(1, "Swing", "private", SOURCE))
        .await;

    assert_eq!(
        (seen.status, seen.code()),
        (409, "agent_too_old"),
        "{}",
        seen.text
    );
    assert_eq!(setup.db.modes().find(id).await.unwrap(), before);
    assert_eq!(setup.entries_of(&AuditAction::MODE_UPDATE).await, []);
    assert_eq!(setup.hints.take(), []);
}

/// The same edit goes through when every bot on the mode is on an agent
/// at its level or offline.
#[sqlx::test(migrations = false)]
async fn an_edit_into_a_script_goes_through_when_its_bots_agents_run_it_or_are_offline(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = scripted_setup(options, Compiler::fleet_script()).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let id = setup
        .add_mode(1, admin.id, "Swing", "private", &swinging())
        .await;
    let (_, current) = setup.bot_of(admin.id, 1).await;
    let (_, offline) = setup.bot_of(admin.id, 2).await;
    for (bot, agent) in [(current, 1), (offline, 2)] {
        setup.on_mode(bot, id).await;
        setup.assigned(bot, agent, None).await;
    }
    let _stream = setup.holding_at(1, 1, &[current]);

    let seen = setup
        .put_mode(&admin, id, scripted_edit(1, "Swing", "private", SOURCE))
        .await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    assert_eq!(seen.body["script"]["api"], 1);
    let stored = setup.db.modes().find(id).await.unwrap().unwrap();
    assert!(stored.definition.script().is_some());
    let entries = setup.entries_of(&AuditAction::MODE_UPDATE).await;
    assert_eq!(metadata(&entries[0])["body"], "script");
}

/// The allowlist reads a script's declared messages as it reads a step's
/// chat: a declared command off it is pointed to the twin, which takes it,
/// and a bot is switched to such a mode only with Manage (S16, P15.21).
#[sqlx::test(migrations = false)]
async fn a_declared_command_off_the_allowlist_is_pointed_to_the_twin(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let watched = Watched::default();
    let setup = scripted_setup(options, watched.compiler()).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let member = setup.person(3, "member", Role::Member).await;
    let mut body = scripted_body("Ops", "shared", SOURCE);
    body["script"]["messages"][1] = json!({ "key": "pay", "text": "/op AfkBot2", "values": [] });

    let plain = setup
        .post(MODES, Some(&admin.token), Some(body.clone()))
        .await;
    let twin = setup
        .post(
            "/api/v1/modes/with-commands",
            Some(&admin.token),
            Some(body),
        )
        .await;

    assert_eq!(
        (plain.status, plain.code()),
        (403, "mode_needs_step_up"),
        "{}",
        plain.text
    );
    assert_eq!(twin.status, 201, "{}", twin.text);
    assert_eq!(twin.body["unlisted_command"], true);
    assert_eq!(watched.calls(), 1, "only the twin's was compiled");
    let mode: ModeId = twin.body["id"].as_str().unwrap().parse().unwrap();
    let (account, bot) = setup.bot_of(admin.id, 1).await;
    setup
        .share(
            account,
            member.id,
            fleet_core::authz::GrantLevel::Control,
            admin.id,
        )
        .await;
    let controlled = setup.set_mode(bot, mode, &member).await;
    assert_eq!(controlled.status, 403, "{}", controlled.text);
}

// --- The level a script needs, and its signs (Plan.md P16.15; ADR-0052, G1) -------------

/// A script that opens the auction house's menu: level 2.
const MENUS: &str = "fn on_menu_open() {\n    menu.close();\n}\n";

/// A scripted body with `source` and a sign whose first line is its price
/// alone: as this app sends it, `signs` always there.
fn signed_body(name: &str, source: &str) -> Value {
    let mut body = scripted_body(name, "private", source);
    body["script"]["signs"] = json!([
        { "key": "price", "lines": ["{price}", "", "afk"], "values": [
            { "name": "price", "type": "integer", "min": 1, "max": 1_000_000 }
        ] }
    ]);
    body
}

/// The script a stored mode holds.
async fn stored_script(setup: &Setup, id: ModeId) -> Script {
    setup
        .db
        .modes()
        .find(id)
        .await
        .unwrap()
        .unwrap()
        .definition
        .script()
        .unwrap()
        .clone()
}

/// The server compiles a script at the lowest level that runs it and
/// stamps that one: a farm stays at level 1, so an agent of the last
/// release still runs it; a script that uses menus or declares a sign is
/// level 2 (Plan.md P16.15).
#[sqlx::test(migrations = false)]
async fn a_script_is_stamped_at_save_with_the_lowest_level_that_runs_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = scripted_setup(options, Compiler::fleet_script()).await;
    let admin = setup.person(2, "admin", Role::Admin).await;

    let farm = setup
        .post(
            MODES,
            Some(&admin.token),
            Some(scripted_body("Holder", "private", SOURCE)),
        )
        .await;
    let menus = setup
        .post(
            MODES,
            Some(&admin.token),
            Some(scripted_body("Menus", "private", MENUS)),
        )
        .await;
    let signed = setup
        .post(
            MODES,
            Some(&admin.token),
            Some(signed_body("Signed", SOURCE)),
        )
        .await;

    for (seen, level) in [(&farm, 1), (&menus, 2), (&signed, 2)] {
        assert_eq!(seen.status, 201, "{}", seen.text);
        assert_eq!(seen.body["script"]["api"], level, "{}", seen.text);
        let id: ModeId = seen.body["id"].as_str().unwrap().parse().unwrap();
        assert_eq!(stored_script(&setup, id).await.api().get(), level);
    }
    assert_eq!(
        signed.body["script"]["signs"],
        json!([{ "key": "price", "lines": ["{price}", "", "afk"], "values": [
            { "name": "price", "type": "integer", "min": 1, "max": 1_000_000 }
        ] }])
    );
    assert_eq!(farm.body["script"]["signs"], json!([]));
}

/// An app of the last release (0.2.1) doesn't know a mode's signs: it
/// reads a level-2 mode without them and saves it without `signs`. That
/// save is refused at `script.signs`, so the signs are never lost, and
/// nothing is written (Plan.md P16.15). Its save of a mode without signs
/// goes through as before, and this app's `[]` takes them away.
#[sqlx::test(migrations = false)]
async fn an_older_apps_save_without_signs_of_a_mode_that_declares_them_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = scripted_setup(options, Compiler::fleet_script()).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let created = setup
        .post(
            MODES,
            Some(&admin.token),
            Some(signed_body("Signed", SOURCE)),
        )
        .await;
    assert_eq!(created.status, 201, "{}", created.text);
    let id: ModeId = created.body["id"].as_str().unwrap().parse().unwrap();
    let plain = setup.scripted_mode(&admin, "Plain", "private").await;
    let before = setup.db.modes().find(id).await.unwrap();
    // The last release's request shape: its script has no `signs`.
    let older = scripted_edit(1, "Signed", "private", "fn on_join(first) {\n}\n");
    assert!(older["script"].get("signs").is_none());

    let refused = setup.put_mode(&admin, id, older.clone()).await;
    let after_refusal = setup.db.modes().find(id).await.unwrap();
    let older_plain = setup
        .put_mode(
            &admin,
            plain,
            scripted_edit(1, "Plain", "private", "fn on_join(first) {\n}\n"),
        )
        .await;
    let mut cleared = older;
    cleared["script"]["signs"] = json!([]);
    let this_app = setup.put_mode(&admin, id, cleared).await;

    assert_eq!(refused.status, 422, "{}", refused.text);
    let fields = refused.body["error"]["fields"].as_array().unwrap();
    assert_eq!(
        (
            fields.len(),
            fields[0]["path"].clone(),
            fields[0]["message"].clone()
        ),
        (1, json!("script.signs"), json!(MODE_SIGNS_UNSEEN)),
        "{}",
        refused.text
    );
    assert_eq!(after_refusal, before);
    assert_eq!(older_plain.status, 200, "{}", older_plain.text);
    assert_eq!(this_app.status, 200, "{}", this_app.text);
    assert_eq!(this_app.body["script"]["signs"], json!([]));
    assert_eq!(this_app.body["script"]["api"], 1);
    let updates = setup.entries_of(&AuditAction::MODE_UPDATE).await;
    assert_eq!(updates.len(), 2, "the two saves that went through");
}

/// A level-2 mode never goes to a bot whose agent runs level 1, as the
/// last release's does: 409 `agent_too_old`. A level-1 script still does.
#[sqlx::test(migrations = false)]
async fn a_level_2_mode_is_409_for_a_bot_on_an_agent_of_the_last_release_and_a_level_1_goes(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = scripted_setup(options, Compiler::fleet_script()).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let farm = setup.scripted_mode(&admin, "Holder", "private").await;
    let menus = setup
        .post(
            MODES,
            Some(&admin.token),
            Some(scripted_body("Menus", "private", MENUS)),
        )
        .await;
    let menus: ModeId = menus.body["id"].as_str().unwrap().parse().unwrap();
    let (_, bot) = setup.bot_of(admin.id, 1).await;
    setup.assigned(bot, 1, None).await;
    let _stream = setup.holding_at(1, 1, &[bot]);

    let refused = setup.set_mode(bot, menus, &admin).await;
    let kept = setup.mode_of(bot).await;
    let taken = setup.set_mode(bot, farm, &admin).await;

    assert_eq!(
        (refused.status, refused.code()),
        (409, "agent_too_old"),
        "{}",
        refused.text
    );
    assert_eq!(kept, ModePreset::Afk.id());
    assert_eq!(taken.status, 200, "{}", taken.text);
}
