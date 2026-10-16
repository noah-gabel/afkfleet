//! The gRPC port's listener (Plan.md P10.3, P10.4; ADR-0019, S13, S14, S22,
//! S23).
//!
//! - **TLS** ([`tls`]): the server certificate and key of `pki init` as the
//!   identity, the CA as the root for client certificates, and client
//!   certificates optional: `Enroll` runs before an agent has one. A
//!   certificate that is presented is always verified, so one from another
//!   CA, or an expired one, fails the handshake. The handshake gets
//!   [`HANDSHAKE_TIMEOUT`].
//! - **Each connection's age** ([`Bounded`]): every accepted socket is
//!   wrapped before tonic's TLS and cut at [`CONNECTION_AGE`], unless a
//!   [`Lift`] is held on it: an agent's `Connect` stream takes one once it
//!   is welcomed, and drops it when it ends, so the age starts again then.
//!   A connection without a client certificate, or with one that never got
//!   welcomed, thus lives at most 80 s, while an agent's stays as long as
//!   its stream (S22). tonic's own age limit isn't used: it applies to every
//!   connection of the server.
//! - **Each connection's info** ([`Connection`]): the peer's address and the
//!   bound's handle, which tonic puts into every request as
//!   `TlsConnectInfo<Connection>`. The gates read it there; tonic's
//!   `Request::peer_certs()` and `remote_addr()` only know its own TCP info.
//! - **Calls and liveness** ([`router`]): at most [`MAX_CONCURRENT_STREAMS`]
//!   calls at once on a connection, and HTTP/2 keepalive from `[grpc]`'s
//!   heartbeats: a ping after an interval without anything read, and the
//!   connection closed when it isn't answered by the heartbeat timeout
//!   (S23).
//! - **Serving** ([`serve`]): on a listener the caller bound, until its stop
//!   token is cancelled; then in-flight calls finish, within the server's
//!   shutdown deadline.

use core::pin::Pin;
use core::task::{Context, Poll, Waker};
use core::time::Duration;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpListener;
use tokio::time::{Instant, Sleep, sleep_until};
use tokio_stream::{Stream, StreamExt as _};
use tokio_util::sync::CancellationToken;
use tonic::transport::server::{Connected, Router, TcpIncoming};
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tracing::error;

use super::control::ControlRoute;
use super::enrollment::{EnrollGate, EnrollmentGrpc};
use crate::config::Heartbeats;
use crate::infra::pki::LoadedPki;

/// How long a TLS handshake may take.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How many calls one connection may run at once.
pub const MAX_CONCURRENT_STREAMS: u32 = 16;
/// How long a connection lives without a [`Lift`]: from its accept, or from
/// when its last lift ended. It's the cut group B's listener made at 60 s
/// plus 20 s of grace (S14).
pub const CONNECTION_AGE: Duration = Duration::from_secs(80);

/// How long the port waits before it accepts again after an accept error
/// that isn't one connection's: out of file descriptors (`EMFILE`,
/// `ENFILE`), every accept fails until a connection ends, and tonic would
/// try again at once (axum waits the same).
pub const ACCEPT_PAUSE: Duration = Duration::from_secs(1);

/// The enrollment service behind its gate, as [`router`] takes it.
pub type EnrollmentRoute = tonic::service::interceptor::InterceptedService<
    fleet_proto::generated::agent_enrollment_server::AgentEnrollmentServer<EnrollmentGrpc>,
    EnrollGate,
>;

/// The listener's TLS over `pki`; see the [module docs](self).
#[must_use]
pub fn tls(pki: &LoadedPki) -> ServerTlsConfig {
    ServerTlsConfig::new()
        .identity(Identity::from_pem(
            &pki.server_cert_pem,
            pki.server_key_pem.as_slice(),
        ))
        .client_ca_root(Certificate::from_pem(&pki.ca_cert_pem))
        .client_auth_optional(true)
        .timeout(HANDSHAKE_TIMEOUT)
}

