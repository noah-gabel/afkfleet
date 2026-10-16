//! A `Connect` stream's session: its hello, its `Welcome`, and every
//! message after it until the stream ends; see the [module docs](super).
//!
//! It runs on the [`Sessions`](super::Sessions) tracker, with a child of
//! their stop token, in two parts:
//! 1. **The handshake**: the first message within the hello timeout, read
//!    by [`read_hello`] (the version first), the name byte for byte, the
//!    agent's slot claimed in the registry (`Reject(AlreadyConnected)` while
//!    another stream holds it), then the app's hello transaction. Every
//!    failure here is the stream's last answer; nothing of the stream is
//!    recorded but what the app records of a refusal.
//! 2. **The welcomed stream**: the claim is activated (it fails if the
//!    agent's permission ended meanwhile), the connection's age lifted and
//!    `Welcome` queued. From then on every message that converts resets
//!    the heartbeat deadline and passes the registry's `admit`; a message
//!    that doesn't convert is dropped. The stream ends at the deadline, at
//!    the certificate's expiry, when the registry ends it (a permission
//!    that ended, a full queue), at the shutdown, at a second hello, or when
//!    the agent ends it. A stream that ends by itself (the agent's close,
//!    or a failed transport) within one heartbeat interval of its deadline
//!    ended of silence: HTTP/2 keepalive closes a silent connection then,
//!    and tonic may read that as a clean end. At its end the stream's switch
//!    is thrown, so nothing queued goes out any more, the slot is freed, so
//!    the agent can connect again at once, and the last answer is sent;
//!    only then is the end recorded and the lift dropped.

use core::time::Duration;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::agent::certificate_time_left;
use fleet_core::system::Clock;
use fleet_proto::control::{
    AgentMessage, Hello, RejectReason, ServerMessage, SessionDenied, Welcome, read_hello,
};
use fleet_proto::generated as wire;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep, timeout};
use tokio_util::sync::CancellationToken;
use tonic::{Code, Status, Streaming};
use tracing::{debug, error, info};

use super::outbound::Last;
use super::{
    ControlTimings, FAILED, FIRST_MESSAGE_UNREADABLE, HEARTBEAT_MISSED, HELLO_MISSING,
    HELLO_TOO_SLOW, MESSAGE_UNREADABLE, ONE_HELLO, SHUTTING_DOWN, TOO_LARGE, TOO_SLOW, refused,
};
use crate::grpc::listener::Connection;
use crate::grpc::registry::{AgentRegistry, Halt, Lease, Verdict};
use crate::grpc::status::refusal;
use crate::http::error::chain;
use crate::ports::agent_connections::{AdmitError, Admitted, AgentConnections, Ended, StreamEnd};

/// What a session owns; see the [module docs](self).
pub(super) struct Session {
    /// Who the certificate admitted.
    pub(super) admitted: Admitted,
    /// What the agent sends.
    pub(super) inbound: Streaming<wire::AgentMessage>,
    /// The stream's queue to the agent.
    pub(super) queue: mpsc::Sender<wire::ServerMessage>,
    /// The stream's switch, which stops its answers but the last.
    pub(super) halt: Halt,
    /// The connection, whose age a welcomed stream lifts.
    pub(super) connection: Connection,
    /// The app's side of the stream.
    pub(super) connections: Arc<dyn AgentConnections>,
    /// The server's clock.
    pub(super) clock: Arc<dyn Clock>,
    /// The hello timeout and the heartbeats.
    pub(super) timings: ControlTimings,
    /// Cancelled at the shutdown.
    pub(super) stop: CancellationToken,
    /// Each agent's one stream.
    pub(super) registry: AgentRegistry,
}

/// How a welcomed stream ended: why, when the agent was last heard from,
/// and its last answer (none: the stream just ends, OK).
struct End {
    reason: StreamEnd,
    last_seen: DateTime<Utc>,
    answer: Option<Last>,
}

impl End {
    const fn new(reason: StreamEnd, last_seen: DateTime<Utc>, answer: Option<Last>) -> Self {
        Self {
            reason,
            last_seen,
            answer,
        }
    }
}

