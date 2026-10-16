//! The gRPC port's listener (Plan.md P10.3; ADR-0019, S13, S14).
//!
//! - **TLS** ([`tls`]): the server certificate and key of `pki init` as the
//!   identity, the CA as the root for client certificates, and client
//!   certificates optional: `Enroll` runs before an agent has one. A
//!   certificate that is presented is always verified, so one from another
//!   CA, or an expired one, fails the handshake. The handshake gets
//!   [`HANDSHAKE_TIMEOUT`].
//! - **Connections** ([`router`]): at most [`MAX_CONCURRENT_STREAMS`] calls
//!   at once on one, and none lives longer than [`MAX_CONNECTION_AGE`] plus
//!   [`MAX_CONNECTION_AGE_GRACE`]: a hard cut at 80 s. With the grace set,
//!   tonic 0.14.6 sends no GOAWAY at 60 s; it sleeps for the age plus the
//!   grace, then closes the connection forcefully, with any call still
//!   running on it. While `Enroll` is the port's one service this bounds
//!   every connection; group C replaces it with a bound for connections
//!   without a client certificate only (R9).
//! - **Serving** ([`serve`]): on a listener the caller bound, until its stop
//!   token is cancelled; then in-flight calls finish, within the server's
//!   shutdown deadline.

use core::time::Duration;

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tonic::transport::server::{Router, TcpIncoming};
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};

use super::enrollment::{EnrollGate, EnrollmentGrpc};
use crate::infra::pki::LoadedPki;

/// How long a TLS handshake may take.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How many calls one connection may run at once.
pub const MAX_CONCURRENT_STREAMS: u32 = 16;
/// A connection's age limit. With [`MAX_CONNECTION_AGE_GRACE`] set, tonic
/// 0.14.6 doesn't ask a connection to close at this age: the grace is added,
/// and the connection is cut then.
pub const MAX_CONNECTION_AGE: Duration = Duration::from_secs(60);
/// Added to [`MAX_CONNECTION_AGE`]: at the sum, 80 s, tonic 0.14.6 closes
/// the connection forcefully, with no GOAWAY first and whatever calls it
/// runs.
pub const MAX_CONNECTION_AGE_GRACE: Duration = Duration::from_secs(20);

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

/// The port's server over `tls`, with its connection limits, serving
/// `enrollment`.
///
/// # Errors
/// tonic's error when `tls` can't be used.
pub fn router(
    tls: ServerTlsConfig,
    enrollment: EnrollmentRoute,
) -> Result<Router, tonic::transport::Error> {
    let mut server = Server::builder()
        .tls_config(tls)?
        .max_concurrent_streams(MAX_CONCURRENT_STREAMS)
        .max_connection_age(MAX_CONNECTION_AGE)
        .max_connection_age_grace(MAX_CONNECTION_AGE_GRACE);
    Ok(server.add_service(enrollment))
}

/// Serves `router` on `listener` until `stop` is cancelled, then lets
/// in-flight calls finish.
///
/// # Errors
/// tonic's error when the server fails.
pub async fn serve(
    router: Router,
    listener: TcpListener,
    stop: CancellationToken,
) -> Result<(), tonic::transport::Error> {
    router
        .serve_with_incoming_shutdown(TcpIncoming::from(listener), stop.cancelled_owned())
        .await
}
