//! The live events socket's transport (Plan.md P11.5; ADR-0020, S35, R9,
//! R19–R21): its bounds, the owner of every socket's task, and the rules a
//! socket follows for what its client sends and when it says `resync`.
//!
//! - **The bounds** ([`SocketTimings`], U13): a ping every 15 s, and a close
//!   (4000) once 45 s pass without a pong; each send within 10 s, or the
//!   socket closes (4002); one `resync` at most every 5 s; 1 s for the close
//!   frame at shutdown. The client's frames hold at most
//!   [`MAX_CLIENT_FRAME`] bytes and are read through a [`READ_BUFFER`]; what
//!   the server writes goes out one frame at a time, with at most
//!   [`MAX_WRITE_BUFFER`] bytes buffered.
//! - **What a client may send** ([`classify`]): a pong that answers one of
//!   the server's pings ([`Pings`]: at most [`PONG_CREDITS`] outstanding),
//!   and a close. Anything else (text, binary, a ping, a pong nobody asked
//!   for, a frame too large, a protocol error) is refused with 1008. A lost
//!   connection is told apart, and closes nothing: an `io::Error` among the
//!   error's sources, or the connection ending without a close frame.
//! - **`resync` and the session check** ([`Gap`]): a socket that lagged
//!   behind the bus says so once, without a count, and never more often
//!   than one gap; a socket checks its session after the commit count
//!   moved, at most once per [`CHECK_GAP`]. What's owed within a gap waits
//!   for its end, so no lost event goes unsaid and no commit unchecked.
//! - **What it sends** ([`Project`], R29–R34): the events of the bus its
//!   viewer may see, each built for them right before its own send, one per
//!   turn of its loop, so pings, the client's frames and the stop are served
//!   between them. It takes at most [`MERGE_MAX`] events off the bus at a
//!   time; the rest wait in the bus's receiver, its only queue.
//! - **The owner** ([`LiveSockets`], R21): a tracker token is taken as the
//!   connect starts, and the socket's task runs on the tracker, so every
//!   socket and every connect has an owner of ours, and a connect after the
//!   stop is turned away. At shutdown each socket sends 1001, within two
//!   close bounds (R20: a stuck send gets one to finish, else the socket is
//!   dropped), and serve waits for them, within its drain, before the
//!   database closes.

use core::future::{self, Future};
use core::time::Duration;
use std::io;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use fleet_api_types::events::{EventMessage, SocketClose};
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::sync::watch;
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep, sleep_until, timeout};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tokio_util::task::task_tracker::TaskTrackerToken;
use tracing::info;

use crate::app::auth::{SocketCheckError, SocketSession, TokenService};
use crate::app::event_tickets::EventTickets;
use crate::app::events::{BusEvent, EventBus, LiveRights};
use crate::http::error::ApiError;
use crate::http::rate_limit::SocketSlot;

/// How often the server pings an open socket.
pub const PING_INTERVAL: Duration = Duration::from_secs(15);

/// How long a socket stays open without a pong.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(45);

/// How long one frame may take to go out before the socket closes as too
/// slow.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a closing socket gives its close frame, at shutdown.
pub const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// The shortest gap between two `resync`s of one socket.
pub const RESYNC_GAP: Duration = Duration::from_secs(5);

/// How many of the server's pings may wait for a pong.
pub const PONG_CREDITS: u8 = 3;

/// The most bytes of one frame, and of one message, a client may send: a
/// control frame's payload is at most 125 bytes anyway.
pub const MAX_CLIENT_FRAME: usize = 125;

/// The read buffer of each socket, in bytes.
pub const READ_BUFFER: usize = 4096;

/// The most bytes a socket buffers for its client before a send waits.
pub const MAX_WRITE_BUFFER: usize = 128 * 1024;

/// A socket's timings; tests replace them with short ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketTimings {
    /// How often the server pings ([`PING_INTERVAL`]).
    pub ping_interval: Duration,
    /// How long without a pong ([`IDLE_TIMEOUT`]).
    pub idle_timeout: Duration,
    /// How long one send may take ([`SEND_TIMEOUT`]).
    pub send_timeout: Duration,
    /// How long the close frame may take ([`CLOSE_TIMEOUT`]).
    pub close_timeout: Duration,
    /// The gap between two `resync`s ([`RESYNC_GAP`]).
    pub resync_gap: Duration,
    /// The gap between two checks of the session ([`CHECK_GAP`]).
    pub check_gap: Duration,
}

