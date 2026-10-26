//! [`LiveEvents`]: the live events socket inside the client (Plan.md P11.8;
//! ADR-0020, U25, S53, S54, R73, R74, R76): the socket, its stop, its states
//! and one connect. Its rules are on [`LiveEvents`].

use core::fmt;
use core::future::Future;
use core::pin::{Pin, pin};
use core::time::Duration;
use std::collections::VecDeque;
use std::sync::Arc;

use fleet_api_types::events::{EventMessage, EventMessageKind, Received};
use futures_util::future::{Either, select};
use futures_util::{SinkExt, StreamExt as _};
use secrecy::ExposeSecret as _;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::watch;
use tokio::time::error::Elapsed;
use tokio::time::{Instant, sleep_until, timeout_at};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tracing::{Level, debug, info, warn};

use super::policy::{
    Action, Backoff, CLOSE_TIMEOUT, End, READY_TIMEOUT, Refused, SILENCE_TIMEOUT, level, repeats,
};
use super::read as read_frame;
use super::upgrade;
use crate::error::{ClientError, Unexpected};
use crate::session::{Core, upgrade_refusal};
use crate::store::SessionStore;
use crate::transport::{ReqwestTransport, Transport, Upgrade};

/// What the live events socket says next; see [`LiveEvents`].
#[derive(Debug)]
pub enum LiveUpdate {
    /// Fetch everything shown: after every `ready`, a `resync`, or a frame
    /// that didn't read. Each supersedes the events before it: drop anything
    /// held, fetch, then apply the events that come after it once the fetch
    /// is done.
    Refetch(RefetchReason),
    /// An event of a kind this version knows, with every field it carries.
    Event(Box<EventMessage>),
    /// The socket is down. The next `recv` waits `retry_in`, then connects
    /// again, and says `Refetch(Ready)` once it's live.
    Disconnected {
        /// Why.
        why: Disconnect,
        /// How long until the next connect: zero for the reconnect the
        /// access token's end asks for.
        retry_in: Duration,
    },
}

/// Why everything is to be fetched again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefetchReason {
    /// The socket is live: it says `ready` after every connect.
    Ready,
    /// The server says some events didn't reach this socket.
    Resync,
    /// A frame didn't read, so one may have been missed.
    Unreadable,
}

/// Why the socket went down.
#[derive(Debug)]
pub enum Disconnect {
    /// The server closed it, with this code (`SocketClose::of_code` names
    /// the ones the protocol knows), or without one.
    Closed {
        /// The close code.
        code: Option<u16>,
    },
    /// The connection went without a close, or broke the protocol: reset,
    /// ended, a frame too large or out of turn.
    Lost,
    /// No frame came in time, or no `ready` after the connect: the server is
    /// taken for gone.
    Silent,
    /// A connect failed: its ticket's request, or its upgrade, as this
    /// error says.
    NotOpened(ClientError),
}

/// How the live events socket ended: every `recv` after says the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveEnd {
    /// It was stopped.
    Stopped,
    /// The session it belongs to ended: a logout, a refused refresh, or none
    /// to begin with.
    NoSession,
    /// The server's certificate was refused, which gets no timer: a new
    /// socket once the connection is checked again (ADR-0020, R73).
    Certificate,
}

/// Stops a [`LiveEvents`] from anywhere: at once, idempotently, and for
/// good, even before its first `recv`. Clones stop the same socket.
#[derive(Clone)]
pub struct LiveStop {
    stopped: Arc<watch::Sender<bool>>,
    ended: Arc<watch::Sender<bool>>,
}

impl LiveStop {
    fn new() -> Self {
        Self {
            stopped: Arc::new(watch::Sender::new(false)),
            ended: Arc::new(watch::Sender::new(false)),
        }
    }

    /// Stops the socket: its next or current `recv` closes it and answers
    /// [`LiveEnd::Stopped`]. Never blocks.
    pub fn stop(&self) {
        self.stopped.send_replace(true);
    }

    /// Whether the socket was stopped.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        *self.stopped.borrow()
    }

    /// Waits until the socket has ended, or was dropped.
    pub async fn ended(&self) {
        let mut ended = self.ended.subscribe();
        // The sender lives in this handle, so the wait ends only with a true.
        let _ = ended.wait_for(|ended| *ended).await;
    }
}

/// Whether it was stopped and ended; nothing else is in it.
impl fmt::Debug for LiveStop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveStop")
            .field("stopped", &self.is_stopped())
            .field("ended", &*self.ended.borrow())
            .finish()
    }
}

/// The live events socket of a client's session, made by
/// `Client::live_events`.
///
/// Its consumer drives it with [`LiveEvents::recv`], which connects, reads
/// and reconnects inside; the client spawns nothing for it. Each connect asks
/// for a new ticket, through the session's usual refresh-if-needed path, and
/// spends it at once on an HTTP/1.1 upgrade of the client's own reqwest
/// connection, with the session's access token: neither ever leaves the
/// client. Then:
///
/// - **`ready`** says `Refetch(Ready)`, after every connect: a new socket may
///   have missed events. So do `resync` and a frame that doesn't read. Each
///   `Refetch` supersedes the events before it; the consumer fetches what it
///   shows and holds the events that come meanwhile.
/// - **Events** of the kinds this version knows come as they are; a kind only
///   a newer server knows is skipped (the protocol's reading rules).
/// - **An end** (the server's close, a lost connection, silence) says
///   `Disconnected` with the wait before the next connect, from the reconnect
///   policy (S54). Only the socket reconnects by itself; the REST calls never
///   retry.
/// - **It ends** when stopped ([`LiveStop`], or [`LiveEvents::close`]), at
///   once when the session ends (a logout, a refused refresh), sending its
///   close, and when the server's certificate is refused, which gets no timer
///   (R73).
///
/// It sends nothing but tungstenite's own pongs and its close. Its consumer
/// keeps `recv` polled and forwards each update at once (R74): pings are
/// answered only while it reads, and a slow reader is the server's to close
/// (4002). `recv` is cancel-safe: a wait, a connect and a frame half read all
/// go on at the next call. The waits combine through futures-util's
/// `select`, so fleet-client's tokio keeps its features (R76).
pub struct LiveEvents<S: SessionStore> {
    live: Live<S, ReqwestTransport>,
}

impl<S: SessionStore> LiveEvents<S> {
    /// The socket of `core`'s session, before any connect.
    pub(crate) fn new(core: Arc<Core<S, ReqwestTransport>>) -> Self {
        Self {
            live: Live::new(core, Backoff::seeded()),
        }
    }

    /// The next update: connects, reads and reconnects inside. Cancel-safe.
    ///
    /// # Errors
    /// [`LiveEnd`] once the socket has ended, at this call and every one
    /// after.
    pub async fn recv(&mut self) -> Result<LiveUpdate, LiveEnd> {
        self.live.recv().await
    }

