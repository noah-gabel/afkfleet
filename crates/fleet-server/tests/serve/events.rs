//! A live events socket at serve's shutdown (Plan.md P11.5; ADR-0015's note,
//! ADR-0020, S35, R20): axum's graceful shutdown doesn't wait for an
//! upgraded connection, so each socket closes itself with 1001 at the first
//! signal, and serve waits for it before the database closes.
//!
//! serve runs as `run` builds it, over a real socket and a real database, in
//! real time; every wait is bounded by [`PATIENCE`].

use core::time::Duration;
use std::net::SocketAddr;
use std::sync::Arc;

use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::auth::SessionPolicy;
use fleet_core::authz::Role;
use fleet_core::id::UserId;
use fleet_core::system::Clock as _;
use fleet_core::value::Username;
use fleet_server::app::auth::TokenService;
use fleet_server::cli::healthcheck;
use fleet_server::cli::serve::{self, Exit};
use fleet_server::config::ServerConfig;
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::infra::sqlite::{self, DatabaseOptions};
use fleet_server::infra::system::{OsRandom, SystemClock};
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::store::Store as _;
use fleet_server::ports::users::{NewUser, PasswordHash};
use fleet_startup::signals::Signal;
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio_stream::StreamExt as _;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;

use crate::support::{ChannelSignals, listener, server_config, test_dir, vault_keys};

/// The longest the test waits for anything.
const PATIENCE: Duration = Duration::from_secs(30);

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";

/// A member and a live session of theirs in `config`'s database, which is
/// created and migrated here and closed again: its access token.
async fn member_session(config: &ServerConfig) -> SecretString {
    let database = sqlite::open(&config.database, DatabaseOptions::default())
        .await
        .unwrap();
    let user = UserId::new_v7(SystemClock.now(), [3; 10]).unwrap();
    let random = Arc::new(OsRandom);
    let prepared = TokenService::new(
        Arc::new(SystemClock),
        random.clone(),
        Arc::new(database.clone()),
        Arc::new(Sha256Tokens::new(random)),
        SessionPolicy::default(),
    )
    .prepare(user)
    .unwrap();
    let mut tx = database.write().await.unwrap();
    tx.users()
        .insert(&NewUser {
            id: user,
            username: Username::try_from("AfkMember").unwrap(),
            password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
            role: Role::Member,
            created_at: SystemClock.now(),
        })
        .await
        .unwrap();
    tx.sessions().insert(&prepared.new_session).await.unwrap();
    tx.commit(NewAuditEntry::new(
        SystemClock.now(),
        AuditAction::try_from("test.serve_setup").unwrap(),
        AuditOutcome::Success,
    ))
    .await
    .unwrap();
    database.close().await;
    prepared.tokens.access_token
}

/// A ticket from `POST /api/v1/events/ticket`, over one plain HTTP/1.1
/// request: the test needs no HTTP client for it.
async fn ticket(address: SocketAddr, bearer: &SecretString) -> SecretString {
    let mut stream = TcpStream::connect(address).await.unwrap();
    let request = format!(
        "POST /api/v1/events/ticket HTTP/1.1\r\nhost: {address}\r\nauthorization: Bearer {}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        bearer.expose_secret()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut answer = String::new();
    tokio::time::timeout(PATIENCE, stream.read_to_string(&mut answer))
        .await
        .unwrap()
        .unwrap();
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    let body = answer.split_once("\r\n\r\n").unwrap().1;
    let body: Value = serde_json::from_str(body).unwrap();
    SecretString::from(body["ticket"].as_str().unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_open_socket_gets_its_1001_before_the_database_closes() {
    let dir = test_dir("serve-events-shutdown");
    let (listener, address) = listener().await;
    let config = server_config(&dir, address, false);
    let bearer = member_session(&config).await;
    let (sender, signals) = ChannelSignals::new();
    let keys = vault_keys(&config);
    let server = tokio::spawn({
        let config = config.clone();
        async move { serve::run(&config, listener, None, signals, keys).await }
    });
    healthcheck::get(address, "/health/ready", PATIENCE)
        .await
        .unwrap();
    let ticket = ticket(address, &bearer).await;
    let mut request = format!(
        "ws://{address}/api/v1/events?ticket={}",
        ticket.expose_secret()
    )
    .into_client_request()
    .unwrap();
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {}", bearer.expose_secret())).unwrap(),
    );
    let stream = TcpStream::connect(address).await.unwrap();
    let (mut socket, _) = tokio_tungstenite::client_async(request, stream)
        .await
        .unwrap();
    let ready = tokio::time::timeout(PATIENCE, socket.next()).await.unwrap();
    assert!(
        matches!(ready, Some(Ok(Message::Text(ref text))) if text.as_str() == r#"{"type":"ready"}"#)
    );

    sender.send(Signal::Terminate).await.unwrap();
    let closed = tokio::time::timeout(PATIENCE, socket.next()).await.unwrap();
    let exit = tokio::time::timeout(PATIENCE, server)
        .await
        .expect("the server should stop within its deadline")
        .unwrap();

    let Some(Ok(Message::Close(Some(CloseFrame { code, reason })))) = closed else {
        panic!("not a close: {closed:?}");
    };
    assert_eq!(u16::from(code), 1001);
    assert_eq!(reason.as_str(), "The server is shutting down.");
    assert_eq!(exit, Exit::Stopped);
    assert!(
        !dir.join("afkfleet.db-wal").exists(),
        "the database closed after the socket"
    );
}