impl Default for SocketTimings {
    /// The constants above.
    fn default() -> Self {
        Self {
            ping_interval: PING_INTERVAL,
            idle_timeout: IDLE_TIMEOUT,
            send_timeout: SEND_TIMEOUT,
            close_timeout: CLOSE_TIMEOUT,
            resync_gap: RESYNC_GAP,
            check_gap: CHECK_GAP,
        }
    }
}

/// The owner of every live events socket's task: each watches the stop
/// token, and [`shutdown`](Self::shutdown) closes them all and waits for
/// them. Clones share them.
#[derive(Debug, Clone)]
pub struct LiveSockets {
    tasks: TaskTracker,
    stop: CancellationToken,
}

impl LiveSockets {
    /// The owner of sockets that close when `stop` is cancelled.
    #[must_use]
    pub fn new(stop: CancellationToken) -> Self {
        Self {
            tasks: TaskTracker::new(),
            stop,
        }
    }

    /// A token counted as one socket until it's dropped: taken as a connect
    /// starts, so the connect's own checks and the gap between its answer
    /// and the socket's task count too.
    pub fn token(&self) -> TaskTrackerToken {
        self.tasks.token()
    }

    /// Runs `socket` on the tracker.
    pub fn spawn<F>(&self, socket: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        drop(self.tasks.spawn(socket));
    }

    /// The token every socket watches: cancelled at shutdown.
    #[must_use]
    pub fn stop(&self) -> CancellationToken {
        self.stop.clone()
    }

    /// How many sockets, or upgrades about to be one, are open.
    #[must_use]
    pub fn open(&self) -> usize {
        self.tasks.len()
    }

    /// Closes every socket and waits until each has ended. The caller bounds
    /// the wait (serve's drain).
    pub async fn shutdown(&self) {
        self.stop.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }
}

/// What the live events routes share: the tickets, the bus, the sockets'
/// owner, the rights every socket judges its events on, and the sockets'
/// timings. Clones share them.
#[derive(Debug, Clone)]
pub struct LiveEvents {
    pub(crate) tickets: EventTickets,
    pub(crate) bus: EventBus,
    pub(crate) sockets: LiveSockets,
    pub(crate) rights: LiveRights,
    pub(crate) timings: SocketTimings,
}

impl LiveEvents {
    /// The live events over `tickets` and `bus`, their sockets owned by
    /// `sockets` and judging what they send on `rights`, with the default
    /// timings.
    #[must_use]
    pub fn new(
        tickets: EventTickets,
        bus: EventBus,
        sockets: LiveSockets,
        rights: LiveRights,
    ) -> Self {
        Self {
            tickets,
            bus,
            sockets,
            rights,
            timings: SocketTimings::default(),
        }
    }

    /// The same, with `timings` instead: tests run them short.
    #[must_use]
    pub fn with_timings(self, timings: SocketTimings) -> Self {
        Self { timings, ..self }
    }

    /// The sockets' owner, which serve shuts down.
    #[must_use]
    pub fn sockets(&self) -> &LiveSockets {
        &self.sockets
    }

    /// The rights every socket judges what it sends on (S30).
    #[must_use]
    pub fn rights(&self) -> &LiveRights {
        &self.rights
    }
}

/// What one open socket needs: who it's for, the place it holds, and its
/// share of the live events.
#[derive(Debug)]
pub(crate) struct OpenSocket {
    /// The session it was opened by, as its connect found it.
    pub(crate) session: SocketSession,
    /// The user's place, given back when the socket ends.
    pub(crate) slot: SocketSlot,
    /// The commit count its session check follows, subscribed before the
    /// connect's own check, so a commit after that check is never missed.
    pub(crate) commits: watch::Receiver<u64>,
    /// When it closes at the latest: when the access token it opened with
    /// expires, counted at the connect.
    pub(crate) deadline: Instant,
    /// The bus, the stop token and the timings.
    pub(crate) events: LiveEvents,
    /// The session check.
    pub(crate) tokens: TokenService,
}

/// How long a socket waits for its session check: the read pool's acquire
/// timeout.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// The shortest gap between two checks of one socket's session: a stream of
/// commits costs each socket one read per gap, and a commit within the gap
/// is checked when it ends (PR #66's review).
pub const CHECK_GAP: Duration = Duration::from_millis(250);

/// How many events a socket takes off the bus before it sent what they make
/// (R33): the rest wait in the bus's receiver.
pub const MERGE_MAX: usize = 64;

