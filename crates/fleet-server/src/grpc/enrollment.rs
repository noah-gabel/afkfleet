//! `fleet.v1.AgentEnrollment` on the gRPC port (Plan.md P10.3; ADR-0019,
//! S14, S21): an agent trades an enrollment token and its CSR for its
//! certificate, over TLS without a client certificate.
//!
//! Each call passes two layers:
//! 1. **The gate** ([`EnrollGate`], tonic's interceptor), before the
//!    request's body is read: it mints the call's request ID, takes the
//!    peer's address from the connection (none is a wiring bug: `INTERNAL`)
//!    and spends the peer's `[rate_limits] enroll_per_ip_per_min`
//!    (`RESOURCE_EXHAUSTED`, with `retry-after`).
//! 2. **The handler** ([`EnrollmentGrpc`]): it converts the message
//!    (`INVALID_ARGUMENT`), then runs [`EnrollmentService::enroll`], which
//!    checks the token, the name and the CSR in that order and signs it,
//!    within its deadline (`DEADLINE_EXCEEDED`; the server's is
//!    [`SERVER_DEADLINE`](fleet_proto::enrollment::SERVER_DEADLINE)). Its
//!    writes after that run to their end, past the deadline, and the
//!    answer is sent. Only the client can still drop the call there, by
//!    resetting its stream or with a `grpc-timeout` of its own.
//!
//! The service's decoding limit is [`MAX_REQUEST_BYTES`] (a larger request
//! is tonic's own `OUT_OF_RANGE`) and its encoding limit
//! [`MAX_RESPONSE_BYTES`]; it accepts no compressed request.
//!
//! Every answer carries `x-request-id`, and every refusal one fixed message
//! per case, never what exactly failed:
//!
//! | Case | Code | Message |
//! |---|---|---|
//! | rate-limited | `RESOURCE_EXHAUSTED` | [`RATE_LIMITED`], with `retry-after` |
//! | a malformed request | `INVALID_ARGUMENT` | [`INVALID_REQUEST`] |
//! | any token problem | `UNAUTHENTICATED` | [`INVALID_TOKEN`] |
//! | the name is taken | `ALREADY_EXISTS` | [`NAME_TAKEN`] |
//! | a refused CSR | `INVALID_ARGUMENT` | [`INVALID_CSR`] |
//! | the database is busy | `UNAVAILABLE` | [`BUSY`], with `retry-after: 1` |
//! | too slow | `DEADLINE_EXCEEDED` | [`TOO_SLOW`] |
//! | a failure on the server's side | `INTERNAL` | [`FAILED`] |
//!
//! **Logs:** one "an enrollment finished" line per call with its request
//! ID, its code and its latency, at `info` (at `debug` for a rate-limited
//! call, which the minute's summary counts), and a failure on the server's
//! side at `error`. Never the token, its hash, the CSR or the peer's
//! address.

use core::time::Duration;
use std::net::IpAddr;

use fleet_core::id::RequestId;
use fleet_proto::ConvertError;
use fleet_proto::enrollment::{
    EnrollRequest, EnrollResponse, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES,
};
use fleet_proto::generated as wire;
use fleet_proto::generated::agent_enrollment_server::{AgentEnrollment, AgentEnrollmentServer};
use tokio::time::Instant;
use tonic::metadata::{AsciiMetadataValue, MetadataMap};
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::{Code, Request, Response, Status};
use tracing::{debug, error, info, warn};

use crate::app::enrollment::{EnrollError, EnrollmentService};
use crate::http::error::{BUSY_RETRY_AFTER, chain, whole_seconds};
use crate::http::middleware::RequestIds;
use crate::http::rate_limit::RateLimits;

/// The metadata every answer carries: the call's request ID.
pub const REQUEST_ID: &str = fleet_proto::enrollment::REQUEST_ID;
/// The metadata of a rate-limited or busy answer: whole seconds to wait.
pub const RETRY_AFTER: &str = "retry-after";

