//! Chat on the socket, built for each viewer (ADR-0020, S34, S9, S11, U3,
//! R33, R35): a `chat` frame's line equals that viewer's history line, a
//! user's line comes before its `chat_sent` or `chat_failed`, a failure's
//! reason reaches only viewers with Control or more, a lowered grant
//! hides the next line's arguments at once, and a format created or deleted
//! reads the next line, unreadable formats giving only their own server's
//! bots no parsed sender (S79, S81, R147).

use fleet_core::auth::SessionPolicy;
use fleet_core::authz::{GrantLevel, Role};
use fleet_core::bot::{BotSnapshot, BotState};
use fleet_core::chat::{ChatKind, ChatMessage, IncomingChat};
use fleet_core::id::{AccountId, BotId, ChatRequestId};
use fleet_core::retention::RetentionPolicy;
use fleet_core::system::Clock as _;
use fleet_core::value::Username;
use fleet_proto::control::ChatFailedReason;
use fleet_server::app::events::{LiveEvent, LiveEventKind, NotSent};
use fleet_server::app::retention::RetentionService;
use fleet_server::http::ws::SocketTimings;
use fleet_server::ports::chat::{ChatEntry, ChatRecord, ListedChat, NewChatMessage};
use fleet_server::ports::live_hints::LiveHints as _;
use fleet_server::ports::store::Store as _;
use fleet_testkit::system::ManualClock;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::super::bots::Stream;
use super::super::support::{Options, Person, Setup, setup, setup_entry, setup_with};
use super::{PATIENCE, Served, Socket, next_event_after_pings, ready, until_quiet};

/// A planted argument: only viewers with Manage see it.
const CANARY: &str = "hunter2-canary";

/// A Member's online bot, held by agent 1's stream, shared at Manage,
/// Control and View, with the Owner and an Admin looking on, and a stranger
/// with a bot of their own.
struct Chatting {
    the_owner: Person,
    admin: Person,
    member: Person,
    manager: Person,
    controller: Person,
    viewer: Person,
    stranger: Person,
    account: AccountId,
    bot: BotId,
    stream: Stream,
    own: BotId,
}

