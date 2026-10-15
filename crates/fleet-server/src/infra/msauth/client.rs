//! The adapter's HTTP side (ADR-0018): the one client builder, the capped
//! reader and what a reqwest error means.
//!
//! Both of the adapter's constructors build their client here, so the
//! tests' client, which may speak plain http to a local fake, has exactly
//! the production client's other settings:
//! - the connect and request timeouts
//! - no redirects: a 3xx comes back as it is
//! - no proxy: environment variables can't route the traffic elsewhere
//!
//! The production client also refuses anything but https.
//!
//! The reader refuses a `Content-Length` over
//! [`MAX_ANSWER_BYTES`] before reading, and drops the response (and with it
//! the connection) as soon as a body grows past it. It reserves the body's
//! whole size up front, so the buffer never moves and leaves no copy behind
//! when it's wiped. hyper's own receive buffers can't be wiped (a known
//! limit, like the request bodies reqwest holds).
//!
//! This is the one module in fleet-server that names reqwest's client types:
//! clippy.toml bans them everywhere else, and the adapter holds its client
//! as an [`HttpClient`].
#![expect(
    clippy::disallowed_types,
    reason = "the one place fleet-server builds and holds a reqwest client (ADR-0018, approved)"
)]

use core::error::Error;
use core::time::Duration;

use zeroize::Zeroizing;

use super::answers::Answer;
use crate::ports::msauth::{Endpoint, Failure, MAX_ANSWER_BYTES, Rule};

/// The bounds of the adapter's calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Timeouts {
    /// DNS, TCP and the TLS handshake.
    pub(super) connect: Duration,
    /// One whole request, its body included.
    pub(super) request: Duration,
    /// A whole call with one request: the device code, a poll, a refresh.
    pub(super) call: Duration,
    /// A whole `minecraft_session`: five requests in a row.
    pub(super) session: Duration,
}

impl Timeouts {
    /// The production bounds (ADR-0018).
    pub(super) const MICROSOFT: Self = Self {
        connect: Duration::from_secs(5),
        request: Duration::from_secs(15),
        call: Duration::from_secs(20),
        session: Duration::from_secs(60),
    };
}