/// `RESOURCE_EXHAUSTED`'s message.
pub const RATE_LIMITED: &str = "Too many enrollments from this address; try again later.";
/// `INVALID_ARGUMENT`'s message for a request that doesn't convert.
pub const INVALID_REQUEST: &str = "The enrollment request isn't valid.";
/// `UNAUTHENTICATED`'s message, one for every token problem.
pub const INVALID_TOKEN: &str =
    "The enrollment token isn't valid: it's unknown, used or expired; create a new one.";
/// `ALREADY_EXISTS`'s message.
pub const NAME_TAKEN: &str =
    "An enabled agent has this name already: give this agent another name.";
/// `INVALID_ARGUMENT`'s message for a refused CSR.
pub const INVALID_CSR: &str = "The certificate request isn't acceptable.";
/// `UNAVAILABLE`'s message.
pub const BUSY: &str = "The server is busy; try again in a moment.";
/// `DEADLINE_EXCEEDED`'s message.
pub const TOO_SLOW: &str = "The enrollment took too long; try again.";
/// `INTERNAL`'s message.
pub const FAILED: &str = "The enrollment failed on the server.";

/// What the gate hands the handler: the call's ID, its peer, and when it
/// arrived.
#[derive(Debug, Clone, Copy)]
struct Call {
    id: RequestId,
    peer: IpAddr,
    started: Instant,
}

/// The gate in front of the service; see the [module docs](self).
#[derive(Debug, Clone)]
pub struct EnrollGate {
    limits: RateLimits,
    ids: RequestIds,
}

impl EnrollGate {
    /// The gate spending `limits`' enrollments, minting request IDs from
    /// `ids`.
    #[must_use]
    pub const fn new(limits: RateLimits, ids: RequestIds) -> Self {
        Self { limits, ids }
    }
}

impl Interceptor for EnrollGate {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        let started = Instant::now();
        let id = match self.ids.mint() {
            Ok((id, _)) => id,
            Err(failure) => {
                error!(
                    error = %chain(&failure),
                    "an enrollment was refused: no request ID could be minted"
                );
                return Err(Status::internal(FAILED));
            }
        };
        let Some(peer) = request.remote_addr() else {
            error!(request_id = %id, "an enrollment was refused: its connection has no peer address");
            finished(id, started, Code::Internal);
            return Err(refusal(Code::Internal, FAILED, id, None));
        };
        if let Err(refused) = self.limits.check_enroll(peer.ip()) {
            // The minute's summary counts these, so a flood doesn't flood
            // the log.
            debug!(
                request_id = %id,
                code = code_name(Code::ResourceExhausted),
                latency_ms = latency_ms(started),
                "an enrollment finished"
            );
            return Err(refusal(
                Code::ResourceExhausted,
                RATE_LIMITED,
                id,
                Some(whole_seconds(refused.retry_after)),
            ));
        }
        request.extensions_mut().insert(Call {
            id,
            peer: peer.ip(),
            started,
        });
        Ok(request)
    }
}

/// Why a call that passed the gate wasn't answered with a certificate.
#[derive(Debug)]
enum Failure {
    /// The message didn't convert.
    Malformed,
    /// The service refused or failed.
    Enroll(EnrollError),
    /// The answer didn't convert.
    Answer(ConvertError),
}

impl Failure {
    /// The refusal the client gets for call `id`; a failure on the server's
    /// side is logged here.
    fn status(self, id: RequestId) -> Status {
        match self {
            Self::Malformed => refusal(Code::InvalidArgument, INVALID_REQUEST, id, None),
            Self::Enroll(EnrollError::Token(_)) => {
                refusal(Code::Unauthenticated, INVALID_TOKEN, id, None)
            }
            Self::Enroll(EnrollError::NameTaken) => {
                refusal(Code::AlreadyExists, NAME_TAKEN, id, None)
            }
            Self::Enroll(EnrollError::Csr(_)) => {
                refusal(Code::InvalidArgument, INVALID_CSR, id, None)
            }
            Self::Enroll(EnrollError::Busy) => refusal(
                Code::Unavailable,
                BUSY,
                id,
                Some(whole_seconds(BUSY_RETRY_AFTER)),
            ),
            Self::Enroll(error @ EnrollError::Internal(_)) => {
                error!(request_id = %id, error = %chain(&error), "an enrollment failed");
                refusal(Code::Internal, FAILED, id, None)
            }
            Self::Enroll(EnrollError::TooSlow) => {
                warn!(request_id = %id, "an enrollment took longer than its deadline");
                refusal(Code::DeadlineExceeded, TOO_SLOW, id, None)
            }
            Self::Answer(error) => {
                error!(request_id = %id, error = %chain(&error), "an enrollment's answer couldn't be written");
                refusal(Code::Internal, FAILED, id, None)
            }
        }
    }
}

