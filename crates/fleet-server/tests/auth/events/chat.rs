//! Chat on the socket, built for each viewer (ADR-0020, S34, S9, S11, U3,
//! R33, R35): a `chat` frame's line equals that viewer's history line, a
//! user's line comes before its `chat_sent` or `chat_failed`, a failure's
//! reason reaches only viewers with Control or more, and a lowered grant
//! hides the next line's arguments at once.

use fleet_core::authz::{GrantLevel, Role};
use fleet_core::bot::{BotSnapshot, BotState};
use fleet_core::chat::{ChatKind, ChatMessage, IncomingChat};
use fleet_core::id::{AccountId, BotId, ChatRequestId};
use fleet_core::system::Clock as _;
use fleet_core::value::Username;
use fleet_proto::control::ChatFailedReason;
use fleet_server::app::events::{LiveEvent, LiveEventKind, NotSent};
use fleet_server::ports::chat::{ChatEntry, ChatRecord, ListedChat, NewChatMessage};
use fleet_server::ports::live_hints::LiveHints as _;
use fleet_server::ports::store::Store as _;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::super::bots::Stream;
use super::super::support::{Person, Setup, setup, setup_entry};
use super::{PATIENCE, Served, Socket, next_event_after_pings, ready};

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
    let agent = chatting.stream.lease.agent();
    let now = setup.clock.now();
    let mut tx = setup.db.write().await.unwrap();
    let id = tx
        .chat()
        .append(&NewChatMessage {
            bot: chatting.bot,
            agent,
            at: now,
            entry: entry.clone(),
        })
        .await
        .unwrap()
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    setup.events.publish(LiveEvent {
        bot: chatting.bot,
        agent,
        generation: chatting.stream.lease.generation(),
        at: now,
        kind: LiveEventKind::Chat {
            line: ListedChat {
                record: ChatRecord {
                    id,
                    bot: chatting.bot,
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
    for ((name, _, _, _), socket) in chatting.viewers().into_iter().zip(&mut sockets) {
        let line = next_event_after_pings(socket).await;
        assert_eq!(
            (line["type"].clone(), line["request"].clone()),
            (json!("chat"), request.clone()),
            "{name}"
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
