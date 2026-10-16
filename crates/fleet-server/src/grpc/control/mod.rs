//! `fleet.v1.AgentControl.Connect` on the gRPC port (Plan.md P10.4;
//! ADR-0019, S13, S22–S30): an enrolled agent's one stream, admitted only by
//! the client certificate it presents.
//!
//! Each call passes three steps:
//! 1. **The gate** ([`ControlGate`], tonic's interceptor), before anything
//!    the caller sent is read: no client certificate is
//!    `UNAUTHENTICATED` at once; then the call's request ID is minted, the
//!    certificate's fingerprint computed and its `[rate_limits]
//!    connect_per_cert_per_min` spent (`RESOURCE_EXHAUSTED`, with
//!    `retry-after`).
//! 2. **Admission** ([`ControlGrpc`]), before the stream opens: the
//!    [`AgentConnections`] port reads the fingerprint's row and its agent,
//!    and only then the certificate's SAN. Every identity problem is one
//!    `UNAUTHENTICATED`; a busy database is `UNAVAILABLE` with
//!    `retry-after: 1`.
//! 3. **The session** (`session.rs`), once the stream is open with its
//!    request ID in the headers: the hello within the hello timeout, read
//!    by `fleet_proto::control::read_hello` (version first), the name byte
//!    for byte, the hello's transaction, then `Welcome` with the agent's ID
//!    and the heartbeat interval. From then on any message that converts
//!    keeps the stream alive until the heartbeat timeout, and the stream
//!    ends at the latest when its certificate expires.
//!
//! The answers, one fixed message each; every status after the
//! certificate's check carries the call's request ID:
//!
//! | Case | When | Answer |
//! |---|---|---|
//! | no client certificate | gate | `UNAUTHENTICATED` [`CERTIFICATE_REQUIRED`] |
//! | rate-limited | gate | `RESOURCE_EXHAUSTED` [`RATE_LIMITED`], `retry-after` |
//! | unknown, revoked, expired, disabled, SAN mismatch | before the stream | `UNAUTHENTICATED` [`CERTIFICATE_REFUSED`] |
//! | database busy | before the stream, or the hello's write | `UNAVAILABLE` [`BUSY`], `retry-after: 1` |
//! | a failure on the server's side | anywhere | `INTERNAL` [`FAILED`] |
//! | no hello in time | open | `DEADLINE_EXCEEDED` [`HELLO_TOO_SLOW`] |
//! | first message unreadable (or too large), or the stream failed | open | `INVALID_ARGUMENT` [`FIRST_MESSAGE_UNREADABLE`] |
//! | stream ended before its hello | open | `CANCELLED` [`HELLO_MISSING`] |
//! | not a hello, or a hello that doesn't convert | open | `Reject(InvalidHello)`, then OK |
//! | another protocol version, 0 included | open | `Reject(IncompatibleProtocol)`, then OK |
//! | a name other than the agent's | open | `Reject(NameMismatch)`, then OK |
//! | revoked or disabled meanwhile | the hello's write | `UNAUTHENTICATED` [`CERTIFICATE_REFUSED`] |
//! | a second hello | after `Welcome` | `FAILED_PRECONDITION` [`ONE_HELLO`] |
//! | nothing within the heartbeat timeout | after `Welcome` | `DEADLINE_EXCEEDED` [`HEARTBEAT_MISSED`] |
//! | a message over the size limit | after `Welcome` | `OUT_OF_RANGE` [`TOO_LARGE`] |
//! | a message that can't be decoded | after `Welcome` | `INVALID_ARGUMENT` [`MESSAGE_UNREADABLE`] |
//! | the agent closed its side, or reset the stream | after `Welcome` | OK |
//! | the queue to the agent full | after `Welcome` | `RESOURCE_EXHAUSTED` [`TOO_SLOW`] |
//! | the certificate expired | after `Welcome` | `Reject(CertificateExpired)`, then OK |
//! | the server shuts down | open | `UNAVAILABLE` [`SHUTTING_DOWN`] |
//!
//! A message after `Welcome` that decodes but doesn't convert is dropped
//! and logged at `debug`; it's no sign of life.
//!
//! **Logs:** never the peer's address, the hello's name or the certificate's
//! bytes; an agent appears under its stored name and ID only. A call
//! without a certificate and a rate-limited call are `debug` lines (the
//! minute's summary counts the latter); the admission's refusals are the
//! service's lines; a session logs its `Welcome` and its end at `info`.

mod gate;
mod outbound;
mod session;

use core::time::Duration;
use std::sync::Arc;

use fleet_core::id::RequestId;
use fleet_core::system::Clock;
use fleet_proto::control::{HELLO_TIMEOUT, MAX_AGENT_MESSAGE_BYTES, MAX_SERVER_MESSAGE_BYTES};
use fleet_proto::generated as wire;
use fleet_proto::generated::agent_control_server::{AgentControl, AgentControlServer};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::server::TlsConnectInfo;
use tonic::{Code, Request, Response, Status, Streaming};
use tracing::error;

