//! The HTTP layer under the session (ADR-0017): a crate-private
//! [`Transport`] seam, and [`ReqwestTransport`], the only one outside tests.
//!
//! The seam lets the session's unit tests script answers and hold them
//! behind a `Notify`, so each interleaving of refreshes, retries and ends is
//! forced, not hoped for. `Client` itself always runs on reqwest with the
//! crate's constants.
//!
//! reqwest is built with no redirects (a 3xx comes back as it is), no
//! proxies (environment variables can't route the traffic elsewhere),
//! HTTP/1.1 only (the live events socket is an HTTP/1.1 upgrade, which h2
//! doesn't have; ADR-0020, R75), the connect and whole-request timeouts, and
//! the [`Trust`] the config names. The live events socket rides on this same
//! client ([`Transport::upgrade`]), so its TLS is exactly reqwest's (U25).
//! Each response body is read up to
//! [`MAX_RESPONSE_BYTES`](crate::MAX_RESPONSE_BYTES): a larger
//! `Content-Length` is refused before reading, and a body that grows past it
//! is cut off and its connection dropped.

use core::error::Error;
use core::fmt;
use core::future::Future;
use core::time::Duration;

use reqwest::header::{
    CONNECTION, CONTENT_TYPE, HeaderMap, RETRY_AFTER, SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_VERSION,
    UPGRADE,
};
use reqwest::{Method, StatusCode, Url};
use secrecy::{ExposeSecret as _, SecretString};
use tokio::io::{AsyncRead, AsyncWrite};

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

/// A WebSocket upgrade ready to send (ADR-0020, U25): `GET url` with the
/// session's bearer and the `Sec-WebSocket-Key` it offers. The URL holds the
/// live events ticket, so it's never shown.
pub(crate) struct UpgradeRequest {
    pub(crate) url: Url,
    pub(crate) bearer: SecretString,
    pub(crate) key: String,
}

/// The path only: the URL's query holds the ticket, the bearer is a secret.
impl fmt::Debug for UpgradeRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UpgradeRequest")
            .field("path", &self.url.path())
            .finish_non_exhaustive()
    }
}

/// How an upgrade was answered.
pub(crate) enum Upgrade<S> {
    /// 101: the server switched protocols. Its headers, for the caller to
    /// check, and the connection, which carries the WebSocket from here on.
    Switched { headers: HeaderMap, socket: S },
    /// Any other answer, its body read as [`Transport::send`] reads one.
    Refused(Incoming),
}

/// Which answer only: the headers may name cookies, the socket is a stream.
impl<S> fmt::Debug for Upgrade<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Switched { .. } => f.write_str("Switched"),
            Self::Refused(incoming) => f.debug_tuple("Refused").field(incoming).finish(),
        }
    }
}

/// Sends a request and reads its response, or upgrades a connection to a
/// WebSocket.
pub(crate) trait Transport: Send + Sync + 'static {
    /// The connection an upgrade hands over.
    type Socket: AsyncRead + AsyncWrite + Unpin + Send + 'static;

    fn send(
        &self,
        request: Outgoing,
    ) -> impl Future<Output = Result<Incoming, TransportError>> + Send;

    /// Sends `request` as an HTTP/1.1 upgrade to `websocket`: a 101 hands
    /// over its connection; any other answer is read as `send` reads one.
    fn upgrade(
        &self,
        request: UpgradeRequest,
    ) -> impl Future<Output = Result<Upgrade<Self::Socket>, TransportError>> + Send;
}

#[cfg(test)]
pub(crate) mod scripted;

/// The transport over reqwest; see the [module docs](self).
#[derive(Debug)]
pub(crate) struct ReqwestTransport {
    client: reqwest::Client,
    timeouts: Timeouts,
}

impl ReqwestTransport {
    /// A transport that trusts `trust`, bounded by `timeouts`.
    pub(crate) fn new(trust: &Trust, timeouts: Timeouts) -> Result<Self, BuildError> {
        let client = builder(trust, timeouts)
            .build()
            .map_err(|error| BuildError(Box::new(error.without_url())))?;
        Ok(Self { client, timeouts })
    }
}

