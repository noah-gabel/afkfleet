//! fleet-client's live events socket (Plan.md P11.8; ADR-0020, U25, S53,
//! S54) against the in-process server: `ready` first, a 4001 at the access
//! token's end answered by a reconnect with a refreshed one, a revoked
//! session ending it, the shutdown's 1001, the per-user cap's Retry-After,
//! and a logout ending it at once; neither its `Debug` nor any log line, at
//! any level, shows a ticket or a token (S56). And through serve's own
//! wiring, each viewer's frame after each action equals what their
//! `GET /bots/{id}` answers then (S32, S42).

use core::time::Duration;

use fleet_api_types::accounts::{ChangeGrantRequest, CreateGrantRequest, GrantLevel};
use fleet_api_types::bots::{BotModeRequest, BotPatchRequest};
use fleet_api_types::invites::CreateInviteRequest;
use fleet_api_types::users::UserRole;
use fleet_api_types::{BotId, ErrorCode, PageQuery, SecretText};
use fleet_client::{
    Client, ClientConfig, Disconnect, LiveEnd, LiveEvents, LiveUpdate, MemoryStore, RefetchReason,
    Trust,
};
use fleet_core::mode::ModePreset;
use fleet_testkit::log_capture;
use serde_json::{Value, json};
use tokio::time::timeout;

use crate::bots::bot_of;
use crate::relay::Relay;
use crate::support::{OWNER, PASSWORD, Server, is_api, register, step_up};

/// A viewer by name, their client, and their socket.
type Viewing<'a> = (&'a str, &'a Client<MemoryStore>, LiveEvents<MemoryStore>);

/// The longest a test waits for the socket.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long an access token lives: the server's default.
const ACCESS_TTL: Duration = Duration::from_mins(15);

/// The next update of `live`, within [`PATIENCE`].
async fn next_update(live: &mut LiveEvents<MemoryStore>) -> Result<LiveUpdate, LiveEnd> {
    timeout(PATIENCE, live.recv())
        .await
        .expect("an update in time")
}

/// Whether `update` is the refetch every `ready` asks for.
fn is_ready(update: &Result<LiveUpdate, LiveEnd>) -> bool {
    matches!(update, Ok(LiveUpdate::Refetch(RefetchReason::Ready)))
}

/// A socket of `client`'s session, its `ready` read.
async fn live_of(client: &Client<MemoryStore>) -> LiveEvents<MemoryStore> {
    let mut live = client.live_events();
    let first = next_update(&mut live).await;
    assert!(is_ready(&first), "{first:?}");
    live
}

/// The next event on `live`, as its frame's JSON.
async fn next_event(live: &mut LiveEvents<MemoryStore>) -> Value {
    match next_update(live).await {
        Ok(LiveUpdate::Event(event)) => serde_json::to_value(&*event).unwrap(),
        other => panic!("not an event: {other:?}"),
    }
}

/// The disconnect `update` is, with its wait.
fn disconnected(update: Result<LiveUpdate, LiveEnd>) -> (Disconnect, Duration) {
    match update {
        Ok(LiveUpdate::Disconnected { why, retry_in }) => (why, retry_in),
        other => panic!("not a disconnect: {other:?}"),
    }
}

#[tokio::test]
async fn a_live_socket_says_ready_and_shows_only_its_server_and_state() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    let mut live = owner.live_events();
    let before = format!("{live:?}");

    let first = next_update(&mut live).await;

    assert!(is_ready(&first), "{first:?}");
    let base = owner.server().as_str();
    assert_eq!(
        before,
        format!("LiveEvents {{ server: {base:?}, state: \"waiting\" }}")
    );
    assert_eq!(
        format!("{live:?}"),
        format!("LiveEvents {{ server: {base:?}, state: \"live\" }}")
    );
    live.close().await;
    server.stop().await;
}

#[tokio::test]
async fn the_access_tokens_end_closes_with_4001_and_it_reconnects_at_once_with_a_refreshed_one() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    let mut live = live_of(&owner).await;
    server.advance(ACCESS_TTL + Duration::from_secs(1));

    server.commit_anything().await;
    let (why, retry_in) = disconnected(next_update(&mut live).await);
    let again = next_update(&mut live).await;

    assert!(
        matches!(why, Disconnect::Closed { code: Some(4001) }),
        "{why:?}"
    );
    assert_eq!(retry_in, Duration::ZERO);
    assert!(is_ready(&again), "{again:?}");
    live.close().await;
    server.stop().await;
}

#[tokio::test]
async fn a_revoked_session_closes_with_4001_and_then_ends_with_no_session() {
    let server = Server::start().await;
    let (owner, _, app) = server.owner().await;
    let (other, _) = server.client();
    server.log_in(&other, OWNER, &app).await;
    let mut live = live_of(&owner).await;
    let sessions = other
        .sessions(&PageQuery {
            before: None,
            limit: None,
        })
        .await
        .unwrap();
    let owners = sessions
        .items
        .iter()
        .find(|session| !session.current)
        .unwrap()
        .id;

    other.revoke_session(owners).await.unwrap();
    let (why, retry_in) = disconnected(next_update(&mut live).await);
    let ended = next_update(&mut live).await;

    assert!(
        matches!(why, Disconnect::Closed { code: Some(4001) }),
        "{why:?}"
    );
    assert_eq!(retry_in, Duration::ZERO);
    assert!(matches!(ended, Err(LiveEnd::NoSession)), "{ended:?}");
    server.stop().await;
}

