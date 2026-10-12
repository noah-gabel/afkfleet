//! The HTTP layer under the session (ADR-0017): a crate-private
//! [`Transport`] seam, and [`ReqwestTransport`], the only one outside tests.
//!
//! The seam lets the session's unit tests script answers and hold them
//! behind a `Notify`, so each interleaving of refreshes, retries and ends is
//! forced, not hoped for. `Client` itself always runs on reqwest with the
//! crate's constants.
//!
//! reqwest is built with no redirects (a 3xx comes back as it is), no
//! proxies (environment variables can't route the traffic elsewhere), the
//! connect and whole-request timeouts, and the [`Trust`] the config names.
//! Each response body is read up to
//! [`MAX_RESPONSE_BYTES`](crate::MAX_RESPONSE_BYTES): a larger
//! `Content-Length` is refused before reading, and a body that grows past it
//! is cut off and its connection dropped.

use core::error::Error;
use core::fmt;
use core::future::Future;
use core::time::Duration;

use reqwest::header::{CONTENT_TYPE, RETRY_AFTER};
use reqwest::{Method, Url};
use secrecy::{ExposeSecret as _, SecretString};

use crate::error::{BuildError, Cause};
use crate::trust::Trust;
use crate::{CONNECT_TIMEOUT, MAX_RESPONSE_BYTES, REQUEST_TIMEOUT, STORE_TIMEOUT};

/// The bounds of every call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Timeouts {
    /// DNS, TCP and the TLS handshake.
    pub(crate) connect: Duration,
    /// A whole request, its response body included.
    pub(crate) request: Duration,
    /// One session-store call.
    pub(crate) store: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: CONNECT_TIMEOUT,
            request: REQUEST_TIMEOUT,
            store: STORE_TIMEOUT,
        }
    }
}

/// A request ready to send.
pub(crate) struct Outgoing {
    pub(crate) method: Method,
    pub(crate) url: Url,
    /// The `Authorization: Bearer` token, if the route takes one.
    pub(crate) bearer: Option<SecretString>,
    /// A JSON body, if the route takes one.
    pub(crate) body: Option<Vec<u8>>,
}

/// The method and the path only: the bearer and the body are secrets.
impl fmt::Debug for Outgoing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Outgoing")
            .field("method", &self.method)
            .field("path", &self.url.path())
            .finish_non_exhaustive()
    }
}

/// A response, its body read.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Incoming {
    pub(crate) status: u16,
    /// `Retry-After` in whole seconds, if it's there and readable.
    pub(crate) retry_after: Option<Duration>,
    pub(crate) body: Vec<u8>,
}

/// The status and the body's length only: a body may hold tokens.
impl fmt::Debug for Incoming {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Incoming")
            .field("status", &self.status)
            .field("retry_after", &self.retry_after)
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

/// Why no response arrived.
#[derive(Debug)]
pub(crate) enum TransportError {
    Unreachable(Option<Cause>),
    Certificate(Cause),
    TimedOut(Option<Cause>),
    TooLarge,
}

/// Sends a request and reads its response.
pub(crate) trait Transport: Send + Sync + 'static {
    fn send(
        &self,
        request: Outgoing,
    ) -> impl Future<Output = Result<Incoming, TransportError>> + Send;
}

/// The transport over reqwest; see the [module docs](self).
#[derive(Debug)]
pub(crate) struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// A transport that trusts `trust`, bounded by `timeouts`.
    pub(crate) fn new(trust: &Trust, timeouts: Timeouts) -> Result<Self, BuildError> {
        let builder = reqwest::Client::builder()
            .connect_timeout(timeouts.connect)
            .timeout(timeouts.request)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();
        let builder = match trust {
            Trust::System => builder,
            Trust::CustomCa(ca) => builder.tls_certs_only(ca.certificates()),
        };
        let client = builder
            .build()
            .map_err(|error| BuildError(Box::new(error.without_url())))?;
        Ok(Self { client })
    }
}