impl Session {
    /// Runs the session to its end, and sends its last answer on `last`.
    pub(super) async fn run(mut self, last: oneshot::Sender<Last>) {
        match self.handshake().await {
            Ok((lease, connected_at)) => self.welcomed(lease, connected_at, last).await,
            Err(answer) => {
                // An agent that's gone dropped the receiver; nothing to tell.
                let _ = last.send(*answer);
            }
        }
    }

    /// The stream's first message up to the app's acceptance of its hello;
    /// when that was, or the stream's last answer (boxed: a `Status` is
    /// large).
    async fn handshake(&mut self) -> Result<(Lease, DateTime<Utc>), Box<Last>> {
        let first = tokio::select! {
            () = self.stop.cancelled() => {
                return Err(Box::new(Err(self.status(Code::Unavailable, SHUTTING_DOWN))));
            }
            // `Streaming::message` is cancel-safe: it polls the stream,
            // which keeps whatever it read.
            first = timeout(self.timings.hello_timeout, self.inbound.message()) => first,
        };
        let message = match first {
            Err(_elapsed) => {
                return Err(self.before_welcome(
                    Code::DeadlineExceeded,
                    HELLO_TOO_SLOW,
                    "no_hello",
                ));
            }
            Ok(Ok(None)) => {
                return Err(self.before_welcome(Code::Cancelled, HELLO_MISSING, "closed"));
            }
            Ok(Err(_)) => {
                return Err(self.before_welcome(
                    Code::InvalidArgument,
                    FIRST_MESSAGE_UNREADABLE,
                    "unreadable",
                ));
            }
            Ok(Ok(Some(message))) => message,
        };
        let hello = match read_hello(message) {
            Ok(hello) => hello,
            Err(refusal) => {
                let reason = refusal.reason();
                info!(
                    request_id = %self.admitted.request,
                    agent_id = %self.admitted.agent,
                    agent = %self.admitted.name,
                    refusal = ?refusal,
                    "an agent's hello was rejected"
                );
                return Err(Box::new(self.reject(reason)));
            }
        };
        self.accept(&hello).await
    }

    /// Checks `hello`'s name, claims the agent's slot, then has the app
    /// accept the hello and record it.
    async fn accept(&mut self, hello: &Hello) -> Result<(Lease, DateTime<Utc>), Box<Last>> {
        // Byte for byte: any other name, in any case, is another agent's
        // (S24). The app records the refusal and logs it.
        if hello.name.as_str() != self.admitted.name.as_str() {
            self.connections.name_refused(&self.admitted).await;
            return Err(Box::new(self.reject(RejectReason::NameMismatch)));
        }
        // Not audited: a restart finds its old stream still open, and would
        // use up the daily cap (R12). A real second holder shows as a
        // conflict on the live stream (S26).
        let Ok(lease) = self.registry.claim(&self.admitted) else {
            info!(
                request_id = %self.admitted.request,
                agent_id = %self.admitted.agent,
                agent = %self.admitted.name,
                "an agent's second connection was refused: its stream lives"
            );
            return Err(Box::new(self.reject(RejectReason::AlreadyConnected)));
        };
        let connected_at = self.clock.now();
        // The store's calls are bounded by the pools' acquire timeout and
        // SQLite's busy timeout.
        if let Err(failure) = self
            .connections
            .hello(&self.admitted, hello.capacity, &hello.agent_version)
            .await
        {
            if matches!(failure, AdmitError::Internal(_)) {
                error!(
                    request_id = %self.admitted.request,
                    agent_id = %self.admitted.agent,
                    error = %chain(&failure),
                    "an agent's hello couldn't be recorded"
                );
            }
            return Err(Box::new(Err(refused(&failure, self.admitted.request))));
        }
        info!(
            request_id = %self.admitted.request,
            agent_id = %self.admitted.agent,
            agent = %self.admitted.name,
            version = %hello.agent_version,
            capacity = hello.capacity.get(),
            "an agent connected"
        );
        Ok((lease, connected_at))
    }