    /// What stops this socket from elsewhere.
    #[must_use]
    pub fn stopper(&self) -> LiveStop {
        self.live.stopper()
    }

    /// Stops the socket and closes it, within [`CLOSE_TIMEOUT`]. Answers how
    /// it ended: [`LiveEnd::Stopped`], unless an end was decided before, such
    /// as the session's, whose close a dropped `recv` left running.
    pub async fn close(self) -> LiveEnd {
        self.live.close().await
    }
}

/// The server and the state's name; never a ticket or a token.
impl<S: SessionStore> fmt::Debug for LiveEvents<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveEvents")
            .field("server", &self.live.machine.core.server().as_str())
            .field("state", &self.live.machine.state.name())
            .finish()
    }
}

/// The socket over any transport: [`LiveEvents`] over reqwest, the unit
/// tests' over in-memory pipes.
pub(crate) struct Live<S, T: Transport> {
    machine: Machine<S, T>,
    stop: LiveStop,
    stopped: watch::Receiver<bool>,
    /// The session's epoch as every end moves it, and the one this socket
    /// belongs to.
    epochs: watch::Receiver<u64>,
    epoch: u64,
}

impl<S: SessionStore, T: Transport> Live<S, T> {
    pub(crate) fn new(core: Arc<Core<S, T>>, backoff: Backoff) -> Self {
        let stop = LiveStop::new();
        let stopped = stop.stopped.subscribe();
        let epochs = core.epochs();
        let epoch = core.epoch();
        Self {
            machine: Machine {
                core,
                backoff,
                state: State::Waiting {
                    until: Instant::now(),
                },
                queued: VecDeque::new(),
                last_logged: None,
            },
            stop,
            stopped,
            epochs,
            epoch,
        }
    }

    pub(crate) fn stopper(&self) -> LiveStop {
        self.stop.clone()
    }

    pub(crate) async fn recv(&mut self) -> Result<LiveUpdate, LiveEnd> {
        loop {
            if let Some(update) = self.machine.queued.pop_front() {
                return Ok(update);
            }
            if let State::Ended(end) = self.machine.state {
                self.stop.ended.send_replace(true);
                return Err(end);
            }
            // Both are marked seen at every turn, so their `changed` below
            // waits for a new change; an end once decided stands.
            let stopped = *self.stopped.borrow_and_update();
            let session_ended = *self.epochs.borrow_and_update() != self.epoch;
            if !self.machine.ending() {
                if stopped {
                    self.machine.finish(LiveEnd::Stopped);
                    continue;
                }
                if session_ended {
                    info!("the session ended: the live events socket closes");
                    self.machine.finish(LiveEnd::NoSession);
                    continue;
                }
            }
            // Each branch is cancel-safe: `changed` loses no change, and the
            // machine's step keeps its progress in its state.
            let sender_gone = {
                let stopped = pin!(self.stopped.changed());
                let ended = pin!(self.epochs.changed());
                let step = pin!(self.machine.step());
                matches!(
                    select(select(stopped, ended), step).await,
                    Either::Left((Either::Left((Err(_), _)) | Either::Right((Err(_), _)), _))
                )
            };
            if sender_gone && !self.machine.ending() {
                // The stop's sender lives in `self.stop` and the epoch's in
                // the session, so only a session dropped meanwhile: it can't
                // refresh any more.
                self.machine.finish(LiveEnd::NoSession);
            }
        }
    }

    pub(crate) async fn close(mut self) -> LiveEnd {
        self.stop.stop();
        self.machine.finish(LiveEnd::Stopped);
        loop {
            if let State::Ended(end) = self.machine.state {
                return end;
            }
            // Only a close is left: its step ends within its deadline.
            self.machine.step().await;
        }
    }
}

impl<S, T: Transport> Drop for Live<S, T> {
    fn drop(&mut self) {
        self.stop.ended.send_replace(true);
    }
}

/// What the socket is doing.
enum State<X> {
    /// Waiting until `until` before the next connect.
    Waiting { until: Instant },
    /// A connect: the ticket's request, the upgrade, the 101's checks.
    Connecting(Pin<Box<dyn Future<Output = Result<WebSocketStream<X>, Failed>> + Send>>),
    /// A socket open, live once its `ready` came.
    Open(Box<Open<X>>),
    /// An open socket closing after an end already decided.
    Closing(Box<Closing<X>>),
    /// Over.
    Ended(LiveEnd),
}

impl<X> State<X> {
    /// The state's name, for `Debug`.
    const fn name(&self) -> &'static str {
        match self {
            Self::Waiting { .. } => "waiting",
            Self::Connecting(_) => "connecting",
            Self::Open(open) if open.ready.is_none() => "opening",
            Self::Open(_) => "live",
            Self::Closing(_) => "closing",
            Self::Ended(_) => "ended",
        }
    }
}

/// An open socket.
struct Open<X> {
    socket: WebSocketStream<X>,
    /// When its `ready` came.
    ready: Option<Instant>,
    /// When it's taken for gone: [`READY_TIMEOUT`] after the 101 until
    /// `ready`, then [`SILENCE_TIMEOUT`] after the last frame.
    deadline: Instant,
}

/// A socket closing: the end it closes for is decided and kept here, so a
/// `recv` dropped during the close loses neither it nor the close (R81,
/// R92).
struct Closing<X> {
    socket: WebSocketStream<X>,
    /// When the close is given up: [`CLOSE_TIMEOUT`] after the decision.
    until: Instant,
    /// How it closes.
    how: Close,
    /// What follows it.
    then: After,
}

/// What follows a socket's close.
enum After {
    /// The retry an end decided.
    Retry(Then),
    /// The socket's own end: stopped, or the session's.
    End(LiveEnd),
}

/// How a socket closes.
#[derive(Clone, Copy)]
enum Close {
    /// The server closed it: read on until it's gone, so tungstenite's
    /// answer goes out.
    Drain,
    /// The client closes it: its 1000 is queued once, then flushed.
    Send { queued: bool },
}

/// The retry an end decided, waiting for its socket's close.
struct Then {
    end: End,
    why: Disconnect,
    /// How long the socket had been live when it ended, if it was.
    live: Option<Duration>,
}

impl<X> Closing<X>
where
    X: AsyncRead + AsyncWrite + Unpin,
{
    /// Closes the socket. Cancel-safe: a call dropped midway goes on where
    /// it stopped at the next.
    async fn close(&mut self) {
        match &mut self.how {
            Close::Drain => while self.socket.next().await.is_some() {},
            Close::Send { queued } => {
                if !*queued {
                    if self
                        .socket
                        .feed(Message::Close(Some(normal())))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    *queued = true;
                }
                // tokio-tungstenite's `poll_close` only flushes a close
                // that's queued already, at every call.
                let _ = SinkExt::close(&mut self.socket).await;
            }
        }
    }
}

