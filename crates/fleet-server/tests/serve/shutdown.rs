//! The graceful shutdown (Plan.md P6.12, ADR-0015): one deadline from the
//! first signal, the drain up to the request timeout, then the database's
//! close with the rest.
//!
//! - `drive`'s timing runs on paused time with stand-in futures for the
//!   server and the close, so every wait is exact and nothing touches sqlx.
//! - `serve` runs in real time over real sockets and a real database, with
//!   short timeouts where a test waits for one (CLAUDE.md: never paused time
//!   with sqlx). Every wait there is bounded by [`PATIENCE`], so a server
//!   that doesn't do its part fails the test instead of hanging it.

use core::time::Duration;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::extract::State;
use axum::routing::get;
use fleet_server::cli::healthcheck;
use fleet_server::cli::serve::{self, Exit, ShutdownTimeouts};
use fleet_server::config::DatabaseConfig;
use fleet_server::infra::sqlite::{self, Database, DatabaseOptions};
use fleet_startup::signals::Signal;
use fleet_testkit::log_buffer::LogBuffer;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::support::{
    ChannelSignals, app_of, capture, listener, server_config, test_dir, vault_keys,
};

/// The server's defaults: a 15 s request timeout and the 5 s close.
const TIMEOUTS: ShutdownTimeouts = ShutdownTimeouts {
    drain: Duration::from_secs(15),
    close: Duration::from_secs(5),
};

const fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

/// The longest a real-time test waits for anything.
const PATIENCE: Duration = Duration::from_secs(30);

/// `future`, or a failure naming `what` after [`PATIENCE`].
async fn within<T>(what: &str, future: impl Future<Output = T>) -> T {
    tokio::time::timeout(PATIENCE, future)
        .await
        .unwrap_or_else(|_| panic!("{what} within {PATIENCE:?}"))
}

/// A stand-in server that ends `after` it's asked to shut down.
fn draining(
    shutdown: &CancellationToken,
    after: Duration,
) -> impl Future<Output = io::Result<()>> + use<> {
    let shutdown = shutdown.clone();
    async move {
        shutdown.cancelled().await;
        tokio::time::sleep(after).await;
        Ok(())
    }
}

/// A stand-in server that never finishes draining.
fn never_draining() -> impl Future<Output = io::Result<()>> {
    core::future::pending()
}

/// A stand-in close that takes `after`, and a flag that's set once it's
/// done.
fn closing(after: Duration) -> (impl Future<Output = ()>, Arc<AtomicBool>) {
    let closed = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&closed);
    let close = async move {
        tokio::time::sleep(after).await;
        flag.store(true, Ordering::SeqCst);
    };
    (close, closed)
}

