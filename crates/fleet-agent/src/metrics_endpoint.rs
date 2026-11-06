//! The agent's metrics endpoint (Plan.md P12.4; ADR-0022, S9, S10): `GET
//! /metrics` on `[metrics] bind`, in Prometheus' text format, answered by a
//! responder of the agent's own that speaks just enough HTTP/1.1.
//!
//! - **Only `GET /metrics`** gets the metrics. Another path is 404, another
//!   method 405, a request head that isn't one, or that's larger than
//!   [`Limits::head_bytes`], 400. Every answer closes its connection, and a
//!   client that closes before a whole head gets none.
//! - **Bounded:** at most [`Limits::connections`] connections at once (one
//!   more is closed at once, unanswered), each within [`Limits::timeout`] of
//!   its accept, its request and its answer together. An accept that fails
//!   for a reason that isn't one connection's (too many open files) is one
//!   `error` line and a pause of [`Limits::accept_pause`], so it can't spin.
//! - **Owned:** every connection is a task of the responder's own
//!   `JoinSet`; the stop ends the accept loop and every open connection.
//! - **Nothing from a request is logged.**
//!
//! The port is internal: never published beyond the host, never behind a
//! proxy (ADR-0022, U1, U5). It needs no credentials: it shows counts under
//! fixed labels, and no label of the agent's holds a name, an address or an
//! ID (S11).

use core::fmt::{self, Write as _};
use core::future::Future;
use core::time::Duration;
use std::io;
use std::sync::Arc;

use metrics_exporter_prometheus::PrometheusHandle;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument as _, error, info};

/// The one path answered with the metrics.
pub const PATH: &str = "/metrics";

/// The content type of Prometheus' text format.
pub const TEXT_FORMAT: &str = "text/plain; version=0.0.4; charset=utf-8";

/// The content type of every other answer.
const PLAIN: &str = "text/plain; charset=utf-8";

/// How much the responder takes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Connections at once.
    pub connections: usize,
    /// One connection's whole exchange, from its accept.
    pub timeout: Duration,
    /// The largest request head read.
    pub head_bytes: usize,
    /// The wait after an accept that failed for a reason that isn't one
    /// connection's.
    pub accept_pause: Duration,
}

impl Default for Limits {
    /// Four connections, 5 s each, a 2 KiB head and a 1 s pause: a scraper
    /// asks one request every few seconds.
    fn default() -> Self {
        Self {
            connections: 4,
            timeout: Duration::from_secs(5),
            head_bytes: 2048,
            accept_pause: Duration::from_secs(1),
        }
    }
}

/// What the endpoint answers with: every series, now.
pub trait Render: Send + Sync + 'static {
    /// The series in Prometheus' text format.
    fn render(&self) -> String;
}

impl Render for PrometheusHandle {
    fn render(&self) -> String {
        Self::render(self)
    }
}

/// Where connections come from: a TCP listener, or a test's pipes.
pub trait Incoming: Send + 'static {
    /// One connection.
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;

    /// The next connection. Cancel-safe: one dropped before it's ready
    /// loses none.
    fn accept(&mut self) -> impl Future<Output = io::Result<Self::Stream>> + Send;
}

impl Incoming for TcpListener {
    type Stream = TcpStream;

    async fn accept(&mut self) -> io::Result<TcpStream> {
        // Cancel-safe, as tokio documents it.
        Self::accept(self).await.map(|(stream, _peer)| stream)
    }
}

/// What a run logs when the endpoint listens, with its address.
pub const METRICS_LISTENING: &str = "the metrics endpoint is listening";

/// What a run logs when the endpoint is off (ADR-0022, S10).
pub const METRICS_OFF: &str = "metrics are off: [metrics] bind isn't set";

/// The endpoint a run serves: the listener main.rs bound on `[metrics]
/// bind`, and the handle of the recorder it installed.
pub struct Endpoint {
    /// The listener.
    pub listener: TcpListener,
    /// What `GET /metrics` renders.
    pub render: PrometheusHandle,
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Endpoint")
            .field("listener", &self.listener)
            .finish_non_exhaustive()
    }
}