    /// The welcomed stream of `lease` until it ends, and its last answer on
    /// `last`.
    async fn welcomed(
        &mut self,
        lease: Lease,
        connected_at: DateTime<Utc>,
        last: oneshot::Sender<Last>,
    ) {
        // Lifted before `Welcome` leaves, so the age can't cut the
        // connection between the two.
        let lift = self.connection.lift();
        let end = match self.registry.activate(
            &lease,
            self.queue.clone(),
            self.halt.clone(),
            connected_at,
        ) {
            // Its permission ended during the hello's transaction (R13).
            Err(reason) => End::new(reason, connected_at, Some(self.ended_by(reason))),
            Ok(ended) => match self.welcome() {
                Ok(()) => self.serve(&lease, connected_at, ended).await,
                Err(answer) => End::new(StreamEnd::StreamError, connected_at, Some(*answer)),
            },
        };
        // Nothing queued leaves after this, and the agent's slot is free
        // before it hears of the end; the answer doesn't wait for the
        // database.
        self.halt.halt();
        drop(lease);
        match end.answer {
            // An agent that's gone dropped the receiver; nothing to tell.
            Some(answer) => {
                let _ = last.send(answer);
            }
            None => drop(last),
        }
        self.connections
            .ended(
                &self.admitted,
                Ended {
                    reason: end.reason,
                    connected_at,
                    last_seen: end.last_seen,
                },
            )
            .await;
        info!(
            request_id = %self.admitted.request,
            agent_id = %self.admitted.agent,
            agent = %self.admitted.name,
            reason = end.reason.as_str(),
            "an agent's stream ended"
        );
        drop(lift);
    }

    /// Queues `Welcome`, the stream's first answer; or the stream's last
    /// answer when it can't.
    fn welcome(&self) -> Result<(), Box<Last>> {
        let welcome = ServerMessage::Welcome(Welcome {
            heartbeat_interval: self.timings.heartbeats.interval(),
            agent_id: self.admitted.agent,
        });
        let queued = wire::ServerMessage::try_from(welcome)
            .map_err(|failure| {
                error!(
                    request_id = %self.admitted.request,
                    error = %chain(&failure),
                    "an agent's Welcome couldn't be written"
                );
            })
            .and_then(|message| {
                // The queue is new and empty; only a stream already gone
                // refuses it.
                self.queue.try_send(message).map_err(|_| ())
            });
        queued.map_err(|()| Box::new(Err(self.status(Code::Internal, FAILED))))
    }

    /// Reads `lease`'s welcomed stream until something ends it; `ended`
    /// gets the registry's reason when it ends it.
    async fn serve(
        &mut self,
        lease: &Lease,
        connected_at: DateTime<Utc>,
        mut ended: oneshot::Receiver<StreamEnd>,
    ) -> End {
        let mut last_seen = connected_at;
        let left = certificate_time_left(self.admitted.not_after, self.clock.now());
        let expiry = sleep(left);
        let silence = sleep(self.timings.heartbeats.timeout());
        tokio::pin!(expiry, silence);
        loop {
            // Every branch is cancel-safe: the token, the receiver and the
            // timers stay as they are, and `Streaming::message` keeps what it
            // read.
            let message = tokio::select! {
                () = self.stop.cancelled() => {
                    return End::new(StreamEnd::ServerShutdown, last_seen, Some(Err(self.status(Code::Unavailable, SHUTTING_DOWN))));
                }
                reason = &mut ended => {
                    // Its sender lives in the registry's slot, which this
                    // session's lease keeps: it's never dropped unsent.
                    let reason = reason.unwrap_or(StreamEnd::StreamError);
                    return End::new(reason, last_seen, Some(self.ended_by(reason)));
                }
                () = &mut expiry => {
                    return End::new(StreamEnd::CertificateExpired, last_seen, Some(self.ended_by(StreamEnd::CertificateExpired)));
                }
                () = &mut silence => {
                    return End::new(StreamEnd::HeartbeatTimeout, last_seen, Some(Err(self.status(Code::DeadlineExceeded, HEARTBEAT_MISSED))));
                }
                message = self.inbound.message() => message,
            };
            let message = match message {
                Ok(Some(message)) => message,
                // A reset arrives as an end too: tonic reads a request's
                // CANCEL as one.
                Ok(None) => {
                    return self.ended_itself(StreamEnd::Closed, silence.deadline(), last_seen);
                }
                Err(failure) => {
                    let (reason, answer) = read_failure(failure.code());
                    if reason == StreamEnd::StreamError {
                        return self.ended_itself(reason, silence.deadline(), last_seen);
                    }
                    let answer = answer.map(|(code, message)| Err(self.status(code, message)));
                    return End::new(reason, last_seen, answer);
                }
            };
            if matches!(message.msg, Some(wire::agent_message::Msg::Hello(_))) {
                return End::new(
                    StreamEnd::ProtocolError,
                    last_seen,
                    Some(Err(self.status(Code::FailedPrecondition, ONE_HELLO))),
                );
            }
            match AgentMessage::try_from(message) {
                Ok(message) => {
                    last_seen = self.clock.now();
                    silence
                        .as_mut()
                        .reset(Instant::now() + self.timings.heartbeats.timeout());
                    self.admit(lease, &message).await;
                }
                Err(problem) => debug!(
                    request_id = %self.admitted.request,
                    agent_id = %self.admitted.agent,
                    problem = %problem,
                    "an agent's message that doesn't convert was dropped"
                ),
            }
        }
    }

