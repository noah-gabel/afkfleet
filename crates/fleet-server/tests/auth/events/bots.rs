//! The bots' events, built for each viewer (ADR-0020, S32, S41, R28,
//! R32–R34): a `bot` frame equals what that viewer's `GET /bots/{id}`
//! answers when it's sent, deaths and alerts reach whoever may see the bot,
//! what REST hides from a viewer never reaches their socket, hints merge
//! into one read, and a client that doesn't read is closed with 4002.

use core::time::Duration;

use chrono::{DateTime, Utc};
use fleet_api_types::events::SocketClose;
use fleet_core::authz::{GrantLevel, Role};
use fleet_core::bot::{BotNotification, BotSnapshot, BotState, FailReason, PauseReason};
use fleet_core::disconnect::ConflictKind;
use fleet_core::id::{AccountId, AgentId, BotId};
use fleet_core::system::Clock as _;
use fleet_core::value::AgentName;
use fleet_server::app::events::{LiveEvent, LiveEventKind};
use fleet_server::http::ws::SocketTimings;
use fleet_server::ports::agents::NewAgent;
use fleet_server::ports::live_hints::LiveHints as _;
use fleet_server::ports::store::Store as _;
use fleet_testkit::log_capture;

use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::io::AsyncWriteExt as _;
use tokio::time::timeout;

use super::super::support::{Options, Person, Setup, T0, at, setup, setup_entry, setup_with};
use super::{
    PATIENCE, Piped, Served, Socket, closed, next_event, next_event_after_pings, ready, said,
    ticket,
};

/// A planted server address: only viewers with Control or more see it.
const SERVER: &str = "canary.example.com:25565";
/// A planted agent's name: only Admins and the Owner see it.
const AGENT: &str = "agent-canary";
/// The bot's owner's username: only the viewers who see the account's owner
/// see it.
const OWNER_NAME: &str = "canaryowner";

/// A Member's bot with a server, on an agent, shared at Manage, Control and
/// View, with the Owner and an Admin looking on, and a stranger with a bot
/// of their own and no grant on this one.
pub(super) struct Watched {
    pub(super) the_owner: Person,
    pub(super) admin: Person,
    pub(super) member: Person,
    pub(super) manager: Person,
    pub(super) controller: Person,
    pub(super) viewer: Person,
    pub(super) stranger: Person,
    pub(super) account: AccountId,
    pub(super) bot: BotId,
    pub(super) agent: AgentId,
    /// The stranger's own bot.
    pub(super) own: BotId,
}