/// What one socket sends its viewer (ADR-0020, R29): the bus's events they
/// may see, each built for them right before its send, on the rights as
/// they are then (S30, S32–S34, R30–R33). `handlers::events::Viewer` is the
/// one implementation; the socket's loop drives it.
pub(crate) trait Project: Send + 'static {
    /// Reads, before `ready`, the rights the socket starts from and what its
    /// viewer sees then.
    ///
    /// # Errors
    /// [`Unprojected`] when the read fails: the socket closes with 1013.
    fn open(&mut self) -> impl Future<Output = Result<(), Unprojected>> + Send;

    /// Whether it takes another event: it holds fewer than [`MERGE_MAX`].
    fn has_room(&self) -> bool;

    /// Takes `event`, the bus's next one.
    fn take(&mut self, event: Arc<BusEvent>);

    /// Whether it holds anything to send.
    fn has_work(&self) -> bool;

    /// Its next step, built now on the rights as they are now.
    ///
    /// # Errors
    /// [`Unprojected`] when a read fails: the socket closes with 1013.
    fn next(&mut self) -> impl Future<Output = Result<Step, Unprojected>> + Send;

    /// The socket said `resync`: its viewer fetches everything again.
    fn resynced(&mut self);
}

/// What a socket does next for its viewer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Step {
    /// Sends this frame.
    Send(Box<EventMessage>),
    /// Owes a `resync`, said within its gap.
    Resync,
    /// Nothing: what it took makes no frame for this viewer.
    Nothing,
}

/// A read the projection needed failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Unprojected;

/// How a socket ended, for its close and its last log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// The server closes it, with this code and reason; `why` says what for.
    Closed {
        /// The close.
        close: SocketClose,
        /// What for, in the log line.
        why: &'static str,
    },
    /// The client closed it.
    ClientClosed,
    /// The connection is gone.
    Lost,
    /// At the shutdown, a send didn't go out within the close bound: the
    /// socket is dropped without a close frame (R20).
    Abandoned,
}

impl Ended {
    /// The server closes with `close` for `why`.
    const fn closed(close: SocketClose, why: &'static str) -> Self {
        Self::Closed { close, why }
    }
}

/// Runs one socket until it ends (ADR-0020, S31, S35, R18–R20): subscribes
/// to the bus, opens `project` and says `ready`, then, in this order of
/// precedence:
/// - closes with 1001 at the shutdown, and with 4001 once the access token
///   it opened with expires, without a read;
/// - takes a pong that answers a ping and the client's close; closes with
///   1008 for anything else, and with 4000 once no pong came for the idle
///   timeout;
/// - pings on its interval;
/// - checks its session again once the commit count moved, at most once per
///   [`CHECK_GAP`], closing with 4001 when it no longer counts and with
///   1013 when the check can't be made;
/// - says `resync` once it lagged behind the bus, at most once per gap;
/// - sends what `project` builds of the events it took, one step a turn;
/// - takes the bus's next events, while `project` has room.
///
/// A send that doesn't go out in time closes it with 4002. The shutdown
/// races every send and every check (R20): a send then gets the close bound
/// to finish, else the socket is dropped, and a check is given up at once,
/// so a socket ends within two close bounds of the stop: a send finishing,
/// then its 1001. One `info` line says when it opened, and one when and why
/// it ended.
pub(crate) async fn run<P: Project>(socket: WebSocket, open: OpenSocket, project: P) {
    let OpenSocket {
        session,
        slot: _held,
        commits,
        deadline,
        events,
        tokens,
    } = open;
    let started = Instant::now();
    let mut running = Running {
        socket,
        session,
        commits,
        bus: events.bus.subscribe(),
        tokens,
        stop: events.sockets.stop(),
        timings: events.timings,
        resyncs: 0,
        project,
    };
    info!("a live events socket opened");
    let ended = match running.open().await {
        Ok(()) => match running.send(&EventMessage::ready()).await {
            Ok(()) => running.serve(deadline).await,
            Err(failed) => failed.ended(),
        },
        Err(ended) => ended,
    };
    match ended {
        Ended::Closed { close, .. } => running.close(close).await,
        Ended::ClientClosed => running.drain().await,
        Ended::Lost | Ended::Abandoned => {}
    }
    let (why, code) = match ended {
        Ended::Closed { close, why } => (why, Some(close.code())),
        Ended::ClientClosed => ("the client closed it", None),
        Ended::Lost => ("the connection was lost", None),
        Ended::Abandoned => ("a send was stuck when the server stopped", None),
    };
    info!(
        why,
        code,
        lasted_secs = started.elapsed().as_secs(),
        resyncs = running.resyncs,
        "a live events socket closed"
    );
}

/// One socket while it runs: its connection, and what its loop reads.
struct Running<P> {
    socket: WebSocket,
    /// The session it was opened by.
    session: SocketSession,
    /// The commit count its session check follows.
    commits: watch::Receiver<u64>,
    /// Its subscription to the bus.
    bus: Receiver<Arc<BusEvent>>,
    /// The session check.
    tokens: TokenService,
    /// Cancelled at the shutdown.
    stop: CancellationToken,
    timings: SocketTimings,
    /// How many `resync`s it said, for its last log line.
    resyncs: u32,
    /// What it sends its viewer.
    project: P,
}

