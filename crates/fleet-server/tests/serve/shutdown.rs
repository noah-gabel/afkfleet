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
use fleet_server::cli::serve::{self, Exit, ShutdownTimeouts, Watched};
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

// --- The reconciler, watched (ADR-0019, PR #52's review) -----------------------

/// Runs the reconciler and the ingest's writer that `reconciler` and
/// `ingest` make from their stop tokens, watched by one watch as serve runs
/// them, behind a stand-in port and API; fails the test if the server
/// doesn't stop on its own within a day.
async fn serve_watching<R, I>(
    reconciler: impl FnOnce(CancellationToken) -> R,
    ingest: impl FnOnce(CancellationToken) -> I,
    signals: ChannelSignals,
) -> (Exit, Arc<AtomicBool>)
where
    R: Future<Output = ()> + Send + 'static,
    I: Future<Output = ()> + Send + 'static,
{
    let shutdown = CancellationToken::new();
    let mut tasks = tokio::task::JoinSet::new();
    let (watcher, watch) = serve::task_watch();
    let stop = shutdown.child_token();
    serve::spawn_watched(
        &mut tasks,
        Watched::Reconciler,
        reconciler(stop.clone()),
        stop,
        watcher.clone(),
    );
    let stop = shutdown.child_token();
    serve::spawn_watched(
        &mut tasks,
        Watched::Ingest,
        ingest(stop.clone()),
        stop,
        watcher,
    );
    let (close, closed) = closing(secs(1));
    let close = async move {
        close.await;
        while tasks.join_next().await.is_some() {}
    };
    let exit = tokio::time::timeout(
        secs(86_400),
        serve::drive(
            serve::both(
                draining(&shutdown, secs(1)),
                Some(serve::watched(
                    draining(&shutdown, secs(2)),
                    watch,
                    shutdown.clone(),
                )),
                &shutdown,
            ),
            &shutdown,
            close,
            signals,
            TIMEOUTS,
        ),
    )
    .await
    .expect("the server should stop on its own");
    (exit, closed)
}

/// A watched task that stops only when asked.
async fn until_stopped(stop: CancellationToken) {
    stop.cancelled().await;
}

/// [`serve_watching`] with the reconciler `reconciler` makes and an ingest
/// that stops when asked.
async fn serve_with_reconciler<R>(
    reconciler: impl FnOnce(CancellationToken) -> R,
    signals: ChannelSignals,
) -> (Exit, Arc<AtomicBool>)
where
    R: Future<Output = ()> + Send + 'static,
{
    serve_watching(reconciler, until_stopped, signals).await
}

#[tokio::test(start_paused = true)]
async fn a_reconciler_that_ends_unasked_stops_the_server_with_an_error() {
    let (logs, _guard) = capture();

    let (exit, closed) = serve_with_reconciler(|_| async {}, ChannelSignals::silent()).await;

    assert_eq!(exit, Exit::ServerFailed);
    assert!(closed.load(Ordering::SeqCst));
    let reconciler = logs
        .lines_with("the reconciler ended unasked; the server stops")
        .unwrap();
    assert_eq!(reconciler.len(), 1, "{}", logs.text());
    assert_eq!(reconciler[0]["level"], "ERROR");
    let server = logs.lines_with("the server ended unasked").unwrap();
    assert_eq!(server[0]["error"], serve::RECONCILER_ENDED);
}

#[tokio::test(start_paused = true)]
async fn a_reconciler_that_panics_stops_the_server_with_an_error() {
    let (logs, _guard) = capture();

    let (exit, _closed) = serve_with_reconciler(
        |_| async { panic!("an injected failure") },
        ChannelSignals::silent(),
    )
    .await;

    assert_eq!(exit, Exit::ServerFailed);
    let failed = logs
        .lines_with("the reconciler failed; the server stops")
        .unwrap();
    assert_eq!(failed.len(), 1, "{}", logs.text());
    assert_eq!(failed[0]["level"], "ERROR");
}

#[tokio::test(start_paused = true)]
async fn a_reconciler_that_stops_with_the_shutdown_is_no_failure() {
    let (logs, _guard) = capture();
    let (sender, signals) = ChannelSignals::new();
    sender.send(Signal::Terminate).await.unwrap();

    let (exit, _closed) =
        serve_with_reconciler(|stop| async move { stop.cancelled().await }, signals).await;

    assert_eq!(exit, Exit::Stopped);
    let unasked = logs
        .lines_with("the reconciler ended unasked; the server stops")
        .unwrap();
    assert!(unasked.is_empty(), "{}", logs.text());
}