/// The port's server over `tls`, with its connection limits and the
/// keepalive of `heartbeats`, serving `enrollment` and `control`.
///
/// # Errors
/// tonic's error when `tls` can't be used.
pub fn router(
    tls: ServerTlsConfig,
    enrollment: EnrollmentRoute,
    control: ControlRoute,
    heartbeats: Heartbeats,
) -> Result<Router, tonic::transport::Error> {
    let mut server = Server::builder()
        .tls_config(tls)?
        .max_concurrent_streams(MAX_CONCURRENT_STREAMS)
        .http2_keepalive_interval(Some(heartbeats.keepalive_interval()))
        .http2_keepalive_timeout(Some(heartbeats.keepalive_timeout()));
    Ok(server.add_service(enrollment).add_service(control))
}

/// Serves `router` on `listener` until `stop` is cancelled, then lets
/// in-flight calls finish. Every accepted socket is [`Bounded`] at `age`.
///
/// # Errors
/// tonic's error when the server fails.
pub async fn serve(
    router: Router,
    listener: TcpListener,
    age: Duration,
    stop: CancellationToken,
) -> Result<(), tonic::transport::Error> {
    // An accepted socket is never turned into an error here: tonic ends its
    // accept loop on an error it can't recover from.
    let incoming = paced(TcpIncoming::from(listener)).map(move |accepted| {
        accepted.map(|socket| {
            let peer = socket.peer_addr().ok();
            Bounded::new(socket, peer, age)
        })
    });
    serve_incoming(router, incoming, stop).await
}

/// `incoming`, waiting [`ACCEPT_PAUSE`] after an accept error that isn't
/// one connection's before it passes the error on.
fn paced<S, T>(incoming: S) -> impl Stream<Item = io::Result<T>>
where
    S: Stream<Item = io::Result<T>>,
{
    incoming.then(|accepted| async move {
        if let Err(error) = &accepted
            && !matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::ConnectionReset
            )
        {
            // Once a second at most: until a connection ends, every accept
            // fails the same way.
            error!(error = %error, "the gRPC port can't accept a connection; it tries again in 1 s");
            tokio::time::sleep(ACCEPT_PAUSE).await;
        }
        accepted
    })
}

/// Serves `router` on the connections `incoming` yields until `stop` is
/// cancelled, then lets in-flight calls finish: [`serve`]'s path for any
/// transport, so a test can run the port over an in-memory one.
///
/// # Errors
/// tonic's error when the server fails.
pub async fn serve_incoming<I, IO>(
    router: Router,
    incoming: I,
    stop: CancellationToken,
) -> Result<(), tonic::transport::Error>
where
    I: Stream<Item = io::Result<Bounded<IO>>> + Send + 'static,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    router
        .serve_with_incoming_shutdown(incoming, stop.cancelled_owned())
        .await
}

/// A connection's bound, shared by its [`Bounded`] IO, its [`Connection`]
/// info and its [`Lift`]s.
#[derive(Debug)]
struct Bound {
    age: Duration,
    state: Mutex<BoundState>,
}

/// What [`Bound`] guards; never held across an `.await`.
#[derive(Debug)]
struct BoundState {
    /// How many lifts are held.
    lifts: usize,
    /// When the age started: the accept, or the end of the last lift.
    since: Instant,
    /// The connection's task, woken when the last lift ends so its timer
    /// starts again.
    waker: Option<Waker>,
}

impl Bound {
    fn state(&self) -> std::sync::MutexGuard<'_, BoundState> {
        // Every section leaves the state consistent, so a poisoned lock's
        // value is still good.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A connection's info, as tonic hands it to every request: its peer's
/// address and its bound. See the [module docs](self).
#[derive(Clone)]
pub struct Connection {
    peer: Option<SocketAddr>,
    bound: Arc<Bound>,
}

impl core::fmt::Debug for Connection {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The peer's address stays out of logs (S14).
        f.debug_struct("Connection").finish_non_exhaustive()
    }
}

impl Connection {
    /// The peer's address, when the socket knew it.
    #[must_use]
    pub const fn peer(&self) -> Option<SocketAddr> {
        self.peer
    }