impl<P: Project> Running<P> {
    /// The socket's loop once `ready` went out; see [`run`]. `deadline` is
    /// when the access token it opened with expires.
    async fn serve(&mut self, deadline: Instant) -> Ended {
        let timings = self.timings;
        let stop = self.stop.clone();
        let mut pings = Pings::default();
        let mut ticker = interval_at(
            Instant::now() + timings.ping_interval,
            timings.ping_interval,
        );
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let idle = sleep(timings.idle_timeout);
        let expiry = sleep_until(deadline);
        tokio::pin!(idle, expiry);
        let mut resync = Gap::new(timings.resync_gap);
        let mut checks = Gap::new(timings.check_gap);
        loop {
            // The count is read at every turn, whichever branch woke it, so
            // no busy branch can hide a commit; one between the connect's
            // check and the first turn is seen at once (R18).
            match self.commits.has_changed() {
                Ok(true) => {
                    self.commits.borrow_and_update();
                    checks.owe();
                }
                Ok(false) => {}
                Err(_) => return Ended::closed(SocketClose::Busy, "the commit count is gone"),
            }
            let now = Instant::now();
            let check_at = checks.due(now);
            let resync_at = resync.due(now);
            tokio::select! {
                biased;
                // Every branch is cancel-safe: `cancelled`, the sleeps,
                // `recv`, `tick` and `changed` lose nothing when another
                // wins, and every send and check runs in a branch's body,
                // never as a branch. The client's frames, the idle close and
                // the pings come before the session check, so no stream of
                // commits can hold them up.
                () = stop.cancelled() => return Ended::closed(SocketClose::ShuttingDown, "the server is shutting down"),
                () = &mut expiry => return Ended::closed(SocketClose::Unauthenticated, "its access token expired"),
                received = self.socket.recv() => match classify(received) {
                    Inbound::Pong if pings.answered() => {
                        idle.as_mut().reset(Instant::now() + timings.idle_timeout);
                    }
                    Inbound::Pong | Inbound::Refused => {
                        return Ended::closed(SocketClose::RefusedFrame, "the client sent a frame the server doesn't take");
                    }
                    Inbound::Close => return Ended::ClientClosed,
                    Inbound::Lost => return Ended::Lost,
                },
                () = &mut idle => return Ended::closed(SocketClose::Idle, "no pong came in time"),
                _ = ticker.tick() => {
                    if pings.send()
                        && let Err(failed) = self.send_frame(Message::Ping(Bytes::new())).await
                    {
                        return failed.ended();
                    }
                }
                () = sleep_until(check_at.unwrap_or(deadline)), if check_at.is_some() => {
                    checks.done(Instant::now());
                    if let Err(ended) = self.check().await {
                        return ended;
                    }
                }
                () = sleep_until(resync_at.unwrap_or(deadline)), if resync_at.is_some() => {
                    if let Err(failed) = self.send(&EventMessage::resync()).await {
                        return failed.ended();
                    }
                    resync.done(Instant::now());
                    self.resyncs = self.resyncs.saturating_add(1);
                    self.project.resynced();
                }
                () = future::ready(()), if self.project.has_work() => match self.step().await {
                    Ok(Step::Send(message)) => {
                        if let Err(failed) = self.send(&message).await {
                            return failed.ended();
                        }
                    }
                    Ok(Step::Resync) => resync.owe(),
                    Ok(Step::Nothing) => {}
                    Err(ended) => return ended,
                },
                received = self.bus.recv(), if self.project.has_room() => match received {
                    Ok(event) => {
                        self.project.take(event);
                        self.take_waiting(&mut resync);
                    }
                    // No count: it would count events of bots the user can't
                    // see.
                    Err(RecvError::Lagged(_)) => resync.owe(),
                    Err(RecvError::Closed) => return Ended::closed(SocketClose::ShuttingDown, "the bus is gone"),
                },
                // Returning marks the count seen, so the check is owed here.
                changed = self.commits.changed() => {
                    if changed.is_err() {
                        return Ended::closed(SocketClose::Busy, "the commit count is gone");
                    }
                    checks.owe();
                }
            }
        }
    }