// --- The ingest's writer, watched (P10.8; ADR-0019, S70) ----------------------

#[tokio::test(start_paused = true)]
async fn an_ingest_writer_that_ends_unasked_stops_the_server_with_an_error() {
    let (logs, _guard) = capture();

    let (exit, closed) =
        serve_watching(until_stopped, |_| async {}, ChannelSignals::silent()).await;

    assert_eq!(exit, Exit::ServerFailed);
    assert!(closed.load(Ordering::SeqCst));
    let ingest = logs
        .lines_with("the ingest's writer ended unasked; the server stops")
        .unwrap();
    assert_eq!(ingest.len(), 1, "{}", logs.text());
    assert_eq!(ingest[0]["level"], "ERROR");
    let server = logs.lines_with("the server ended unasked").unwrap();
    assert_eq!(server[0]["error"], serve::INGEST_ENDED);
    let reconciler = logs.lines_with("the reconciler ended unasked").unwrap();
    assert!(reconciler.is_empty(), "{}", logs.text());
}

#[tokio::test(start_paused = true)]
async fn an_ingest_writer_that_panics_stops_the_server_with_an_error() {
    let (logs, _guard) = capture();

    let (exit, _closed) = serve_watching(
        until_stopped,
        |_| async { panic!("an injected failure") },
        ChannelSignals::silent(),
    )
    .await;

    assert_eq!(exit, Exit::ServerFailed);
    let failed = logs
        .lines_with("the ingest's writer failed; the server stops")
        .unwrap();
    assert_eq!(failed.len(), 1, "{}", logs.text());
    assert_eq!(failed[0]["level"], "ERROR");
}

#[tokio::test(start_paused = true)]
async fn an_ingest_writer_that_stops_with_the_shutdown_is_no_failure() {
    let (logs, _guard) = capture();
    let (sender, signals) = ChannelSignals::new();
    sender.send(Signal::Terminate).await.unwrap();

    let (exit, _closed) = serve_watching(until_stopped, until_stopped, signals).await;

    assert_eq!(exit, Exit::Stopped);
    let unasked = logs
        .lines_with("the ingest's writer ended unasked; the server stops")
        .unwrap();
    assert!(unasked.is_empty(), "{}", logs.text());
}

#[tokio::test(start_paused = true)]
async fn the_ingests_writer_stops_only_once_every_stream_has_ended() {
    let stop = CancellationToken::new();
    let ingest_stop = CancellationToken::new();
    let task = tokio::spawn(serve::end_streams_then_ingest(
        stop.clone(),
        // The streams take 3 s to end, from the signal on.
        async { tokio::time::sleep(secs(3)).await },
        ingest_stop.clone(),
    ));

    tokio::time::sleep(Duration::from_hours(1)).await;
    let before_the_signal = ingest_stop.is_cancelled();
    stop.cancel();
    tokio::time::sleep(Duration::from_millis(2_999)).await;
    let while_streams_end = ingest_stop.is_cancelled();
    tokio::time::sleep(Duration::from_millis(1)).await;
    task.await.unwrap();

    assert!(!before_the_signal, "nothing stops before the signal");
    assert!(!while_streams_end, "not while a stream may still admit");
    assert!(ingest_stop.is_cancelled());
}

// --- The session-token service's stop (P10.6; ADR-0019, S64) ------------------

/// `drive` with a close that first stops the session-token service, whose
/// shutdown is `stopped` and which still runs `running` refreshes, then
/// closes the database in `close_takes`: as serve joins its background
/// tasks before the database closes.
async fn drive_with_session_tokens(
    stopped: impl Future<Output = ()>,
    running: usize,
    close_takes: Duration,
) -> (Exit, Duration, Arc<AtomicBool>) {
    let (sender, signals) = ChannelSignals::new();
    let shutdown = CancellationToken::new();
    let (close, closed) = closing(close_takes);
    let tokens =
        serve::stop_session_tokens(shutdown.child_token(), stopped, TIMEOUTS.drain, move || {
            running
        });
    sender.send(Signal::Terminate).await.unwrap();
    let started = Instant::now();
    let exit = serve::drive(
        draining(&shutdown, Duration::ZERO),
        &shutdown,
        async move {
            tokens.await;
            close.await;
        },
        signals,
        TIMEOUTS,
    )
    .await;
    (exit, started.elapsed(), closed)
}