impl Transport for ReqwestTransport {
    async fn send(&self, request: Outgoing) -> Result<Incoming, TransportError> {
        let mut builder = self.client.request(request.method, request.url);
        if let Some(token) = &request.bearer {
            builder = builder.bearer_auth(token.expose_secret());
        }
        if let Some(body) = request.body {
            builder = builder.header(CONTENT_TYPE, "application/json").body(body);
        }
        let mut response = builder.send().await.map_err(classify)?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|text| text.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        if response
            .content_length()
            .is_some_and(|length| length > MAX_BYTES)
        {
            return Err(TransportError::TooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(classify)? {
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                // Dropping the response drops its connection, so the rest
                // is never read.
                return Err(TransportError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Incoming {
            status,
            retry_after,
            body,
        })
    }
}

/// [`MAX_RESPONSE_BYTES`] as a `Content-Length`.
const MAX_BYTES: u64 = MAX_RESPONSE_BYTES as u64;

/// What a reqwest error means for the caller, kept without its URL: a
/// timeout, a refused certificate (rustls's `InvalidCertificate` anywhere in
/// its chain), or a server that can't be reached.
fn classify(error: reqwest::Error) -> TransportError {
    let refused =
        rustls_error(&error).is_some_and(|tls| matches!(tls, rustls::Error::InvalidCertificate(_)));
    let timed_out = error.is_timeout();
    let cause: Cause = Box::new(error.without_url());
    if timed_out {
        TransportError::TimedOut(Some(cause))
    } else if refused {
        TransportError::Certificate(cause)
    } else {
        TransportError::Unreachable(Some(cause))
    }
}

/// The rustls error in `error`'s chain, also inside an `io::Error`, whose
/// own `source()` skips the error it wraps.
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
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::net::{SocketAddr, TcpListener};
    use std::thread;

    use super::*;

    /// How long a test's local server waits for the client before giving up.
    const SOCKET_WAIT: Duration = Duration::from_secs(5);

    /// A server on a thread that answers one request with `answer`, the raw
    /// bytes of a whole HTTP response, after reading the request's head.
    fn answering(answer: Vec<u8>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(SOCKET_WAIT)).unwrap();
            stream.set_write_timeout(Some(SOCKET_WAIT)).unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|read| read > 2) {
                line.clear();
            }
            let _ = stream.write_all(&answer);
        });
        address
    }

    /// A server on a thread that accepts one connection and never answers:
    /// it reads until the client gives up, or [`SOCKET_WAIT`] passes.
    fn silent() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(SOCKET_WAIT)).unwrap();
            let mut buffer = [0; 1024];
            while std::io::Read::read(&mut stream, &mut buffer).is_ok_and(|read| read > 0) {}
        });
        address
    }

    fn get(address: SocketAddr) -> Outgoing {
        Outgoing {
            method: Method::GET,
            url: Url::parse(&format!("http://{address}/health/live")).unwrap(),
            bearer: None,
            body: None,
        }
    }

    fn transport(timeouts: Timeouts) -> ReqwestTransport {
        ReqwestTransport::new(&Trust::System, timeouts).unwrap()
    }

    fn short() -> Timeouts {
        Timeouts {
            connect: Duration::from_millis(100),
            request: Duration::from_millis(100),
            store: Duration::from_millis(100),
        }
    }

    #[tokio::test]
    async fn a_response_is_read_with_its_retry_after() {
        let address = answering(
            b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 7\r\nContent-Length: 2\r\n\r\n{}"
                .to_vec(),
        );

        let incoming = transport(Timeouts::default())
            .send(get(address))
            .await
            .unwrap();

        assert_eq!(incoming.status, 429);
        assert_eq!(incoming.retry_after, Some(Duration::from_secs(7)));
        assert_eq!(incoming.body, b"{}");
    }

    #[tokio::test]
    async fn a_redirect_is_never_followed() {
        let address = answering(
            b"HTTP/1.1 302 Found\r\nLocation: http://192.0.2.10/\r\nContent-Length: 0\r\n\r\n"
                .to_vec(),
        );

        let incoming = transport(Timeouts::default())
            .send(get(address))
            .await
            .unwrap();

        assert_eq!(incoming.status, 302);
    }

    #[tokio::test]
    async fn a_body_announced_larger_than_the_cap_is_refused_before_reading() {
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            MAX_RESPONSE_BYTES + 1
        );
        let mut answer = head.into_bytes();
        answer.extend(vec![b'x'; MAX_RESPONSE_BYTES + 1]);
        let address = answering(answer);

        let error = transport(Timeouts::default())
            .send(get(address))
            .await
            .unwrap_err();

        assert!(matches!(error, TransportError::TooLarge), "{error:?}");
    }

    #[tokio::test]
    async fn a_body_growing_past_the_cap_is_cut_off() {
        let mut answer = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
        answer.extend(vec![b'x'; MAX_RESPONSE_BYTES + 1]);
        let address = answering(answer);

        let error = transport(Timeouts::default())
            .send(get(address))
            .await
            .unwrap_err();

        assert!(matches!(error, TransportError::TooLarge), "{error:?}");
    }

    #[tokio::test]
    async fn a_body_of_exactly_the_cap_is_read() {
        let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {MAX_RESPONSE_BYTES}\r\n\r\n");
        let mut answer = head.into_bytes();
        answer.extend(vec![b'x'; MAX_RESPONSE_BYTES]);
        let address = answering(answer);

        let incoming = transport(Timeouts::default())
            .send(get(address))
            .await
            .unwrap();

        assert_eq!(incoming.body.len(), MAX_RESPONSE_BYTES);
    }

    #[tokio::test]
    async fn a_server_that_never_answers_times_out() {
        let address = silent();

        let error = transport(short()).send(get(address)).await.unwrap_err();

        assert!(matches!(error, TransportError::TimedOut(_)), "{error:?}");
    }

    #[tokio::test]
    async fn a_closed_port_is_unreachable() {
        let address = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();

        let error = transport(Timeouts::default())
            .send(get(address))
            .await
            .unwrap_err();

        assert!(matches!(error, TransportError::Unreachable(_)), "{error:?}");
    }

    #[test]
    fn a_rustls_error_is_found_inside_an_io_error() {
        let inner = rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer);
        let io = std::io::Error::new(std::io::ErrorKind::InvalidData, inner);
        let outer: Box<dyn Error + Send + Sync> = Box::new(Wrapper(io));

        let found = rustls_error(outer.as_ref());

        assert!(
            matches!(found, Some(rustls::Error::InvalidCertificate(_))),
            "{found:?}"
        );
    }

    #[test]
    fn debug_shows_no_bearer_and_no_body() {
        let outgoing = Outgoing {
            method: Method::POST,
            url: Url::parse("https://example.com/api/v1/auth/refresh").unwrap(),
            bearer: Some(SecretString::from("afk_at_secret")),
            body: Some(b"{\"refresh_token\":\"afk_rt_secret\"}".to_vec()),
        };
        let incoming = Incoming {
            status: 200,
            retry_after: None,
            body: b"{\"access_token\":\"afk_at_secret\"}".to_vec(),
        };

        let shown = format!("{outgoing:?} {incoming:?}");

        assert!(!shown.contains("secret"), "{shown}");
        assert!(shown.contains("/api/v1/auth/refresh"), "{shown}");
    }

    /// An error whose source is `0`, like hyper's around an IO error.
    #[derive(Debug)]
    struct Wrapper(std::io::Error);

    impl fmt::Display for Wrapper {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("connection failed")
        }
    }

    impl Error for Wrapper {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(&self.0)
        }
    }
}