    /// The end of a stream that ended by itself as `end`, whose heartbeat
    /// deadline is `deadline`; see [`own_end`].
    fn ended_itself(&self, end: StreamEnd, deadline: Instant, last_seen: DateTime<Utc>) -> End {
        let left = deadline.saturating_duration_since(Instant::now());
        match own_end(end, left, self.timings.heartbeats.interval()) {
            StreamEnd::HeartbeatTimeout => End::new(
                StreamEnd::HeartbeatTimeout,
                last_seen,
                Some(Err(self.status(Code::DeadlineExceeded, HEARTBEAT_MISSED))),
            ),
            other => End::new(other, last_seen, None),
        }
    }

    /// Passes `message` through the registry: records a conflict it proves,
    /// and answers a session request.
    async fn admit(&self, lease: &Lease, message: &AgentMessage) {
        let (verdict, conflict) = self.registry.admit(lease, message);
        if let Some(conflict) = conflict {
            self.connections.conflict(&self.admitted, conflict).await;
        }
        match verdict {
            Verdict::Pass => {}
            Verdict::Dropped => debug!(
                request_id = %self.admitted.request,
                agent_id = %self.admitted.agent,
                "an agent's report about a bot it doesn't hold was dropped"
            ),
            Verdict::DenySession(request) => {
                let denied = ServerMessage::SessionDenied(SessionDenied {
                    id: request.id,
                    bot_id: request.bot_id,
                    // `false` would fail the bot for good (S28).
                    retryable: true,
                });
                // A full queue ends the stream through the registry.
                if let Err(failure) = self.registry.send(self.admitted.agent, denied) {
                    debug!(
                        request_id = %self.admitted.request,
                        agent_id = %self.admitted.agent,
                        error = %failure,
                        "a session request's denial wasn't queued"
                    );
                }
            }
        }
    }

    /// The last answer of a stream the registry, or its certificate's
    /// expiry, ended for `reason`.
    fn ended_by(&self, reason: StreamEnd) -> Last {
        match reason {
            StreamEnd::Disabled => self.reject(RejectReason::AgentDisabled),
            StreamEnd::CertificateRevoked => self.reject(RejectReason::CertificateRevoked),
            StreamEnd::CertificateExpired => self.reject(RejectReason::CertificateExpired),
            StreamEnd::TooSlow => Err(self.status(Code::ResourceExhausted, TOO_SLOW)),
            StreamEnd::Closed
            | StreamEnd::HeartbeatTimeout
            | StreamEnd::StreamError
            | StreamEnd::ProtocolError
            | StreamEnd::ServerShutdown => Err(self.status(Code::Internal, FAILED)),
        }
    }

    /// The last answer of a stream that ended before its `Welcome`, logged
    /// with `what` ended it.
    fn before_welcome(&self, code: Code, message: &'static str, what: &'static str) -> Box<Last> {
        info!(
            request_id = %self.admitted.request,
            agent_id = %self.admitted.agent,
            agent = %self.admitted.name,
            reason = what,
            "an agent's stream ended before its hello"
        );
        Box::new(Err(self.status(code, message)))
    }

