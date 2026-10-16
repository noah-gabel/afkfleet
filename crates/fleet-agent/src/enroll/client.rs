//! `Enroll` over the server's gRPC port (Plan.md P10.3; ADR-0019, S14, S19,
//! R8).
//!
//! - **TLS trusts only the pinned CA.** tonic's client starts with no root,
//!   no system or webpki roots are built in, and the agent adds only the
//!   pinned CA. The server's certificate must name the URL's host.
//! - **No client certificate:** the agent has none yet.
//! - **Bounds:** connecting [`CONNECT_TIMEOUT`], the TLS handshake
//!   [`HANDSHAKE_TIMEOUT`], the call [`CLIENT_DEADLINE`] (also sent as
//!   `grpc-timeout`), all of it [`OVERALL_TIMEOUT`]; the answer at most
//!   [`MAX_RESPONSE_BYTES`]. No retry: the token is single-use.
//! - **Answers** become a [`CallError`] by their code alone ([`answered`],
//!   [`refusal`]): the server's message is never shown.
//! - **Unclear ends** ([`Unclear`]): once the channel is up, the request
//!   may reach the server, so running out of time, a status without the
//!   server's `x-request-id` (a broken connection's `UNAVAILABLE`, tonic's
//!   own `CANCELLED` at the deadline) and the server's `INTERNAL` (its
//!   answer can fail after it stored the agent) can't tell whether the agent
//!   was enrolled. Before that, nothing was sent.

use core::error::Error;
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use fleet_proto::ConvertError;
use fleet_proto::enrollment::{CLIENT_DEADLINE, EnrollResponse, MAX_RESPONSE_BYTES, REQUEST_ID};
use fleet_proto::generated as wire;
use fleet_proto::generated::agent_enrollment_client::AgentEnrollmentClient;
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Uri};
use tonic::{Code, Status};

use super::pin::PinnedCa;

/// How long connecting may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the TLS handshake may take.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the whole enrollment may take, connecting included.
pub const OVERALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Why the call didn't enroll the agent.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    /// The URL can't be dialed, or isn't `https://` (the config's
    /// validation refuses both first).
    #[error("the control plane's URL can't be dialed")]
    BadUrl,
    /// The server couldn't be reached.
    #[error("the server can't be reached")]
    Unreachable(#[source] tonic::transport::Error),
    /// The server's certificate isn't one the pinned CA signed for the URL's
    /// host.
    #[error("the server's certificate isn't one the pinned CA signed for its name")]
    Certificate(#[source] tonic::transport::Error),
    /// Nothing came back in time.
    #[error("the server didn't answer in time")]
    TimedOut,
    /// The server refused.
    #[error("the server refused the enrollment")]
    Refused(Refusal),
    /// The request may have reached the server, and no clear answer came
    /// back: the server may have enrolled the agent.
    #[error("no clear answer came back; the server may have enrolled the agent")]
    Unclear(Unclear),
    /// The answer isn't a valid one.
    #[error("the server's answer isn't valid")]
    Answer(#[source] ConvertError),
}

/// A refusal, by its code; see the server's table (ADR-0019, S14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// `UNAUTHENTICATED`: the token is unknown, used or expired.
    InvalidToken,
    /// `ALREADY_EXISTS`: an enabled agent has the name.
    NameTaken,
    /// `INVALID_ARGUMENT`: the request or its CSR was refused.
    Invalid,
    /// `OUT_OF_RANGE`: the request was too large.
    TooLarge,
    /// `RESOURCE_EXHAUSTED`: too many enrollments from this address; try
    /// again after the seconds given, if any.
    RateLimited {
        /// `retry-after`, in whole seconds.
        retry_after: Option<u64>,
    },
    /// `UNAVAILABLE`: the server is busy.
    Unavailable,
    /// `DEADLINE_EXCEEDED`: the call took too long.
    TooSlow,
    /// Any other code.
    Other(Code),
}

/// Why a call whose request may have reached the server has no clear
/// answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unclear {
    /// Nothing came back within [`OVERALL_TIMEOUT`].
    TimedOut,
    /// The call ended with this code, but not as the server's answer (no
    /// `x-request-id`): the connection broke, or tonic's client gave up at
    /// [`CLIENT_DEADLINE`] (`CANCELLED`).
    Broken(Code),
    /// The server failed (`INTERNAL`), perhaps after it stored the agent.
    Failed,
}