/// The one reqwest client builder: `timeouts`, no redirects, no proxy,
/// HTTP/1.1 only, since h2 has no upgrade for the live events socket
/// (ADR-0020, R75), and the certificates `trust` names.
fn builder(trust: &Trust, timeouts: Timeouts) -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder()
        .connect_timeout(timeouts.connect)
        .timeout(timeouts.request)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .http1_only();
    match trust {
        Trust::System => builder,
        Trust::CustomCa(ca) => builder.tls_certs_only(ca.certificates()),
    }
}

impl ReqwestTransport {
    /// `request` as reqwest sends it: `GET` with the bearer, which
    /// `bearer_auth` marks sensitive, and the upgrade's headers.
    fn upgrading(&self, request: UpgradeRequest) -> reqwest::RequestBuilder {
        self.client
            .get(request.url)
            .bearer_auth(request.bearer.expose_secret())
            .header(CONNECTION, "Upgrade")
            .header(UPGRADE, "websocket")
            .header(SEC_WEBSOCKET_VERSION, "13")
            .header(SEC_WEBSOCKET_KEY, request.key)
    }
}

impl Transport for ReqwestTransport {
    type Socket = reqwest::Upgraded;

    async fn send(&self, request: Outgoing) -> Result<Incoming, TransportError> {
        let mut builder = self.client.request(request.method, request.url);
        if let Some(token) = &request.bearer {
            builder = builder.bearer_auth(token.expose_secret());
        }
        if let Some(body) = request.body {
            builder = builder.header(CONTENT_TYPE, "application/json").body(body);
        }
        let response = builder.send().await.map_err(classify)?;
        incoming(response).await
    }

    async fn upgrade(
        &self,
        request: UpgradeRequest,
    ) -> Result<Upgrade<reqwest::Upgraded>, TransportError> {
        let response = self.upgrading(request).send().await.map_err(classify)?;
        if response.status() != StatusCode::SWITCHING_PROTOCOLS {
            return incoming(response).await.map(Upgrade::Refused);
        }
        let headers = response.headers().clone();
        // The 101 is in, so the connection is handed over at once; bounded
        // anyway, as every network await is.
        let socket = tokio::time::timeout(self.timeouts.request, response.upgrade())
            .await
            .map_err(|_elapsed| TransportError::TimedOut(None))?
            .map_err(classify)?;
        Ok(Upgrade::Switched { headers, socket })
    }
}

