//! fleet-client's live events handshake (Plan.md P11.5; ADR-0020, S28,
//! R16) against the in-process server: a fresh ticket per handshake, with
//! the session's bearer, which opens a socket whose first frame is `ready`;
//! nothing shows either secret. And through serve's own wiring, each
//! viewer's frame after each action equals what their `GET /bots/{id}`
//! answers then (S32, S42).

use fleet_api_types::accounts::{ChangeGrantRequest, CreateGrantRequest, GrantLevel};
use fleet_api_types::bots::{BotModeRequest, BotPatchRequest};
use fleet_api_types::invites::CreateInviteRequest;
use fleet_api_types::users::UserRole;
use fleet_api_types::{BotId, SecretText};
use fleet_client::events::{Received, read};
use fleet_client::{Client, ClientError, MemoryStore};
use fleet_core::mode::ModePreset;
use futures_util::StreamExt as _;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::bots::bot_of;
use crate::support::{PASSWORD, Server, register, step_up};

/// An open live events socket.
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A viewer by name, their client, and their socket.
type Viewing<'a> = (&'a str, &'a Client<MemoryStore>, Socket);

/// The longest a test waits for the socket.
const PATIENCE: core::time::Duration = core::time::Duration::from_secs(30);

#[tokio::test]
async fn a_handshake_names_the_ws_url_with_a_fresh_ticket_and_the_sessions_bearer() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;

    let first = owner.events_handshake().await.unwrap();
    let second = owner.events_handshake().await.unwrap();

    let url = first.url().expose_secret();
    let base = owner.server().as_str().replacen("http://", "ws://", 1);
    let ticket = url
        .strip_prefix(&format!("{base}api/v1/events?ticket="))
        .unwrap_or_else(|| panic!("an events URL"));
    assert!(ticket.starts_with("afk_wt_"));
    assert_eq!(ticket.len(), 50);
    assert_ne!(
        url,
        second.url().expose_secret(),
        "each handshake mints its own"
    );
    assert!(
        first
            .authorization()
            .expose_secret()
            .starts_with("Bearer afk_at_")
    );
    let shown = format!("{first:?}");
    assert!(!shown.contains(ticket), "{shown}");
    assert!(!shown.contains("afk_at_"), "{shown}");
    server.stop().await;
}

#[tokio::test]
async fn a_handshake_opens_one_socket_whose_first_frame_is_ready() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    let handshake = owner.events_handshake().await.unwrap();
    let mut request = handshake
        .url()
        .expose_secret()
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(handshake.authorization().expose_secret()).unwrap(),
    );

    let (mut socket, _) = timeout(PATIENCE, tokio_tungstenite::connect_async(request))
        .await
        .unwrap()
        .unwrap();
    let first = timeout(PATIENCE, socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let Message::Text(text) = first else {
        panic!("not a text frame: {first:?}");
    };
    assert!(matches!(read(&text), Received::Event(_)));
    assert_eq!(text.as_str(), r#"{"type":"ready"}"#);
    server.stop().await;
}

#[tokio::test]
async fn a_handshake_without_a_session_is_no_session() {
    let server = Server::start().await;
    let (client, _) = server.client();

    let refused = client.events_handshake().await;

    assert!(
        matches!(refused, Err(ClientError::NoSession)),
        "{refused:?}"
    );
    server.stop().await;
}

/// A socket of `client`'s session, its `ready` read.
async fn socket_of(client: &Client<MemoryStore>) -> Socket {
    let handshake = client.events_handshake().await.unwrap();
    let mut request = handshake
        .url()
        .expose_secret()
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(handshake.authorization().expose_secret()).unwrap(),
    );
    let (mut socket, _) = timeout(PATIENCE, tokio_tungstenite::connect_async(request))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next_event(&mut socket).await, json!({ "type": "ready" }));
    socket
}

/// The next event on `socket`, past the pings, within [`PATIENCE`].
async fn next_event(socket: &mut Socket) -> Value {
    let reading = async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => return serde_json::from_str(&text).unwrap(),
                Some(Ok(Message::Ping(_))) => {}
                other => panic!("not an event: {other:?}"),
            }
        }
    };
    timeout(PATIENCE, reading).await.expect("an event in time")
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
    server.advance(core::time::Duration::from_secs(2));
    for (name, client, socket) in sockets.iter_mut() {
        let frame = next_event(socket).await;
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
        sockets.push((name, client, socket_of(client).await));
    }
    let mut strangers = socket_of(&stranger).await;

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
    server.stop().await;
}