#[tokio::test]
async fn the_servers_shutdown_closes_with_1001_and_backs_off() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    let mut live = live_of(&owner).await;

    let ((), update) = tokio::join!(server.stop(), next_update(&mut live));

    let (why, retry_in) = disconnected(update);
    assert!(
        matches!(why, Disconnect::Closed { code: Some(1001) }),
        "{why:?}"
    );
    assert!(
        (Duration::from_secs(1)..=Duration::from_secs(2)).contains(&retry_in),
        "{retry_in:?}"
    );
}

#[tokio::test]
async fn a_fourth_socket_waits_the_caps_retry_after() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    let mut open = Vec::new();
    for _ in 0..3 {
        open.push(live_of(&owner).await);
    }
    let mut fourth = owner.live_events();

    let (why, retry_in) = disconnected(next_update(&mut fourth).await);

    assert!(
        matches!(&why, Disconnect::NotOpened(error) if is_api(error, 429, ErrorCode::TooManyConnections)),
        "{why:?}"
    );
    assert_eq!(retry_in, Duration::from_secs(45));
    drop(fourth);
    drop(open);
    server.stop().await;
}

#[tokio::test]
async fn logging_out_ends_the_socket_at_once_with_no_session() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    let mut live = live_of(&owner).await;

    owner.logout().await.unwrap();
    let ended = next_update(&mut live).await;

    assert!(matches!(ended, Err(LiveEnd::NoSession)), "{ended:?}");
    assert!(matches!(live.recv().await, Err(LiveEnd::NoSession)));
    server.stop().await;
}

#[tokio::test]
async fn a_socket_without_a_session_ends_with_no_session() {
    let server = Server::start().await;
    let (client, _) = server.client();
    let mut live = client.live_events();

    let ended = next_update(&mut live).await;

    assert!(matches!(ended, Err(LiveEnd::NoSession)), "{ended:?}");
    server.stop().await;
}

/// The frame of `bot` as `client`'s `GET /bots/{id}` answers it now.
async fn as_get_reads(client: &Client<MemoryStore>, bot: BotId) -> Value {
    let read = client.bot(bot).await.unwrap();
    json!({ "type": "bot", "bot_id": bot.to_string(), "bot": read })
}

/// A user of `role` named `username`, invited by `owner` (stepped up) and
/// logged in with 2FA.
async fn invited(
    server: &Server,
    owner: &Client<MemoryStore>,
    username: &str,
    role: UserRole,
) -> Client<MemoryStore> {
    let invite = owner
        .create_invite(&CreateInviteRequest { role })
        .await
        .unwrap();
    let (client, _) = server.client();
    client
        .register(&register(invite.code.expose(), username, PASSWORD))
        .await
        .unwrap();
    server.first_login(&client, username).await;
    client
}