/// Why a connect didn't open a socket.
enum Failed {
    /// The socket ends.
    End(LiveEnd),
    /// The policy decides what follows `end`; `error` says it.
    Retry { end: End, error: Box<ClientError> },
}

/// The socket's states and what they owe; see [`Live`].
struct Machine<S, T: Transport> {
    core: Arc<Core<S, T>>,
    backoff: Backoff,
    state: State<T::Socket>,
    queued: VecDeque<LiveUpdate>,
    /// The last end logged, so a cause that repeats goes to `debug`.
    last_logged: Option<End>,
}

impl<S: SessionStore, T: Transport> Machine<S, T> {
    /// One step of the state: a wait ends, a connect ends, or a frame comes.
    /// Cancel-safe: the state changes only once a step's await is done.
    async fn step(&mut self) {
        match &mut self.state {
            State::Ended(_) => core::future::pending().await,
            State::Waiting { until } => {
                sleep_until(*until).await;
                self.state = State::Connecting(Box::pin(connect(Arc::clone(&self.core))));
            }
            State::Connecting(connecting) => match connecting.as_mut().await {
                Ok(socket) => {
                    self.state = State::Open(Box::new(Open {
                        socket,
                        ready: None,
                        deadline: Instant::now() + READY_TIMEOUT,
                    }));
                }
                Err(Failed::End(end)) => self.state = State::Ended(end),
                Err(Failed::Retry { end, error }) => {
                    self.retry(end, Disconnect::NotOpened(*error), None);
                }
            },
            State::Open(open) => {
                let read = timeout_at(open.deadline, open.socket.next()).await;
                self.frame(read);
            }
            State::Closing(closing) => {
                let _ = timeout_at(closing.until, closing.close()).await;
                let old = core::mem::replace(
                    &mut self.state,
                    State::Waiting {
                        until: Instant::now(),
                    },
                );
                if let State::Closing(closing) = old {
                    match closing.then {
                        After::Retry(Then { end, why, live }) => self.retry(end, why, live),
                        After::End(end) => self.state = State::Ended(end),
                    }
                }
            }
        }
    }

    /// What a read of the open socket brought: a frame, its end, or its
    /// deadline.
    fn frame(&mut self, read: Read) {
        let next = {
            let State::Open(open) = &mut self.state else {
                return;
            };
            judge(open, read)
        };
        match next {
            Next::Nothing => {}
            Next::Update(update) => {
                if matches!(update, LiveUpdate::Refetch(RefetchReason::Ready)) {
                    self.last_logged = None;
                }
                self.queued.push_back(update);
            }
            Next::Retry(end, why) => self.retry_open(end, why),
            Next::Close(end, why, how) => self.close_then(end, why, how),
        }
    }

    /// Closes the open socket as `how` says, then retries after `end`: both
    /// decided now, before the close's first await (R81).
    fn close_then(&mut self, end: End, why: Disconnect, how: Close) {
        let live = self.live_for();
        let old = core::mem::replace(
            &mut self.state,
            State::Waiting {
                until: Instant::now(),
            },
        );
        if let State::Open(open) = old {
            self.state = State::Closing(Box::new(Closing {
                socket: open.socket,
                until: Instant::now() + CLOSE_TIMEOUT,
                how,
                then: After::Retry(Then { end, why, live }),
            }));
        }
    }

    /// Retries after the open socket's `end`, live for as long as it was.
    fn retry_open(&mut self, end: End, why: Disconnect) {
        let live = self.live_for();
        self.retry(end, why, live);
    }

    /// How long the open socket has been live, if it is.
    fn live_for(&self) -> Option<Duration> {
        match &self.state {
            State::Open(open) => open.ready.map(|ready| ready.elapsed()),
            _ => None,
        }
    }

    /// Waits as the policy says after `end`, telling the consumer `why`, or
    /// ends when the policy gives up.
    fn retry(&mut self, end: End, why: Disconnect, live: Option<Duration>) {
        let (action, wait) = self.backoff.next(end, live);
        let Some(wait) = wait else {
            warn!(cause = ?end, "the live events socket's server certificate was refused: it stops");
            self.state = State::Ended(LiveEnd::Certificate);
            return;
        };
        self.log(end, action, wait);
        self.state = State::Waiting {
            until: Instant::now() + wait,
        };
        self.queued.push_back(LiveUpdate::Disconnected {
            why,
            retry_in: wait,
        });
    }

    /// One line for `end`, which `action` follows, at the level the policy
    /// gives it: its own the first time in a row, then `debug` (S54, R86).
    fn log(&mut self, end: End, action: Action, wait: Duration) {
        let wait_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
        let repeat = repeats(self.last_logged, end);
        self.last_logged = Some(end);
        match level(end, action, repeat) {
            Level::WARN => {
                warn!(cause = ?end, wait_ms, "the live events socket is down: it reconnects");
            }
            Level::INFO => {
                info!(cause = ?end, wait_ms, "the live events socket is down: it reconnects");
            }
            _ => {
                debug!(cause = ?end, wait_ms, "the live events socket is down again: it reconnects");
            }
        }
    }

    /// Whether the socket's own end is decided: ended, or closing for it.
    const fn ending(&self) -> bool {
        match &self.state {
            State::Ended(_) => true,
            State::Closing(closing) => matches!(closing.then, After::End(_)),
            State::Waiting { .. } | State::Connecting(_) | State::Open(_) => false,
        }
    }

    /// Decides the socket's end, `end`, unless one is decided already, which
    /// stands: an open socket closes first, as a step of its own (R92), so a
    /// `recv` dropped during the close loses neither the end nor the close;
    /// a closing one closes for the end instead of its retry.
    fn finish(&mut self, end: LiveEnd) {
        if self.ending() {
            return;
        }
        let old = core::mem::replace(&mut self.state, State::Ended(end));
        match old {
            State::Open(open) => {
                self.state = State::Closing(Box::new(Closing {
                    socket: open.socket,
                    until: Instant::now() + CLOSE_TIMEOUT,
                    how: SEND,
                    then: After::End(end),
                }));
            }
            State::Closing(mut closing) => {
                closing.then = After::End(end);
                self.state = State::Closing(closing);
            }
            State::Waiting { .. } | State::Connecting(_) | State::Ended(_) => {}
        }
    }
}

/// A read of the open socket: a frame, its end, or its deadline passed.
type Read = Result<Option<Result<Message, WsError>>, Elapsed>;

/// What a read of the open socket asks for.
enum Next {
    /// Nothing for the consumer: a ping, a pong, a kind this version doesn't
    /// know.
    Nothing,
    /// This, for the consumer.
    Update(LiveUpdate),
    /// The socket is gone: retry after `End`.
    Retry(End, Disconnect),
    /// Close the socket as `Close` says, then retry after `End`.
    Close(End, Disconnect, Close),
}