impl Chatting {
    /// Everyone who may see the bot, by name, and whether they're at Control
    /// or more, and at Manage.
    fn viewers(&self) -> [(&'static str, &Person, bool, bool); 6] {
        [
            ("the_owner", &self.the_owner, true, true),
            ("admin", &self.admin, true, true),
            ("member", &self.member, true, true),
            ("manager", &self.manager, true, true),
            ("controller", &self.controller, true, false),
            ("viewer", &self.viewer, false, false),
        ]
    }
}

async fn chatting(setup: &Setup) -> Chatting {
    let the_owner = setup.person(6, "theowner", Role::Owner).await;
    let admin = setup.person(7, "admin", Role::Admin).await;
    let member = setup.person(2, "member", Role::Member).await;
    let manager = setup.person(3, "manager", Role::Member).await;
    let controller = setup.person(4, "controller", Role::Member).await;
    let viewer = setup.person(5, "viewer", Role::Member).await;
    let stranger = setup.person(8, "stranger", Role::Member).await;
    let (account, bot, stream) = setup.online_bot(member.id, 10).await;
    for (who, level) in [
        (manager.id, GrantLevel::Manage),
        (controller.id, GrantLevel::Control),
        (viewer.id, GrantLevel::View),
    ] {
        setup.share(account, who, level, member.id).await;
    }
    let (_, own) = setup.bot_of(stranger.id, 20).await;
    Chatting {
        the_owner,
        admin,
        member,
        manager,
        controller,
        viewer,
        stranger,
        account,
        bot,
        stream,
        own,
    }
}

/// A socket for each of the viewers and the stranger, past `ready`.
async fn sockets(setup: &Setup, served: &Served, chatting: &Chatting) -> (Vec<Socket>, Socket) {
    let mut sockets = Vec::new();
    for (_, person, _, _) in chatting.viewers() {
        sockets.push(ready(setup, served, &person.token).await);
    }
    let stranger = ready(setup, served, &chatting.stranger.token).await;
    (sockets, stranger)
}

/// Stores `entry` as a line of `chatting`'s bot, and puts it on the bus as
/// the ingest or a send does, a user's line with its sender and `request`.
async fn said(
    setup: &Setup,
    chatting: &Chatting,
    entry: ChatEntry,
    sender: Option<&str>,
    request: Option<ChatRequestId>,
) {
    said_on(setup, chatting, chatting.bot, entry, sender, request).await;
}

/// As [`said`], for `bot`: any bot the chatting bot's agent runs.
async fn said_on(
    setup: &Setup,
    chatting: &Chatting,
    bot: BotId,
    entry: ChatEntry,
    sender: Option<&str>,
    request: Option<ChatRequestId>,
) {
    let agent = chatting.stream.lease.agent();
    let now = setup.clock.now();
    let mut tx = setup.db.write().await.unwrap();
    let id = tx
        .chat()
        .append(&NewChatMessage {
            bot,
            agent,
            at: now,
            entry: entry.clone(),
        })
        .await
        .unwrap()
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    setup.events.publish(LiveEvent {
        bot,
        agent,
        generation: chatting.stream.lease.generation(),
        at: now,
        kind: LiveEventKind::Chat {
            line: ListedChat {
                record: ChatRecord {
                    id,
                    bot,
                    at: now,
                    entry,
                },
                sent_by: sender.map(|name| Username::try_from(name).unwrap()),
            },
            request,
        },
    });
}

/// `kind` from the bot's agent, as the ingest publishes it.
fn from_agent(setup: &Setup, chatting: &Chatting, kind: LiveEventKind) {
    setup.events.publish(LiveEvent {
        bot: chatting.bot,
        agent: chatting.stream.lease.agent(),
        generation: chatting.stream.lease.generation(),
        at: setup.clock.now(),
        kind,
    });
}

/// `person`'s history of `bot`, newest first.
async fn history(setup: &Setup, bot: BotId, person: &Person) -> Vec<Value> {
    let seen = setup
        .get(&format!("/api/v1/bots/{bot}/chat"), Some(&person.token))
        .await;
    assert_eq!(seen.status, 200, "{}", seen.text);
    seen.body["items"].as_array().unwrap().clone()
}

/// A retention run deletes the bot's chat while its owner's socket is open:
/// the commits move its count and its rights are read again, but it gets
/// no frame, since deleted chat is fetched, not pushed (S42, R55), as the
/// hints table says of a retention batch (PR #71's review).
#[sqlx::test(migrations = false)]
async fn a_retention_run_deleting_a_viewers_chat_sends_their_socket_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        Options {
            sockets: SocketTimings {
                ping_interval: core::time::Duration::from_millis(100),
                idle_timeout: core::time::Duration::from_secs(5),
                ..SocketTimings::default()
            },
            ..Options::default()
        },
    )
    .await;
    let chatting = chatting(&setup).await;
    let old = IncomingChat::from_parts(ChatKind::System, None, "an old line", false).unwrap();
    said(&setup, &chatting, ChatEntry::Received(old), None, None).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &chatting.member.token).await;
    let commits = *setup.db.commits().borrow();
    // The retention's own clock, past the chat's cut: the server's stays,
    // so the socket's session lives on.
    let later = setup.clock.now() + chrono::TimeDelta::days(15);
    let retention = RetentionService::new(
        Arc::new(ManualClock::new(later)),
        Arc::new(setup.db.clone()),
        RetentionPolicy::default(),
        SessionPolicy::default(),
    )
    .with_pause(core::time::Duration::ZERO);

    let run = retention.run_once(&CancellationToken::new()).await;

    assert!(run.deleted.chat_by_age >= 1, "{run:?}");
    assert!(*setup.db.commits().borrow() > commits, "a batch committed");
    assert_eq!(until_quiet(&mut socket).await, Vec::<Value>::new());
    assert_eq!(
        history(&setup, chatting.bot, &chatting.member).await,
        Vec::<Value>::new()
    );
}

fn request(n: u8) -> ChatRequestId {
    ChatRequestId::new_v7(
        chrono::DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
        [n; 10],
    )
    .unwrap()
}

