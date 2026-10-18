//! The link (Plan.md P10.7; ADR-0019, S67, R48, R51–R53): the control
//! stream's attempts, their ends, and the waits between them.
//!
//! - **The endpoint** is built once from the identity: TLS trusting only
//!   the CA, the URL's host as the name, 5 s to connect and 5 s for the
//!   handshake, HTTP/2 keepalive every `KEEPALIVE_INTERVAL` with a
//!   `KEEPALIVE_TIMEOUT`, while idle too. Never `Endpoint::timeout`, a
//!   `grpc-timeout` or TCP keepalive: the stream lives as long as it may.
//! - **An attempt** checks the kept certificate on the run's clock first
//!   (R52), dials a fresh connection, runs the conductor's barrier (at most
//!   [`BARRIER_BOUND`]), opens the outbox with the hello and calls
//!   `Connect`, decoding up to `MAX_SERVER_MESSAGE_BYTES` and encoding up to
//!   `MAX_AGENT_MESSAGE_BYTES`. `WELCOME_TIMEOUT` bounds it all until
//!   `Welcome` or `Reject`, dialing included, which keeps S45's grace exact.
//! - **The reader** hands session answers to the credentials and the six
//!   instructions to the conductor's FIFO; a full FIFO, a second `Welcome`
//!   or a message it can't read ends the stream.
//! - **The ends** ([`ends`]): a stop forgets every kept grant before the
//!   stream's end is noted, so nothing hands one out after the server's
//!   refusal (S63), and the run stops every bot and exits 5; the rest wait
//!   [`backoff`]'s time. Every attempt that opened ends the outbox's stream
//!   and the credentials' generation (R51). A cause repeated logs at
//!   `debug` after its first line.
//! - **Logs** name the agent's ID, the interval, the attempts and the
//!   cause; never the URL, the host, a peer address, a status's message or
//!   a transport error's text.

pub mod backoff;
pub mod ends;

use core::time::Duration;
use std::sync::Arc;

use fleet_core::value::{AgentCapacity, AgentName, AgentVersion};
use fleet_proto::PROTOCOL_VERSION;
use fleet_proto::control::{
    Hello, KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, MAX_AGENT_MESSAGE_BYTES,
    MAX_SERVER_MESSAGE_BYTES, ServerMessage, WELCOME_TIMEOUT,
};
use fleet_proto::generated::agent_control_client::AgentControlClient;
use fleet_runtime::RuntimeClock;
use rustls::pki_types::UnixTime;
use tokio::time::{Instant, timeout_at};
use tokio_stream::StreamExt as _;
use tokio_util::sync::CancellationToken;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tracing::{debug, error, info};

use self::backoff::Backoff;
use self::ends::{Cause, End, Next, Refusal};
use crate::enroll::certificate::CertificateProblem;
use crate::enroll::client::CONNECT_TIMEOUT;
use crate::managed::conductor::{Inbox, InboxError, Instruction};
use crate::managed::credentials::{LinkGeneration, ManagedCredentials, SessionAnswer};
use crate::managed::identity::KeptCertificate;
use crate::managed::outbound::{Outbound, Outbox};

/// How long the hello's barrier may take, inside `WELCOME_TIMEOUT` (R48).
pub const BARRIER_BOUND: Duration = Duration::from_secs(5);

/// The link's timings: the protocol's in the agent ([`LinkTimings::default`]),
/// shorter in the tests that wait for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkTimings {
    /// How long an attempt may take until `Welcome` or `Reject`.
    pub welcome: Duration,
    /// How often HTTP/2 keepalive pings the connection.
    pub keepalive_interval: Duration,
    /// How long a ping may go unanswered.
    pub keepalive_timeout: Duration,
}

/// `WELCOME_TIMEOUT`, `KEEPALIVE_INTERVAL` and `KEEPALIVE_TIMEOUT`.
impl Default for LinkTimings {
    fn default() -> Self {
        Self {
            welcome: WELCOME_TIMEOUT,
            keepalive_interval: KEEPALIVE_INTERVAL,
            keepalive_timeout: KEEPALIVE_TIMEOUT,
        }
    }
}