    /// Takes the events already waiting on the bus while the projection has
    /// room, in order (R33); a lag among them owes a `resync`.
    fn take_waiting(&mut self, resync: &mut Gap) {
        while self.project.has_room() {
            match self.bus.try_recv() {
                Ok(event) => self.project.take(event),
                Err(TryRecvError::Lagged(_)) => resync.owe(),
                // A closed bus shows at the next `recv`.
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
    }

    /// Opens the projection within [`CHECK_TIMEOUT`]: 1013 when its rights
    /// can't be read, and 1001 at once when the server stops meanwhile.
    async fn open(&mut self) -> Result<(), Ended> {
        let opened = tokio::select! {
            biased;
            // Both are cancel-safe: opening only reads.
            () = self.stop.cancelled() => {
                return Err(Ended::closed(SocketClose::ShuttingDown, "the server is shutting down"));
            }
            opened = timeout(CHECK_TIMEOUT, self.project.open()) => opened,
        };
        match opened {
            Ok(Ok(())) => Ok(()),
            Ok(Err(Unprojected)) | Err(_) => Err(Ended::closed(
                SocketClose::Busy,
                "its rights couldn't be read",
            )),
        }
    }

    /// The projection's next step within [`CHECK_TIMEOUT`]: 1013 when a read
    /// it needs fails, and 1001 at once when the server stops meanwhile.
    async fn step(&mut self) -> Result<Step, Ended> {
        let stepped = tokio::select! {
            biased;
            // Dropping a step only drops reads; the socket ends either way.
            () = self.stop.cancelled() => {
                return Err(Ended::closed(SocketClose::ShuttingDown, "the server is shutting down"));
            }
            stepped = timeout(CHECK_TIMEOUT, self.project.next()) => stepped,
        };
        match stepped {
            Ok(Ok(step)) => Ok(step),
            Ok(Err(Unprojected)) | Err(_) => Err(Ended::closed(
                SocketClose::Busy,
                "an event couldn't be read",
            )),
        }
    }

    /// Checks the socket's session again within [`CHECK_TIMEOUT`]: 4001 when
    /// it no longer counts, 1013 when it can't be checked, and 1001 at once
    /// when the server stops meanwhile (R20).
    async fn check(&mut self) -> Result<(), Ended> {
        let checked = tokio::select! {
            biased;
            // Both are cancel-safe: the check only reads.
            () = self.stop.cancelled() => {
                return Err(Ended::closed(SocketClose::ShuttingDown, "the server is shutting down"));
            }
            checked = timeout(CHECK_TIMEOUT, self.tokens.check_socket(&self.session)) => checked,
        };
        match checked {
            Ok(Ok(())) => Ok(()),
            Ok(Err(SocketCheckError::Refused(_))) => Err(Ended::closed(
                SocketClose::Unauthenticated,
                "its session no longer counts",
            )),
            Ok(Err(SocketCheckError::Store(_))) | Err(_) => Err(Ended::closed(
                SocketClose::Busy,
                "its session couldn't be checked",
            )),
        }
    }

    /// Sends `message` as one text frame.
    async fn send(&mut self, message: &EventMessage) -> Result<(), SendFailed> {
        let text = serde_json::to_string(message).map_err(|_| SendFailed::Unwritable)?;
        self.send_frame(Message::Text(text.into())).await
    }

    /// Sends `frame` within the send timeout. Once the server stops, the
    /// send gets the close bound to finish, and the socket is given up when
    /// it doesn't (R20).
    async fn send_frame(&mut self, frame: Message) -> Result<(), SendFailed> {
        let timings = self.timings;
        let sending = self.socket.send(frame);
        tokio::pin!(sending);
        tokio::select! {
            biased;
            // The send isn't cancel-safe, so it's never dropped while it may
            // still finish: the stop's branch goes on polling it.
            sent = &mut sending => sent.map_err(|_| SendFailed::Lost),
            () = self.stop.cancelled() => match timeout(timings.close_timeout, &mut sending).await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(_)) => Err(SendFailed::Lost),
                Err(_) => Err(SendFailed::Abandoned),
            },
            () = sleep(timings.send_timeout) => Err(SendFailed::TooSlow),
        }
    }

    /// Closes the socket with `why`'s code and reason, then reads until the
    /// client's close or the end, so the TCP connection doesn't end with a
    /// reset: both within one close bound (R20).
    async fn close(&mut self, why: SocketClose) {
        let bound = self.timings.close_timeout;
        let frame = CloseFrame {
            code: why.code(),
            reason: why.reason().into(),
        };
        let closing = async {
            if self.socket.send(Message::Close(Some(frame))).await.is_ok() {
                while let Some(Ok(_)) = self.socket.recv().await {}
            }
        };
        // Past the bound the socket is dropped; nothing waits for it.
        let _ = timeout(bound, closing).await;
    }

