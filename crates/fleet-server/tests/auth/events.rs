//! Tests for the live events socket (Plan.md P11.5, §7.4, Appendix B;
//! ADR-0020, S28, S29, S35, R10, R17): `POST /api/v1/events/ticket` hands a
//! session a single-use ticket, and `GET /api/v1/events` opens the socket
//! with the same session's access token and that ticket.
//!
//! A socket needs a real connection, which `oneshot` can't upgrade, so the
//! harness's app is served on `127.0.0.1:0` and a tokio-tungstenite client
//! connects to it with the bearer on its handshake. Time isn't paused (sqlx
//! and real sockets): every wait is bounded by [`PATIENCE`], and nothing
//! sleeps.

mod bots;
mod limits;
mod liveness;
mod rights;
mod tickets;

use core::time::Duration;
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use fleet_api_types::events::SocketClose;
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::system::Clock as _;
use fleet_server::http::ws::SocketTimings;
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::store::Store as _;
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::io::DuplexStream;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};
use tokio_stream::StreamExt as _;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_util::sync::CancellationToken;

use super::support::{Options, Setup, setup, setup_with};

/// How long a test waits for anything a socket should do.
pub(super) const PATIENCE: Duration = Duration::from_secs(10);

/// A client's socket.
pub(super) type Socket = WebSocketStream<TcpStream>;

/// The harness's app, served for real sockets until it's dropped.
pub(super) struct Served {
    address: SocketAddr,
    stop: CancellationToken,
    task: JoinHandle<()>,
}

impl Served {
    /// Serves `setup`'s app on a fresh port.
    pub(super) async fn start(setup: &Setup) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let stop = CancellationToken::new();
        let shutdown = stop.clone();
        let app = setup.app.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service())
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
                .unwrap();
        });
        Self {
            address,
            stop,
            task,
        }
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        self.stop.cancel();
        self.task.abort();
    }
}

/// How many bytes a pipe holds each way: fewer than the shortest event
/// frame, so a client that stops reading holds up the server's next send at
/// once.
const PIPE_BYTES: usize = 8;

/// The address a piped connection claims; the harness's app has its own.
const PIPE_ADDRESS: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

/// The harness's app served over in-memory pipes of [`PIPE_BYTES`] each way
/// instead of TCP, whose buffers the server's few small frames would never
/// fill: for a client that doesn't read.
pub(super) struct Piped {
    connects: mpsc::Sender<DuplexStream>,
    task: JoinHandle<()>,
}

/// Hands axum each pipe a client opens.
struct PipeListener(mpsc::Receiver<DuplexStream>);

impl axum::serve::Listener for PipeListener {
    type Io = DuplexStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.0.recv().await {
            Some(pipe) => (pipe, PIPE_ADDRESS),
            // The test is over: nothing connects again.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(PIPE_ADDRESS)
    }
}

impl Piped {
    /// Serves `setup`'s app over pipes.
    pub(super) fn start(setup: &Setup) -> Self {
        let (connects, incoming) = mpsc::channel(1);
        let app = setup.app.clone();
        let task = tokio::spawn(async move {
            axum::serve(PipeListener(incoming), app.into_make_service())
                .await
                .unwrap();
        });
        Self { connects, task }
    }

    /// Opens a socket over a fresh pipe with `bearer` and `ticket`; nothing
    /// reads it unless the test does.
    pub(super) async fn open(
        &self,
        bearer: &SecretString,
        ticket: &SecretString,
    ) -> WebSocketStream<DuplexStream> {
        let (client, server) = tokio::io::duplex(PIPE_BYTES);
        self.connects.send(server).await.unwrap();
        let url = format!(
            "ws://127.0.0.1/api/v1/events?ticket={}",
            ticket.expose_secret()
        );
        let mut request = url.into_client_request().unwrap();
        request.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {}", bearer.expose_secret())).unwrap(),
        );
        let (socket, _) = timeout(PATIENCE, tokio_tungstenite::client_async(request, client))
            .await
            .expect("the handshake in time")
            .unwrap();
        socket
    }
}

impl Drop for Piped {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Commits a write that changes nothing, which moves the commit count and
/// so wakes every socket to check its session.
pub(super) async fn commit_anything(setup: &Setup) {
    let tx = setup.db.write().await.unwrap();
    tx.commit(NewAuditEntry::new(
        setup.clock.now(),
        AuditAction::try_from("test.any_commit").unwrap(),
        AuditOutcome::Success,
    ))
    .await
    .unwrap();
}

/// The server's refusal of a connect, as a client reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Refused {
    pub(super) status: u16,
    /// Every header but the date and the request ID.
    pub(super) headers: BTreeMap<String, String>,
    /// The body, the request ID replaced.
    pub(super) body: Value,
}

/// A ticket for `bearer`'s session, from `POST /api/v1/events/ticket`.
pub(super) async fn ticket(setup: &Setup, bearer: &SecretString) -> SecretString {
    let seen = setup
        .post("/api/v1/events/ticket", Some(bearer), None)
        .await;
    assert_eq!(seen.status, 200, "{}", seen.text);
    SecretString::from(seen.body["ticket"].as_str().unwrap())
}

