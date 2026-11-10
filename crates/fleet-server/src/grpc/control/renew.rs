//! `RenewCertificate`'s handler (Plan.md P13.6; ADR-0027); see the
//! [module docs](super).

use std::sync::Arc;

use fleet_core::id::RequestId;
use fleet_proto::ConvertError;
use fleet_proto::control::RenewResponse;
use fleet_proto::generated as wire;
use tokio::time::Instant;
use tonic::transport::server::TlsConnectInfo;
use tonic::{Code, Request, Response, Status};
use tracing::{error, info, warn};

use super::gate::ControlCall;
use super::{BUSY, CERTIFICATE_REFUSED, ControlGrpc, RENEWAL_FAILED, RENEWAL_TOO_SLOW};
use crate::grpc::listener::Connection;
use crate::grpc::status::{code_name, latency_ms, refusal, request_id};
use crate::http::error::{BUSY_RETRY_AFTER, chain, whole_seconds};
use crate::ports::agent_connections::{AdmitError, Presented};
use crate::ports::renewals::RenewError;

/// Why a call that passed the gate wasn't answered with a certificate.
#[derive(Debug)]
enum Failure {
    /// The call's certificate doesn't admit an agent, at its admission or
    /// in the renewal's transaction.
    Refused,
    /// The database is busy.
    Busy,
    /// The signing outlasted its deadline.
    TooSlow,
    /// The admission or the renewal failed on the server's side.
    Internal(Box<dyn std::error::Error + Send + Sync>),
    /// The answer didn't convert.
    Answer(ConvertError),
}

impl From<AdmitError> for Failure {
    fn from(error: AdmitError) -> Self {
        match error {
            AdmitError::Refused => Self::Refused,
            AdmitError::Busy => Self::Busy,
            AdmitError::Internal(source) => Self::Internal(source),
        }
    }
}

impl From<RenewError> for Failure {
    fn from(error: RenewError) -> Self {
        match error {
            RenewError::Refused => Self::Refused,
            RenewError::Busy => Self::Busy,
            RenewError::TooSlow => Self::TooSlow,
            RenewError::Internal(source) => Self::Internal(source),
        }
    }
}

impl Failure {
    /// The refusal the client gets for call `id`; a failure on the server's
    /// side is logged here.
    fn status(self, id: RequestId) -> Status {
        match self {
            Self::Refused => refusal(Code::Unauthenticated, CERTIFICATE_REFUSED, id, None),
            Self::Busy => refusal(
                Code::Unavailable,
                BUSY,
                id,
                Some(whole_seconds(BUSY_RETRY_AFTER)),
            ),
            Self::TooSlow => {
                warn!(request_id = %id, "a certificate renewal took longer than its deadline");
                refusal(Code::DeadlineExceeded, RENEWAL_TOO_SLOW, id, None)
            }
            Self::Internal(source) => {
                error!(request_id = %id, error = %chain(source.as_ref()), "a certificate renewal failed");
                refusal(Code::Internal, RENEWAL_FAILED, id, None)
            }
            Self::Answer(source) => {
                error!(request_id = %id, error = %chain(&source), "a certificate renewal's answer couldn't be written");
                refusal(Code::Internal, RENEWAL_FAILED, id, None)
            }
        }
    }
}

impl ControlGrpc {
    /// `RenewCertificate`: the gate has spent the certificate's limit; the
    /// admission is `Connect`'s, then the renewal within its deadline.
    pub(super) async fn renew(
        &self,
        request: Request<wire::RenewRequest>,
    ) -> Result<Response<wire::RenewResponse>, Status> {
        let started = Instant::now();
        let Some(call) = request.extensions().get::<ControlCall>().cloned() else {
            error!("a renew call arrived without passing its gate");
            return Err(Status::internal(RENEWAL_FAILED));
        };
        let peer = request
            .extensions()
            .get::<TlsConnectInfo<Connection>>()
            .map(|info| info.get_ref().peer().map(|peer| peer.ip()));
        let answered = match peer {
            Some(peer) => self.answer(&call, peer).await,
            None => Err(Failure::Internal(Box::new(NoConnection))),
        };
        let (code, answer) = match answered {
            Ok(message) => {
                let mut response = Response::new(message);
                request_id(response.metadata_mut(), call.id);
                (Code::Ok, Ok(response))
            }
            Err(failure) => {
                let status = failure.status(call.id);
                (status.code(), Err(status))
            }
        };
        info!(
            request_id = %call.id,
            code = code_name(code),
            latency_ms = latency_ms(started),
            "a certificate renewal finished"
        );
        answer
    }

