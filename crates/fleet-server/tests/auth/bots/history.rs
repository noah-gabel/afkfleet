//! Tests for a bot's chat history (Plan.md P11.3, Appendix B; ADR-0020, S6,
//! S9, S11, U3, U5): `GET /api/v1/bots/{id}/chat` pages the bot's chat newest
//! first for anyone who may see the bot, in the extractors' order (401, a
//! malformed path's 404, the query's 400/422, an unseen bot's 404), and
//! builds each line for its viewer: a command's arguments only for Manage
//! holders, who sent a user's line only for viewers who see the account's
//! owner (and only while that user exists), `mine` on the viewer's own lines,
//! and a received line break as ` | `. Time isn't paused (sqlx).

use std::collections::BTreeMap;

use fleet_core::authz::{GrantLevel, Role};
use fleet_core::chat::{ChatKind, ChatMessage, IncomingChat, PlayerChatKind};
use fleet_core::id::{AccountId, AgentId, BotId, UserId};
use fleet_server::ports::chat::{ChatEntry, NewChatMessage};
use fleet_server::ports::store::Store as _;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::super::support::{Person, Seen, Setup, T0, at, redacted, setup, setup_entry};
use super::BOTS;

/// The argument planted in commands, which no viewer below Manage may read.
const CANARY: &str = "hunter2-secret";

/// A received player's UUID: an invented one.
const STEVE: &str = "00000000-0000-4000-8000-000000000001";

fn history_path(bot: BotId, query: &str) -> String {
    format!("{BOTS}/{bot}/chat{query}")
}

fn chat(name: &str, uuid: Option<&str>, text: &str) -> ChatEntry {
    ChatEntry::Received(IncomingChat::player(
        PlayerChatKind::Chat,
        name,
        uuid.map(|uuid| uuid.parse().unwrap()),
        text,
    ))
}

fn system(text: &str) -> ChatEntry {
    ChatEntry::Received(IncomingChat::system(text))
}

fn mode(text: &str) -> ChatEntry {
    ChatEntry::ModeSent(ChatMessage::try_from(text).unwrap())
}

fn user(text: &str, by: UserId) -> ChatEntry {
    ChatEntry::UserSent {
        message: ChatMessage::try_from(text).unwrap(),
        by: Some(by),
    }
}

impl Setup {
    /// Appends `entries` to `bot`'s chat, oldest first, a second apart, as
    /// `agent`, which runs the bot, reported or sent them.
    async fn said(&self, bot: BotId, agent: AgentId, entries: Vec<ChatEntry>) {
        let mut tx = self.db.write().await.unwrap();
        for (n, entry) in (1_i64..).zip(entries) {
            let stored = tx
                .chat()
                .append(&NewChatMessage {
                    bot,
                    agent,
                    at: at(T0 + n * 1000),
                    entry,
                })
                .await
                .unwrap();
            assert!(stored.is_some());
        }
        tx.commit(setup_entry()).await.unwrap();
    }

    /// `owner`'s bot `n`, assigned to agent `n`, with its agent.
    async fn bot_with_agent(&self, owner: UserId, n: u8) -> (AccountId, BotId, AgentId) {
        let (account, bot) = self.bot_of(owner, n).await;
        let agent = self.assigned(bot, n, None).await;
        (account, bot, agent)
    }

    async fn history(&self, bot: BotId, by: &Person, query: &str) -> Seen {
        self.get(&history_path(bot, query), Some(&by.token)).await
    }
}

/// The texts of a page's lines, newest first.
fn texts(page: &Value) -> Vec<&str> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line["text"].as_str().unwrap())
        .collect()
}

// --- Who may read it, and the query -----------------------------------------------------

#[sqlx::test(migrations = false)]
async fn the_history_needs_a_session(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;

    let seen = setup.get(&history_path(bot, ""), None).await;

    assert_eq!(seen.status, 401, "{}", seen.text);
}