impl Watched {
    /// Everyone who may see the bot, by name.
    pub(super) fn viewers(&self) -> [(&'static str, &Person); 6] {
        [
            ("the_owner", &self.the_owner),
            ("admin", &self.admin),
            ("member", &self.member),
            ("manager", &self.manager),
            ("controller", &self.controller),
            ("viewer", &self.viewer),
        ]
    }
}

pub(super) async fn watched(setup: &Setup) -> Watched {
    let the_owner = setup.person(6, "theowner", Role::Owner).await;
    let admin = setup.person(7, "admin", Role::Admin).await;
    let member = setup.person(2, OWNER_NAME, Role::Member).await;
    let manager = setup.person(3, "manager", Role::Member).await;
    let controller = setup.person(4, "controller", Role::Member).await;
    let viewer = setup.person(5, "viewer", Role::Member).await;
    let stranger = setup.person(8, "stranger", Role::Member).await;
    let (account, bot) = setup.bot_of(member.id, 10).await;
    let (_, own) = setup.bot_of(stranger.id, 20).await;
    for (who, level) in [
        (manager.id, GrantLevel::Manage),
        (controller.id, GrantLevel::Control),
        (viewer.id, GrantLevel::View),
    ] {
        setup.share(account, who, level, member.id).await;
    }
    let agent = AgentId::new_v7(at(T0), [9; 10]).unwrap();
    let mut tx = setup.db.write().await.unwrap();
    tx.agents()
        .insert(&NewAgent {
            id: agent,
            name: AgentName::try_from(AGENT).unwrap(),
            created_at: at(T0),
        })
        .await
        .unwrap();
    assert!(tx.bots().assign(bot, agent).await.unwrap());
    tx.commit(setup_entry()).await.unwrap();
    let id = bot.to_string();
    sqlx::query!(
        "UPDATE bots SET server_address = ? WHERE id = ?",
        SERVER,
        id
    )
    .execute(setup.db.write_pool())
    .await
    .unwrap();
    Watched {
        the_owner,
        admin,
        member,
        manager,
        controller,
        viewer,
        stranger,
        account,
        bot,
        agent,
        own,
    }
}

/// `kind` of `bot` from `agent`, as the ingest publishes it now.
pub(super) fn agent_event(setup: &Setup, bot: BotId, agent: AgentId, kind: LiveEventKind) {
    setup.events.publish(LiveEvent {
        bot,
        agent,
        generation: 1,
        at: setup.clock.now(),
        kind,
    });
}

/// What `person`'s `GET /bots/{id}` answers for `bot` now.
pub(super) async fn get_bot(setup: &Setup, bot: BotId, person: &Person) -> Value {
    let seen = setup
        .get(&format!("/api/v1/bots/{bot}"), Some(&person.token))
        .await;
    assert_eq!(seen.status, 200, "{}", seen.text);
    seen.body
}

/// `bot`'s frame as `GET /bots/{id}` showed `body`.
pub(super) fn bot_frame(bot: BotId, body: &Value) -> Value {
    json!({ "type": "bot", "bot_id": bot.to_string(), "bot": body })
}

/// The time `at` as the API writes it.
fn written(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// A socket for each of `watched`'s viewers and its stranger, past `ready`.
async fn sockets(
    setup: &Setup,
    served: &Served,
    watched: &Watched,
) -> (Vec<(&'static str, Socket)>, Socket) {
    let mut sockets = Vec::new();
    for (name, person) in watched.viewers() {
        sockets.push((name, ready(setup, served, &person.token).await));
    }
    let stranger = ready(setup, served, &watched.stranger.token).await;
    (sockets, stranger)
}

#[sqlx::test(migrations = false)]
async fn each_viewers_bot_frame_equals_their_get_bot(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watched = watched(&setup).await;
    let served = Served::start(&setup).await;
    let (mut sockets, mut stranger) = sockets(&setup, &served, &watched).await;

    setup.events.bots(&[watched.bot]);
    // The stranger's next frame is about their own bot: nothing came first.
    setup.events.bots(&[watched.own]);

    for (name, socket) in &mut sockets {
        let frame = next_event_after_pings(socket).await;
        let person = watched
            .viewers()
            .into_iter()
            .find(|(n, _)| n == name)
            .unwrap()
            .1;
        let expected = bot_frame(watched.bot, &get_bot(&setup, watched.bot, person).await);
        assert_eq!(frame, expected, "{name}");
    }
    let own = get_bot(&setup, watched.own, &watched.stranger).await;
    assert_eq!(
        next_event_after_pings(&mut stranger).await,
        bot_frame(watched.own, &own)
    );
}

#[sqlx::test(migrations = false)]
async fn a_status_report_reaches_each_viewer_as_their_bot_frame(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watched = watched(&setup).await;
    let served = Served::start(&setup).await;
    let (mut sockets, mut stranger) = sockets(&setup, &served, &watched).await;
    let snapshot = BotSnapshot {
        bot_id: watched.bot,
        state: BotState::Stopped,
        since: setup.clock.now(),
        last_disconnect: None,
    };

    agent_event(
        &setup,
        watched.bot,
        watched.agent,
        LiveEventKind::Status(snapshot),
    );
    setup.events.bots(&[watched.own]);

    for (name, socket) in &mut sockets {
        let person = watched
            .viewers()
            .into_iter()
            .find(|(n, _)| n == name)
            .unwrap()
            .1;
        let expected = bot_frame(watched.bot, &get_bot(&setup, watched.bot, person).await);
        assert_eq!(next_event_after_pings(socket).await, expected, "{name}");
    }
    assert_eq!(
        next_event_after_pings(&mut stranger).await["bot_id"],
        json!(watched.own.to_string())
    );
}

#[sqlx::test(migrations = false)]
async fn died_and_alert_carry_only_the_bot_its_time_and_state(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watched = watched(&setup).await;
    let served = Served::start(&setup).await;
    let (mut sockets, mut stranger) = sockets(&setup, &served, &watched).await;
    let now = written(setup.clock.now());
    let bot = watched.bot.to_string();

    agent_event(&setup, watched.bot, watched.agent, LiveEventKind::Died);
    agent_event(
        &setup,
        watched.bot,
        watched.agent,
        LiveEventKind::Alert(BotNotification::Paused {
            reason: PauseReason::Conflict {
                kind: ConflictKind::DuplicateLogin,
            },
        }),
    );
    agent_event(
        &setup,
        watched.bot,
        watched.agent,
        LiveEventKind::Alert(BotNotification::Failed {
            reason: FailReason::Auth,
        }),
    );
    setup.events.bots(&[watched.own]);

    for (name, socket) in &mut sockets {
        assert_eq!(
            next_event_after_pings(socket).await,
            json!({ "type": "bot_died", "bot_id": bot, "at": now }),
            "{name}"
        );
        assert_eq!(
            next_event_after_pings(socket).await,
            json!({
                "type": "bot_alert", "bot_id": bot, "at": now,
                "state": "paused", "reason": "duplicate_login"
            }),
            "{name}"
        );
        assert_eq!(
            next_event_after_pings(socket).await,
            json!({
                "type": "bot_alert", "bot_id": bot, "at": now,
                "state": "failed", "reason": "auth"
            }),
            "{name}"
        );
    }
    assert_eq!(
        next_event_after_pings(&mut stranger).await["bot_id"],
        json!(watched.own.to_string())
    );
}

/// Each viewer's frames for a hint and a death, against what their REST
/// answer shows: the planted server address only at Control or more, the
/// agent's name only for Admins and the Owner, the owner's username only
/// for those who see the account's owner (S54, U3).
#[sqlx::test(migrations = false)]
async fn what_rest_hides_from_a_viewer_never_reaches_their_socket(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watched = watched(&setup).await;
    let served = Served::start(&setup).await;
    let (mut sockets, _stranger) = sockets(&setup, &served, &watched).await;
    // Whether each viewer sees the server, the agent and the owner's name.
    let expected = [
        ("the_owner", [true, true, true]),
        ("admin", [true, true, true]),
        ("member", [true, false, true]),
        ("manager", [true, false, true]),
        ("controller", [true, false, false]),
        ("viewer", [false, false, false]),
    ];

    setup.events.bots(&[watched.bot]);
    agent_event(&setup, watched.bot, watched.agent, LiveEventKind::Died);

    for ((name, socket), (expected_name, sees)) in sockets.iter_mut().zip(expected) {
        assert_eq!(*name, expected_name);
        let person = watched
            .viewers()
            .into_iter()
            .find(|(n, _)| n == name)
            .unwrap()
            .1;
        let get = get_bot(&setup, watched.bot, person).await.to_string();
        let mut texts = String::new();
        for _ in 0..2 {
            texts.push_str(&next_event_after_pings(socket).await.to_string());
        }
        for (canary, seen) in [SERVER, AGENT, OWNER_NAME].into_iter().zip(sees) {
            assert_eq!(get.contains(canary), seen, "{name}: REST and {canary}");
            assert_eq!(texts.contains(canary), seen, "{name}: {canary} in {texts}");
        }
    }
}

#[sqlx::test(migrations = false)]
async fn pending_hints_merge_into_one_read(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watched = watched(&setup).await;
    let (_, marker) = setup.bot_of(watched.member.id, 30).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &watched.member.token).await;
    let reads = setup.hooked.bot_reads();

    // The first hint's read is held; 39 more wait on the bus meanwhile.
    let mut held = setup.hooked.hold_next_bot_read();
    setup.events.bots(&[watched.bot]);
    held.read(PATIENCE).await;
    for _ in 0..39 {
        setup.events.bots(&[watched.bot]);
    }
    held.release();
    let first = next_event_after_pings(&mut socket).await;
    let second = next_event_after_pings(&mut socket).await;
    setup.events.bots(&[marker]);
    let third = next_event_after_pings(&mut socket).await;

    assert_eq!(first["bot_id"], json!(watched.bot.to_string()));
    assert_eq!(second["bot_id"], json!(watched.bot.to_string()));
    assert_eq!(
        third["bot_id"],
        json!(marker.to_string()),
        "nothing came between"
    );
    assert_eq!(
        setup.hooked.bot_reads() - reads,
        3,
        "the first, the 39 merged, the marker"
    );
}

#[sqlx::test(migrations = false)]
async fn a_client_that_doesnt_read_its_events_is_closed_with_4002(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    log_capture::install().unwrap();
    let setup = setup_with(
        options,
        Options {
            sockets: SocketTimings {
                send_timeout: Duration::from_millis(300),
                close_timeout: Duration::from_millis(200),
                ..SocketTimings::default()
            },
            ..Options::default()
        },
    )
    .await;
    let watched = watched(&setup).await;
    let piped = Piped::start(&setup);
    let ticket = ticket(&setup, &watched.member.token).await;
    let mut socket = piped.open(&watched.member.token, &ticket).await;
    assert_eq!(next_event(&mut socket).await, json!({ "type": "ready" }));

    // Held mid-step, the socket reads nothing from its client until the
    // bot's frame, which doesn't fit the pipe, went out or timed out.
    let mut held = setup.hooked.hold_next_bot_read();
    setup.events.bots(&[watched.bot]);
    held.read(PATIENCE).await;
    held.release();
    // Written, never read: once the client's side is full, the write waits
    // until the server drops its end, which ends it.
    let mut pipe = socket.into_inner();
    let ended = timeout(PATIENCE, async {
        loop {
            if pipe.write_all(&[0; 64]).await.is_err() {
                break;
            }
        }
    })
    .await;

    assert!(ended.is_ok(), "the server gave the socket up in time");
    assert_eq!(
        log_capture::count(
            "fleet_server::http::ws",
            "why=the client didn't read in time"
        ),
        1
    );
    assert_eq!(
        log_capture::count(
            "fleet_server::http::ws",
            &format!("code={}", SocketClose::TooSlow.code())
        ),
        1
    );
}

#[sqlx::test(migrations = false)]
async fn a_bot_read_that_fails_closes_the_socket_with_1013(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let watched = watched(&setup).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &watched.member.token).await;

    setup.hooked.fail_bot_reads();
    setup.events.bots(&[watched.bot]);

    assert_eq!(closed(&mut socket).await, said(SocketClose::Busy));
}