pub use super::status::{REQUEST_ID, RETRY_AFTER};
pub use gate::ControlGate;
pub use outbound::Outbound;

use self::gate::ControlCall;
use self::session::Session;
use super::listener::Connection;
use super::status::refusal;
use crate::config::Heartbeats;
use crate::http::error::{BUSY_RETRY_AFTER, chain, whole_seconds};
use crate::http::middleware::RequestIds;
use crate::http::rate_limit::RateLimits;
use crate::ports::agent_connections::{AdmitError, AgentConnections, Presented};

/// No client certificate.
pub const CERTIFICATE_REQUIRED: &str =
    "Connecting needs the agent's certificate: enroll the agent first.";
/// Every identity problem.
pub const CERTIFICATE_REFUSED: &str = "The agent's certificate isn't accepted: it's unknown, revoked or expired, or its agent is disabled; enroll the agent again.";
/// Too many calls with one certificate.
pub const RATE_LIMITED: &str = "Too many connections with this certificate; try again later.";
/// The database is busy.
pub const BUSY: &str = "The server is busy; try again in a moment.";
/// A failure on the server's side.
pub const FAILED: &str = "The connection failed on the server.";
/// The first message couldn't be read.
pub const FIRST_MESSAGE_UNREADABLE: &str =
    "The stream's first message couldn't be read; connect again.";
/// No hello in time.
pub const HELLO_TOO_SLOW: &str = "No hello arrived in time; connect again.";
/// The stream ended before its hello.
pub const HELLO_MISSING: &str = "The stream ended before its hello.";
/// A second hello.
pub const ONE_HELLO: &str = "A stream has one hello; connect again.";
/// Nothing within the heartbeat timeout.
pub const HEARTBEAT_MISSED: &str = "The agent sent nothing within the heartbeat timeout.";
/// A message over the size limit.
pub const TOO_LARGE: &str = "A message was larger than the limit; connect again.";
/// A message after `Welcome` that couldn't be decoded.
pub const MESSAGE_UNREADABLE: &str = "A message couldn't be read; connect again.";
/// The queue to the agent filled up.
pub const TOO_SLOW: &str = "The agent read too slowly; connect again.";
/// The server shuts down.
pub const SHUTTING_DOWN: &str = "The server is shutting down; connect again later.";

/// How many messages a stream's queue to its agent holds: a session answer
/// and a spec change for each of 1000 bots, a `Welcome` and a full
/// reconcile (ADR-0019, S25).
pub const QUEUE_DEPTH: usize = 2048;

/// How long a session waits for what: the heartbeats of `[grpc]`, and
/// [`HELLO_TIMEOUT`] for the first message. Tests lower them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlTimings {
    /// The agent's beat and the server's wait for a sign of life.
    pub heartbeats: Heartbeats,
    /// How long the first message may take, from the stream's opening.
    pub hello_timeout: Duration,
}

impl ControlTimings {
    /// `heartbeats` with the protocol's [`HELLO_TIMEOUT`].
    #[must_use]
    pub const fn new(heartbeats: Heartbeats) -> Self {
        Self {
            heartbeats,
            hello_timeout: HELLO_TIMEOUT,
        }
    }
}

/// The owner of every session's task: each gets a child of the stop token,
/// and [`shutdown`](Self::shutdown) ends them all and waits for them.
#[derive(Debug, Clone)]
pub struct Sessions {
    tasks: TaskTracker,
    stop: CancellationToken,
}

impl Sessions {
    /// The owner of sessions that end when `stop` is cancelled.
    #[must_use]
    pub fn new(stop: CancellationToken) -> Self {
        Self {
            tasks: TaskTracker::new(),
            stop,
        }
    }

    /// Ends every session and waits until each has recorded its end. The
    /// caller bounds the wait (serve's shutdown deadline).
    pub async fn shutdown(&self) {
        self.stop.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }
}

/// `Connect`'s handler; see the [module docs](self).
#[derive(Debug, Clone)]
pub struct ControlGrpc {
    connections: Arc<dyn AgentConnections>,
    clock: Arc<dyn Clock>,
    timings: ControlTimings,
    sessions: Sessions,
}

#[tonic::async_trait]
impl AgentControl for ControlGrpc {
    type ConnectStream = Outbound;