#[sqlx::test(migrations = false)]
async fn an_unknown_a_malformed_or_an_unseen_bots_history_is_404(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let stranger = setup.person(3, "stranger", Role::Member).await;
    let (_, bot, agent) = setup.bot_with_agent(owner.id, 10).await;
    setup
        .said(bot, agent, vec![system("only the owner reads this")])
        .await;
    let unknown = BotId::new_v7(at(T0), [0xAB; 10]).unwrap();

    let seen = [
        setup.history(bot, &stranger, "").await,
        setup.history(unknown, &owner, "").await,
        setup
            .get(&format!("{BOTS}/not-an-id/chat"), Some(&owner.token))
            .await,
    ];

    for seen in &seen {
        assert_eq!(seen.status, 404, "{}", seen.text);
        assert_eq!(seen.code(), "not_found");
        assert!(!seen.text.contains("only the owner"), "{}", seen.text);
    }
}

/// A query that doesn't parse is 400, and one out of range 422 on its
/// field, both before the bot is looked up: a stranger gets them too.
#[sqlx::test(migrations = false)]
async fn a_query_that_doesnt_parse_is_400_and_one_out_of_range_422(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let stranger = setup.person(3, "stranger", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;
    let unparsed = [
        "?before=x",
        "?before=9007199254740992",
        "?before=1.5",
        "?page=2",
        "?limit=1&limit=2",
    ];
    let out_of_range = [
        ("?limit=0", "limit", "lower than 1"),
        ("?limit=101", "limit", "greater than 100"),
        ("?before=0", "before", "lower than 1"),
        ("?before=-1", "before", "lower than 1"),
    ];

    for query in unparsed {
        for by in [&owner, &stranger] {
            let seen = setup.history(bot, by, query).await;
            assert_eq!(seen.status, 400, "{query}: {}", seen.text);
        }
    }
    for (query, field, rule) in out_of_range {
        for by in [&owner, &stranger] {
            let seen = setup.history(bot, by, query).await;
            assert_eq!(seen.status, 422, "{query}: {}", seen.text);
            assert_eq!(seen.body["error"]["fields"][0]["path"], field, "{query}");
            assert_eq!(seen.body["error"]["fields"][0]["message"], rule, "{query}");
        }
    }
}

// --- The pages --------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn the_history_pages_newest_first_before_its_cursor_and_ends_without_next(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot, agent) = setup.bot_with_agent(owner.id, 10).await;
    let (_, other, other_agent) = setup.bot_with_agent(owner.id, 11).await;
    setup
        .said(
            bot,
            agent,
            vec![system("one"), system("two"), system("three")],
        )
        .await;
    setup
        .said(other, other_agent, vec![system("another bot's")])
        .await;

    let first = setup.history(bot, &owner, "?limit=2").await;
    assert_eq!(first.status, 200, "{}", first.text);
    assert_eq!(texts(&first.body), ["three", "two"]);
    let next = first.body["next"].as_i64();
    assert!(next.is_some(), "{}", first.text);
    let second = setup
        .history(
            bot,
            &owner,
            &format!("?limit=2&before={}", next.unwrap_or_default()),
        )
        .await;
    let empty = setup.history(bot, &owner, "?before=1").await;

    assert_eq!(texts(&second.body), ["one"]);
    assert_eq!(second.body.get("next"), None, "the last page");
    assert_eq!(texts(&empty.body), Vec::<&str>::new());
    assert_eq!(empty.body.get("next"), None);
}

#[sqlx::test(migrations = false)]
async fn a_page_holds_50_lines_unless_its_limit_says_otherwise(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot, agent) = setup.bot_with_agent(owner.id, 10).await;
    setup
        .said(
            bot,
            agent,
            (1..=101).map(|n| system(&format!("line {n}"))).collect(),
        )
        .await;

    let default = setup.history(bot, &owner, "").await;
    let most = setup.history(bot, &owner, "?limit=100").await;

    assert_eq!(texts(&default.body).len(), 50);
    assert_eq!(texts(&default.body)[0], "line 101");
    assert!(default.body["next"].is_i64());
    assert_eq!(texts(&most.body).len(), 100);
    assert!(most.body["next"].is_i64(), "one line is left");
}