    /// Admits `call`'s certificate, which came from `peer`, and renews it.
    /// The store's calls are bounded by the pools' acquire timeout and
    /// SQLite's busy timeout, the signing by the renewal's deadline.
    async fn answer(
        &self,
        call: &ControlCall,
        peer: Option<std::net::IpAddr>,
    ) -> Result<wire::RenewResponse, Failure> {
        let certificate = call.certificate.to_vec();
        let admitted = self
            .connections
            .admit(Presented {
                request: call.id,
                fingerprint: call.fingerprint,
                certificate: certificate.clone(),
                peer,
            })
            .await?;
        let renewed = Arc::clone(&self.renewals)
            .renew(&admitted, certificate, self.timings.renew_deadline)
            .await?;
        wire::RenewResponse::try_from(RenewResponse {
            certificate_pem: renewed.certificate_pem,
            not_after: renewed.not_after,
        })
        .map_err(Failure::Answer)
    }
}

/// A renew call arrived without its connection's info: a wiring bug.
#[derive(Debug, thiserror::Error)]
#[error("the call has no connection info")]
struct NoConnection;

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use fleet_proto::Problem;

    use super::*;
    use crate::grpc::status::{REQUEST_ID, RETRY_AFTER};

    fn id() -> RequestId {
        RequestId::new_v7(DateTime::UNIX_EPOCH, [7; 10]).unwrap()
    }

    fn disk() -> Box<dyn std::error::Error + Send + Sync> {
        Box::new(std::io::Error::other("disk /srv/afkfleet is full"))
    }

    #[test]
    fn each_failure_has_its_code_message_and_request_id() {
        let request_id = id().to_string();
        let cases = [
            (
                Failure::from(AdmitError::Refused),
                Code::Unauthenticated,
                CERTIFICATE_REFUSED,
                None,
            ),
            (
                Failure::from(RenewError::Refused),
                Code::Unauthenticated,
                CERTIFICATE_REFUSED,
                None,
            ),
            (
                Failure::from(AdmitError::Busy),
                Code::Unavailable,
                BUSY,
                Some("1"),
            ),
            (
                Failure::from(RenewError::Busy),
                Code::Unavailable,
                BUSY,
                Some("1"),
            ),
            (
                Failure::from(RenewError::TooSlow),
                Code::DeadlineExceeded,
                RENEWAL_TOO_SLOW,
                None,
            ),
            (
                Failure::from(AdmitError::Internal(disk())),
                Code::Internal,
                RENEWAL_FAILED,
                None,
            ),
            (
                Failure::from(RenewError::Internal(disk())),
                Code::Internal,
                RENEWAL_FAILED,
                None,
            ),
            (
                Failure::Internal(Box::new(NoConnection)),
                Code::Internal,
                RENEWAL_FAILED,
                None,
            ),
            (
                Failure::Answer(ConvertError::new(
                    "RenewResponse.not_after_unix_ms",
                    Problem::OutOfRange,
                )),
                Code::Internal,
                RENEWAL_FAILED,
                None,
            ),
        ];

        for (failure, code, message, retry_after) in cases {
            let shown = format!("{failure:?}");
            let status = failure.status(id());
            let text = |key| {
                status
                    .metadata()
                    .get(key)
                    .map(|value| value.to_str().unwrap())
            };
            assert_eq!(
                (
                    status.code(),
                    status.message(),
                    text(REQUEST_ID),
                    text(RETRY_AFTER)
                ),
                (code, message, Some(request_id.as_str()), retry_after),
                "{shown}"
            );
        }
    }

    #[test]
    fn a_failure_on_the_servers_side_never_reaches_the_message() {
        let status = Failure::from(RenewError::Internal(disk())).status(id());

        assert!(
            !format!("{status:?}").contains("/srv/afkfleet"),
            "{status:?}"
        );
    }
}