    /// Lifts the connection's age while the returned [`Lift`] is held.
    #[must_use]
    pub fn lift(&self) -> Lift {
        self.bound.state().lifts += 1;
        Lift {
            bound: Arc::clone(&self.bound),
        }
    }
}

/// While held, its connection isn't cut at its age; see the
/// [module docs](self).
#[derive(Debug)]
pub struct Lift {
    bound: Arc<Bound>,
}

impl Drop for Lift {
    fn drop(&mut self) {
        let waker = {
            let mut state = self.bound.state();
            state.lifts = state.lifts.saturating_sub(1);
            if state.lifts > 0 {
                return;
            }
            state.since = Instant::now();
            state.waker.take()
        };
        // Wakes the connection's task, so its next poll sets the timer.
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

/// An accepted socket with its age; see the [module docs](self).
pub struct Bounded<IO> {
    io: IO,
    connection: Connection,
    timer: Pin<Box<Sleep>>,
    cut: bool,
}

impl<IO> core::fmt::Debug for Bounded<IO> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Bounded")
            .field("cut", &self.cut)
            .finish_non_exhaustive()
    }
}

impl<IO> Bounded<IO> {
    /// `io`, accepted now from `peer`, cut at `age` unless lifted.
    pub fn new(io: IO, peer: Option<SocketAddr>, age: Duration) -> Self {
        let now = Instant::now();
        let bound = Arc::new(Bound {
            age,
            state: Mutex::new(BoundState {
                lifts: 0,
                since: now,
                waker: None,
            }),
        });
        Self {
            io,
            connection: Connection { peer, bound },
            timer: Box::pin(sleep_until(now)),
            cut: false,
        }
    }

    /// Whether the connection may go on.
    fn check(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        if self.cut {
            return Err(cut());
        }
        let deadline = {
            let mut state = self.connection.bound.state();
            if !state
                .waker
                .as_ref()
                .is_some_and(|waker| waker.will_wake(cx.waker()))
            {
                state.waker = Some(cx.waker().clone());
            }
            if state.lifts > 0 {
                return Ok(());
            }
            state.since.checked_add(self.connection.bound.age)
        };
        // An age past what an Instant holds never comes.
        let Some(deadline) = deadline else {
            return Ok(());
        };
        if self.timer.deadline() != deadline {
            self.timer.as_mut().reset(deadline);
        }
        if self.timer.as_mut().poll(cx).is_ready() {
            self.cut = true;
            return Err(cut());
        }
        Ok(())
    }
}

/// The error every call on a cut connection gets.
fn cut() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "the connection reached its age")
}

impl<IO> Connected for Bounded<IO> {
    type ConnectInfo = Connection;

    fn connect_info(&self) -> Connection {
        self.connection.clone()
    }
}

impl<IO: AsyncRead + Unpin> AsyncRead for Bounded<IO> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Err(error) = this.check(cx) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut this.io).poll_read(cx, buf)
    }
}