/// Opens the socket at `served` with `bearer` (if any) and `query`, as
/// `GET /api/v1/events?{query}`.
pub(super) async fn open_with(
    served: &Served,
    bearer: Option<&SecretString>,
    query: &str,
) -> Result<Socket, Refused> {
    let url = format!("ws://{}/api/v1/events?{query}", served.address);
    let mut request = url.into_client_request().unwrap();
    if let Some(bearer) = bearer {
        request.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {}", bearer.expose_secret())).unwrap(),
        );
    }
    let stream = TcpStream::connect(served.address).await.unwrap();
    match timeout(PATIENCE, tokio_tungstenite::client_async(request, stream))
        .await
        .unwrap()
    {
        Ok((socket, _)) => Ok(socket),
        Err(WsError::Http(response)) => {
            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .filter(|(name, _)| !matches!(name.as_str(), "x-request-id" | "date"))
                .map(|(name, value)| (name.to_string(), value.to_str().unwrap().to_owned()))
                .collect();
            let mut body: Value =
                serde_json::from_slice(response.body().as_deref().unwrap_or_default()).unwrap();
            if let Some(error) = body.get_mut("error") {
                error["request_id"] = json!("[request_id]");
            }
            Err(Refused {
                status,
                headers,
                body,
            })
        }
        Err(other) => panic!("the connect failed: {other}"),
    }
}

/// Opens the socket with `bearer` and `ticket`.
pub(super) async fn open(
    served: &Served,
    bearer: &SecretString,
    ticket: &SecretString,
) -> Result<Socket, Refused> {
    open_with(
        served,
        Some(bearer),
        &format!("ticket={}", ticket.expose_secret()),
    )
    .await
}

/// A socket of `bearer`'s session, whose `ready` was read.
pub(super) async fn ready(setup: &Setup, served: &Served, bearer: &SecretString) -> Socket {
    let ticket = ticket(setup, bearer).await;
    let mut socket = open(served, bearer, &ticket).await.unwrap();
    assert_eq!(next_event(&mut socket).await, json!({ "type": "ready" }));
    socket
}

/// The next frame, within [`PATIENCE`]; `None` once the socket ended.
pub(super) async fn next<S>(socket: &mut WebSocketStream<S>) -> Option<Message>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    timeout(PATIENCE, socket.next())
        .await
        .expect("a frame in time")
        .map(|frame| frame.unwrap())
}

/// The next frame, which must be an event.
pub(super) async fn next_event<S>(socket: &mut WebSocketStream<S>) -> Value
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match next(socket).await {
        Some(Message::Text(text)) => serde_json::from_str(&text).unwrap(),
        other => panic!("not an event: {other:?}"),
    }
}

/// The server's close: its code and reason, within [`PATIENCE`] in all;
/// nothing but pings may come before it.
pub(super) async fn closed(socket: &mut Socket) -> (u16, String) {
    let reading = async {
        loop {
            match next(socket).await {
                Some(Message::Close(Some(CloseFrame { code, reason }))) => {
                    return (code.into(), reason.to_string());
                }
                Some(Message::Ping(_)) => {}
                other => panic!("not a close: {other:?}"),
            }
        }
    };
    timeout(PATIENCE, reading).await.expect("a close in time")
}

/// The next event, past the pings that come meanwhile, within [`PATIENCE`]
/// in all.
pub(super) async fn next_event_after_pings(socket: &mut Socket) -> Value {
    let reading = async {
        loop {
            match next(socket).await {
                Some(Message::Text(text)) => return serde_json::from_str(&text).unwrap(),
                Some(Message::Ping(_)) => {}
                other => panic!("not an event: {other:?}"),
            }
        }
    };
    timeout(PATIENCE, reading).await.expect("an event in time")
}

/// `close`'s code and reason.
pub(super) fn said(close: SocketClose) -> (u16, String) {
    (close.code(), close.reason().to_owned())
}