    /// Reads until the close handshake ends, within the close bound: a read
    /// writes what tungstenite queued, the reply to a client's close among
    /// it.
    async fn drain(&mut self) {
        let bound = self.timings.close_timeout;
        let draining = async { while let Some(Ok(_)) = self.socket.recv().await {} };
        // Past the bound the socket is dropped; nothing waits for it.
        let _ = timeout(bound, draining).await;
    }
}

/// Why a frame didn't go out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendFailed {
    /// The connection failed.
    Lost,
    /// The client didn't read it within the send timeout.
    TooSlow,
    /// The message didn't serialize, which can't happen.
    Unwritable,
    /// The server stopped, and the send didn't finish within the close
    /// bound after.
    Abandoned,
}

impl SendFailed {
    /// How the socket ends after it.
    const fn ended(self) -> Ended {
        match self {
            Self::Lost => Ended::Lost,
            Self::TooSlow => Ended::closed(SocketClose::TooSlow, "the client didn't read in time"),
            Self::Unwritable => Ended::closed(SocketClose::Busy, "an event didn't serialize"),
            Self::Abandoned => Ended::Abandoned,
        }
    }
}

/// The answer to a request the upgrade refused: 405 for a method that isn't
/// `GET` (a `HEAD`, which `get` routes serve too), 400 for anything else: a
/// request that isn't an upgrade, a missing key, a version other than 13, a
/// connection that can't be upgraded. Never the bare status axum would
/// answer, which `render_errors` takes for a wiring bug.
#[must_use]
pub fn refused_upgrade(rejection: &WebSocketUpgradeRejection) -> ApiError {
    match rejection {
        WebSocketUpgradeRejection::MethodNotGet(_) => ApiError::MethodNotAllowed,
        // A non-exhaustive enum of axum's: every other refusal is the
        // request's.
        _ => ApiError::BadRequest,
    }
}

/// What a client's frame is to the socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inbound {
    /// A pong: it must answer a ping.
    Pong,
    /// The client closes.
    Close,
    /// Anything the server doesn't take: closed with 1008.
    Refused,
    /// The connection is gone: nothing more can be sent.
    Lost,
}

/// What `received`, the socket's next read, is to the socket; `None` is a
/// connection that ended.
#[must_use]
pub fn classify(received: Option<Result<Message, axum::Error>>) -> Inbound {
    match received {
        None => Inbound::Lost,
        Some(Err(error)) if connection_gone(&error) => Inbound::Lost,
        Some(Ok(Message::Pong(_))) => Inbound::Pong,
        Some(Ok(Message::Close(_))) => Inbound::Close,
        Some(Ok(Message::Text(_) | Message::Binary(_) | Message::Ping(_)) | Err(_)) => {
            Inbound::Refused
        }
    }
}

/// What tungstenite's `ProtocolError::ResetWithoutClosingHandshake` says:
/// the connection ended (a FIN, a client killed) without a close frame.
/// fleet-server reaches tungstenite only through axum, so it's told by its
/// text; `a_connection_closed_without_a_close_frame_is_lost` pins the text
/// against the real error, so a tungstenite bump that changes it fails.
const ENDED_WITHOUT_CLOSE: &str = "Connection reset without closing handshake";

/// Whether `error` says the connection is gone, rather than that the client
/// broke the protocol: an I/O error among its sources, or the connection
/// ending without a close frame ([`ENDED_WITHOUT_CLOSE`]).
fn connection_gone(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if error.is::<io::Error>() || error.to_string() == ENDED_WITHOUT_CLOSE {
            return true;
        }
        current = error.source();
    }
    false
}

/// The pings that wait for a pong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pings {
    outstanding: u8,
}

impl Pings {
    /// Whether one more ping may go out: no more than [`PONG_CREDITS`] wait.
    /// Counted when it may.
    pub fn send(&mut self) -> bool {
        if self.outstanding >= PONG_CREDITS {
            return false;
        }
        self.outstanding = self.outstanding.saturating_add(1);
        true
    }

    /// Whether a pong answers: it does while any ping waits, and answers them
    /// all, since a client may answer only the latest (RFC 6455, 5.5.3); one
    /// that comes while none waits is refused.
    pub fn answered(&mut self) -> bool {
        if self.outstanding == 0 {
            return false;
        }
        self.outstanding = 0;
        true
    }
}

/// Something a socket owes at most once per gap: a `resync` once it lagged
/// behind the bus ([`RESYNC_GAP`]), a check of its session once the commit
/// count moved ([`CHECK_GAP`]). One owed within the gap is done at its end,
/// never dropped, and many owed within one gap are done once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    every: Duration,
    last: Option<Instant>,
    owed: bool,
}