/// The endpoint of the server's gRPC port at `url` (`https://host:port`),
/// over `tls`, with `timings`' keepalive; see the [module docs](self).
///
/// # Errors
/// tonic's, when the URL or the TLS config can't be used.
pub fn endpoint(
    url: &str,
    tls: ClientTlsConfig,
    timings: LinkTimings,
) -> Result<Endpoint, tonic::transport::Error> {
    Ok(Endpoint::from_shared(url.to_owned())?
        .tls_config(tls)?
        .connect_timeout(CONNECT_TIMEOUT)
        .http2_keep_alive_interval(timings.keepalive_interval)
        .keep_alive_timeout(timings.keepalive_timeout)
        .keep_alive_while_idle(true))
}

/// What the hello says about the agent.
#[derive(Debug, Clone)]
pub struct HelloParts {
    /// Its name.
    pub name: AgentName,
    /// Its version.
    pub agent_version: AgentVersion,
    /// `[runtime] max_bots`.
    pub capacity: AgentCapacity,
}

/// Everything a link is built from.
#[derive(Debug)]
pub struct LinkParts {
    /// The server's endpoint ([`endpoint`]).
    pub endpoint: Endpoint,
    /// The certificate, checked before each attempt (R52).
    pub kept: KeptCertificate,
    /// The hello's fields but its snapshot.
    pub hello: HelloParts,
    /// The outbox, opened with each hello.
    pub outbox: Outbox,
    /// The credentials the session answers go to.
    pub credentials: Arc<ManagedCredentials<Outbox>>,
    /// The conductor's FIFO.
    pub inbox: Inbox,
    /// The run's clock, for the certificate's check.
    pub clock: RuntimeClock,
    /// The waits between attempts.
    pub backoff: Backoff,
    /// The welcome timeout.
    pub timings: LinkTimings,
}

/// How the link ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkEnd {
    /// Its stop token was cancelled.
    Stopped,
    /// The server refused the agent for good; every kept grant is
    /// forgotten.
    Refused(Refusal),
}

/// The link; see the [module docs](self).
#[derive(Debug)]
pub struct Link {
    parts: LinkParts,
    /// The last end's cause, so a repeat logs at `debug`.
    last_cause: Option<Cause>,
    /// Attempts since the last `Welcome`.
    attempts: u32,
}

/// An attempt that ran to its end.
struct Attempted {
    end: End,
    /// How long it stayed welcomed, if it was.
    welcomed: Option<Duration>,
}

impl Link {
    /// A link of `parts`.
    #[must_use]
    pub const fn new(parts: LinkParts) -> Self {
        Self {
            parts,
            last_cause: None,
            attempts: 0,
        }
    }

    /// Runs attempts until `stop`, or until the server refuses the agent.
    pub async fn run(mut self, stop: CancellationToken) -> LinkEnd {
        loop {
            let Some(Attempted { end, welcomed }) = self.attempt(&stop).await else {
                return LinkEnd::Stopped;
            };
            if let Next::Stop(refusal) = end.next {
                self.log(&end, Duration::ZERO);
                return LinkEnd::Refused(refusal);
            }
            let wait = self
                .parts
                .backoff
                .after(end.next, welcomed, end.retry_after);
            self.log(&end, wait);
            tokio::select! {
                biased;
                // Both are cancel-safe: waiting loses nothing.
                () = stop.cancelled() => return LinkEnd::Stopped,
                () = tokio::time::sleep(wait) => {}
            }
        }
    }

    /// One attempt, or none when `stop` came first.
    async fn attempt(&mut self, stop: &CancellationToken) -> Option<Attempted> {
        self.attempts = self.attempts.saturating_add(1);
        if let Some(end) = self.certificate() {
            if matches!(end.next, Next::Stop(_)) {
                self.parts.credentials.forget_all();
            }
            return Some(Attempted {
                end,
                welcomed: None,
            });
        }
        let deadline = Instant::now() + self.parts.timings.welcome;
        let opened = tokio::select! {
            biased;
            // Dropping the dial or the barrier is cancel-safe: nothing is
            // open yet.
            () = stop.cancelled() => return None,
            opened = self.open(deadline) => opened,
        };
        let (generation, channel, outbound) = match opened {
            Ok(opened) => opened,
            Err(end) => {
                return Some(Attempted {
                    end,
                    welcomed: None,
                });
            }
        };
        let streamed = self
            .stream(generation, channel, outbound, deadline, stop)
            .await;
        let attempted = streamed.map(|(end, welcomed)| Attempted {
            end: if self.parts.outbox.overflowed() {
                End::own(Cause::Overflow)
            } else {
                end
            },
            welcomed,
        });
        // A refusal forgets every kept grant before the stream's end makes
        // the agent fall back to them (S63).
        if attempted
            .as_ref()
            .is_some_and(|attempted| matches!(attempted.end.next, Next::Stop(_)))
        {
            self.parts.credentials.forget_all();
        }
        self.parts.outbox.ended(generation);
        self.parts.credentials.link_ended(generation);
        attempted
    }