    async fn connect(
        &self,
        request: Request<Streaming<wire::AgentMessage>>,
    ) -> Result<Response<Outbound>, Status> {
        let Some(call) = request.extensions().get::<ControlCall>().cloned() else {
            error!("a connect call arrived without passing its gate");
            return Err(Status::internal(FAILED));
        };
        let Some(connection) = request
            .extensions()
            .get::<TlsConnectInfo<Connection>>()
            .map(|info| info.get_ref().clone())
        else {
            error!(request_id = %call.id, "a connect call arrived without its connection's info");
            return Err(refusal(Code::Internal, FAILED, call.id, None));
        };
        // The store's calls are bounded by the pools' acquire timeout and
        // SQLite's busy timeout.
        let admitted = self
            .connections
            .admit(Presented {
                request: call.id,
                fingerprint: call.fingerprint,
                certificate: call.certificate.to_vec(),
                peer: connection.peer().map(|peer| peer.ip()),
            })
            .await
            .map_err(|failure| {
                if matches!(failure, AdmitError::Internal(_)) {
                    error!(request_id = %call.id, error = %chain(&failure), "a connect call's admission failed");
                }
                refused(&failure, call.id)
            })?;
        let (queue, queued) = mpsc::channel(QUEUE_DEPTH);
        let (last, lasting) = oneshot::channel();
        let session = Session {
            admitted,
            inbound: request.into_inner(),
            queue,
            connection,
            connections: Arc::clone(&self.connections),
            clock: Arc::clone(&self.clock),
            timings: self.timings,
            stop: self.sessions.stop.child_token(),
        };
        self.sessions.tasks.spawn(session.run(last));
        let mut response = Response::new(Outbound::new(queued, lasting));
        super::status::request_id(response.metadata_mut(), call.id);
        Ok(response)
    }
}

/// The answer to an admission or a hello the app didn't accept, for call
/// `id`. A failure on the server's side is the caller's to log.
fn refused(failure: &AdmitError, id: RequestId) -> Status {
    match failure {
        AdmitError::Refused => refusal(Code::Unauthenticated, CERTIFICATE_REFUSED, id, None),
        AdmitError::Busy => refusal(
            Code::Unavailable,
            BUSY,
            id,
            Some(whole_seconds(BUSY_RETRY_AFTER)),
        ),
        AdmitError::Internal(_) => refusal(Code::Internal, FAILED, id, None),
    }
}

/// The control service as the listener serves it, behind its gate.
pub type ControlRoute = InterceptedService<AgentControlServer<ControlGrpc>, ControlGate>;

/// [`ControlGrpc`] over `connections`, stamping with `clock`, waiting as
/// `timings` says and owning its sessions in `sessions`, with the shared
/// message limits, behind a [`ControlGate`] over `limits` and `ids`.
#[must_use]
pub fn control_service(
    connections: Arc<dyn AgentConnections>,
    clock: Arc<dyn Clock>,
    limits: RateLimits,
    ids: RequestIds,
    timings: ControlTimings,
    sessions: Sessions,
) -> ControlRoute {
    InterceptedService::new(
        AgentControlServer::new(ControlGrpc {
            connections,
            clock,
            timings,
            sessions,
        })
        .max_decoding_message_size(MAX_AGENT_MESSAGE_BYTES)
        .max_encoding_message_size(MAX_SERVER_MESSAGE_BYTES),
        ControlGate::new(limits, ids),
    )
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;
    use crate::grpc::status::{REQUEST_ID, RETRY_AFTER};

    fn id() -> RequestId {
        RequestId::new_v7(DateTime::UNIX_EPOCH, [7; 10]).unwrap()
    }

    #[test]
    fn every_admission_failure_has_its_answer_with_the_calls_id() {
        let failures = [
            (
                AdmitError::Refused,
                Code::Unauthenticated,
                CERTIFICATE_REFUSED,
                None,
            ),
            (AdmitError::Busy, Code::Unavailable, BUSY, Some("1")),
            (
                AdmitError::Internal(Box::new(std::io::Error::other("disk"))),
                Code::Internal,
                FAILED,
                None,
            ),
        ];

        for (failure, code, message, retry_after) in failures {
            let status = refused(&failure, id());
            assert_eq!(status.code(), code, "{failure:?}");
            assert_eq!(status.message(), message);
            assert_eq!(
                status.metadata().get(REQUEST_ID).unwrap().to_str().unwrap(),
                id().to_string()
            );
            assert_eq!(
                status
                    .metadata()
                    .get(RETRY_AFTER)
                    .map(|value| value.to_str().unwrap()),
                retry_after
            );
        }
    }

    #[test]
    fn every_answer_has_its_own_message() {
        let messages = [
            CERTIFICATE_REQUIRED,
            CERTIFICATE_REFUSED,
            RATE_LIMITED,
            BUSY,
            FAILED,
            FIRST_MESSAGE_UNREADABLE,
            HELLO_TOO_SLOW,
            HELLO_MISSING,
            ONE_HELLO,
            HEARTBEAT_MISSED,
            TOO_LARGE,
            MESSAGE_UNREADABLE,
            TOO_SLOW,
            SHUTTING_DOWN,
        ];

        let distinct: std::collections::BTreeSet<_> = messages.iter().collect();
        assert_eq!(distinct.len(), messages.len());
    }
}