/// Our own close, queued at its first step.
const SEND: Close = Close::Send { queued: false };

/// What `read`, the open socket's last read, asks for: frames before
/// `ready` other than `ready` end the socket; after it, each frame moves the
/// silence deadline.
fn judge<X>(open: &mut Open<X>, read: Read) -> Next {
    let now = Instant::now();
    let message = match read {
        Err(_elapsed) if open.ready.is_some() => {
            return Next::Close(End::Silent, Disconnect::Silent, SEND);
        }
        Err(_elapsed) => return Next::Close(End::NoReady, Disconnect::Silent, SEND),
        Ok(Some(Err(WsError::Capacity(_)))) => {
            return Next::Close(End::TooLarge, Disconnect::Lost, SEND);
        }
        Ok(None | Some(Err(_))) => return Next::Retry(End::Lost, Disconnect::Lost),
        Ok(Some(Ok(message))) => message,
    };
    let live = open.ready.is_some();
    if live {
        open.deadline = now + SILENCE_TIMEOUT;
    }
    let received = match message {
        Message::Text(text) => read_frame(&text),
        Message::Binary(_) => Received::Unreadable,
        Message::Close(frame) => {
            let code = frame.map(|frame| u16::from(frame.code));
            // tungstenite queued the close's answer: read on, briefly, so it
            // goes out.
            return Next::Close(End::Closed(code), Disconnect::Closed { code }, Close::Drain);
        }
        // tungstenite answers each ping itself; a pong or a raw frame only
        // shows the server is there.
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => return Next::Nothing,
    };
    match received {
        Received::Ignored => Next::Nothing,
        Received::Unreadable if live => {
            Next::Update(LiveUpdate::Refetch(RefetchReason::Unreadable))
        }
        Received::Event(event) => match event.kind.known() {
            Some(EventMessageKind::Ready) => {
                open.ready.get_or_insert(now);
                open.deadline = now + SILENCE_TIMEOUT;
                debug!("the live events socket is live");
                Next::Update(LiveUpdate::Refetch(RefetchReason::Ready))
            }
            Some(EventMessageKind::Resync) if live => {
                Next::Update(LiveUpdate::Refetch(RefetchReason::Resync))
            }
            _ if live => Next::Update(LiveUpdate::Event(event)),
            _ => Next::Close(End::EarlyEvent, Disconnect::Lost, SEND),
        },
        Received::Unreadable => Next::Close(End::EarlyEvent, Disconnect::Lost, SEND),
    }
}

/// The client's own close: 1000, normal.
fn normal() -> CloseFrame {
    CloseFrame {
        code: CloseCode::Normal,
        reason: "".into(),
    }
}

/// One connect: a new ticket through the session's refresh-if-needed path,
/// the upgrade with the session's bearer, and the 101's checks.
async fn connect<S, T>(core: Arc<Core<S, T>>) -> Result<WebSocketStream<T::Socket>, Failed>
where
    S: SessionStore,
    T: Transport,
{
    let (ticket, bearer) = core
        .events_ticket()
        .await
        .map_err(|failure| match failure.error {
            ClientError::NoSession => Failed::End(LiveEnd::NoSession),
            error => Failed::Retry {
                // A refused answer's status and wait, whatever its body, as
                // the upgrade's below.
                end: End::Ticket(failure.refusal.map_or_else(
                    || refused(&error),
                    |refusal| Refused::Status {
                        status: refusal.status,
                        retry_after: refusal.retry_after,
                    },
                )),
                error: Box::new(error),
            },
        })?;
    let request = upgrade::request(core.server(), ticket.expose_secret(), bearer);
    let key = request.key.clone();
    let switched = core.upgrade(request).await.map_err(|error| Failed::Retry {
        end: End::Upgrade(refused(&error)),
        error: Box::new(error),
    })?;
    match switched {
        Upgrade::Refused(answer) => Err(Failed::Retry {
            end: End::Upgrade(Refused::Status {
                status: answer.status,
                retry_after: answer.retry_after,
            }),
            error: Box::new(upgrade_refusal(answer)),
        }),
        Upgrade::Switched { headers, socket } => {
            if upgrade::checked(&headers, &key).is_err() {
                return Err(Failed::Retry {
                    end: End::Handshake,
                    error: Box::new(ClientError::UnexpectedResponse {
                        status: 101,
                        reason: Unexpected::Handshake,
                    }),
                });
            }
            Ok(upgrade::framed(socket).await)
        }
    }
}