    /// The kept certificate on the run's clock (R52): expired is a stop,
    /// anything else wrong a backoff with an `error` line.
    fn certificate(&self) -> Option<End> {
        let now = self.parts.clock.now();
        let secs = u64::try_from(now.timestamp()).unwrap_or(0);
        let checked = self
            .parts
            .kept
            .validity(UnixTime::since_unix_epoch(Duration::from_secs(secs)));
        match checked {
            Ok(()) => None,
            Err(CertificateProblem::Expired { .. }) => Some(End {
                next: Next::Stop(Refusal::OwnCertificateExpired),
                cause: Cause::Expired,
                retry_after: None,
            }),
            Err(
                CertificateProblem::NotYetValid
                | CertificateProblem::NotOnePem
                | CertificateProblem::NotFromTheCa
                | CertificateProblem::NotOurKey,
            ) => Some(End::own(Cause::NotYetValid)),
        }
    }

    /// Dials, runs the barrier and opens the outbox with the hello, before
    /// `deadline`.
    async fn open(
        &mut self,
        deadline: Instant,
    ) -> Result<(LinkGeneration, Channel, Outbound), End> {
        let channel = match timeout_at(deadline, self.parts.endpoint.connect()).await {
            Err(_elapsed) => return Err(End::own(Cause::WelcomeTimeout)),
            // The error's text may name the address: never logged.
            Ok(Err(_unreachable)) => return Err(End::own(Cause::Unreachable)),
            Ok(Ok(channel)) => channel,
        };
        let bound = deadline.min(Instant::now() + BARRIER_BOUND);
        let running = match timeout_at(bound, self.parts.inbox.barrier()).await {
            Ok(Ok(running)) => running,
            Ok(Err(failure)) => {
                debug!(error = %failure, "the hello's snapshot failed");
                return Err(End::own(Cause::Snapshot));
            }
            Err(_elapsed) => return Err(End::own(Cause::Snapshot)),
        };
        let hello = Hello {
            name: self.parts.hello.name.clone(),
            agent_version: self.parts.hello.agent_version.clone(),
            protocol_version: PROTOCOL_VERSION,
            capacity: self.parts.hello.capacity,
            running,
        };
        let (generation, outbound) = self.parts.outbox.open(hello);
        Ok((generation, channel, outbound))
    }

    /// The stream of `generation`: `Connect`, the welcome, then the reader,
    /// until its end, or none when `stop` came first. With its end, how long
    /// it stayed welcomed.
    async fn stream(
        &mut self,
        generation: LinkGeneration,
        channel: Channel,
        outbound: Outbound,
        deadline: Instant,
        stop: &CancellationToken,
    ) -> Option<(End, Option<Duration>)> {
        let mut client = AgentControlClient::new(channel)
            .max_decoding_message_size(MAX_SERVER_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_AGENT_MESSAGE_BYTES);
        let welcome = async {
            let mut messages = match timeout_at(deadline, client.connect(outbound)).await {
                Err(_elapsed) => return Err(End::own(Cause::WelcomeTimeout)),
                Ok(Err(status)) => return Err(End::of_status(&status)),
                Ok(Ok(response)) => response.into_inner(),
            };
            let first = match timeout_at(deadline, messages.next()).await {
                Err(_elapsed) => return Err(End::own(Cause::WelcomeTimeout)),
                Ok(None) => return Err(End::own(Cause::Ended)),
                Ok(Some(Err(status))) => return Err(End::of_status(&status)),
                Ok(Some(Ok(first))) => first,
            };
            match ServerMessage::try_from(first) {
                Ok(ServerMessage::Welcome(welcome)) => Ok((messages, welcome)),
                Ok(ServerMessage::Reject(reason)) => Err(End::of_reject(reason)),
                Ok(_) => Err(End::own(Cause::UnexpectedFirst)),
                Err(_unreadable) => Err(End::own(Cause::Unreadable)),
            }
        };
        let welcomed = tokio::select! {
            biased;
            // Dropping the call is cancel-safe: the stream just ends.
            () = stop.cancelled() => return None,
            welcomed = welcome => welcomed,
        };
        let (mut messages, welcome) = match welcomed {
            Ok(welcomed) => welcomed,
            Err(end) => return Some((end, None)),
        };
        self.parts
            .outbox
            .welcomed(generation, welcome.heartbeat_interval);
        info!(
            agent_id = %welcome.agent_id,
            heartbeat_interval_secs = welcome.heartbeat_interval.as_secs(),
            attempts = self.attempts,
            "the agent is connected"
        );
        self.attempts = 0;
        self.last_cause = None;
        let since = Instant::now();
        let end = loop {
            let message = tokio::select! {
                biased;
                // `StreamExt::next` is cancel-safe. No read deadline: HTTP/2
                // keepalive ends a silent connection (S23 rejected a server
                // heartbeat, S67).
                () = stop.cancelled() => return None,
                message = messages.next() => message,
            };
            let message = match message {
                None => break End::own(Cause::Ended),
                Some(Err(status)) => break End::of_status(&status),
                Some(Ok(message)) => message,
            };
            if let Some(end) = self.route(generation, message) {
                break end;
            }
        };
        Some((end, Some(since.elapsed())))
    }