impl<IO: AsyncWrite + Unpin> AsyncWrite for Bounded<IO> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if let Err(error) = this.check(cx) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut this.io).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if let Err(error) = this.check(cx) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut this.io).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Err(error) = this.check(cx) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut this.io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, DuplexStream, duplex};
    use tokio::time::advance;

    use super::*;

    const AGE: Duration = CONNECTION_AGE;

    /// A pipe whose server end is bounded at [`CONNECTION_AGE`].
    fn pipe(buffer: usize) -> (DuplexStream, Bounded<DuplexStream>) {
        let (client, server) = duplex(buffer);
        (client, Bounded::new(server, None, AGE))
    }

    /// `wait`, bounded at twice the age, so a connection that's never cut
    /// fails the test instead of hanging it.
    async fn within<T>(wait: impl Future<Output = T>) -> T {
        tokio::time::timeout(AGE * 2, wait)
            .await
            .expect("the wait ended within twice the age")
    }

    async fn read_one(bounded: &mut Bounded<DuplexStream>) -> io::Result<u8> {
        let mut byte = [0];
        within(bounded.read_exact(&mut byte)).await.map(|_| byte[0])
    }

    #[tokio::test(start_paused = true)]
    async fn an_accept_error_pauses_the_accepts_and_a_connections_own_doesnt() {
        let accepts = [
            Err(io::Error::other("too many open files")),
            Ok(1),
            Err(io::Error::from(io::ErrorKind::ConnectionReset)),
            Ok(2),
        ];
        let start = Instant::now();

        let seen: Vec<(Duration, bool)> = paced(tokio_stream::iter(accepts))
            .map(|accepted| (start.elapsed(), accepted.is_ok()))
            .collect()
            .await;

        assert_eq!(
            seen,
            [
                (ACCEPT_PAUSE, false),
                (ACCEPT_PAUSE, true),
                (ACCEPT_PAUSE, false),
                (ACCEPT_PAUSE, true),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_connection_without_a_lift_is_cut_at_its_age() {
        let (mut client, mut bounded) = pipe(64);
        advance(AGE.saturating_sub(Duration::from_millis(1))).await;
        client.write_all(b"x").await.unwrap();

        assert_eq!(read_one(&mut bounded).await.unwrap(), b'x');
        advance(Duration::from_millis(1)).await;
        client.write_all(b"y").await.unwrap();
        let error = read_one(&mut bounded).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test(start_paused = true)]
    async fn a_pending_read_is_woken_and_cut_at_the_age_without_traffic() {
        let (_client, mut bounded) = pipe(64);
        let start = Instant::now();

        let error = read_one(&mut bounded).await.unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(start.elapsed(), AGE);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_writer_is_cut_at_the_age_too() {
        let (_client, mut bounded) = pipe(8);
        bounded.write_all(&[0; 8]).await.unwrap();
        let start = Instant::now();

        let error = within(bounded.write_all(&[0; 8])).await.unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(start.elapsed(), AGE);
    }

    #[tokio::test(start_paused = true)]
    async fn a_lifted_connection_lives_far_past_its_age() {
        let (mut client, mut bounded) = pipe(64);
        let lift = bounded.connect_info().lift();

        advance(Duration::from_hours(24)).await;
        client.write_all(b"x").await.unwrap();
        bounded.write_all(b"y").await.unwrap();
        let mut back = [0];
        within(client.read_exact(&mut back)).await.unwrap();

        assert_eq!(read_one(&mut bounded).await.unwrap(), b'x');
        assert_eq!(back, *b"y");
        drop(lift);
    }

    #[tokio::test(start_paused = true)]
    async fn the_age_starts_again_when_the_last_lift_ends() {
        let (_client, mut bounded) = pipe(64);
        let first = bounded.connect_info().lift();
        let second = bounded.connect_info().lift();
        advance(Duration::from_secs(100)).await;
        drop(first);
        advance(Duration::from_secs(100)).await;
        drop(second);
        let released = Instant::now();

        let error = read_one(&mut bounded).await.unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(released.elapsed(), AGE);
    }

    #[tokio::test(start_paused = true)]
    async fn a_cut_connection_stays_cut_even_if_lifted_afterwards() {
        let (mut client, mut bounded) = pipe(64);
        assert!(read_one(&mut bounded).await.is_err());

        let _lift = bounded.connect_info().lift();
        client.write_all(b"x").await.unwrap();

        let error = read_one(&mut bounded).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test(start_paused = true)]
    async fn the_connect_info_carries_the_peer_and_the_bound() {
        let peer: SocketAddr = "192.0.2.10:50000".parse().unwrap();
        let (_client, server) = duplex(64);
        let bounded = Bounded::new(server, Some(peer), AGE);

        let info = bounded.connect_info();

        assert_eq!(info.peer(), Some(peer));
        assert!(Arc::ptr_eq(&info.bound, &bounded.connection.bound));
        assert!(!format!("{info:?}").contains("192.0.2.10"));
    }
}