#[sqlx::test(migrations = false)]
async fn each_viewers_chat_frame_equals_their_history_line(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let chatting = chatting(&setup).await;
    let served = Served::start(&setup).await;
    let (mut sockets, mut stranger) = sockets(&setup, &served, &chatting).await;
    let line = |text: &str| ChatMessage::try_from(text).unwrap();
    let mine = request(1);

    said(
        &setup,
        &chatting,
        ChatEntry::Received(
            IncomingChat::from_parts(ChatKind::System, None, "line one\nline two", false).unwrap(),
        ),
        None,
        None,
    )
    .await;
    said(
        &setup,
        &chatting,
        ChatEntry::ModeSent(line(&format!("/login {CANARY}"))),
        None,
        None,
    )
    .await;
    said(
        &setup,
        &chatting,
        ChatEntry::UserSent {
            message: line(&format!("/login {CANARY}")),
            by: Some(chatting.manager.id),
        },
        Some("manager"),
        Some(mine),
    )
    .await;
    setup.events.bots(&[chatting.own]);

    for ((name, person, _, manage), socket) in chatting.viewers().into_iter().zip(&mut sockets) {
        let history = history(&setup, chatting.bot, person).await;
        let mut texts = String::new();
        for n in 0..3 {
            let frame = next_event_after_pings(socket).await;
            texts.push_str(&frame.to_string());
            let id = &frame["line"]["id"];
            let stored = history.iter().find(|line| &line["id"] == id).unwrap();
            let mut expected =
                json!({ "type": "chat", "bot_id": chatting.bot.to_string(), "line": stored });
            if n == 2 {
                expected["request"] = json!(mine.to_string());
            }
            assert_eq!(frame, expected, "{name}, line {n}");
        }
        assert_eq!(texts.contains(CANARY), manage, "{name}: {texts}");
    }
    assert_eq!(
        next_event_after_pings(&mut stranger).await["bot_id"],
        json!(chatting.own.to_string()),
        "the stranger got none of it"
    );
}

#[sqlx::test(migrations = false)]
async fn a_users_line_reaches_every_viewer_before_its_chat_sent(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let mut chatting = chatting(&setup).await;
    let served = Served::start(&setup).await;
    let (mut sockets, _) = sockets(&setup, &served, &chatting).await;
    let bot = chatting.bot;
    let (sent, _) = tokio::join!(
        setup.chat(bot, &chatting.controller, "hello"),
        setup.answer(&mut chatting.stream, bot, LiveEventKind::ChatSent),
    );

    assert_eq!(sent.status, 200, "{}", sent.text);
    let request = sent.body["request_id"].clone();
    for ((name, person, _, _), socket) in chatting.viewers().into_iter().zip(&mut sockets) {
        let line = next_event_after_pings(socket).await;
        assert_eq!(
            (line["type"].clone(), line["request"].clone()),
            (json!("chat"), request.clone()),
            "{name}"
        );
        assert_eq!(
            line["line"],
            history(&setup, bot, person).await[0],
            "{name}: the sent line, as their history shows it"
        );
        assert_eq!(
            next_event_after_pings(socket).await,
            json!({ "type": "chat_sent", "bot_id": bot.to_string(), "request": request }),
            "{name}"
        );
    }
}

#[sqlx::test(migrations = false)]
async fn a_users_line_reaches_every_viewer_before_its_chat_failed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let mut chatting = chatting(&setup).await;
    let served = Served::start(&setup).await;
    let (mut sockets, _) = sockets(&setup, &served, &chatting).await;
    let bot = chatting.bot;
    let (failed, _) = tokio::join!(
        setup.chat(bot, &chatting.controller, "hello"),
        setup.answer(&mut chatting.stream, bot, |request| {
            LiveEventKind::ChatFailed(request, ChatFailedReason::ChatUnavailable)
        }),
    );

    assert_eq!(failed.status, 409, "{}", failed.text);
    assert_eq!(failed.code(), "chat_unavailable");
    for ((name, _, control, _), socket) in chatting.viewers().into_iter().zip(&mut sockets) {
        let line = next_event_after_pings(socket).await;
        assert_eq!(line["type"], json!("chat"), "{name}");
        let mut expected = json!({
            "type": "chat_failed", "bot_id": bot.to_string(), "request": line["request"]
        });
        if control {
            expected["failure"] = json!("chat_unavailable");
        }
        assert_eq!(next_event_after_pings(socket).await, expected, "{name}");
    }
}

#[sqlx::test(migrations = false)]
async fn a_line_refused_after_its_commit_reaches_viewers_as_chat_failed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let chatting = chatting(&setup).await;
    let served = Served::start(&setup).await;
    let (mut sockets, _) = sockets(&setup, &served, &chatting).await;
    let bot = chatting.bot.to_string();

    for (n, why, failure) in [
        (2, NotSent::Unreachable, Some("bot_unreachable")),
        (3, NotSent::Unsendable, None),
    ] {
        from_agent(
            &setup,
            &chatting,
            LiveEventKind::ChatNotSent {
                request: request(n),
                why,
            },
        );
        for ((name, _, control, _), socket) in chatting.viewers().into_iter().zip(&mut sockets) {
            let mut expected = json!({
                "type": "chat_failed", "bot_id": bot, "request": request(n).to_string()
            });
            if let Some(failure) = failure.filter(|_| control) {
                expected["failure"] = json!(failure);
            }
            assert_eq!(
                next_event_after_pings(socket).await,
                expected,
                "{name}: {why:?}"
            );
        }
    }
}