/// A refusal with `code` and `message`, carrying the call's request ID and,
/// if given, `retry-after` in whole seconds.
fn refusal(code: Code, message: &'static str, id: RequestId, retry_after: Option<u64>) -> Status {
    let mut metadata = MetadataMap::new();
    if let Ok(value) = AsciiMetadataValue::try_from(id.to_string()) {
        metadata.insert(REQUEST_ID, value);
    }
    if let Some(seconds) = retry_after {
        metadata.insert(RETRY_AFTER, AsciiMetadataValue::from(seconds));
    }
    Status::with_metadata(code, message, metadata)
}

/// Logs the call's one "an enrollment finished" line.
fn finished(id: RequestId, started: Instant, code: Code) {
    info!(
        request_id = %id,
        code = code_name(code),
        latency_ms = latency_ms(started),
        "an enrollment finished"
    );
}

/// The milliseconds since `started`.
fn latency_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// `code`'s name in a log line, as gRPC spells it.
const fn code_name(code: Code) -> &'static str {
    match code {
        Code::Ok => "ok",
        Code::Cancelled => "cancelled",
        Code::Unknown => "unknown",
        Code::InvalidArgument => "invalid_argument",
        Code::DeadlineExceeded => "deadline_exceeded",
        Code::NotFound => "not_found",
        Code::AlreadyExists => "already_exists",
        Code::PermissionDenied => "permission_denied",
        Code::ResourceExhausted => "resource_exhausted",
        Code::FailedPrecondition => "failed_precondition",
        Code::Aborted => "aborted",
        Code::OutOfRange => "out_of_range",
        Code::Unimplemented => "unimplemented",
        Code::Internal => "internal",
        Code::Unavailable => "unavailable",
        Code::DataLoss => "data_loss",
        Code::Unauthenticated => "unauthenticated",
    }
}

/// The service's handler; see the [module docs](self).
#[derive(Debug, Clone)]
pub struct EnrollmentGrpc {
    service: EnrollmentService,
    deadline: Duration,
}

#[tonic::async_trait]
impl AgentEnrollment for EnrollmentGrpc {
    async fn enroll(
        &self,
        request: Request<wire::EnrollRequest>,
    ) -> Result<Response<wire::EnrollResponse>, Status> {
        let Some(call) = request.extensions().get::<Call>().copied() else {
            error!("an enrollment arrived without passing its gate");
            return Err(Status::internal(FAILED));
        };
        // The deadline is the service's: it bounds the checks and the signing,
        // and the write transaction that follows runs to its end.
        match self.answer(request.into_inner(), call.peer).await {
            Ok(message) => {
                finished(call.id, call.started, Code::Ok);
                let mut response = Response::new(message);
                if let Ok(value) = AsciiMetadataValue::try_from(call.id.to_string()) {
                    response.metadata_mut().insert(REQUEST_ID, value);
                }
                Ok(response)
            }
            Err(failure) => {
                let status = failure.status(call.id);
                finished(call.id, call.started, status.code());
                Err(status)
            }
        }
    }
}

impl EnrollmentGrpc {
    /// Enrolls the agent of `message`, which came from `peer`.
    async fn answer(
        &self,
        message: wire::EnrollRequest,
        peer: IpAddr,
    ) -> Result<wire::EnrollResponse, Failure> {
        let request = EnrollRequest::try_from(message).map_err(|_| Failure::Malformed)?;
        let enrolled = self
            .service
            .enroll(request, peer, self.deadline)
            .await
            .map_err(Failure::Enroll)?;
        wire::EnrollResponse::try_from(EnrollResponse {
            agent_id: enrolled.agent,
            certificate_pem: enrolled.certificate_pem,
            not_after: enrolled.not_after,
        })
        .map_err(Failure::Answer)
    }
}