/// Starts `endpoint`, if there is one, as a task of `tasks` that `stop`
/// ends, with [`Limits::default`], and logs where it listens; without one,
/// logs that metrics are off.
pub(crate) fn start(endpoint: Option<Endpoint>, tasks: &mut JoinSet<()>, stop: CancellationToken) {
    let Some(Endpoint { listener, render }) = endpoint else {
        info!("{METRICS_OFF}");
        return;
    };
    match listener.local_addr() {
        Ok(address) => info!(%address, "{METRICS_LISTENING}"),
        Err(error) => info!(%error, "{METRICS_LISTENING}"),
    }
    tasks.spawn(serve(listener, Arc::new(render), Limits::default(), stop).in_current_span());
}

/// Answers the connections `incoming` yields with `render`'s series, within
/// `limits`, until `stop` is cancelled; see the [module docs](self).
pub async fn serve<I, R>(mut incoming: I, render: Arc<R>, limits: Limits, stop: CancellationToken)
where
    I: Incoming,
    R: Render,
{
    let slots = Arc::new(Semaphore::new(limits.connections));
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            // Cancel-safe: waiting for the token loses nothing.
            () = stop.cancelled() => break,
            // Cancel-safe: it reaps a finished connection, so the set stays
            // as small as the cap.
            Some(_ended) = connections.join_next(), if !connections.is_empty() => {}
            // Cancel-safe, as `Incoming::accept` promises.
            accepted = incoming.accept() => match accepted {
                Ok(stream) => match Arc::clone(&slots).try_acquire_owned() {
                    Ok(slot) => {
                        connections.spawn(answer(stream, Arc::clone(&render), limits, slot));
                    }
                    // Over the cap: closed at once, unanswered.
                    Err(_full) => drop(stream),
                },
                Err(error) if is_one_connections(&error) => {}
                Err(error) => {
                    // Until a connection ends, every accept fails the same
                    // way: once per pause at most.
                    error!(%error, "the metrics endpoint can't accept a connection; it waits before it tries again");
                    tokio::select! {
                        // Both cancel-safe: a token and a sleep.
                        () = stop.cancelled() => break,
                        () = tokio::time::sleep(limits.accept_pause) => {}
                    }
                }
            },
        }
    }
    // The open connections end with the stop, unanswered.
    connections.shutdown().await;
}

/// Whether `error` is one connection's, which the next accept doesn't
/// share: the gRPC port's rule (ADR-0019).
fn is_one_connections(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
    )
}

/// One connection's exchange, within `limits.timeout`, holding its `slot`
/// until it ends.
async fn answer<S, R>(mut stream: S, render: Arc<R>, limits: Limits, slot: OwnedSemaphorePermit)
where
    S: AsyncRead + AsyncWrite + Unpin,
    R: Render,
{
    // A client that sends or reads too slowly loses its connection at the
    // timeout; either way the exchange ends here, and so does the slot.
    let _ended = tokio::time::timeout(limits.timeout, async {
        let head = read_head(&mut stream, limits.head_bytes).await?;
        if let Some(reply) = reply(&head, &*render) {
            stream.write_all(&reply).await?;
            stream.shutdown().await?;
        }
        io::Result::Ok(())
    })
    .await;
    drop(slot);
}

/// A request head as read.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Head {
    /// Everything before the blank line that ends it.
    Complete(Vec<u8>),
    /// Longer than the limit before it ended.
    TooLarge,
    /// The client closed before it ended.
    Cut,
}

/// Reads `stream` until the blank line that ends a request head, at most
/// `limit` bytes of it.
async fn read_head<S: AsyncRead + Unpin>(stream: &mut S, limit: usize) -> io::Result<Head> {
    let mut head = Vec::with_capacity(limit.min(512));
    let mut chunk = [0_u8; 512];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(Head::Cut);
        }
        head.extend_from_slice(chunk.get(..read).unwrap_or_default());
        if let Some(end) = head.windows(4).position(|window| window == b"\r\n\r\n") {
            if end > limit {
                return Ok(Head::TooLarge);
            }
            head.truncate(end);
            return Ok(Head::Complete(head));
        }
        if head.len() > limit {
            return Ok(Head::TooLarge);
        }
    }
}

/// What a request head asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// `GET /metrics`.
    Metrics,
    /// Another path.
    NotFound,
    /// Another method on [`PATH`].
    MethodNotAllowed,
    /// No request line of HTTP/1.0 or HTTP/1.1.
    BadRequest,
}