/// The messages `logs` holds, in order, each with its level.
fn messages(logs: &LogBuffer) -> Vec<(String, String)> {
    logs.json_lines()
        .unwrap()
        .iter()
        .map(|line| {
            (
                line["level"].as_str().unwrap().to_owned(),
                line["message"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn line(level: &str, message: &str) -> (String, String) {
    (level.to_owned(), message.to_owned())
}

const DRAIN_WARNING: &str =
    "connections were still open at the drain limit; the server stops waiting for them";
const CLOSE_ERROR: &str = "the database didn't close by the shutdown deadline";

#[tokio::test(start_paused = true)]
async fn a_signal_drains_the_server_closes_the_database_and_stops() {
    let (logs, _guard) = capture();
    let (sender, signals) = ChannelSignals::new();
    let shutdown = CancellationToken::new();
    let (close, closed) = closing(secs(1));
    sender.send(Signal::Terminate).await.unwrap();
    let started = Instant::now();

    let exit = serve::drive(
        draining(&shutdown, secs(2)),
        &shutdown,
        close,
        signals,
        TIMEOUTS,
    )
    .await;

    assert_eq!(exit, Exit::Stopped);
    assert_eq!(started.elapsed(), secs(3));
    assert!(shutdown.is_cancelled());
    assert!(closed.load(Ordering::SeqCst));
    assert_eq!(messages(&logs), [line("INFO", "shutting down")]);
    assert_eq!(
        logs.lines_with("shutting down").unwrap()[0]["signal"],
        "SIGTERM"
    );
}

#[tokio::test(start_paused = true)]
async fn a_server_still_draining_at_the_limit_is_left_and_the_database_still_closes() {
    let (logs, _guard) = capture();
    let (sender, signals) = ChannelSignals::new();
    let shutdown = CancellationToken::new();
    let (close, closed) = closing(secs(1));
    sender.send(Signal::Interrupt).await.unwrap();
    let started = Instant::now();

    let exit = serve::drive(never_draining(), &shutdown, close, signals, TIMEOUTS).await;

    assert_eq!(
        exit,
        Exit::Stopped,
        "connections cut at the limit still stop cleanly"
    );
    assert_eq!(started.elapsed(), TIMEOUTS.drain + secs(1));
    assert!(closed.load(Ordering::SeqCst));
    assert_eq!(
        messages(&logs),
        [line("INFO", "shutting down"), line("WARN", DRAIN_WARNING)]
    );
}

#[tokio::test(start_paused = true)]
async fn a_close_that_never_ends_fails_exactly_at_the_deadline() {
    let (logs, _guard) = capture();
    let (sender, signals) = ChannelSignals::new();
    let shutdown = CancellationToken::new();
    sender.send(Signal::CtrlC).await.unwrap();
    let started = Instant::now();

    let exit = serve::drive(
        draining(&shutdown, Duration::ZERO),
        &shutdown,
        core::future::pending(),
        signals,
        TIMEOUTS,
    )
    .await;

    assert_eq!(exit, Exit::ServerFailed);
    assert_eq!(exit.code(), 4);
    assert_eq!(started.elapsed(), TIMEOUTS.deadline());
    assert_eq!(
        messages(&logs),
        [line("INFO", "shutting down"), line("ERROR", CLOSE_ERROR)]
    );
}

#[tokio::test(start_paused = true)]
async fn an_early_drain_leaves_the_close_the_rest_of_the_deadline() {
    let (_logs, _guard) = capture();
    let (sender, signals) = ChannelSignals::new();
    let shutdown = CancellationToken::new();
    // Longer than the close's own 5 s, but within the 19 s the drain left.
    let (close, closed) = closing(secs(18));
    sender.send(Signal::Terminate).await.unwrap();
    let started = Instant::now();

    let exit = serve::drive(
        draining(&shutdown, secs(1)),
        &shutdown,
        close,
        signals,
        TIMEOUTS,
    )
    .await;

    assert_eq!(exit, Exit::Stopped);
    assert_eq!(started.elapsed(), secs(19));
    assert!(closed.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn a_second_signal_is_only_logged_and_changes_no_timing() {
    let (logs, _guard) = capture();
    let (sender, signals) = ChannelSignals::new();
    let shutdown = CancellationToken::new();
    sender.send(Signal::Terminate).await.unwrap();
    sender.send(Signal::Interrupt).await.unwrap();
    let started = Instant::now();

    let exit = serve::drive(
        never_draining(),
        &shutdown,
        core::future::pending(),
        signals,
        TIMEOUTS,
    )
    .await;

    assert_eq!(exit, Exit::ServerFailed);
    assert_eq!(started.elapsed(), TIMEOUTS.deadline());
    assert_eq!(
        messages(&logs),
        [
            line("INFO", "shutting down"),
            line("INFO", "already shutting down"),
            line("WARN", DRAIN_WARNING),
            line("ERROR", CLOSE_ERROR),
        ]
    );
    assert_eq!(
        logs.lines_with("already shutting down").unwrap()[0]["signal"],
        "SIGINT"
    );
}

#[tokio::test(start_paused = true)]
async fn a_server_that_ends_unasked_fails_and_still_closes_the_database() {
    let (logs, _guard) = capture();
    let shutdown = CancellationToken::new();
    let (close, closed) = closing(secs(1));
    let started = Instant::now();

    let exit = serve::drive(
        async { Err(io::Error::other("the listener broke")) },
        &shutdown,
        close,
        ChannelSignals::silent(),
        TIMEOUTS,
    )
    .await;

    assert_eq!(exit, Exit::ServerFailed);
    assert_eq!(started.elapsed(), secs(1));
    assert!(shutdown.is_cancelled());
    assert!(closed.load(Ordering::SeqCst));
    let ended = logs.lines_with("the server ended unasked").unwrap();
    assert_eq!(ended.len(), 1, "{}", logs.text());
    assert_eq!(ended[0]["level"], "ERROR");
    assert_eq!(ended[0]["error"], "the listener broke");
}

#[tokio::test(start_paused = true)]
async fn after_an_unasked_end_the_close_gets_its_own_limit() {
    let (logs, _guard) = capture();
    let shutdown = CancellationToken::new();
    let started = Instant::now();

    let exit = serve::drive(
        async { Ok(()) },
        &shutdown,
        core::future::pending(),
        ChannelSignals::silent(),
        TIMEOUTS,
    )
    .await;

    assert_eq!(exit, Exit::ServerFailed);
    assert_eq!(started.elapsed(), TIMEOUTS.close);
    assert_eq!(
        messages(&logs),
        [
            line("ERROR", "the server ended unasked"),
            line("ERROR", CLOSE_ERROR)
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_signal_source_that_ended_never_stops_the_server() {
    let shutdown = CancellationToken::new();
    let (sender, signals) = ChannelSignals::new();
    drop(sender);

    let running = tokio::time::timeout(
        secs(3600),
        serve::drive(
            never_draining(),
            &shutdown,
            core::future::pending(),
            signals,
            TIMEOUTS,
        ),
    )
    .await;

    assert!(running.is_err(), "the server kept running");
    assert!(!shutdown.is_cancelled());
}

// --- serve, over real sockets and a real database ---------------------------

/// A database in a fresh folder; the folder lives as long as it's held.
async fn database() -> (TempDir, Database) {
    let dir = TempDir::new().unwrap();
    let config = DatabaseConfig {
        path: dir.path().join("afkfleet.db"),
    };
    let database = sqlite::open(&config, DatabaseOptions::default())
        .await
        .unwrap();
    (dir, database)
}

/// A handler that answers only once the test lets it.
#[derive(Debug, Clone, Default)]
struct Gate {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

async fn held(State(gate): State<Gate>) -> &'static str {
    gate.entered.notify_one();
    gate.release.notified().await;
    "done"
}

/// Waits until `address` refuses connections. Windows takes about 2 s to
/// report a refused one.
async fn refused(address: std::net::SocketAddr) {
    within(
        "new connections should be refused during the drain",
        async {
            while let Ok(stream) = TcpStream::connect(address).await {
                drop(stream);
                tokio::task::yield_now().await;
            }
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_in_flight_request_finishes_after_the_signal_while_new_connections_are_refused() {
    let (dir, database) = database().await;
    let gate = Gate::default();
    let app = Router::new()
        .route("/held", get(held))
        .with_state(gate.clone());
    let (listener, address) = listener().await;
    let (sender, signals) = ChannelSignals::new();
    let server = tokio::spawn({
        let database = database.clone();
        async move {
            serve::serve(
                listener,
                app_of(app, &database),
                &database,
                signals,
                TIMEOUTS,
            )
            .await
        }
    });
    let request = tokio::spawn(healthcheck::get(address, "/held", PATIENCE));
    within(
        "the request should reach its handler",
        gate.entered.notified(),
    )
    .await;

    sender.send(Signal::Terminate).await.unwrap();
    refused(address).await;
    gate.release.notify_one();

    let answer = request.await.unwrap().unwrap();
    assert_eq!(answer.status, 200);
    assert_eq!(answer.body, b"done");
    let exit = within("the server should stop", server).await.unwrap();
    assert_eq!(exit, Exit::Stopped);
    assert!(database.read_pool().is_closed());
    assert!(database.write_pool().is_closed());
    assert!(!dir.path().join("afkfleet.db-wal").exists());
}

/// The housekeeping (ADR-0016), the server's one long-lived background task,
/// stops at the first signal. The drain and close limits are far beyond the
/// test's own wait, so a housekeeping that kept running would hold up the
/// close until the test fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_housekeeping_stops_at_the_first_signal() {
    let (_dir, database) = database().await;
    let app = Router::new().route("/ok", get(|| async { "ok" }));
    let (listener, _address) = listener().await;
    let (sender, signals) = ChannelSignals::new();
    let timeouts = ShutdownTimeouts {
        drain: secs(600),
        close: secs(600),
    };
    let server = tokio::spawn({
        let database = database.clone();
        async move {
            serve::serve(
                listener,
                app_of(app, &database),
                &database,
                signals,
                timeouts,
            )
            .await
        }
    });

    sender.send(Signal::Terminate).await.unwrap();

    let exit = within("the server should stop with its housekeeping", server)
        .await
        .unwrap();
    assert_eq!(exit, Exit::Stopped);
    assert!(database.write_pool().is_closed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_keep_alive_connection_doesnt_hold_up_the_shutdown() {
    let (_dir, database) = database().await;
    let app = Router::new().route("/ok", get(|| async { "ok" }));
    let (listener, address) = listener().await;
    let (sender, signals) = ChannelSignals::new();
    // A drain limit far beyond the test's own wait.
    let timeouts = ShutdownTimeouts {
        drain: secs(600),
        close: secs(5),
    };
    let server = tokio::spawn({
        let database = database.clone();
        async move {
            serve::serve(
                listener,
                app_of(app, &database),
                &database,
                signals,
                timeouts,
            )
            .await
        }
    });
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(b"GET /ok HTTP/1.1\r\nHost: test\r\n\r\n")
        .await
        .unwrap();
    within("the server should answer", async {
        let mut answer = Vec::new();
        while !answer.ends_with(b"ok") {
            let mut chunk = [0_u8; 512];
            let read = stream.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0, "the answer ended early");
            answer.extend_from_slice(&chunk[..read]);
        }
    })
    .await;

    sender.send(Signal::Terminate).await.unwrap();

    let exit = within("the server should stop", server).await.unwrap();
    assert_eq!(exit, Exit::Stopped);
    let mut rest = [0_u8; 16];
    let read = within(
        "the server should close the idle connection",
        stream.read(&mut rest),
    )
    .await;
    assert!(
        !matches!(read, Ok(count) if count > 0),
        "the idle connection was closed: {read:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connection_the_database_never_gets_back_fails_the_shutdown_at_the_deadline() {
    let (_dir, database) = database().await;
    let held = database.read_pool().acquire().await.unwrap();
    let app = Router::new().route("/ok", get(|| async { "ok" }));
    let (listener, _address) = listener().await;
    let (sender, signals) = ChannelSignals::new();
    let timeouts = ShutdownTimeouts {
        drain: secs(1),
        close: secs(1),
    };
    let server = tokio::spawn({
        let database = database.clone();
        async move {
            serve::serve(
                listener,
                app_of(app, &database),
                &database,
                signals,
                timeouts,
            )
            .await
        }
    });
    let started = Instant::now();

    sender.send(Signal::Terminate).await.unwrap();

    let exit = within("the server should stop", server).await.unwrap();
    assert_eq!(exit, Exit::ServerFailed);
    assert!(
        started.elapsed() >= timeouts.deadline(),
        "{:?}",
        started.elapsed()
    );
    drop(held);
    database.close().await;
}

/// `just dev-rotate-key` reads a `-wal` next to the dev database as a running
/// server (ADR-0018), so a server stopped by a signal must leave none behind:
/// the real `run`, a request that writes (an unknown name's failed login,
/// counted toward the lockout), then the stop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_stopped_by_a_signal_leaves_no_wal_or_shm() {
    let dir = test_dir("clean_stop_leaves_no_wal");
    let (listener, address) = listener().await;
    let config = server_config(&dir, address, false);
    let keys = vault_keys(&config);
    let (sender, signals) = ChannelSignals::new();
    let server = tokio::spawn(async move { serve::run(&config, listener, signals, keys).await });
    let body = r#"{"username":"nobody","password":"not the password at all"}"#;
    let request = format!(
        "POST /api/v1/auth/login HTTP/1.1\r\nHost: test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let answer = within("the server should answer the login", async {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).await.unwrap();
        String::from_utf8_lossy(&answer).into_owned()
    })
    .await;
    assert!(answer.starts_with("HTTP/1.1 401"), "{answer}");

    sender.send(Signal::Terminate).await.unwrap();

    let exit = within("the server should stop", server).await.unwrap();
    assert_eq!(exit, Exit::Stopped);
    assert!(dir.join("afkfleet.db").is_file());
    assert!(!dir.join("afkfleet.db-wal").exists());
    assert!(!dir.join("afkfleet.db-shm").exists());
}