    /// One message of a welcomed stream; its end, if it ends the stream.
    fn route(
        &self,
        generation: LinkGeneration,
        message: fleet_proto::generated::ServerMessage,
    ) -> Option<End> {
        let message = match ServerMessage::try_from(message) {
            Ok(message) => message,
            Err(unreadable) => {
                debug!(error = %unreadable, "a message from the server couldn't be read");
                return Some(End::own(Cause::Unreadable));
            }
        };
        let instruction = match message {
            // The answers go straight to the credentials, never behind a
            // Fleet call (S65).
            ServerMessage::SessionGrant(grant) => {
                self.parts.credentials.answer(SessionAnswer::Grant(grant));
                return None;
            }
            ServerMessage::SessionDenied(denied) => {
                self.parts.credentials.answer(SessionAnswer::Denied(denied));
                return None;
            }
            ServerMessage::Reject(reason) => return Some(End::of_reject(reason)),
            ServerMessage::Welcome(_) => return Some(End::own(Cause::SecondWelcome)),
            ServerMessage::Reconcile(assignments) => Instruction::Reconcile(assignments),
            ServerMessage::Assign(assignment) => Instruction::Assign(assignment),
            ServerMessage::Unassign(bot) => Instruction::Unassign(bot),
            ServerMessage::Update(spec) => Instruction::Update(spec),
            ServerMessage::SendChat(chat) => Instruction::SendChat(chat),
            ServerMessage::Lifecycle(command) => Instruction::Lifecycle(command),
        };
        match self.parts.inbox.instruction(generation, instruction) {
            Ok(()) => None,
            Err(InboxError::Full | InboxError::Gone) => Some(End::own(Cause::QueueFull)),
        }
    }

    /// Logs `end`, once at its level per cause, then at `debug`; `wait` is
    /// the time to the next attempt.
    fn log(&mut self, end: &End, wait: Duration) {
        let repeated = self.last_cause == Some(end.cause);
        self.last_cause = Some(end.cause);
        let cause = end.cause.as_str();
        let wait_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
        if repeated {
            debug!(
                cause,
                wait_ms, "the control stream ended again; the agent reconnects"
            );
            return;
        }
        match end.next {
            Next::Stop(refusal) => error!(
                cause,
                refusal = refusal.as_str(),
                "the server refused this agent; every bot stops, then the agent exits"
            ),
            Next::Slow => error!(
                cause,
                wait_ms,
                "the server can't read this agent's hello; it tries again slowly: is the agent older or newer than the server?"
            ),
            Next::Backoff if end.level() == tracing::Level::ERROR => error!(
                cause,
                wait_ms,
                "the agent's certificate isn't valid yet by this host's clock; it tries again: fix the clock (NTP)"
            ),
            Next::Backoff => info!(
                cause,
                wait_ms, "the control stream ended; the agent reconnects"
            ),
        }
    }
}