#[tokio::test(start_paused = true)]
async fn refreshes_still_running_at_the_drain_are_left_with_one_warn_and_the_database_closes() {
    let (logs, _guard) = capture();

    let (exit, took, closed) = drive_with_session_tokens(core::future::pending(), 2, secs(4)).await;

    assert_eq!(exit, Exit::Stopped, "a refresh left behind is no failure");
    assert_eq!(took, TIMEOUTS.drain + secs(4), "the drain, then the close");
    assert!(closed.load(Ordering::SeqCst));
    let left = logs.lines_with(serve::SESSION_TOKENS_LEFT).unwrap();
    assert_eq!(left.len(), 1, "{}", logs.text());
    assert_eq!(left[0]["level"], "WARN");
    assert_eq!(left[0]["flights"], 2);
}

#[tokio::test(start_paused = true)]
async fn refreshes_that_end_within_the_drain_leave_no_warn() {
    let (logs, _guard) = capture();

    let (exit, took, closed) =
        drive_with_session_tokens(tokio::time::sleep(secs(3)), 0, secs(1)).await;

    assert_eq!(exit, Exit::Stopped);
    assert_eq!(took, secs(4));
    assert!(closed.load(Ordering::SeqCst));
    assert!(
        logs.lines_with(serve::SESSION_TOKENS_LEFT)
            .unwrap()
            .is_empty(),
        "{}",
        logs.text()
    );
}

#[tokio::test(start_paused = true)]
async fn the_session_token_service_stops_only_at_the_signal() {
    let stop = CancellationToken::new();
    let asked = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&asked);
    let task = tokio::spawn(serve::stop_session_tokens(
        stop.clone(),
        async move { flag.store(true, Ordering::SeqCst) },
        TIMEOUTS.drain,
        || 0,
    ));

    tokio::time::sleep(Duration::from_hours(1)).await;
    let before = asked.load(Ordering::SeqCst);
    stop.cancel();
    task.await.unwrap();

    assert!(!before, "nothing stops before the signal");
    assert!(asked.load(Ordering::SeqCst));
}

// --- both: the API and the gRPC port as one server (ADR-0019) -----------------

/// A stand-in port that fails with `what` `after` it starts.
async fn failing(after: Duration, what: &str) -> io::Result<()> {
    tokio::time::sleep(after).await;
    Err(io::Error::other(what.to_owned()))
}

#[tokio::test(start_paused = true)]
async fn a_grpc_port_that_fails_drains_the_api_and_ends_with_its_error() {
    let shutdown = CancellationToken::new();
    let started = Instant::now();

    let ended = tokio::time::timeout(
        secs(3600),
        serve::both(
            draining(&shutdown, secs(2)),
            Some(failing(secs(3), "the port failed")),
            &shutdown,
        ),
    )
    .await
    .expect("the API should drain once the port failed");

    assert_eq!(ended.unwrap_err().to_string(), "the port failed");
    assert!(shutdown.is_cancelled());
    assert_eq!(started.elapsed(), secs(5));
}

#[tokio::test(start_paused = true)]
async fn an_api_that_ends_drains_the_grpc_port_and_ends_with_its_error() {
    let shutdown = CancellationToken::new();
    let started = Instant::now();

    let ended = tokio::time::timeout(
        secs(3600),
        serve::both(
            failing(secs(1), "the API failed"),
            Some(draining(&shutdown, secs(2))),
            &shutdown,
        ),
    )
    .await
    .expect("the port should drain once the API ended");

    assert_eq!(ended.unwrap_err().to_string(), "the API failed");
    assert!(shutdown.is_cancelled());
    assert_eq!(started.elapsed(), secs(3));
}

#[tokio::test(start_paused = true)]
async fn a_shutdown_waits_for_the_slower_of_the_two() {
    let shutdown = CancellationToken::new();
    shutdown.cancel();
    let started = Instant::now();

    let ended = serve::both(
        draining(&shutdown, secs(2)),
        Some(draining(&shutdown, secs(4))),
        &shutdown,
    )
    .await;

    assert!(ended.is_ok(), "{ended:?}");
    assert_eq!(started.elapsed(), secs(4));
}

#[tokio::test(start_paused = true)]
async fn without_a_grpc_port_it_is_the_api_alone() {
    let shutdown = CancellationToken::new();
    let started = Instant::now();

    let ended = serve::both(
        failing(secs(1), "the API failed"),
        None::<core::future::Ready<io::Result<()>>>,
        &shutdown,
    )
    .await;

    assert_eq!(ended.unwrap_err().to_string(), "the API failed");
    assert!(!shutdown.is_cancelled());
    assert_eq!(started.elapsed(), secs(1));
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
                None,
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
                None,
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
                None,
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
                None,
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
    let server =
        tokio::spawn(async move { serve::run(&config, listener, None, signals, keys).await });
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