// --- What each viewer sees (S6, S9, S11, U3) --------------------------------------------

/// The people of the field-rule tests, around a Member's bot.
struct Viewers {
    member: Person,
    the_owner: Person,
    admin: Person,
    manager: Person,
    controller: Person,
    viewer: Person,
    /// A sender who never had a session, so they can be deleted.
    leaver: UserId,
}

/// A Member's bot shared at Manage, Control and View, with the Owner and an
/// Admin looking on, and its chat: received lines (one with a sender and
/// UUID, one cut, one starting with `/`, one with a line break), a mode's
/// command with arguments, and user lines by three senders.
async fn shared_bot(setup: &Setup) -> (Viewers, AccountId, BotId) {
    let member = setup.person(2, "member", Role::Member).await;
    let manager = setup.person(3, "manager", Role::Member).await;
    let controller = setup.person(4, "controller", Role::Member).await;
    let viewer = setup.person(5, "viewer", Role::Member).await;
    let the_owner = setup.person(6, "theowner", Role::Owner).await;
    let admin = setup.person(7, "admin", Role::Admin).await;
    let leaver = setup.add_user(8, "leaver", Role::Member).await;
    let (account, bot, agent) = setup.bot_with_agent(member.id, 10).await;
    for (who, level) in [
        (manager.id, GrantLevel::Manage),
        (controller.id, GrantLevel::Control),
        (viewer.id, GrantLevel::View),
        (leaver, GrantLevel::Control),
    ] {
        setup.share(account, who, level, member.id).await;
    }
    let cut = ChatEntry::Received(
        IncomingChat::from_parts(
            ChatKind::Whisper,
            Some(("Alex", None)),
            "a whisper cut short",
            true,
        )
        .unwrap(),
    );
    setup
        .said(
            bot,
            agent,
            vec![
                chat("Steve", Some(STEVE), "hello bot"),
                cut,
                system("/spawn is disabled here"),
                system("line one\nline two"),
                mode(&format!("/login {CANARY}")),
                user("hello there", manager.id),
                user("/spawn", manager.id),
                user("/home base", manager.id),
                user(&format!("/login {CANARY}"), controller.id),
                user("/", leaver),
                user("/ x", leaver),
            ],
        )
        .await;
    let viewers = Viewers {
        member,
        the_owner,
        admin,
        manager,
        controller,
        viewer,
        leaver,
    };
    (viewers, account, bot)
}

impl Viewers {
    fn all(&self) -> [(&'static str, &Person); 6] {
        [
            ("member", &self.member),
            ("the_owner", &self.the_owner),
            ("admin", &self.admin),
            ("manager", &self.manager),
            ("controller", &self.controller),
            ("viewer", &self.viewer),
        ]
    }

    /// Each user's ID with the name a snapshot shows it under.
    fn names(&self) -> Vec<(String, &'static str)> {
        vec![
            (self.member.id.to_string(), "member"),
            (self.manager.id.to_string(), "manager"),
            (self.controller.id.to_string(), "controller"),
            (self.the_owner.id.to_string(), "the_owner"),
            (self.leaver.to_string(), "leaver"),
        ]
    }
}

/// The line of `page` whose text starts with `start`.
fn line<'a>(page: &'a Value, start: &str) -> &'a Value {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|line| line["text"].as_str().unwrap().starts_with(start))
        .unwrap_or_else(|| panic!("no line starting with {start:?} in {page}"))
}