/// The enrollment service as the listener serves it: [`EnrollmentGrpc`]
/// over `service` with `deadline` (the server's is
/// [`SERVER_DEADLINE`](fleet_proto::enrollment::SERVER_DEADLINE)) and its
/// message limits, behind an [`EnrollGate`] over `limits` and `ids`.
#[must_use]
pub fn enrollment_service(
    service: EnrollmentService,
    limits: RateLimits,
    ids: RequestIds,
    deadline: Duration,
) -> InterceptedService<AgentEnrollmentServer<EnrollmentGrpc>, EnrollGate> {
    InterceptedService::new(
        AgentEnrollmentServer::new(EnrollmentGrpc { service, deadline })
            .max_decoding_message_size(MAX_REQUEST_BYTES)
            .max_encoding_message_size(MAX_RESPONSE_BYTES),
        EnrollGate::new(limits, ids),
    )
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use fleet_proto::Problem;

    use super::*;
    use crate::app::enrollment::TokenProblem;
    use crate::ports::pki::CsrRefusal;

    fn id() -> RequestId {
        RequestId::new_v7(DateTime::UNIX_EPOCH, [7; 10]).unwrap()
    }

    fn seen(status: &Status) -> (Code, &str, Option<&str>, Option<&str>) {
        let text = |key| {
            status
                .metadata()
                .get(key)
                .map(|value| value.to_str().unwrap())
        };
        (
            status.code(),
            status.message(),
            text(REQUEST_ID),
            text(RETRY_AFTER),
        )
    }

    #[test]
    fn each_failure_has_its_code_message_and_request_id() {
        let request_id = id().to_string();
        let cases = [
            (
                Failure::Malformed,
                Code::InvalidArgument,
                INVALID_REQUEST,
                None,
            ),
            (
                Failure::Enroll(EnrollError::Token(TokenProblem::Expired)),
                Code::Unauthenticated,
                INVALID_TOKEN,
                None,
            ),
            (
                Failure::Enroll(EnrollError::NameTaken),
                Code::AlreadyExists,
                NAME_TAKEN,
                None,
            ),
            (
                Failure::Enroll(EnrollError::Csr(CsrRefusal::KeyType)),
                Code::InvalidArgument,
                INVALID_CSR,
                None,
            ),
            (
                Failure::Enroll(EnrollError::Busy),
                Code::Unavailable,
                BUSY,
                Some("1"),
            ),
            (
                Failure::Enroll(EnrollError::Internal(Box::new(std::io::Error::other(
                    "a test's failure",
                )))),
                Code::Internal,
                FAILED,
                None,
            ),
            (
                Failure::Answer(ConvertError::new(
                    "EnrollResponse.not_after_unix_ms",
                    Problem::OutOfRange,
                )),
                Code::Internal,
                FAILED,
                None,
            ),
            (
                Failure::Enroll(EnrollError::TooSlow),
                Code::DeadlineExceeded,
                TOO_SLOW,
                None,
            ),
        ];

        for (failure, code, message, retry_after) in cases {
            let shown = format!("{failure:?}");
            let status = failure.status(id());
            assert_eq!(
                seen(&status),
                (code, message, Some(request_id.as_str()), retry_after),
                "{shown}"
            );
        }
    }

    #[test]
    fn a_failure_on_the_servers_side_never_reaches_the_message() {
        let status = Failure::Enroll(EnrollError::Internal(Box::new(std::io::Error::other(
            "disk /srv/afkfleet is full",
        ))))
        .status(id());

        assert!(
            !format!("{status:?}").contains("/srv/afkfleet"),
            "{status:?}"
        );
    }

    #[test]
    fn every_code_has_its_grpc_name() {
        assert_eq!(code_name(Code::Ok), "ok");
        assert_eq!(code_name(Code::ResourceExhausted), "resource_exhausted");
        assert_eq!(code_name(Code::Unauthenticated), "unauthenticated");
        assert_eq!(code_name(Code::DeadlineExceeded), "deadline_exceeded");
    }
}