/// Why the adapter's HTTP client couldn't be built: reqwest found no TLS
/// setup it could use.
#[derive(Debug, thiserror::Error)]
#[error("the HTTP client for Microsoft can't be built")]
pub struct BuildError(#[source] Box<dyn Error + Send + Sync>);

#[cfg(test)]
impl BuildError {
    /// A build failure caused by `source`, for the tests of what starting
    /// does with one.
    pub(crate) fn caused_by(source: impl Into<Box<dyn Error + Send + Sync>>) -> Self {
        Self(source.into())
    }
}

/// The adapter's client, built only by [`build`].
pub(super) struct HttpClient(reqwest::Client);

impl HttpClient {
    /// A POST to `url`.
    pub(super) fn post(&self, url: impl reqwest::IntoUrl) -> reqwest::RequestBuilder {
        self.0.post(url)
    }

    /// A GET of `url`.
    pub(super) fn get(&self, url: impl reqwest::IntoUrl) -> reqwest::RequestBuilder {
        self.0.get(url)
    }
}

/// The one client builder; `https_only` is false only for the tests' fake.
pub(super) fn build(timeouts: &Timeouts, https_only: bool) -> Result<HttpClient, BuildError> {
    reqwest::Client::builder()
        .connect_timeout(timeouts.connect)
        .timeout(timeouts.request)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .https_only(https_only)
        .build()
        .map(HttpClient)
        .map_err(|error| BuildError(Box::new(error.without_url())))
}

/// Reads `response` from `endpoint` up to [`MAX_ANSWER_BYTES`].
pub(super) async fn read(
    endpoint: Endpoint,
    mut response: reqwest::Response,
) -> Result<Answer, Failure> {
    let too_large = || Failure::Unexpected {
        endpoint,
        rule: Rule::BodyTooLarge,
    };
    let announced = match response.content_length() {
        None => None,
        Some(length) => match usize::try_from(length) {
            Ok(length) if length <= MAX_ANSWER_BYTES => Some(length),
            _ => return Err(too_large()),
        },
    };
    let status = response.status().as_u16();
    // The whole size up front, so the buffer never moves (see the module
    // docs); hyper never delivers more than an announced length.
    let mut body = Zeroizing::new(Vec::with_capacity(announced.unwrap_or(MAX_ANSWER_BYTES)));
    while let Some(chunk) = response.chunk().await.map_err(|error| classify(&error))? {
        if body.len().saturating_add(chunk.len()) > MAX_ANSWER_BYTES {
            // Dropping the response drops its connection, so the rest is
            // never read.
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Answer { status, body })
}

/// What a reqwest error means: a timeout, a refused certificate (rustls's
/// `InvalidCertificate` anywhere in its chain), or no connection. The error
/// itself, which may name a URL, isn't kept.
pub(super) fn classify(error: &reqwest::Error) -> Failure {
    failure_of(error.is_timeout(), rustls_error(error))
}

/// [`classify`]'s rule, apart from reqwest's error so tests can drive it.
fn failure_of(timed_out: bool, tls: Option<&rustls::Error>) -> Failure {
    if timed_out {
        Failure::TimedOut
    } else if matches!(tls, Some(rustls::Error::InvalidCertificate(_))) {
        Failure::Certificate
    } else {
        Failure::Unreachable
    }
}

/// The rustls error in `error`'s chain, also inside an `io::Error`, whose
/// own `source()` skips the error it wraps (fleet-client's walk).
fn rustls_error<'a>(error: &'a (dyn Error + 'static)) -> Option<&'a rustls::Error> {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(tls) = error.downcast_ref::<rustls::Error>() {
            return Some(tls);
        }
        if let Some(inner) = error
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::get_ref)
            .and_then(|inner| rustls_error(inner))
        {
            return Some(inner);
        }
        current = error.source();
    }
    None
}

#[cfg(test)]
mod tests {
    use core::fmt;

    use rstest::rstest;

    use super::*;

    /// An error that wraps another, as hyper and reqwest do.
    #[derive(Debug)]
    struct Wrapper(std::io::Error);

    impl fmt::Display for Wrapper {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("wrapper")
        }
    }

    impl Error for Wrapper {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(&self.0)
        }
    }

    fn refused() -> rustls::Error {
        rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer)
    }

    #[rstest]
    #[case::timeout(true, None, Failure::TimedOut)]
    #[case::timeout_wins(true, Some(refused()), Failure::TimedOut)]
    #[case::certificate(false, Some(refused()), Failure::Certificate)]
    #[case::other_tls(false, Some(rustls::Error::DecryptError), Failure::Unreachable)]
    #[case::no_connection(false, None, Failure::Unreachable)]
    fn each_kind_of_error_has_its_failure(
        #[case] timed_out: bool,
        #[case] tls: Option<rustls::Error>,
        #[case] failure: Failure,
    ) {
        assert_eq!(failure_of(timed_out, tls.as_ref()), failure);
    }

    #[test]
    fn a_rustls_error_is_found_inside_an_io_error() {
        let io = std::io::Error::new(std::io::ErrorKind::InvalidData, refused());
        let outer: Box<dyn Error + Send + Sync> = Box::new(Wrapper(io));

        let found = rustls_error(outer.as_ref());

        assert!(
            matches!(found, Some(rustls::Error::InvalidCertificate(_))),
            "{found:?}"
        );
    }

    #[test]
    fn a_chain_without_rustls_has_none() {
        let io = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let outer: Box<dyn Error + Send + Sync> = Box::new(Wrapper(io));

        assert!(rustls_error(outer.as_ref()).is_none());
    }

    #[test]
    fn the_production_bounds_are_adr_0018s() {
        assert_eq!(
            Timeouts::MICROSOFT,
            Timeouts {
                connect: Duration::from_secs(5),
                request: Duration::from_secs(15),
                call: Duration::from_secs(20),
                session: Duration::from_secs(60),
            }
        );
    }
}
