//! [`ControlGate`]: what every `Connect` call passes before anything it
//! sent is read; see the [module docs](super).

use core::fmt;

use fleet_core::id::RequestId;
use fleet_core::pki::CertFingerprint;
use rustls::pki_types::CertificateDer;
use tokio::time::Instant;
use tonic::service::Interceptor;
use tonic::transport::server::TlsConnectInfo;
use tonic::{Code, Request, Status};
use tracing::{debug, error};

use super::{CERTIFICATE_REQUIRED, FAILED, RATE_LIMITED};
use crate::grpc::listener::Connection;
use crate::grpc::status::{code_name, latency_ms, refusal};
use crate::http::error::{chain, whole_seconds};
use crate::http::middleware::RequestIds;
use crate::http::rate_limit::RateLimits;
use crate::infra::pki::fingerprint;

/// The gate in front of `Connect`.
#[derive(Debug, Clone)]
pub struct ControlGate {
    limits: RateLimits,
    ids: RequestIds,
}

impl ControlGate {
    /// The gate spending `limits`' connections, minting request IDs from
    /// `ids`.
    #[must_use]
    pub const fn new(limits: RateLimits, ids: RequestIds) -> Self {
        Self { limits, ids }
    }
}

/// What the gate hands the handler: the call's request ID and the
/// certificate it presented.
#[derive(Clone)]
pub(super) struct ControlCall {
    /// The call's request ID.
    pub(super) id: RequestId,
    /// The certificate's fingerprint.
    pub(super) fingerprint: CertFingerprint,
    /// The certificate, which is parsed only once its fingerprint is known.
    pub(super) certificate: CertificateDer<'static>,
}

impl fmt::Debug for ControlCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The certificate's bytes say nothing in a log (S24).
        f.debug_struct("ControlCall")
            .field("id", &self.id)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl Interceptor for ControlGate {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        let started = Instant::now();
        let certificates = request
            .extensions()
            .get::<TlsConnectInfo<Connection>>()
            .and_then(TlsConnectInfo::peer_certs);
        let Some(certificate) = presented(certificates.as_deref().map(Vec::as_slice)).cloned()
        else {
            // First of all, before an ID is minted: a call without a
            // certificate costs this line and nothing else (S24). Any
            // number of them may come, so it's at `debug`.
            debug!("a connect call without a client certificate was refused");
            return Err(Status::unauthenticated(CERTIFICATE_REQUIRED));
        };
        let id = match self.ids.mint() {
            Ok((id, _)) => id,
            Err(failure) => {
                error!(
                    error = %chain(&failure),
                    "a connect call was refused: no request ID could be minted"
                );
                return Err(Status::internal(FAILED));
            }
        };
        let fingerprint = fingerprint(&certificate);
        if let Err(refused) = self.limits.check_connect(fingerprint) {
            // The minute's summary counts these, so a flood doesn't flood
            // the log.
            debug!(
                request_id = %id,
                code = code_name(Code::ResourceExhausted),
                latency_ms = latency_ms(started),
                "a connect call was rate-limited"
            );
            return Err(refusal(
                Code::ResourceExhausted,
                RATE_LIMITED,
                id,
                Some(whole_seconds(refused.retry_after)),
            ));
        }
        request.extensions_mut().insert(ControlCall {
            id,
            fingerprint,
            certificate,
        });
        Ok(request)
    }
}

/// The certificate a client presented: the first of `certificates`, its
/// own; none for no list or an empty one.
fn presented<'a>(
    certificates: Option<&'a [CertificateDer<'static>]>,
) -> Option<&'a CertificateDer<'static>> {
    certificates.and_then(<[_]>::first)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::DateTime;
    use fleet_testkit::system::{ManualClock, SeededRandom};

    use super::*;
    use crate::config::RateLimitConfig;
    use crate::grpc::status::REQUEST_ID;

    fn gate() -> ControlGate {
        let clock = Arc::new(ManualClock::new(DateTime::UNIX_EPOCH));
        ControlGate::new(
            RateLimits::new(&RateLimitConfig::default(), clock.clone()),
            RequestIds::new(clock, Arc::new(SeededRandom::new(3))),
        )
    }

    #[test]
    fn the_presented_certificate_is_the_first_and_none_is_none() {
        let own = CertificateDer::from(vec![1, 2, 3]);
        let issuer = CertificateDer::from(vec![4, 5, 6]);
        let both = [own.clone(), issuer];

        assert_eq!(presented(None), None);
        assert_eq!(presented(Some(&[])), None);
        assert_eq!(presented(Some(&both)), Some(&own));
    }

    #[test]
    fn a_call_without_a_certificate_is_refused_before_it_gets_an_id() {
        let refused = gate().call(Request::new(())).unwrap_err();

        assert_eq!(refused.code(), Code::Unauthenticated);
        assert_eq!(refused.message(), CERTIFICATE_REQUIRED);
        assert!(refused.metadata().get(REQUEST_ID).is_none());
    }
}