#[sqlx::test(migrations = false)]
async fn each_viewer_sees_the_page_under_its_field_rules(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (viewers, _, bot) = shared_bot(&setup).await;
    // An Admin with only View on the Owner's bot: names by role, no arguments.
    let (owners_account, owners_bot, owners_agent) =
        setup.bot_with_agent(viewers.the_owner.id, 20).await;
    setup
        .share(
            owners_account,
            viewers.admin.id,
            GrantLevel::View,
            viewers.the_owner.id,
        )
        .await;
    setup
        .said(
            owners_bot,
            owners_agent,
            vec![
                mode(&format!("/login {CANARY}")),
                user("/op AfkBot2 now", viewers.the_owner.id),
            ],
        )
        .await;

    let mut seen = BTreeMap::new();
    for (name, person) in viewers.all() {
        let page = setup.history(bot, person, "").await;
        assert_eq!(page.status, 200, "{name}: {}", page.text);
        seen.insert(name.to_owned(), page.body);
    }
    let admin_on_owners = setup.history(owners_bot, &viewers.admin, "").await;
    seen.insert(
        "admin_view_on_the_owners_bot".to_owned(),
        admin_on_owners.body,
    );

    // Manage holders: arguments, and the senders' names.
    for name in ["member", "the_owner", "admin", "manager"] {
        let page = &seen[name];
        let login = line(page, "/login");
        assert_eq!(login["arguments_hidden"], false, "{name}");
        assert_eq!(line(page, "/home")["text"], "/home base", "{name}");
        assert_eq!(
            line(page, "hello there")["sent_by"]["username"],
            "manager",
            "{name}"
        );
        assert_eq!(line(page, "/ x")["sent_by"]["username"], "leaver", "{name}");
    }
    // Below Manage: `/name` and the flag, no names, but `mine`.
    for name in ["controller", "viewer"] {
        let page = &seen[name];
        assert_eq!(line(page, "/home")["text"], "/home", "{name}");
        assert_eq!(line(page, "/home")["arguments_hidden"], true, "{name}");
        assert_eq!(line(page, "/spawn")["arguments_hidden"], false, "{name}");
        assert_eq!(line(page, "hello there").get("sent_by"), None, "{name}");
    }
    let controllers = &seen["controller"];
    let own = controllers["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|line| line["mine"] == true)
        .unwrap();
    assert_eq!(own["text"], "/login", "the sender's own line, still cut");
    assert_eq!(own["arguments_hidden"], true);
    assert_eq!(
        own.get("sent_by"),
        None,
        "no name below Manage, not even one's own"
    );
    assert_eq!(line(&seen["manager"], "hello there")["mine"], true);
    assert_eq!(line(&seen["member"], "hello there")["mine"], false);
    // Received lines: never cut, the Minecraft sender for everyone, a line
    // break as a separator.
    for (name, page) in &seen {
        if name == "admin_view_on_the_owners_bot" {
            continue;
        }
        assert_eq!(line(page, "/spawn is")["arguments_hidden"], false, "{name}");
        assert_eq!(line(page, "hello bot")["sender"]["uuid"], STEVE, "{name}");
        assert_eq!(
            line(page, "line one")["text"],
            "line one | line two",
            "{name}"
        );
        assert_eq!(line(page, "a whisper")["truncated"], true, "{name}");
    }
    let admin_view = &seen["admin_view_on_the_owners_bot"];
    assert_eq!(line(admin_view, "/op")["text"], "/op");
    assert_eq!(
        line(admin_view, "/op")["sent_by"]["username"],
        "theowner",
        "an Admin sees who sent it by role"
    );
    insta::assert_json_snapshot!(
        "chat_history_as_each_viewer_sees_it",
        redacted(&serde_json::to_value(&seen).unwrap(), &viewers.names())
    );
}

/// The canary: an argument planted in a mode's and a user's command never
/// reaches a viewer below Manage, the Control sender's own line included,
/// and does reach Manage, until that grant is lowered: the level at the
/// time of reading decides (S9).
#[sqlx::test(migrations = false)]
async fn a_planted_argument_never_reaches_a_viewer_below_manage(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (viewers, account, bot) = shared_bot(&setup).await;

    for below in [&viewers.controller, &viewers.viewer] {
        let seen = setup.history(bot, below, "").await;
        assert_eq!(seen.status, 200, "{}", seen.text);
        assert!(!seen.text.contains(CANARY), "{}", seen.text);
    }
    let managers = setup.history(bot, &viewers.manager, "").await;
    assert_eq!(
        managers.text.matches(CANARY).count(),
        2,
        "{}",
        managers.text
    );

    let mut tx = setup.db.write().await.unwrap();
    assert!(
        tx.grants()
            .set_level(
                account,
                viewers.manager.id,
                GrantLevel::Control,
                viewers.member.id,
                at(T0 + 60_000),
            )
            .await
            .unwrap()
    );
    tx.commit(setup_entry()).await.unwrap();
    let lowered = setup.history(bot, &viewers.manager, "").await;

    assert!(!lowered.text.contains(CANARY), "{}", lowered.text);
    assert_eq!(line(&lowered.body, "hello there").get("sent_by"), None);
}

/// A deleted sender's lines keep no sender: no name for anyone, and never
/// anyone's own.
#[sqlx::test(migrations = false)]
async fn a_deleted_senders_lines_show_no_name_to_anyone(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (viewers, account, bot) = shared_bot(&setup).await;
    let leaver = viewers.leaver.to_string();
    let account_id = account.to_string();
    sqlx::query!(
        "DELETE FROM account_grants WHERE user_id = ? AND account_id = ?",
        leaver,
        account_id
    )
    .execute(setup.db.write_pool())
    .await
    .unwrap();
    // The write pool's connections have the foreign keys on, as the app's.
    sqlx::query!("DELETE FROM users WHERE id = ?", leaver)
        .execute(setup.db.write_pool())
        .await
        .unwrap();

    for (name, person) in viewers.all() {
        let seen = setup.history(bot, person, "").await;
        assert_eq!(seen.status, 200, "{name}: {}", seen.text);
        // The leaver's two lines are the newest.
        for (n, line) in seen.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .take(2)
            .enumerate()
        {
            assert_eq!(line.get("sent_by"), None, "{name} {n}");
            assert_eq!(line["mine"], false, "{name} {n}");
            assert_eq!(line["kind"], "user", "{name} {n}");
        }
    }
    let members = setup.history(bot, &viewers.member, "").await;
    assert_eq!(
        line(&members.body, "hello there")["sent_by"]["username"],
        "manager",
        "the other senders keep theirs"
    );
}

/// A sender whose stored username doesn't read back is a corrupt row: the
/// internal error, never a page that leaves the line out.
#[sqlx::test(migrations = false)]
async fn a_line_whose_sender_doesnt_read_back_is_the_internal_error(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot, agent) = setup.bot_with_agent(owner.id, 10).await;
    let sender = UserId::new_v7(at(T0), [9; 10]).unwrap();
    let id = sender.to_string();
    sqlx::query!(
        "INSERT INTO users (id, username, password_hash, role, disabled, created_at, password_changed_at)
         VALUES (?, 'no spaces', '$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g', 'member', 0,
                 '2027-01-15T08:00:00.000Z', '2027-01-15T08:00:00.000Z')",
        id
    )
    .execute(setup.db.write_pool())
    .await
    .unwrap();
    setup.said(bot, agent, vec![user("hello", sender)]).await;

    let seen = setup.history(bot, &owner, "").await;

    assert_eq!(seen.status, 500, "{}", seen.text);
    assert_eq!(seen.code(), "internal");
    assert!(!seen.text.contains("hello"), "{}", seen.text);
}

/// The kinds and directions of what a bot received and sent.
#[sqlx::test(migrations = false)]
async fn every_line_names_its_kind_and_direction(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (viewers, _, bot) = shared_bot(&setup).await;

    let seen = setup.history(bot, &viewers.member, "").await;

    let kinds: Vec<(&str, &str)> = seen.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| {
            (
                line["direction"].as_str().unwrap(),
                line["kind"].as_str().unwrap(),
            )
        })
        .collect();
    let mut expected = vec![("out", "user"); 6];
    expected.extend([
        ("out", "mode"),
        ("in", "system"),
        ("in", "system"),
        ("in", "whisper"),
        ("in", "chat"),
    ]);
    assert_eq!(kinds, expected);
    assert_eq!(seen.body.get("next"), None);
    assert_eq!(
        json!(seen.body["items"].as_array().unwrap().len()),
        json!(11)
    );
}