/// What `head` asks for, from its request line alone.
fn route(head: &[u8]) -> Route {
    let line = head
        .windows(2)
        .position(|window| window == b"\r\n")
        .and_then(|end| head.get(..end))
        .unwrap_or(head);
    let Ok(line) = core::str::from_utf8(line) else {
        return Route::BadRequest;
    };
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Route::BadRequest;
    };
    let is_method = !method.is_empty() && method.bytes().all(|byte| byte.is_ascii_uppercase());
    let is_target = target.starts_with('/') && target.bytes().all(|byte| byte.is_ascii_graphic());
    if !is_method || !is_target || !matches!(version, "HTTP/1.1" | "HTTP/1.0") {
        return Route::BadRequest;
    }
    if target != PATH {
        Route::NotFound
    } else if method == "GET" {
        Route::Metrics
    } else {
        Route::MethodNotAllowed
    }
}

/// The answer to `head`, none for a head the client cut.
fn reply<R: Render>(head: &Head, render: &R) -> Option<Vec<u8>> {
    let route = match head {
        Head::Cut => return None,
        Head::TooLarge => Route::BadRequest,
        Head::Complete(head) => route(head),
    };
    Some(match route {
        Route::Metrics => response("200 OK", TEXT_FORMAT, &[], &render.render()),
        Route::NotFound => response("404 Not Found", PLAIN, &[], "not found\n"),
        Route::MethodNotAllowed => response(
            "405 Method Not Allowed",
            PLAIN,
            &[("Allow", "GET")],
            "method not allowed\n",
        ),
        Route::BadRequest => response("400 Bad Request", PLAIN, &[], "bad request\n"),
    })
}

