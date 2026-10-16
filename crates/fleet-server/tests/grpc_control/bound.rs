//! The connections' age (S22, R9): every connection is cut at its age
//! unless a welcomed stream lifts it, and the age starts again when that
//! stream ends. The age is lowered to 1 s; everything else keeps its
//! production value, the hello timeout's 10 s among them.

use core::time::Duration;

use fleet_core::audit::AuditAction;
use fleet_server::grpc::control::{HELLO_TOO_SLOW, ONE_HELLO};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::io::AsyncReadExt as _;
use tokio::net::TcpStream;
use tokio::time::Instant;

use super::proxy::Proxy;
use super::support::{BOUND, Call, Settings, hello, start};

/// The connections' age in these tests.
const AGE: Duration = Duration::from_secs(1);

fn settings() -> Settings {
    Settings {
        age: AGE,
        ..Settings::default()
    }
}

#[sqlx::test(migrations = false)]
async fn a_welcomed_stream_outlives_its_connections_age_which_starts_again_when_it_ends(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let mut proxy = Proxy::start(port.address).await;
    let agent = port.enroll(1, "agent-1").await;
    let client = port
        .client_at(proxy.address, Some(&agent.identity))
        .await
        .unwrap();
    let mut call = Call::open(client).await.unwrap();
    call.send(hello("agent-1")).await;
    let _ = call.welcome().await;

    // Three ages without an answer or an end.
    let quiet = tokio::time::timeout(3 * AGE, call.answers.message()).await;
    assert!(quiet.is_err(), "{quiet:?}");
    assert_eq!(proxy.closed(), 0);
    call.send(hello("agent-1")).await;
    let status = call.ended().await;
    assert_eq!(status.message(), ONE_HELLO, "{status:?}");

    // The stream ended: the connection gets its age again.
    proxy.server_closed(1).await;
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_stream_never_welcomed_is_cut_with_its_connection_at_the_age(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let agent = port.enroll(1, "agent-1").await;
    let mut call = port.open(&agent).await.unwrap();
    let opened = Instant::now();

    let status = call.ended().await;

    assert_ne!(status.message(), HELLO_TOO_SLOW, "{status:?}");
    assert!(opened.elapsed() < BOUND, "{:?}", opened.elapsed());
    assert_eq!(port.entries(&AuditAction::AGENT_CONNECT).await, Vec::new());
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_socket_that_never_starts_tls_is_cut_at_its_age(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let mut socket = TcpStream::connect(port.address).await.unwrap();
    let connected = Instant::now();

    // TLS's handshake timeout is 10 s: well past the bound.
    let mut byte = [0; 1];
    let read = tokio::time::timeout(BOUND, socket.read(&mut byte))
        .await
        .unwrap();

    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
    assert!(connected.elapsed() >= AGE.saturating_sub(Duration::from_millis(100)));
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_refused_certificate_doesnt_keep_its_connection(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let mut proxy = Proxy::start(port.address).await;
    let agent = port.enroll(1, "agent-1").await;
    port.revoke(&agent).await;
    let client = port
        .client_at(proxy.address, Some(&agent.identity))
        .await
        .unwrap();

    // Kept, so only the server can close the connection.
    let _kept = client.clone();

    let _ = Call::open(client).await.unwrap_err();

    proxy.server_closed(1).await;
    port.stop().await;
}