/// How `error`, a request's failure, reads to the policy.
fn refused(error: &ClientError) -> Refused {
    match error {
        ClientError::Api(failure) => Refused::Status {
            status: failure.status,
            retry_after: failure.retry_after,
        },
        ClientError::UnexpectedResponse { status, .. } if *status >= 400 => Refused::Status {
            status: *status,
            retry_after: None,
        },
        ClientError::UnexpectedResponse { .. } | ClientError::TooLarge => Refused::Malformed,
        ClientError::Unreachable(_) => Refused::Unreachable,
        ClientError::TimedOut(_) => Refused::TimedOut,
        ClientError::Certificate(_) => Refused::Certificate,
        ClientError::NoSession
        | ClientError::NoPendingStep
        | ClientError::AlreadyLoggedIn
        | ClientError::Store(_)
        | ClientError::Interrupted(_) => Refused::Local,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use tokio::join;
    use tokio::time::{sleep, timeout};
    use tokio_tungstenite::tungstenite::Message;

    use super::*;
    use crate::store::MemoryStore;
    use crate::transport::scripted::{Accept, PipeServer, Scripted};
    use crate::transport::{Incoming, TransportError};
    use crate::url::ServerUrl;

    const TICKET: &str = "POST /api/v1/events/ticket";
    const REFRESH: &str = "POST /api/v1/auth/refresh";
    const READY: &str = r#"{"type":"ready"}"#;
    const REMOVED: &str =
        r#"{"type":"bot_removed","bot_id":"01900000-0000-7000-8000-000000000001"}"#;

    type TestCore = Core<MemoryStore, Scripted>;
    type TestLive = Live<MemoryStore, Scripted>;

    /// How long a test may take, in paused time: far past every wait in
    /// these tests, the 60 s `Retry-After` cap included.
    const TEST_BOUND: Duration = Duration::from_secs(600);

    /// `test`, failing once [`TEST_BOUND`] has passed: an upgrade no test
    /// scripted never answers, and paused time doesn't move without a timer.
    async fn bounded(test: impl Future<Output = ()>) {
        timeout(TEST_BOUND, test)
            .await
            .expect("the test didn't end within its bound");
    }

    fn answer(status: u16, body: &Value) -> Incoming {
        Incoming {
            status,
            retry_after: None,
            body: serde_json::to_vec(body).unwrap(),
        }
    }

    fn ticket(n: u32) -> Incoming {
        answer(
            200,
            &json!({ "ticket": format!("afk_wt_ticket{n}"), "expires_in": 30 }),
        )
    }

    fn refusal(status: u16, code: &str, retry_after: Option<u64>) -> Incoming {
        Incoming {
            retry_after: retry_after.map(Duration::from_secs),
            ..answer(
                status,
                &json!({ "error": { "code": code, "message": "Refused.", "request_id": "r-1" } }),
            )
        }
    }

    /// A session holding the access token `afk_at_1` and the refresh token
    /// `afk_rt_1`.
    async fn core() -> Arc<TestCore> {
        let store = MemoryStore::new();
        store
            .save(&secrecy::SecretString::from("afk_rt_1"))
            .await
            .unwrap();
        let core = Arc::new(Core::new(
            ServerUrl::parse("https://example.com").unwrap(),
            Scripted::default(),
            store,
            Duration::from_secs(5),
        ));
        core.hold_access("afk_at_1");
        core
    }

    #[tokio::test(start_paused = true)]
    #[should_panic(expected = "the test didn't end within its bound")]
    async fn an_upgrade_no_test_scripted_fails_the_bound_instead_of_hanging() {
        let core = core().await;
        core.transport().on(TICKET, Ok(ticket(1)));
        let mut live = live(&core);

        bounded(async {
            let _ = live.recv().await;
        })
        .await;
    }

    fn live(core: &Arc<TestCore>) -> TestLive {
        Live::new(Arc::clone(core), Backoff::new(1))
    }

    /// The next connect gets ticket `n` and switches.
    fn connects(core: &TestCore, n: u32) -> PipeServer {
        core.transport().on(TICKET, Ok(ticket(n)));
        core.transport().switch()
    }

    /// The socket's first connect, live: `ready` read as a refetch.
    async fn live_socket(core: &Arc<TestCore>) -> (TestLive, PipeServer) {
        let mut server = connects(core, 1);
        let mut live = live(core);
        let ((), update) = join!(server.send_text(READY), live.recv());
        assert!(
            matches!(update, Ok(LiveUpdate::Refetch(RefetchReason::Ready))),
            "{update:?}"
        );
        (live, server)
    }

    fn within(wait: Duration, lower: u64, upper: u64) -> bool {
        Duration::from_secs(lower) <= wait && wait <= Duration::from_secs(upper)
    }

    /// The `Disconnected` that `update` is, with its wait.
    fn disconnected(update: Result<LiveUpdate, LiveEnd>) -> (Disconnect, Duration) {
        match update {
            Ok(LiveUpdate::Disconnected { why, retry_in }) => (why, retry_in),
            other => panic!("not a disconnect: {other:?}"),
        }
    }

    /// The tickets the upgrades spent, in order.
    fn tickets(core: &TestCore) -> Vec<String> {
        core.transport()
            .upgrades()
            .into_iter()
            .filter_map(|upgrade| upgrade.ticket)
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn a_connect_spends_a_new_ticket_with_the_sessions_bearer_and_ready_asks_for_a_refetch() {
        bounded(async {
            let core = core().await;

            let (_live, _server) = live_socket(&core).await;

            let upgrades = core.transport().upgrades();
            assert_eq!(upgrades.len(), 1);
            assert_eq!(upgrades[0].path, "/api/v1/events");
            assert_eq!(upgrades[0].ticket.as_deref(), Some("afk_wt_ticket1"));
            assert_eq!(upgrades[0].bearer, "afk_at_1");
            assert_eq!(
                core.transport().bearers(TICKET),
                [Some("afk_at_1".to_owned())]
            );
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn events_come_as_they_are_and_a_kind_only_a_newer_server_knows_is_skipped() {
        bounded(async {
            let core = core().await;
            let (mut live, mut server) = live_socket(&core).await;

            let sending = async {
                server.send_text(r#"{"type":"bot_teleported"}"#).await;
                server.send_text(REMOVED).await;
            };
            let ((), update) = join!(sending, live.recv());

            let Ok(LiveUpdate::Event(event)) = update else {
                panic!("not an event: {update:?}");
            };
            assert_eq!(event.kind.known(), Some(EventMessageKind::BotRemoved));
        })
        .await;
    }

    #[rstest::rstest]
    #[case::resync(r#"{"type":"resync"}"#, RefetchReason::Resync)]
    #[case::not_json("ready", RefetchReason::Unreadable)]
    #[case::a_known_kind_without_its_field(r#"{"type":"bot_removed"}"#, RefetchReason::Unreadable)]
    #[tokio::test(start_paused = true)]
    async fn a_resync_or_a_text_frame_that_doesnt_read_asks_for_a_refetch(
        #[case] frame: &str,
        #[case] reason: RefetchReason,
    ) {
        bounded(async {
            let core = core().await;
            let (mut live, mut server) = live_socket(&core).await;

            let ((), update) = join!(server.send_text(frame), live.recv());

            assert!(
                matches!(update, Ok(LiveUpdate::Refetch(got)) if got == reason),
                "{update:?}"
            );
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_binary_frame_asks_for_a_refetch() {
        bounded(async {
            let core = core().await;
            let (mut live, mut server) = live_socket(&core).await;

            let ((), update) = join!(server.send_binary(&[0]), live.recv());

            assert!(
                matches!(update, Ok(LiveUpdate::Refetch(RefetchReason::Unreadable))),
                "{update:?}"
            );
        })
        .await;
    }

    #[rstest::rstest]
    #[case::shutdown(Some(1001))]
    #[case::busy(Some(1013))]
    #[case::no_pong(Some(4000))]
    #[case::too_slow(Some(4002))]
    #[case::refused_frame(Some(1008))]
    #[case::unknown(Some(4999))]
    #[case::no_code(None)]
    #[tokio::test(start_paused = true)]
    async fn a_close_backs_off_and_the_next_connect_spends_a_new_ticket(#[case] code: Option<u16>) {
        bounded(async {
            let core = core().await;
            let (mut live, mut server) = live_socket(&core).await;
            let mut next = connects(&core, 2);

            let ((), update) = join!(server.close(code), live.recv());
            let (why, retry_in) = disconnected(update);
            let down = Instant::now();
            let ((), again) = join!(next.send_text(READY), live.recv());

            assert!(
                matches!(why, Disconnect::Closed { code: got } if got == code),
                "{why:?}"
            );
            assert!(within(retry_in, 1, 2), "{retry_in:?}");
            assert!(matches!(
                again,
                Ok(LiveUpdate::Refetch(RefetchReason::Ready))
            ));
            let upgrades = core.transport().upgrades();
            assert_eq!(
                upgrades[1].at - down,
                retry_in,
                "connected when the wait ended"
            );
            assert_eq!(tickets(&core), ["afk_wt_ticket1", "afk_wt_ticket2"]);
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn the_servers_close_is_answered_before_the_wait() {
        bounded(async {
            let core = core().await;
            let (mut live, mut server) = live_socket(&core).await;

            let closing = async {
                server.close(Some(1001)).await;
                server.next().await
            };
            let (answer, update) = join!(closing, live.recv());

            assert!(
                matches!(&answer, Some(Message::Close(Some(frame))) if u16::from(frame.code) == 1001),
                "{answer:?}"
            );
            assert!(matches!(
                disconnected(update).0,
                Disconnect::Closed { code: Some(1001) }
            ));
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_lost_connection_backs_off() {
        bounded(async {
            let core = core().await;
            let (mut live, server) = live_socket(&core).await;
            drop(server);

            let (why, retry_in) = disconnected(live.recv().await);

            assert!(matches!(why, Disconnect::Lost), "{why:?}");
            assert!(within(retry_in, 1, 2), "{retry_in:?}");
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_4001_reconnects_at_once_and_a_second_before_a_stable_socket_backs_off() {
        bounded(async {
            let core = core().await;
            let (mut live, mut server) = live_socket(&core).await;
            let mut second = connects(&core, 2);

            let ((), first) = join!(server.close(Some(4001)), live.recv());
            let ((), ready) = join!(second.send_text(READY), live.recv());
            let ((), storm) = join!(second.close(Some(4001)), live.recv());

            assert_eq!(disconnected(first).1, Duration::ZERO);
            assert!(matches!(
                ready,
                Ok(LiveUpdate::Refetch(RefetchReason::Ready))
            ));
            assert!(within(disconnected(storm).1, 1, 2));
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_4001_reconnects_with_the_token_its_tickets_refresh_brought() {
        bounded(async {
            let core = core().await;
            let (mut live, mut server) = live_socket(&core).await;
            core.transport()
                .on(TICKET, Ok(refusal(401, "unauthorized", None)));
            core.transport().on(
                REFRESH,
                Ok(answer(
                    200,
                    &json!({
                        "access_token": "afk_at_2",
                        "refresh_token": "afk_rt_2",
                        "access_expires_in": 900,
                        "refresh_expires_in": 604_800,
                    }),
                )),
            );
            let mut next = connects(&core, 2);

            let ((), closed) = join!(server.close(Some(4001)), live.recv());
            let ((), again) = join!(next.send_text(READY), live.recv());

            assert_eq!(disconnected(closed).1, Duration::ZERO);
            assert!(matches!(
                again,
                Ok(LiveUpdate::Refetch(RefetchReason::Ready))
            ));
            let upgrades = core.transport().upgrades();
            assert_eq!(upgrades[1].bearer, "afk_at_2");
            assert_eq!(upgrades[1].ticket.as_deref(), Some("afk_wt_ticket2"));
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn too_many_connections_waits_its_retry_after() {
        bounded(async {
            let core = core().await;
            core.transport().on(TICKET, Ok(ticket(1)));
            core.transport()
                .refuse_upgrade(refusal(429, "too_many_connections", Some(45)));
            let mut next = connects(&core, 2);
            let mut live = live(&core);

            let (why, retry_in) = disconnected(live.recv().await);
            let down = Instant::now();
            let ((), again) = join!(next.send_text(READY), live.recv());

            assert!(
                matches!(&why, Disconnect::NotOpened(ClientError::Api(failure)) if failure.status == 429),
                "{why:?}"
            );
            assert_eq!(retry_in, Duration::from_secs(45));
            assert!(matches!(
                again,
                Ok(LiveUpdate::Refetch(RefetchReason::Ready))
            ));
            assert_eq!(
                core.transport().upgrades()[1].at - down,
                Duration::from_secs(45)
            );
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_ticket_on_the_upgrade_backs_off_then_spends_a_new_one() {
        bounded(async {
            let core = core().await;
            core.transport().on(TICKET, Ok(ticket(1)));
            core.transport()
                .refuse_upgrade(refusal(404, "not_found", None));
            let mut next = connects(&core, 2);
            let mut live = live(&core);

            let (why, retry_in) = disconnected(live.recv().await);
            let ((), again) = join!(next.send_text(READY), live.recv());

            assert!(
                matches!(why, Disconnect::NotOpened(ClientError::Api(_))),
                "{why:?}"
            );
            assert!(within(retry_in, 1, 2), "{retry_in:?}");
            assert!(matches!(
                again,
                Ok(LiveUpdate::Refetch(RefetchReason::Ready))
            ));
            assert_eq!(tickets(&core), ["afk_wt_ticket1", "afk_wt_ticket2"]);
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_ticket_refused_with_a_retry_after_waits_it() {
        bounded(async {
            let core = core().await;
            core.transport()
                .on(TICKET, Ok(refusal(503, "unavailable", Some(10))));
            let mut next = connects(&core, 1);
            let mut live = live(&core);

            let (why, retry_in) = disconnected(live.recv().await);
            let ((), again) = join!(next.send_text(READY), live.recv());

            assert!(
                matches!(&why, Disconnect::NotOpened(ClientError::Api(failure)) if failure.status == 503),
                "{why:?}"
            );
            assert_eq!(retry_in, Duration::from_secs(10));
            assert!(matches!(
                again,
                Ok(LiveUpdate::Refetch(RefetchReason::Ready))
            ));
            assert_eq!(tickets(&core), ["afk_wt_ticket1"]);
        })
        .await;
    }

    #[rstest::rstest]
    #[case::rate_limited(429, 45)]
    #[case::busy(503, 20)]
    #[tokio::test(start_paused = true)]
    async fn a_ticket_refused_without_the_apis_envelope_still_waits_its_retry_after(
        #[case] status: u16,
        #[case] retry_after: u64,
    ) {
        bounded(async {
            let core = core().await;
            core.transport().on(
                TICKET,
                Ok(Incoming {
                    status,
                    retry_after: Some(Duration::from_secs(retry_after)),
                    body: b"a proxy's plain text".to_vec(),
                }),
            );
            let mut live = live(&core);

            let (why, retry_in) = disconnected(live.recv().await);

            assert!(matches!(why, Disconnect::NotOpened(_)), "{why:?}");
            assert_eq!(retry_in, Duration::from_secs(retry_after));
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn an_upgrade_without_an_answer_backs_off() {
        bounded(async {
            let core = core().await;
            core.transport().on(TICKET, Ok(ticket(1)));
            core.transport()
                .fail_upgrade(TransportError::Unreachable(None));
            let mut live = live(&core);

            let (why, retry_in) = disconnected(live.recv().await);

            assert!(
                matches!(why, Disconnect::NotOpened(ClientError::Unreachable(_))),
                "{why:?}"
            );
            assert!(within(retry_in, 1, 2), "{retry_in:?}");
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_frame_over_its_limit_is_closed_and_backs_off() {
        bounded(async {
            let core = core().await;
            let (mut live, mut server) = live_socket(&core).await;
            let too_large = u64::try_from(upgrade::MAX_EVENT_FRAME).unwrap() + 1;
            let mut header = vec![0x81, 127];
            header.extend_from_slice(&too_large.to_be_bytes());

            let sending = async {
                server.raw(&header).await;
                server.next().await
            };
            let (close, update) = join!(sending, live.recv());

            let (why, retry_in) = disconnected(update);
            assert!(matches!(why, Disconnect::Lost), "{why:?}");
            assert!(within(retry_in, 1, 2), "{retry_in:?}");
            assert!(matches!(close, Some(Message::Close(_))), "{close:?}");
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_frame_that_breaks_the_protocol_loses_the_socket() {
        bounded(async {
            let core = core().await;
            let (mut live, mut server) = live_socket(&core).await;

            // A text frame with a reserved bit set, which no extension here
            // allows.
            let ((), update) = join!(server.raw(&[0xC1, 0x00]), live.recv());

            let (why, retry_in) = disconnected(update);
            assert!(matches!(why, Disconnect::Lost), "{why:?}");
            assert!(within(retry_in, 1, 2), "{retry_in:?}");
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_live_socket_without_a_frame_for_30_s_is_taken_for_gone() {
        bounded(async {
            let core = core().await;
            let (mut live, _server) = live_socket(&core).await;
            let since = Instant::now();

            let (why, _) = disconnected(live.recv().await);

            assert!(matches!(why, Disconnect::Silent), "{why:?}");
            assert_eq!(since.elapsed(), SILENCE_TIMEOUT);
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn any_frame_moves_the_silence_deadline() {
        bounded(async {
            let core = core().await;
            let (mut live, mut server) = live_socket(&core).await;
            let since = Instant::now();

            let pinging = async {
                sleep(Duration::from_secs(29)).await;
                server.ping().await;
                let pong = server.next().await;
                let close = server.next().await;
                (pong, close, since.elapsed())
            };
            let ((pong, close, closed_after), update) = join!(pinging, live.recv());

            assert!(matches!(disconnected(update).0, Disconnect::Silent));
            assert!(matches!(pong, Some(Message::Pong(_))), "{pong:?}");
            assert!(matches!(close, Some(Message::Close(_))), "{close:?}");
            assert_eq!(closed_after, Duration::from_secs(29) + SILENCE_TIMEOUT);
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_socket_that_never_says_ready_is_taken_for_gone_after_20_s() {
        bounded(async {
            let core = core().await;
            let _server = connects(&core, 1);
            let mut live = live(&core);
            let since = Instant::now();

            let (why, retry_in) = disconnected(live.recv().await);

            assert!(matches!(why, Disconnect::Silent), "{why:?}");
            assert_eq!(since.elapsed(), READY_TIMEOUT);
            assert!(within(retry_in, 1, 2), "{retry_in:?}");
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn an_event_before_ready_waits_the_plateau() {
        bounded(async {
            let core = core().await;
            let mut server = connects(&core, 1);
            let mut live = live(&core);

            let ((), update) = join!(server.send_text(REMOVED), live.recv());

            let (why, retry_in) = disconnected(update);
            assert!(matches!(why, Disconnect::Lost), "{why:?}");
            assert!(within(retry_in, 15, 30), "{retry_in:?}");
        })
        .await;
    }

    #[rstest::rstest]
    #[case::wrong(Accept::Wrong("dGhlIHNhbXBsZSBub25jZQ==".to_owned()))]
    #[case::missing(Accept::Missing)]
    #[tokio::test(start_paused = true)]
    async fn a_101_with_a_wrong_or_missing_accept_drops_its_connection_and_waits_the_plateau(
        #[case] accept: Accept,
    ) {
        bounded(async {
            let core = core().await;
            core.transport().on(TICKET, Ok(ticket(1)));
            let mut server = core
                .transport()
                .switch_with(accept, reqwest::header::HeaderMap::new());
            let mut live = live(&core);

            let (why, retry_in) = disconnected(live.recv().await);

            assert!(server.next().await.is_none(), "the client end is gone");
            assert!(
                matches!(
                    why,
                    Disconnect::NotOpened(ClientError::UnexpectedResponse {
                        status: 101,
                        reason: Unexpected::Handshake
                    })
                ),
                "{why:?}"
            );
            assert!(within(retry_in, 15, 30), "{retry_in:?}");
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn the_sessions_end_ends_the_socket_at_once_with_its_close() {
        bounded(async {
            let core = core().await;
            let (mut live, mut server) = live_socket(&core).await;
            core.discard().await.unwrap();

            let (close, ended) = join!(server.next(), live.recv());

            assert!(matches!(ended, Err(LiveEnd::NoSession)), "{ended:?}");
            assert!(
                matches!(&close, Some(Message::Close(Some(frame))) if u16::from(frame.code) == 1000),
                "{close:?}"
            );
            assert!(matches!(live.recv().await, Err(LiveEnd::NoSession)));
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_session_whose_refresh_finds_no_token_ends_it_at_the_ticket() {
        bounded(async {
            let core = Arc::new(Core::new(
                ServerUrl::parse("https://example.com").unwrap(),
                Scripted::default(),
                MemoryStore::new(),
                Duration::from_secs(5),
            ));
            let mut live = live(&core);

            let ended = live.recv().await;

            assert!(matches!(ended, Err(LiveEnd::NoSession)), "{ended:?}");
            assert_eq!(core.transport().upgrades(), []);
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_certificate_ends_the_socket_with_no_wait() {
        bounded(async {
            let core = core().await;
            core.transport().on(
                TICKET,
                Err(TransportError::Certificate(Box::new(
                    std::io::Error::other("refused"),
                ))),
            );
            let mut live = live(&core);

            let ended = live.recv().await;

            assert!(matches!(ended, Err(LiveEnd::Certificate)), "{ended:?}");
            assert!(matches!(live.recv().await, Err(LiveEnd::Certificate)));
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_stop_before_the_first_recv_ends_it_without_a_connect() {
        bounded(async {
            let core = core().await;
            let mut live = live(&core);
            live.stopper().stop();

            let ended = live.recv().await;

            assert!(matches!(ended, Err(LiveEnd::Stopped)), "{ended:?}");
            assert_eq!(core.transport().routes(), Vec::<String>::new());
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_stop_during_a_wait_ends_it_at_once() {
        bounded(async {
            let core = core().await;
            let (mut live, server) = live_socket(&core).await;
            drop(server);
            let (_, retry_in) = disconnected(live.recv().await);
            let stop = live.stopper();
            let since = Instant::now();

            let stopping = async {
                sleep(retry_in / 2).await;
                stop.stop();
            };
            let ((), ended) = join!(stopping, live.recv());

            assert!(matches!(ended, Err(LiveEnd::Stopped)), "{ended:?}");
            assert_eq!(since.elapsed(), retry_in / 2);
            assert_eq!(core.transport().upgrades().len(), 1);
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_live_socket_sends_only_pongs_and_its_close() {
        bounded(async {
            let core = core().await;
            let (mut live, mut server) = live_socket(&core).await;
            let stop = live.stopper();

            let talking = async {
                server.ping().await;
                let pong = server.next().await;
                stop.stop();
                let close = server.next().await;
                (pong, close)
            };
            let ((pong, close), ended) = join!(talking, live.recv());

            assert!(matches!(pong, Some(Message::Pong(_))), "{pong:?}");
            assert!(
                matches!(&close, Some(Message::Close(Some(frame))) if u16::from(frame.code) == 1000),
                "{close:?}"
            );
            assert!(matches!(ended, Err(LiveEnd::Stopped)));
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_recv_dropped_during_a_wait_keeps_its_deadline() {
        bounded(async {
            let core = core().await;
            let (mut live, server) = live_socket(&core).await;
            drop(server);
            let (_, retry_in) = disconnected(live.recv().await);
            let down = Instant::now();
            let mut next = connects(&core, 2);

            let dropped = timeout(retry_in / 2, live.recv()).await;
            let ((), again) = join!(next.send_text(READY), live.recv());

            assert!(dropped.is_err(), "nothing came within half the wait");
            assert!(matches!(
                again,
                Ok(LiveUpdate::Refetch(RefetchReason::Ready))
            ));
            assert_eq!(core.transport().upgrades()[1].at - down, retry_in);
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_recv_dropped_while_the_servers_close_drains_keeps_its_prompt_reconnect() {
        bounded(async {
            let core = core().await;
            let (mut live, mut server) = live_socket(&core).await;
            // The client's pong to this ping waits unread in the pipe, so the
            // close's answer can't go out until the server reads.
            let closing = async {
                server.ping().await;
                server.close(Some(4001)).await;
            };
            let ((), dropped) = join!(closing, timeout(CLOSE_TIMEOUT / 2, live.recv()));

            let reading = async { (server.next().await, server.next().await) };
            let ((pong, answer), update) = join!(reading, live.recv());

            assert!(dropped.is_err(), "the drain was still waiting");
            assert!(matches!(pong, Some(Message::Pong(_))), "{pong:?}");
            assert!(
                matches!(&answer, Some(Message::Close(Some(frame))) if u16::from(frame.code) == 4001),
                "{answer:?}"
            );
            let (why, retry_in) = disconnected(update);
            assert!(
                matches!(why, Disconnect::Closed { code: Some(4001) }),
                "{why:?}"
            );
            assert_eq!(retry_in, Duration::ZERO, "the expected 4001's prompt reconnect");
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_recv_dropped_during_its_own_close_keeps_its_outcome_and_deadline() {
        bounded(async {
            let core = core().await;
            let mut server = connects(&core, 1);
            let mut live = live(&core);
            // An event before `ready` closes the socket; the client's pong to
            // the ping before it waits unread, so the close can't go out.
            let sending = async {
                server.ping().await;
                server.send_text(REMOVED).await;
            };
            let ((), dropped) = join!(sending, async {
                let decided = Instant::now();
                (timeout(CLOSE_TIMEOUT / 2, live.recv()).await, decided)
            });
            let (dropped, decided) = dropped;

            let update = live.recv().await;

            assert!(dropped.is_err(), "the close was still waiting");
            let (why, retry_in) = disconnected(update);
            assert!(matches!(why, Disconnect::Lost), "{why:?}");
            assert!(
                within(retry_in, 15, 30),
                "the early event's plateau: {retry_in:?}"
            );
            assert_eq!(
                decided.elapsed(),
                CLOSE_TIMEOUT,
                "the close was given up at its own deadline"
            );
        })
        .await;
    }

    /// The socket's pong to a ping waits unread in the pipe, so its close
    /// can't go out until the server reads; then the session ends.
    async fn session_ends_while_the_close_waits(
        core: &Arc<TestCore>,
        live: &mut TestLive,
        server: &mut PipeServer,
    ) {
        server.ping().await;
        let read_the_ping = timeout(Duration::from_millis(1), live.recv()).await;
        assert!(read_the_ping.is_err(), "a ping is no update");
        core.discard().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_recv_dropped_during_the_sessions_end_close_keeps_the_end_and_the_close() {
        bounded(async {
            let core = core().await;
            let (mut live, mut server) = live_socket(&core).await;
            session_ends_while_the_close_waits(&core, &mut live, &mut server).await;
            let dropped = timeout(CLOSE_TIMEOUT / 2, live.recv()).await;

            let reading = async { (server.next().await, server.next().await) };
            let ((pong, close), ended) = join!(reading, live.recv());

            assert!(dropped.is_err(), "the close was still waiting");
            assert!(matches!(pong, Some(Message::Pong(_))), "{pong:?}");
            assert!(
                matches!(&close, Some(Message::Close(Some(frame))) if u16::from(frame.code) == 1000),
                "{close:?}"
            );
            assert!(matches!(ended, Err(LiveEnd::NoSession)), "{ended:?}");
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_close_after_the_sessions_end_answers_it_and_sends_the_close() {
        bounded(async {
            let core = core().await;
            let (mut live, mut server) = live_socket(&core).await;
            session_ends_while_the_close_waits(&core, &mut live, &mut server).await;
            let dropped = timeout(CLOSE_TIMEOUT / 2, live.recv()).await;

            let reading = async { (server.next().await, server.next().await) };
            let ((_, close), ended) = join!(reading, live.close());

            assert!(dropped.is_err(), "the close was still waiting");
            assert!(
                matches!(&close, Some(Message::Close(Some(frame))) if u16::from(frame.code) == 1000),
                "{close:?}"
            );
            assert_eq!(ended, LiveEnd::NoSession);
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_close_answers_stopped_when_nothing_ended_it_before() {
        bounded(async {
            let core = core().await;
            let (live, mut server) = live_socket(&core).await;

            let (close, ended) = join!(server.next(), live.close());

            assert!(
                matches!(&close, Some(Message::Close(Some(frame))) if u16::from(frame.code) == 1000),
                "{close:?}"
            );
            assert_eq!(ended, LiveEnd::Stopped);
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_dropped_socket_has_ended_and_its_stopper_shows_no_secret() {
        bounded(async {
            let core = core().await;
            let (live, _server) = live_socket(&core).await;
            let stop = live.stopper();

            drop(live);

            timeout(Duration::from_secs(1), stop.ended()).await.unwrap();
            assert_eq!(
                format!("{stop:?}"),
                "LiveStop { stopped: false, ended: true }"
            );
        })
        .await;
    }
}