#[sqlx::test(migrations = false)]
async fn a_ticket_opens_one_socket_and_ready_comes_first(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let ticket = ticket(&setup, &alice.token).await;

    let mut socket = open(&served, &alice.token, &ticket).await.unwrap();
    let first = next(&mut socket).await;

    assert_eq!(first, Some(Message::Text(r#"{"type":"ready"}"#.into())));
}

#[sqlx::test(migrations = false)]
async fn a_ticket_is_a_secret_for_30_s(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;

    let seen = setup
        .post("/api/v1/events/ticket", Some(&alice.token), None)
        .await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    let ticket = seen.body["ticket"].as_str().unwrap();
    assert!(ticket.starts_with("afk_wt_"), "{ticket}");
    assert_eq!(ticket.len(), 50);
    assert_eq!(seen.body["expires_in"], 30);
    assert_eq!(seen.headers["cache-control"], "no-store");
}

#[sqlx::test(migrations = false)]
async fn a_fourth_socket_is_429_too_many_connections_with_retry_after_45(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let mut held = Vec::new();
    for _ in 0..3 {
        held.push(ready(&setup, &served, &alice.token).await);
    }
    let fourth = ticket(&setup, &alice.token).await;

    let refused = open(&served, &alice.token, &fourth).await.unwrap_err();

    assert_eq!(refused.status, 429);
    assert_eq!(refused.headers["retry-after"], "45");
    assert_eq!(refused.body["error"]["code"], "too_many_connections");
    assert_eq!(
        refused.body["error"]["message"],
        "Live updates are already open in too many places. Close the app somewhere else, or try again in a minute."
    );
    let bob = setup
        .person(2, "bob", fleet_core::authz::Role::Member)
        .await;
    assert!(
        open(&served, &bob.token, &ticket(&setup, &bob.token).await)
            .await
            .is_ok(),
        "the cap is each user's own"
    );
}

#[sqlx::test(migrations = false)]
async fn a_closed_socket_frees_its_place(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let mut held = Vec::new();
    for _ in 0..3 {
        held.push(ready(&setup, &served, &alice.token).await);
    }
    let mut leaving = held.pop().unwrap();

    leaving.close(None).await.unwrap();
    while next(&mut leaving).await.is_some() {}
    let again = open(&served, &alice.token, &ticket(&setup, &alice.token).await).await;

    assert!(again.is_ok(), "{:?}", again.err());
}

#[sqlx::test(migrations = false)]
async fn a_socket_closes_with_1001_when_its_owner_shuts_down(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &alice.token).await;

    let shutdown = setup.live_events.sockets().shutdown();
    let close = closed(&mut socket);
    let (said_close, ()) = tokio::join!(close, async {
        timeout(PATIENCE, shutdown).await.unwrap();
    });

    assert_eq!(said_close, said(SocketClose::ShuttingDown));
    assert_eq!(setup.live_events.sockets().open(), 0);
}

#[sqlx::test(migrations = false)]
async fn a_stopped_socket_whose_client_doesnt_read_ends_within_2_s(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    // A send timeout far past the bound: only the stop may end the send.
    let setup = setup_with(
        options,
        Options {
            sockets: SocketTimings {
                send_timeout: Duration::from_secs(60),
                ..SocketTimings::default()
            },
            ..Options::default()
        },
    )
    .await;
    let alice = setup.alice_person().await;
    let piped = Piped::start(&setup);
    let ticket = ticket(&setup, &alice.token).await;
    // Never read: `ready` itself is stuck in the pipe.
    let _socket = piped.open(&alice.token, &ticket).await;

    let started = Instant::now();
    let shutdown = timeout(PATIENCE, setup.live_events.sockets().shutdown()).await;

    assert!(shutdown.is_ok(), "the stop ends a stuck send");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

#[sqlx::test(migrations = false)]
async fn a_stopped_socket_whose_session_check_hangs_says_1001_at_once(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &alice.token).await;
    setup.hooked.hang_socket_checks();
    commit_anything(&setup).await;
    setup.hooked.until_a_socket_check_hangs(PATIENCE).await;

    let started = Instant::now();
    let (said_close, ()) = tokio::join!(closed(&mut socket), async {
        timeout(PATIENCE, setup.live_events.sockets().shutdown())
            .await
            .unwrap();
    });

    assert_eq!(said_close, said(SocketClose::ShuttingDown));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

#[sqlx::test(migrations = false)]
async fn a_connect_still_checking_at_the_shutdown_is_waited_for_and_answers_503(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let ticket = ticket(&setup, &alice.token).await;
    setup.hooked.hang_socket_checks();

    let both = async {
        tokio::join!(open(&served, &alice.token, &ticket), async {
            setup.hooked.until_a_socket_check_hangs(PATIENCE).await;
            let counted = setup.live_events.sockets().open();
            setup.live_events.sockets().shutdown().await;
            counted
        })
    };
    let (connected, counted) = timeout(PATIENCE, both).await.expect("both end in time");

    assert_eq!(counted, 1, "the connect counts while it checks");
    let refused = connected.unwrap_err();
    assert_eq!(refused.status, 503);
    assert_eq!(refused.body["error"]["code"], "busy");
    assert_eq!(setup.live_events.sockets().open(), 0);
}

#[sqlx::test(migrations = false)]
async fn a_connect_after_the_shutdown_began_is_503_busy(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let ticket = ticket(&setup, &alice.token).await;
    timeout(PATIENCE, setup.live_events.sockets().shutdown())
        .await
        .unwrap();

    let refused = open(&served, &alice.token, &ticket).await.unwrap_err();

    assert_eq!(refused.status, 503);
    assert_eq!(refused.body["error"]["code"], "busy");
}

#[sqlx::test(migrations = false)]
async fn a_socket_whose_client_doesnt_answer_its_close_ends_within_its_bound(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(options, Options::default()).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let _socket = ready(&setup, &served, &alice.token).await;

    let shutdown = timeout(PATIENCE, setup.live_events.sockets().shutdown()).await;

    assert!(
        shutdown.is_ok(),
        "the close bound ends a silent client's socket"
    );
}