#[sqlx::test(migrations = false)]
async fn a_lowered_grant_hides_the_next_lines_arguments(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let chatting = chatting(&setup).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &chatting.manager.token).await;

    let mut tx = setup.db.write().await.unwrap();
    assert!(
        tx.grants()
            .set_level(
                chatting.account,
                chatting.manager.id,
                GrantLevel::View,
                chatting.member.id,
                setup.clock.now(),
            )
            .await
            .unwrap()
    );
    tx.commit(setup_entry()).await.unwrap();
    said(
        &setup,
        &chatting,
        ChatEntry::UserSent {
            message: ChatMessage::try_from(format!("/login {CANARY}").as_str()).unwrap(),
            by: Some(chatting.manager.id),
        },
        Some("manager"),
        Some(request(4)),
    )
    .await;

    let frame = next_event_after_pings(&mut socket).await;
    assert_eq!(frame["line"]["text"], json!("/login"));
    assert_eq!(frame["line"]["arguments_hidden"], json!(true));
    assert!(!frame.to_string().contains(CANARY), "{frame}");
}

#[sqlx::test(migrations = false)]
async fn a_line_keeps_its_place_before_its_chat_sent_within_a_merge(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let chatting = chatting(&setup).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &chatting.member.token).await;
    let state = BotSnapshot {
        bot_id: chatting.bot,
        state: BotState::Stopped,
        since: setup.clock.now(),
        last_disconnect: None,
    };

    // While the first hint's read is held, a line, its answer and a state
    // wait on the bus; all are taken together.
    let mut held = setup.hooked.hold_next_bot_read();
    setup.events.bots(&[chatting.bot]);
    held.read(PATIENCE).await;
    said(
        &setup,
        &chatting,
        ChatEntry::UserSent {
            message: ChatMessage::try_from("hello").unwrap(),
            by: Some(chatting.member.id),
        },
        Some("member"),
        Some(request(5)),
    )
    .await;
    from_agent(&setup, &chatting, LiveEventKind::ChatSent(request(5)));
    from_agent(&setup, &chatting, LiveEventKind::Status(state));
    held.release();

    let kinds: Vec<Value> = {
        let mut kinds = Vec::new();
        for _ in 0..4 {
            kinds.push(next_event_after_pings(&mut socket).await["type"].clone());
        }
        kinds
    };
    assert_eq!(
        kinds,
        [
            json!("bot"),
            json!("chat"),
            json!("chat_sent"),
            json!("bot")
        ]
    );
}

// --- Parsed senders (ADR-0020, S79, S81, R146, R147) ------------------------------------

/// The user's two formats (S80): a clan tag, and none.
const FORMATS: [&str; 2] = ["[{any}] {sender} : ", "{sender} : "];

/// The chatting bot's server.
const SERVER: &str = "mc.example.com";

/// Another server.
const OTHER: &str = "other.example.com";

/// What a broken row holds: a brace the formats refuse.
const BROKEN_FORMATS: &str = r#"["{Sender} : "]"#;

/// A received system line.
fn system(text: &str) -> ChatEntry {
    ChatEntry::Received(IncomingChat::system(text))
}

/// With a format on the bot's server, each viewer's frame of a matched line
/// equals their history line, its parsed sender included (S79, R146).
#[sqlx::test(migrations = false)]
async fn with_chat_formats_each_viewers_chat_frame_equals_their_history_line(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let chatting = chatting(&setup).await;
    setup.on_server(chatting.bot, SERVER).await;
    setup.formats_for(1, SERVER, &FORMATS).await;
    let served = Served::start(&setup).await;
    let (mut sockets, _) = sockets(&setup, &served, &chatting).await;

    said(
        &setup,
        &chatting,
        system("[Clan] Notch : hello"),
        None,
        None,
    )
    .await;

    for ((name, person, _, _), socket) in chatting.viewers().into_iter().zip(&mut sockets) {
        let history = history(&setup, chatting.bot, person).await;
        let frame = next_event_after_pings(socket).await;
        let stored = history
            .iter()
            .find(|line| line["id"] == frame["line"]["id"])
            .unwrap();
        assert_eq!(
            frame,
            json!({ "type": "chat", "bot_id": chatting.bot.to_string(), "line": stored }),
            "{name}"
        );
        assert_eq!(frame["line"]["parsed_sender"], "Notch", "{name}");
    }
}