/// Each viewer's next frame after `step`: what their `GET /bots/{id}`
/// answers now, or `bot_removed` for the one it took `bot` from. The clock
/// moves 2 s first, which refills the address's limit every client here
/// shares.
async fn each_sees(
    (server, sockets): (&Server, &mut [Viewing<'_>]),
    bot: BotId,
    step: &str,
    gone: Option<&str>,
) {
    server.advance(Duration::from_secs(2));
    for (name, client, live) in sockets.iter_mut() {
        let frame = next_event(live).await;
        let expected = if gone == Some(*name) {
            json!({ "type": "bot_removed", "bot_id": bot.to_string() })
        } else {
            as_get_reads(client, bot).await
        };
        assert_eq!(frame, expected, "{name} after {step}");
    }
}

#[tokio::test]
async fn after_each_action_every_viewers_next_bot_frame_equals_their_get() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let admin = invited(&server, &owner, "admin", UserRole::Admin).await;
    let (alice, _) = server.member(&owner, "alice").await;
    let (viewer, viewer_id) = server.member(&owner, "viewer").await;
    let (controller, controller_id) = server.member(&owner, "controller").await;
    let (stranger, _) = server.member(&owner, "stranger").await;
    let (account, bot) = bot_of(&alice, "AfkBot1").await;
    let (_, own) = bot_of(&stranger, "AfkBot2").await;
    step_up(&alice).await;
    for (username, level) in [
        ("viewer", GrantLevel::View),
        ("controller", GrantLevel::Control),
    ] {
        alice
            .create_grant(
                account,
                &CreateGrantRequest {
                    username: SecretText::new(username.to_owned()),
                    level,
                },
            )
            .await
            .unwrap();
    }
    let viewers = [
        ("owner", &owner),
        ("admin", &admin),
        ("alice", &alice),
        ("viewer", &viewer),
        ("controller", &controller),
    ];
    let mut sockets = Vec::new();
    for (name, client) in viewers {
        sockets.push((name, client, live_of(client).await));
    }
    let mut strangers = live_of(&stranger).await;

    alice
        .patch_bot(
            bot,
            &BotPatchRequest {
                server_address: "mc.example.com".to_owned(),
            },
        )
        .await
        .unwrap();
    each_sees((&server, &mut sockets), bot, "a server", None).await;
    alice.start_bot(bot).await.unwrap();
    each_sees((&server, &mut sockets), bot, "start", None).await;
    alice
        .set_bot_mode(
            bot,
            &BotModeRequest {
                mode_id: ModePreset::Farm.id(),
            },
        )
        .await
        .unwrap();
    each_sees((&server, &mut sockets), bot, "a mode", None).await;
    alice.stop_bot(bot).await.unwrap();
    each_sees((&server, &mut sockets), bot, "stop", None).await;
    alice
        .change_grant(
            account,
            viewer_id,
            &ChangeGrantRequest {
                level: GrantLevel::Control,
            },
        )
        .await
        .unwrap();
    each_sees((&server, &mut sockets), bot, "a raised grant", None).await;
    alice.revoke_grant(account, controller_id).await.unwrap();
    each_sees(
        (&server, &mut sockets),
        bot,
        "a revoked grant",
        Some("controller"),
    )
    .await;
    step_up(&stranger).await;
    stranger
        .patch_bot(
            own,
            &BotPatchRequest {
                server_address: "mc.example.com".to_owned(),
            },
        )
        .await
        .unwrap();

    assert_eq!(
        next_event(&mut strangers).await,
        as_get_reads(&stranger, own).await,
        "nothing of alice's bot came first"
    );
    drop(sockets);
    drop(strangers);
    server.stop().await;
}

/// The secrets in `requests`: each ticket in a request line, each bearer,
/// and each refresh token in a body.
fn secrets_of(requests: &[String]) -> Vec<String> {
    let mut secrets = Vec::new();
    for request in requests {
        let line = request.lines().next().unwrap_or_default();
        if let Some((_, rest)) = line.split_once("ticket=") {
            secrets.push(rest.split([' ', '&']).next().unwrap().to_owned());
        }
        for line in request.lines() {
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("authorization")
            {
                let token = value.trim().strip_prefix("Bearer ").unwrap();
                secrets.push(token.to_owned());
            }
        }
        if let Some((_, rest)) = request.split_once(r#""refresh_token":""#) {
            secrets.push(rest.split('"').next().unwrap().to_owned());
        }
    }
    secrets.sort();
    secrets.dedup();
    secrets
}

#[tokio::test]
async fn the_live_socket_logs_no_ticket_and_no_token_at_any_level() {
    log_capture::install().unwrap();
    let server = Server::start().await;
    let relay = Relay::start(&server.url).await;
    let client = Client::new(
        ClientConfig {
            server: relay.url.clone(),
            trust: Trust::System,
        },
        MemoryStore::new(),
    )
    .unwrap();
    server.first_login(&client, OWNER).await;
    let mut live = live_of(&client).await;

    relay.drop_next_upgrade();
    relay.cut();
    let (cut, _) = disconnected(next_update(&mut live).await);
    let (dropped, _) = disconnected(next_update(&mut live).await);
    let back = next_update(&mut live).await;
    server.advance(ACCESS_TTL + Duration::from_secs(1));
    server.commit_anything().await;
    let (expired, _) = disconnected(next_update(&mut live).await);
    let refreshed = next_update(&mut live).await;
    live.close().await;
    server.stop().await;

    assert!(matches!(cut, Disconnect::Lost), "{cut:?}");
    assert!(matches!(dropped, Disconnect::NotOpened(_)), "{dropped:?}");
    assert!(is_ready(&back), "{back:?}");
    assert!(
        matches!(expired, Disconnect::Closed { code: Some(4001) }),
        "{expired:?}"
    );
    assert!(is_ready(&refreshed), "{refreshed:?}");
    let requests = relay.requests();
    let secrets = secrets_of(&requests);
    let count = |prefix: &str| {
        secrets
            .iter()
            .filter(|secret| secret.starts_with(prefix))
            .count()
    };
    assert_eq!(
        count("afk_wt_"),
        4,
        "the first, the dropped, back, refreshed"
    );
    assert_eq!(count("afk_at_"), 2, "the login's and the refresh's");
    assert!(count("afk_rt_") >= 1, "the refresh's");
    for secret in &secrets {
        let (prefix, unprefixed) = secret.split_at(7);
        assert!(prefix.starts_with("afk_") && unprefixed.len() >= 32);
        log_capture::check_absent(secret).unwrap();
        log_capture::check_absent(unprefixed).unwrap();
    }
    for prefix in ["afk_wt_", "afk_at_", "afk_rt_"] {
        log_capture::check_absent(prefix).unwrap();
    }
    // The capture saw the socket's frames and the HTTP client's connects,
    // at every level: what it found nothing in.
    for target in [
        "tungstenite::protocol::frame::frame",
        "tokio_tungstenite::compat",
        "reqwest::connect",
        "hyper_util::client::legacy::pool",
    ] {
        assert!(log_capture::count(target, "") > 0, "{target}");
    }
}