/// What `status` means as the end of a call whose request may have reached
/// the server.
#[must_use]
pub fn answered(status: &Status) -> CallError {
    // Every answer of the server carries its request ID; a status without one
    // came from the connection or from tonic's client, not from a decision.
    if status.metadata().get(REQUEST_ID).is_none() {
        return CallError::Unclear(Unclear::Broken(status.code()));
    }
    match status.code() {
        // The server's answer can fail after it stored the agent.
        Code::Internal => CallError::Unclear(Unclear::Failed),
        _ => CallError::Refused(refusal(status)),
    }
}

/// What running out of time means; `connected` says whether the channel
/// was up, so that the request may have reached the server.
#[must_use]
pub const fn timed_out(connected: bool) -> CallError {
    if connected {
        CallError::Unclear(Unclear::TimedOut)
    } else {
        CallError::TimedOut
    }
}

/// The refusal `status` stands for, by its code and `retry-after`.
#[must_use]
pub fn refusal(status: &Status) -> Refusal {
    match status.code() {
        Code::Unauthenticated => Refusal::InvalidToken,
        Code::AlreadyExists => Refusal::NameTaken,
        Code::InvalidArgument => Refusal::Invalid,
        Code::OutOfRange => Refusal::TooLarge,
        Code::ResourceExhausted => Refusal::RateLimited {
            retry_after: status
                .metadata()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|text| text.parse().ok()),
        },
        Code::Unavailable => Refusal::Unavailable,
        Code::DeadlineExceeded => Refusal::TooSlow,
        other => Refusal::Other(other),
    }
}

/// The name the server's certificate must hold: the URL's host, without
/// an IPv6 address's brackets.
#[must_use]
pub fn server_name(uri: &Uri) -> Option<String> {
    let host = uri.host()?;
    let host = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host);
    Some(host.to_owned())
}

/// Whether `error`, or an error it was caused by, is rustls refusing the
/// server's certificate.
#[must_use]
pub fn is_certificate_problem(error: &(dyn Error + 'static)) -> bool {
    let mut next = Some(error);
    while let Some(error) = next {
        let rustls = error.downcast_ref::<rustls::Error>().or_else(|| {
            error
                .downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::get_ref)
                .and_then(|inner| inner.downcast_ref::<rustls::Error>())
        });
        if matches!(rustls, Some(rustls::Error::InvalidCertificate(_))) {
            return true;
        }
        next = error.source();
    }
    false
}