/// A format created, then deleted, reaches the next live line: each commit
/// moves the count, so the socket reads the rights, and with them the
/// formats, again before it sends (S43, R147).
#[sqlx::test(migrations = false)]
async fn a_format_created_or_deleted_reaches_the_next_live_line(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let chatting = chatting(&setup).await;
    setup.on_server(chatting.bot, SERVER).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &chatting.member.token).await;

    said(&setup, &chatting, system("Notch : one"), None, None).await;
    let before = next_event_after_pings(&mut socket).await;
    let settings = setup.formats_for(1, SERVER, &["{sender} : "]).await;
    said(&setup, &chatting, system("Notch : two"), None, None).await;
    let with = next_event_after_pings(&mut socket).await;
    setup.settings_gone(settings).await;
    said(&setup, &chatting, system("Notch : three"), None, None).await;
    let after = next_event_after_pings(&mut socket).await;

    assert_eq!(before["line"]["text"], "Notch : one");
    assert_eq!(before["line"].get("parsed_sender"), None);
    assert_eq!(with["line"]["text"], "Notch : two");
    assert_eq!(with["line"]["parsed_sender"], "Notch");
    assert_eq!(after["line"]["text"], "Notch : three");
    assert_eq!(after["line"].get("parsed_sender"), None);
}

/// S81: a server whose formats don't read back gives only its own bots'
/// lines no parsed sender: another server's bot stays read, and the socket
/// stays open, its rights read again rather than failed.
#[sqlx::test(migrations = false)]
async fn a_servers_unreadable_chat_formats_leave_other_bots_parsed_and_the_socket_open(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let chatting = chatting(&setup).await;
    let (_, other) = setup.bot_of(chatting.member.id, 30).await;
    // Its lines come through the chatting bot's agent, which the append checks.
    let mut tx = setup.db.write().await.unwrap();
    assert!(
        tx.bots()
            .assign(other, chatting.stream.lease.agent())
            .await
            .unwrap()
    );
    tx.commit(setup_entry()).await.unwrap();
    setup.on_server(chatting.bot, SERVER).await;
    setup.on_server(other, OTHER).await;
    let broken = setup.formats_for(1, SERVER, &FORMATS).await;
    setup.formats_for(2, OTHER, &FORMATS).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &chatting.member.token).await;

    setup.break_formats(broken, BROKEN_FORMATS).await;
    said(
        &setup,
        &chatting,
        system("[Clan] Notch : hello"),
        None,
        None,
    )
    .await;
    let unread = next_event_after_pings(&mut socket).await;
    said_on(
        &setup,
        &chatting,
        other,
        system("[Clan] Notch : hi"),
        None,
        None,
    )
    .await;
    let read = next_event_after_pings(&mut socket).await;

    assert_eq!(unread["bot_id"], json!(chatting.bot.to_string()));
    assert_eq!(unread["line"]["text"], "[Clan] Notch : hello");
    assert_eq!(unread["line"].get("parsed_sender"), None);
    assert_eq!(read["bot_id"], json!(other.to_string()));
    assert_eq!(read["line"]["parsed_sender"], "Notch");
}

/// S81: with its server's formats unreadable, each viewer's frame of the
/// line still equals their history line: neither has a parsed sender.
#[sqlx::test(migrations = false)]
async fn with_unreadable_chat_formats_each_viewers_chat_frame_still_equals_their_history_line(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let chatting = chatting(&setup).await;
    setup.on_server(chatting.bot, SERVER).await;
    let broken = setup.formats_for(1, SERVER, &FORMATS).await;
    setup.break_formats(broken, BROKEN_FORMATS).await;
    let served = Served::start(&setup).await;
    let (mut sockets, _) = sockets(&setup, &served, &chatting).await;

    said(
        &setup,
        &chatting,
        system("[Clan] Notch : hello"),
        None,
        None,
    )
    .await;

    for ((name, person, _, _), socket) in chatting.viewers().into_iter().zip(&mut sockets) {
        let history = history(&setup, chatting.bot, person).await;
        let frame = next_event_after_pings(socket).await;
        let stored = history
            .iter()
            .find(|line| line["id"] == frame["line"]["id"])
            .unwrap();
        assert_eq!(
            frame,
            json!({ "type": "chat", "bot_id": chatting.bot.to_string(), "line": stored }),
            "{name}"
        );
        assert_eq!(stored.get("parsed_sender"), None, "{name}");
        assert_eq!(stored["text"], "[Clan] Notch : hello", "{name}");
    }
}