/// A whole HTTP/1.1 response that closes its connection.
fn response(status: &str, content_type: &str, extra: &[(&str, &str)], body: &str) -> Vec<u8> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in extra {
        // Writing to a `String` can't fail.
        let _ = write!(head, "{name}: {value}\r\n");
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body.as_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use tokio::io::{DuplexStream, duplex};
    use tokio::sync::mpsc;

    use super::*;

    /// The series a test's render answers with.
    const SERIES: &str = "afkfleet_test_total 7\n";

    /// A render that answers [`SERIES`].
    struct Fixed;

    impl Render for Fixed {
        fn render(&self) -> String {
            SERIES.to_owned()
        }
    }

    /// Connections a test hands over, one by one.
    struct Pipes(mpsc::Receiver<io::Result<DuplexStream>>);

    impl Incoming for Pipes {
        type Stream = DuplexStream;

        async fn accept(&mut self) -> io::Result<DuplexStream> {
            match self.0.recv().await {
                Some(accepted) => accepted,
                None => core::future::pending().await,
            }
        }
    }

    /// The responder over a test's pipes, with `limits`.
    struct Running {
        connect: mpsc::Sender<io::Result<DuplexStream>>,
        stop: CancellationToken,
        task: tokio::task::JoinHandle<()>,
    }

    impl Running {
        fn start(limits: Limits) -> Self {
            let (connect, accepted) = mpsc::channel(8);
            let stop = CancellationToken::new();
            let task = tokio::spawn(serve(
                Pipes(accepted),
                Arc::new(Fixed),
                limits,
                stop.clone(),
            ));
            Self {
                connect,
                stop,
                task,
            }
        }

        /// A new connection's client end.
        async fn connect(&self) -> DuplexStream {
            let (client, server) = duplex(64 * 1024);
            self.connect.send(Ok(server)).await.unwrap();
            client
        }

        /// One more accept that fails with `error`.
        async fn fail_accept(&self, error: io::Error) {
            self.connect.send(Err(error)).await.unwrap();
        }

        async fn stop(self) {
            self.stop.cancel();
            tokio::time::timeout(Duration::from_secs(1), self.task)
                .await
                .expect("the responder stops at once")
                .unwrap();
        }
    }

    /// Sends `request` on `client` and reads the whole answer.
    async fn exchange(client: &mut DuplexStream, request: &str) -> String {
        client.write_all(request.as_bytes()).await.unwrap();
        read_all(client).await
    }

    /// Everything `client` gets until the responder closes it.
    async fn read_all(client: &mut DuplexStream) -> String {
        let mut answer = String::new();
        client.read_to_string(&mut answer).await.unwrap();
        answer
    }

    /// Lets the responder's tasks run.
    async fn settle() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    #[rstest]
    #[case::the_metrics("GET /metrics HTTP/1.1", Route::Metrics)]
    #[case::http_1_0("GET /metrics HTTP/1.0", Route::Metrics)]
    #[case::with_headers("GET /metrics HTTP/1.1\r\nHost: agent\r\nAccept: */*", Route::Metrics)]
    #[case::another_path("GET /health HTTP/1.1", Route::NotFound)]
    #[case::a_query("GET /metrics?x=1 HTTP/1.1", Route::NotFound)]
    #[case::a_longer_path("GET /metrics/x HTTP/1.1", Route::NotFound)]
    #[case::post("POST /metrics HTTP/1.1", Route::MethodNotAllowed)]
    #[case::head("HEAD /metrics HTTP/1.1", Route::MethodNotAllowed)]
    #[case::a_post_elsewhere("POST /x HTTP/1.1", Route::NotFound)]
    #[case::http_2("GET /metrics HTTP/2", Route::BadRequest)]
    #[case::no_version("GET /metrics", Route::BadRequest)]
    #[case::two_spaces("GET  /metrics HTTP/1.1", Route::BadRequest)]
    #[case::four_parts("GET /metrics HTTP/1.1 x", Route::BadRequest)]
    #[case::lowercase_method("get /metrics HTTP/1.1", Route::BadRequest)]
    #[case::no_slash("GET metrics HTTP/1.1", Route::BadRequest)]
    #[case::an_absolute_url("GET http://agent/metrics HTTP/1.1", Route::BadRequest)]
    #[case::a_control_byte("GET /met\u{1}rics HTTP/1.1", Route::BadRequest)]
    #[case::empty("", Route::BadRequest)]
    fn the_request_line_decides_the_route(#[case] head: &str, #[case] expected: Route) {
        assert_eq!(route(head.as_bytes()), expected);
    }

    #[test]
    fn a_head_that_isnt_utf_8_is_a_bad_request() {
        assert_eq!(route(b"GET /m\xffetrics HTTP/1.1"), Route::BadRequest);
    }

    #[tokio::test(start_paused = true)]
    async fn get_metrics_answers_the_series_and_closes() {
        let running = Running::start(Limits::default());
        let mut client = running.connect().await;

        let answer = exchange(&mut client, "GET /metrics HTTP/1.1\r\nHost: agent\r\n\r\n").await;

        assert_eq!(
            answer,
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {TEXT_FORMAT}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{SERIES}",
                SERIES.len()
            )
        );
        running.stop().await;
    }

    #[rstest]
    #[case::another_path("GET /x HTTP/1.1\r\n\r\n", "HTTP/1.1 404 Not Found\r\n", "not found\n")]
    #[case::another_method(
        "POST /metrics HTTP/1.1\r\nContent-Length: 0\r\n\r\n",
        "HTTP/1.1 405 Method Not Allowed\r\n",
        "method not allowed\n"
    )]
    #[case::garbage(
        "\u{0}\u{1}\u{2}\r\n\r\n",
        "HTTP/1.1 400 Bad Request\r\n",
        "bad request\n"
    )]
    #[tokio::test(start_paused = true)]
    async fn anything_else_gets_its_error_and_never_the_series(
        #[case] request: &str,
        #[case] status: &str,
        #[case] body: &str,
    ) {
        let running = Running::start(Limits::default());
        let mut client = running.connect().await;

        let answer = exchange(&mut client, request).await;

        assert!(answer.starts_with(status), "{answer}");
        assert!(answer.ends_with(&format!("\r\n\r\n{body}")), "{answer}");
        assert!(!answer.contains(SERIES), "{answer}");
        running.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_405_says_get_is_allowed() {
        let running = Running::start(Limits::default());
        let mut client = running.connect().await;

        let answer = exchange(&mut client, "DELETE /metrics HTTP/1.1\r\n\r\n").await;

        assert!(answer.contains("\r\nAllow: GET\r\n"), "{answer}");
        running.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_head_over_the_limit_is_a_bad_request() {
        let running = Running::start(Limits {
            head_bytes: 64,
            ..Limits::default()
        });
        let mut client = running.connect().await;
        let long = format!(
            "GET /metrics HTTP/1.1\r\nX-Long: {}\r\n\r\n",
            "a".repeat(100)
        );

        let answer = exchange(&mut client, &long).await;

        assert!(
            answer.starts_with("HTTP/1.1 400 Bad Request\r\n"),
            "{answer}"
        );
        running.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_head_without_its_end_past_the_limit_is_a_bad_request() {
        let running = Running::start(Limits {
            head_bytes: 64,
            ..Limits::default()
        });
        let mut client = running.connect().await;

        // Never the blank line: the responder stops reading at the limit.
        client.write_all(&[b'a'; 200]).await.unwrap();
        let answer = read_all(&mut client).await;

        assert!(
            answer.starts_with("HTTP/1.1 400 Bad Request\r\n"),
            "{answer}"
        );
        running.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_client_that_closes_before_a_whole_head_gets_nothing() {
        let running = Running::start(Limits::default());
        let mut client = running.connect().await;

        client.write_all(b"GET /metr").await.unwrap();
        client.shutdown().await.unwrap();
        let answer = read_all(&mut client).await;

        assert_eq!(answer, "");
        running.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_connection_is_closed_at_the_timeout() {
        let limits = Limits::default();
        let running = Running::start(limits);
        let mut client = running.connect().await;
        settle().await;

        tokio::time::advance(
            limits
                .timeout
                .checked_sub(Duration::from_millis(10))
                .unwrap(),
        )
        .await;
        settle().await;
        // Its 1 ms ends 9 ms before the responder's timeout.
        let early = tokio::time::timeout(Duration::from_millis(1), read_all(&mut client)).await;
        // Its 20 ms end 11 ms after it.
        let answer = tokio::time::timeout(Duration::from_millis(20), read_all(&mut client)).await;

        assert!(early.is_err(), "still open just before the timeout");
        assert_eq!(
            answer.as_deref(),
            Ok(""),
            "closed unanswered at the timeout"
        );
        running.stop().await;
    }

    /// One connection over the cap is closed at once, unanswered; once a
    /// slot is free again, the next is answered.
    #[tokio::test(start_paused = true)]
    async fn a_connection_over_the_cap_is_closed_at_once() {
        let limits = Limits {
            connections: 1,
            ..Limits::default()
        };
        let running = Running::start(limits);
        let mut first = running.connect().await;
        settle().await;

        let mut over = running.connect().await;
        let refused = read_all(&mut over).await;
        let answered = exchange(&mut first, "GET /metrics HTTP/1.1\r\n\r\n").await;
        settle().await;
        let mut next = running.connect().await;
        let after = exchange(&mut next, "GET /metrics HTTP/1.1\r\n\r\n").await;

        assert_eq!(refused, "");
        assert!(answered.starts_with("HTTP/1.1 200 OK\r\n"), "{answered}");
        assert!(after.starts_with("HTTP/1.1 200 OK\r\n"), "{after}");
        running.stop().await;
    }

    /// An accept that fails for a reason that isn't one connection's waits
    /// the pause before the next accept; one connection's doesn't wait.
    #[rstest]
    #[case::too_many_files(io::Error::other("too many open files"), true)]
    #[case::one_connection_reset(io::Error::from(io::ErrorKind::ConnectionReset), false)]
    #[case::one_connection_aborted(io::Error::from(io::ErrorKind::ConnectionAborted), false)]
    #[tokio::test(start_paused = true)]
    async fn only_an_accept_error_that_isnt_one_connections_pauses(
        #[case] error: io::Error,
        #[case] pauses: bool,
    ) {
        let limits = Limits::default();
        let running = Running::start(limits);
        running.fail_accept(error).await;
        let mut client = running.connect().await;
        client
            .write_all(b"GET /metrics HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        settle().await;

        let before_the_pause =
            tokio::time::timeout(Duration::from_millis(1), read_all(&mut client)).await;

        assert_eq!(before_the_pause.is_err(), pauses);
        if pauses {
            tokio::time::advance(limits.accept_pause).await;
            let answer = read_all(&mut client).await;
            assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
        }
        running.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn the_stop_ends_the_loop_and_every_open_connection() {
        let running = Running::start(Limits::default());
        let mut open = running.connect().await;
        settle().await;

        running.stop().await;
        // Well before the connection's own timeout.
        let answer = tokio::time::timeout(Duration::from_millis(1), read_all(&mut open)).await;

        assert_eq!(answer.as_deref(), Ok(""), "closed at once, unanswered");
    }

    #[test]
    fn the_defaults_are_four_connections_of_5_s_and_a_2_kib_head() {
        assert_eq!(
            Limits::default(),
            Limits {
                connections: 4,
                timeout: Duration::from_secs(5),
                head_bytes: 2048,
                accept_pause: Duration::from_secs(1),
            }
        );
    }
}