impl Gap {
    /// At least `every` between two of them.
    #[must_use]
    pub const fn new(every: Duration) -> Self {
        Self {
            every,
            last: None,
            owed: false,
        }
    }

    /// One is owed.
    pub fn owe(&mut self) {
        self.owed = true;
    }

    /// When the owed one is due, if one is owed: at once, or one gap after
    /// the last.
    #[must_use]
    pub fn due(&self, now: Instant) -> Option<Instant> {
        if !self.owed {
            return None;
        }
        let allowed = self
            .last
            .and_then(|last| last.checked_add(self.every))
            .unwrap_or(now);
        Some(allowed.max(now))
    }

    /// It was done at `at`.
    pub fn done(&mut self, at: Instant) {
        self.owed = false;
        self.last = Some(at);
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Bytes;

    use super::*;

    #[test]
    fn the_bounds_are_u13s() {
        let timings = SocketTimings::default();

        assert_eq!(timings.ping_interval, Duration::from_secs(15));
        assert_eq!(timings.idle_timeout, Duration::from_secs(45));
        assert_eq!(timings.send_timeout, Duration::from_secs(10));
        assert_eq!(timings.close_timeout, Duration::from_secs(1));
        assert_eq!(timings.resync_gap, Duration::from_secs(5));
        assert_eq!(PONG_CREDITS, 3);
        assert_eq!(MAX_CLIENT_FRAME, 125);
        assert_eq!(READ_BUFFER, 4096);
        assert_eq!(MAX_WRITE_BUFFER, 131_072);
    }

    #[test]
    fn a_pong_answers_a_ping_and_an_unsolicited_one_is_refused() {
        let mut pings = Pings::default();

        let unasked = pings.answered();
        assert!(pings.send());
        let answer = pings.answered();
        let again = pings.answered();

        assert!(!unasked);
        assert!(answer);
        assert!(!again);
    }

    #[test]
    fn no_more_than_three_pings_wait_for_a_pong() {
        let mut pings = Pings::default();

        let sent: Vec<_> = (0..4).map(|_| pings.send()).collect();

        assert_eq!(sent, [true, true, true, false]);
    }

    #[test]
    fn one_pong_answers_every_ping_that_waits() {
        // RFC 6455, 5.5.3: a client may answer only the latest ping.
        let mut pings = Pings::default();
        for _ in 0..3 {
            assert!(pings.send());
        }

        let answer = pings.answered();
        let second = pings.answered();
        let room: Vec<_> = (0..4).map(|_| pings.send()).collect();

        assert!(answer);
        assert!(!second, "nothing waits for a second pong");
        assert_eq!(room, [true, true, true, false]);
    }

    #[test]
    fn a_first_resync_goes_at_once_and_the_next_one_gap_later() {
        let start = Instant::now();
        let mut gate = Gap::new(Duration::from_secs(5));

        let idle = gate.due(start);
        gate.owe();
        let first = gate.due(start);
        gate.done(start);
        let after_sending = gate.due(start);
        gate.owe();
        let second = gate.due(start + Duration::from_secs(1));

        assert_eq!(idle, None);
        assert_eq!(first, Some(start));
        assert_eq!(after_sending, None);
        assert_eq!(second, Some(start + Duration::from_secs(5)));
    }

    #[test]
    fn many_lags_within_a_gap_owe_one_resync() {
        let start = Instant::now();
        let mut gate = Gap::new(Duration::from_secs(5));
        gate.owe();
        gate.done(start);

        for _ in 0..10 {
            gate.owe();
        }
        let due = gate.due(start + Duration::from_secs(2));
        gate.done(start + Duration::from_secs(5));

        assert_eq!(due, Some(start + Duration::from_secs(5)));
        assert_eq!(gate.due(start + Duration::from_secs(6)), None);
    }

    #[test]
    fn a_commit_within_the_check_gap_is_checked_when_it_ends() {
        let start = Instant::now();
        let mut checks = Gap::new(CHECK_GAP);
        checks.owe();
        let first = checks.due(start);
        checks.done(start);

        checks.owe();
        checks.owe();
        let next = checks.due(start + Duration::from_millis(100));

        assert_eq!(CHECK_GAP, Duration::from_millis(250));
        assert_eq!(first, Some(start));
        assert_eq!(next, Some(start + Duration::from_millis(250)));
    }

    #[test]
    fn a_lag_after_the_gap_goes_at_once() {
        let start = Instant::now();
        let mut gate = Gap::new(Duration::from_secs(5));
        gate.owe();
        gate.done(start);

        gate.owe();
        let late = start + Duration::from_secs(9);

        assert_eq!(gate.due(late), Some(late));
    }

    #[test]
    fn a_pong_and_a_close_are_taken_and_everything_else_is_refused() {
        let answer = classify(Some(Ok(Message::Pong(Bytes::from_static(b"x")))));
        let close = classify(Some(Ok(Message::Close(Some(CloseFrame {
            code: 1000,
            reason: "bye".into(),
        })))));
        let bare_close = classify(Some(Ok(Message::Close(None))));
        let text = classify(Some(Ok(Message::Text("hello".into()))));
        let binary = classify(Some(Ok(Message::Binary(Bytes::from_static(b"\x00")))));
        let client_ping = classify(Some(Ok(Message::Ping(Bytes::new()))));

        assert_eq!(answer, Inbound::Pong);
        assert_eq!(close, Inbound::Close);
        assert_eq!(bare_close, Inbound::Close);
        assert_eq!(text, Inbound::Refused);
        assert_eq!(binary, Inbound::Refused);
        assert_eq!(client_ping, Inbound::Refused);
    }

    #[test]
    fn a_connection_that_ended_or_failed_in_its_io_is_lost() {
        let ended = classify(None);
        let reset = classify(Some(Err(axum::Error::new(io::Error::from(
            io::ErrorKind::ConnectionReset,
        )))));

        assert_eq!(ended, Inbound::Lost);
        assert_eq!(reset, Inbound::Lost);
    }

    #[test]
    fn a_connection_closed_without_a_close_frame_is_lost() {
        use tokio_tungstenite::tungstenite::error::{Error, ProtocolError};

        let reset = classify(Some(Err(axum::Error::new(Error::Protocol(
            ProtocolError::ResetWithoutClosingHandshake,
        )))));

        assert_eq!(reset, Inbound::Lost);
    }

    #[test]
    fn an_error_that_isnt_the_connections_is_a_refused_frame() {
        let protocol = classify(Some(Err(axum::Error::new(ProtocolBreach))));

        assert_eq!(protocol, Inbound::Refused);
    }

    /// What a route answers that takes the upgrade as the events route does.
    async fn upgrade_answer(request: axum::http::Request<axum::body::Body>) -> u16 {
        use axum::extract::ws::WebSocketUpgrade;
        use axum::response::IntoResponse as _;
        use tower::ServiceExt as _;

        let app = axum::Router::new().route(
            "/",
            axum::routing::get(
                |upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>| async move {
                    match upgrade {
                        Ok(_) => axum::http::StatusCode::NO_CONTENT.into_response(),
                        Err(rejection) => refused_upgrade(&rejection).into_response(),
                    }
                },
            ),
        );
        app.oneshot(request).await.unwrap().status().as_u16()
    }

    fn request(method: &str, upgrade: bool) -> axum::http::Request<axum::body::Body> {
        let mut builder = axum::http::Request::builder().method(method).uri("/");
        if upgrade {
            builder = builder
                .header("connection", "upgrade")
                .header("upgrade", "websocket")
                .header("sec-websocket-version", "13")
                .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==");
        }
        builder.body(axum::body::Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn each_upgrade_refusal_is_an_api_errors_status() {
        let plain = upgrade_answer(request("GET", false)).await;
        let head = upgrade_answer(request("HEAD", true)).await;
        let not_upgradable = upgrade_answer(request("GET", true)).await;

        assert_eq!(plain, 400, "not an upgrade");
        assert_eq!(head, 405, "a method that isn't GET");
        assert_eq!(not_upgradable, 400, "a connection that can't be upgraded");
    }

    /// A protocol error with no I/O error among its sources, as tungstenite
    /// reports a frame too large or a bad opcode.
    #[derive(Debug, thiserror::Error)]
    #[error("a protocol breach")]
    struct ProtocolBreach;

    #[tokio::test(start_paused = true)]
    async fn shutdown_closes_every_socket_and_waits_for_each() {
        let sockets = LiveSockets::new(CancellationToken::new());
        let token = sockets.token();
        let stop = sockets.stop();
        let (ended_tx, mut ended) = tokio::sync::mpsc::channel(1);
        sockets.spawn(async move {
            stop.cancelled().await;
            tokio::time::sleep(Duration::from_millis(500)).await;
            let _ = ended_tx.send(()).await;
        });
        assert_eq!(sockets.open(), 2);

        let shutdown = sockets.shutdown();
        tokio::pin!(shutdown);
        let early = tokio::time::timeout(Duration::from_secs(5), &mut shutdown).await;
        assert!(early.is_err(), "the token still counts");
        drop(token);
        tokio::time::timeout(Duration::from_secs(5), &mut shutdown)
            .await
            .unwrap();

        assert!(ended.try_recv().is_ok(), "the socket ended before");
        assert_eq!(sockets.open(), 0);
    }
}
