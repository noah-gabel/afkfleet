//! fleet-client's live events handshake (Plan.md P11.5; ADR-0020, S28,
//! R16) against the in-process server: a fresh ticket per handshake, with
//! the session's bearer, which opens a socket whose first frame is `ready`;
//! nothing shows either secret.

use fleet_client::ClientError;
use fleet_client::events::{Received, read};
use futures_util::StreamExt as _;
use secrecy::ExposeSecret as _;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;

use crate::support::Server;

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