/// `response` with its body read, up to [`MAX_RESPONSE_BYTES`].
async fn incoming(mut response: reqwest::Response) -> Result<Incoming, TransportError> {
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
            // Dropping the response drops its connection, so the rest is
            // never read.
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
    fn the_client_speaks_http_1_1_only_so_the_upgrade_is_never_h2() {
        // reqwest's builder shows the preference only when it's HTTP/1 alone.
        let shown = format!("{:?}", builder(&Trust::System, short()));

        assert!(shown.contains("http1_only: true"), "{shown}");
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

    fn upgrade_to(address: SocketAddr, key: &str) -> UpgradeRequest {
        UpgradeRequest {
            url: Url::parse(&format!(
                "http://{address}/api/v1/events?ticket=afk_wt_TICKET"
            ))
            .unwrap(),
            bearer: SecretString::from("afk_at_ACCESS"),
            key: key.to_owned(),
        }
    }

    #[test]
    fn an_upgrade_is_a_get_with_the_ticket_a_sensitive_bearer_and_websocket_headers() {
        let address: SocketAddr = "127.0.0.1:8080".parse().unwrap();

        let built = transport(Timeouts::default())
            .upgrading(upgrade_to(address, "a2V5a2V5a2V5a2V5a2V5aw=="))
            .build()
            .unwrap();

        assert_eq!(built.method(), Method::GET);
        assert_eq!(
            built.url().as_str(),
            "http://127.0.0.1:8080/api/v1/events?ticket=afk_wt_TICKET"
        );
        let headers = built.headers();
        let authorization = &headers[reqwest::header::AUTHORIZATION];
        assert_eq!(authorization, "Bearer afk_at_ACCESS");
        assert!(authorization.is_sensitive());
        assert_eq!(headers[CONNECTION], "Upgrade");
        assert_eq!(headers[UPGRADE], "websocket");
        assert_eq!(headers[SEC_WEBSOCKET_VERSION], "13");
        assert_eq!(headers[SEC_WEBSOCKET_KEY], "a2V5a2V5a2V5a2V5a2V5aw==");
        assert!(!format!("{:?}", upgrade_to(address, "k")).contains("afk_"));
    }

    #[tokio::test]
    async fn an_upgrade_answered_without_a_101_is_read_as_a_refusal() {
        let address = answering(
            b"HTTP/1.1 404 Not Found\r\nRetry-After: 3\r\nContent-Length: 2\r\n\r\n{}".to_vec(),
        );

        let answer = transport(Timeouts::default())
            .upgrade(upgrade_to(address, "k"))
            .await
            .unwrap();

        let Upgrade::Refused(incoming) = answer else {
            panic!("not refused: {answer:?}");
        };
        assert_eq!(incoming.status, 404);
        assert_eq!(incoming.retry_after, Some(Duration::from_secs(3)));
        assert_eq!(incoming.body, b"{}");
    }

    #[tokio::test]
    async fn a_101_hands_over_its_connection_with_its_headers() {
        use tokio::io::AsyncReadExt as _;
        let address = answering(
            b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: abc\r\n\r\nhello"
                .to_vec(),
        );

        let answer = transport(Timeouts::default())
            .upgrade(upgrade_to(address, "k"))
            .await
            .unwrap();

        let Upgrade::Switched {
            headers,
            mut socket,
        } = answer
        else {
            panic!("not switched: {answer:?}");
        };
        assert_eq!(headers[SEC_WEBSOCKET_ACCEPT_TEST], "abc");
        let mut read = [0; 5];
        tokio::time::timeout(SOCKET_WAIT, socket.read_exact(&mut read))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&read, b"hello");
    }

    /// The `Sec-WebSocket-Accept` header, for the 101 test.
    const SEC_WEBSOCKET_ACCEPT_TEST: reqwest::header::HeaderName =
        reqwest::header::SEC_WEBSOCKET_ACCEPT;

    /// A WebSocket server pinging every 50 ms, twelve times, then closing:
    /// the upgraded connection lives past five times the client's 100 ms
    /// request timeout, so reqwest's whole-request deadline never cuts it.
    #[tokio::test]
    async fn an_upgraded_connection_outlives_the_request_timeout() {
        use futures_util::{SinkExt as _, StreamExt as _};
        use tokio_tungstenite::tungstenite::{Bytes, Message};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut every = tokio::time::interval(Duration::from_millis(50));
            for _ in 0..12 {
                every.tick().await;
                socket.send(Message::Ping(Bytes::new())).await.unwrap();
            }
            socket.close(None).await.unwrap();
            // The client's pongs and its close: a socket dropped unread
            // resets the connection on Windows.
            while socket.next().await.is_some() {}
        });
        let key = tokio_tungstenite::tungstenite::handshake::client::generate_key();
        let started = tokio::time::Instant::now();

        let answer = transport(short())
            .upgrade(upgrade_to(address, &key))
            .await
            .unwrap();
        let Upgrade::Switched { headers, socket } = answer else {
            panic!("not switched: {answer:?}");
        };
        crate::events::upgrade::checked(&headers, &key).unwrap();
        let mut socket = crate::events::upgrade::framed(socket).await;
        let mut pings = 0;
        let closed = tokio::time::timeout(SOCKET_WAIT, async {
            while let Some(message) = socket.next().await {
                match message.unwrap() {
                    Message::Ping(_) => pings += 1,
                    Message::Close(_) => return true,
                    other => panic!("not a ping: {other:?}"),
                }
            }
            false
        })
        .await
        .unwrap();

        assert!(closed, "the server's close came");
        assert_eq!(pings, 12);
        assert!(started.elapsed() >= Duration::from_millis(500));
        drop(socket);
        tokio::time::timeout(SOCKET_WAIT, server)
            .await
            .unwrap()
            .unwrap();
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