/// Calls `Enroll` at `url`, which must be `https://`, over TLS that trusts
/// only `ca`, with `request`.
///
/// # Errors
/// [`CallError`].
pub async fn call(
    url: &str,
    ca: &PinnedCa,
    request: wire::EnrollRequest,
) -> Result<EnrollResponse, CallError> {
    let uri: Uri = url.parse().map_err(|_| CallError::BadUrl)?;
    // Only over TLS: tonic would dial another scheme in plaintext, token and
    // all, though the TLS config is set.
    if uri.scheme_str() != Some("https") {
        return Err(CallError::BadUrl);
    }
    let name = server_name(&uri).ok_or(CallError::BadUrl)?;
    let tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(&ca.pem))
        .domain_name(name)
        .timeout(HANDSHAKE_TIMEOUT);
    let endpoint = Endpoint::from(uri)
        .tls_config(tls)
        .map_err(|_| CallError::BadUrl)?
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(CLIENT_DEADLINE);
    let connected = AtomicBool::new(false);
    let work = async {
        let channel = endpoint.connect().await.map_err(|error| {
            if is_certificate_problem(&error) {
                CallError::Certificate(error)
            } else {
                CallError::Unreachable(error)
            }
        })?;
        // From here on, the request may reach the server.
        connected.store(true, Ordering::Relaxed);
        let mut client =
            AgentEnrollmentClient::new(channel).max_decoding_message_size(MAX_RESPONSE_BYTES);
        let mut request = tonic::Request::new(request);
        request.set_timeout(CLIENT_DEADLINE);
        let answer = client
            .enroll(request)
            .await
            .map_err(|status| answered(&status))?;
        EnrollResponse::try_from(answer.into_inner()).map_err(CallError::Answer)
    };
    // The bound of everything, the bounds of each step inside it.
    match tokio::time::timeout(OVERALL_TIMEOUT, work).await {
        Ok(result) => result,
        Err(_elapsed) => Err(timed_out(connected.load(Ordering::Relaxed))),
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use tonic::metadata::{AsciiMetadataValue, MetadataMap};

    use super::*;

    fn status(code: Code, retry_after: Option<&'static str>) -> Status {
        let mut metadata = MetadataMap::new();
        if let Some(seconds) = retry_after {
            metadata.insert("retry-after", AsciiMetadataValue::from_static(seconds));
        }
        Status::with_metadata(code, "a message the agent never shows", metadata)
    }

    #[test]
    fn each_code_is_its_refusal() {
        for (code, expected) in [
            (Code::Unauthenticated, Refusal::InvalidToken),
            (Code::AlreadyExists, Refusal::NameTaken),
            (Code::InvalidArgument, Refusal::Invalid),
            (Code::OutOfRange, Refusal::TooLarge),
            (Code::Unavailable, Refusal::Unavailable),
            (Code::DeadlineExceeded, Refusal::TooSlow),
            (Code::Internal, Refusal::Other(Code::Internal)),
            (
                Code::PermissionDenied,
                Refusal::Other(Code::PermissionDenied),
            ),
        ] {
            assert_eq!(refusal(&status(code, None)), expected, "{code:?}");
        }
    }

    #[test]
    fn a_rate_limit_carries_its_wait_when_it_reads() {
        assert_eq!(
            refusal(&status(Code::ResourceExhausted, Some("12"))),
            Refusal::RateLimited {
                retry_after: Some(12)
            }
        );
        assert_eq!(
            refusal(&status(Code::ResourceExhausted, Some("soon"))),
            Refusal::RateLimited { retry_after: None }
        );
        assert_eq!(
            refusal(&status(Code::ResourceExhausted, None)),
            Refusal::RateLimited { retry_after: None }
        );
    }

    #[test]
    fn the_server_name_is_the_urls_host() {
        for (url, name) in [
            ("https://fleet.example.com:7443", "fleet.example.com"),
            ("https://192.0.2.10:7443", "192.0.2.10"),
            ("https://[2001:db8::10]:7443", "2001:db8::10"),
        ] {
            let uri: Uri = url.parse().unwrap();

            assert_eq!(server_name(&uri).as_deref(), Some(name), "{url}");
        }
    }

    /// The server's answer with `code`: it carries a request ID.
    fn servers(code: Code) -> Status {
        let mut metadata = MetadataMap::new();
        metadata.insert(
            fleet_proto::enrollment::REQUEST_ID,
            AsciiMetadataValue::from_static("0190f0a8-3c00-7000-8000-000000000001"),
        );
        Status::with_metadata(code, "a message the agent never shows", metadata)
    }

    #[test]
    fn the_servers_refusals_are_refusals() {
        for (code, expected) in [
            (Code::Unauthenticated, Refusal::InvalidToken),
            (Code::AlreadyExists, Refusal::NameTaken),
            (Code::InvalidArgument, Refusal::Invalid),
            (Code::Unavailable, Refusal::Unavailable),
            (Code::DeadlineExceeded, Refusal::TooSlow),
            (
                Code::ResourceExhausted,
                Refusal::RateLimited { retry_after: None },
            ),
        ] {
            let answer = answered(&servers(code));

            assert!(
                matches!(answer, CallError::Refused(found) if found == expected),
                "{code:?}: {answer:?}"
            );
        }
    }

    #[test]
    fn the_servers_own_failure_is_unclear() {
        let answer = answered(&servers(Code::Internal));

        assert!(
            matches!(answer, CallError::Unclear(Unclear::Failed)),
            "{answer:?}"
        );
    }

    #[test]
    fn a_status_without_the_servers_request_id_is_unclear() {
        for code in [
            Code::Unavailable,
            Code::Cancelled,
            Code::DeadlineExceeded,
            Code::Internal,
            Code::Unknown,
            Code::OutOfRange,
        ] {
            let answer = answered(&status(code, None));

            assert!(
                matches!(answer, CallError::Unclear(Unclear::Broken(found)) if found == code),
                "{code:?}: {answer:?}"
            );
        }
    }

    #[test]
    fn running_out_of_time_is_unclear_once_connected() {
        assert!(matches!(timed_out(false), CallError::TimedOut));
        assert!(matches!(
            timed_out(true),
            CallError::Unclear(Unclear::TimedOut)
        ));
    }

    #[test]
    fn a_certificate_refusal_is_found_through_its_causes() {
        let refused = io::Error::new(
            io::ErrorKind::InvalidData,
            rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer),
        );
        let other = io::Error::new(io::ErrorKind::ConnectionRefused, "refused");
        let tls_other = io::Error::new(
            io::ErrorKind::InvalidData,
            rustls::Error::General("something else".to_owned()),
        );

        assert!(is_certificate_problem(&refused));
        assert!(!is_certificate_problem(&other));
        assert!(!is_certificate_problem(&tls_other));
    }
}