    /// A `Reject` for `reason`, then the stream ends OK.
    fn reject(&self, reason: RejectReason) -> Last {
        wire::ServerMessage::try_from(ServerMessage::Reject(reason))
            .map_err(|_| self.status(Code::Internal, FAILED))
    }

    /// A status with the call's request ID.
    fn status(&self, code: Code, message: &'static str) -> Status {
        refusal(code, message, self.admitted.request, None)
    }
}

/// What a welcomed stream that ended by itself, as `end` (`Closed` or
/// `StreamError`), ended of, `left` before its heartbeat deadline, with
/// heartbeats every `interval`.
///
/// Within one interval of the deadline it ended of silence. HTTP/2
/// keepalive closes a connection only after the heartbeat timeout without
/// anything read, which may be a little before the deadline, counted from
/// when the session last handled a message; tonic may read that close as
/// the agent's own. An agent that beats closes at least `timeout -
/// interval`, one interval or more, before its deadline.
fn own_end(end: StreamEnd, left: Duration, interval: Duration) -> StreamEnd {
    match end {
        StreamEnd::Closed | StreamEnd::StreamError if left <= interval => {
            StreamEnd::HeartbeatTimeout
        }
        other => other,
    }
}

/// Why a welcomed stream ended when reading it failed with `code`, and
/// the code and message of its answer, if anyone is left to answer.
const fn read_failure(code: Code) -> (StreamEnd, Option<(Code, &'static str)>) {
    match code {
        // The size limit, or a hello with too many bots.
        Code::OutOfRange => (
            StreamEnd::ProtocolError,
            Some((Code::OutOfRange, TOO_LARGE)),
        ),
        // The codec's answer to a message it can't decode.
        Code::InvalidArgument => (
            StreamEnd::ProtocolError,
            Some((Code::InvalidArgument, MESSAGE_UNREADABLE)),
        ),
        // The transport failed: no one is left to answer.
        _ => (StreamEnd::StreamError, None),
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    #[rstest]
    #[case::closed_at_the_deadline(StreamEnd::Closed, Duration::ZERO, StreamEnd::HeartbeatTimeout)]
    #[case::closed_within_an_interval_of_it(StreamEnd::Closed, SECOND, StreamEnd::HeartbeatTimeout)]
    #[case::closed_earlier(StreamEnd::Closed, SECOND + Duration::from_millis(1), StreamEnd::Closed)]
    #[case::failed_at_the_deadline(
        StreamEnd::StreamError,
        Duration::ZERO,
        StreamEnd::HeartbeatTimeout
    )]
    #[case::failed_earlier(StreamEnd::StreamError, 3 * SECOND, StreamEnd::StreamError)]
    #[case::a_protocol_error_stays(
        StreamEnd::ProtocolError,
        Duration::ZERO,
        StreamEnd::ProtocolError
    )]
    fn a_stream_that_ends_by_itself_near_its_deadline_ended_of_silence(
        #[case] end: StreamEnd,
        #[case] left: Duration,
        #[case] expected: StreamEnd,
    ) {
        assert_eq!(own_end(end, left, SECOND), expected);
    }

    #[rstest]
    #[case::over_the_limit(Code::OutOfRange, StreamEnd::ProtocolError, Some((Code::OutOfRange, TOO_LARGE)))]
    #[case::undecodable(Code::InvalidArgument, StreamEnd::ProtocolError, Some((Code::InvalidArgument, MESSAGE_UNREADABLE)))]
    #[case::transport(Code::Unknown, StreamEnd::StreamError, None)]
    #[case::h2_internal(Code::Internal, StreamEnd::StreamError, None)]
    #[case::refused_stream(Code::Unavailable, StreamEnd::StreamError, None)]
    fn a_failed_read_ends_the_stream_with_its_reason_and_answer(
        #[case] code: Code,
        #[case] reason: StreamEnd,
        #[case] answer: Option<(Code, &'static str)>,
    ) {
        assert_eq!(read_failure(code), (reason, answer));
    }
}
