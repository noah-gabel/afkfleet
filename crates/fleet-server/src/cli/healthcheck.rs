//! `afkfleet-server healthcheck` (Plan.md P6.11, ADR-0015): asks a running
//! server's `GET /health/ready`, for Docker's HEALTHCHECK.
//!
//! - **Where:** `[http] bind`, except that an unspecified address
//!   (`0.0.0.0` or `::`, as the container config sets) is asked on loopback
//!   (`127.0.0.1` or `::1`) on the same port: connecting to `0.0.0.0` only
//!   happens to work on Linux and fails on Windows.
//! - **How:** one HTTP/1.1 request over a plain TCP connection
//!   (`Connection: close`), with one overall timeout of [`TIMEOUT`], reading
//!   at most [`MAX_RESPONSE_BYTES`]. There's no TLS: the probe never leaves
//!   the host, and Caddy terminates TLS in front of the server.
//! - **Healthy** means only one thing: `HTTP/1.1 200` with an empty body,
//!   which is all `GET /health/ready` ever answers when the database answers.
//!
//! The command prints one line, healthy or not, and exits 0 or 1 (Docker
//! reserves 2). The server's own tests reuse [`get`] as their HTTP client.

use core::fmt;
use core::time::Duration;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

use crate::config::HttpConfig;

/// The path the healthcheck asks.
pub const PATH: &str = "/health/ready";

/// How long the whole exchange may take: connecting, sending and reading.
/// It stays below the 5 s Docker gives a check (P12).
pub const TIMEOUT: Duration = Duration::from_secs(3);

/// The largest answer [`get`] reads. `GET /health/ready` answers with a few
/// headers and at most a small error body.
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024;

/// The address the healthcheck asks for a server bound to `bind`: the same
/// address, or loopback on the same port when `bind` is unspecified.
#[must_use]
pub fn probe_address(bind: SocketAddr) -> SocketAddr {
    let ip = match bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    SocketAddr::new(ip, bind.port())
}

/// What a server answered: its status and its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// The status code.
    pub status: u16,
    /// The body, as sent: no transfer coding is undone.
    pub body: Vec<u8>,
}

/// Why [`get`] got no answer it could read.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// The path isn't an absolute path of visible ASCII characters.
    #[error("the path must start with / and hold only visible ASCII characters")]
    Path,
    /// Nothing accepted the connection.
    #[error("can't connect to {address}: {error}")]
    Connect {
        /// The address asked.
        address: SocketAddr,
        /// The OS's error, shown in the message.
        error: io::Error,
    },
    /// The connection failed while sending or reading.
    #[error("the connection to {address} failed: {error}")]
    Io {
        /// The address asked.
        address: SocketAddr,
        /// The OS's error, shown in the message.
        error: io::Error,
    },
    /// No complete answer came within the timeout.
    #[error("{address} didn't answer within {} ms", .timeout.as_millis())]
    TimedOut {
        /// The address asked.
        address: SocketAddr,
        /// The timeout.
        timeout: Duration,
    },
    /// The answer is longer than [`MAX_RESPONSE_BYTES`].
    #[error("{address}'s answer is longer than {MAX_RESPONSE_BYTES} bytes")]
    TooLarge {
        /// The address asked.
        address: SocketAddr,
    },
    /// The answer doesn't start with an HTTP/1.1 status line and headers.
    #[error("{address}'s answer isn't HTTP/1.1")]
    Malformed {
        /// The address asked.
        address: SocketAddr,
    },
}

/// Sends `GET path` to `address` and reads the whole answer, all within
/// `timeout`.
///
/// # Errors
/// [`ProbeError`]: a bad path, no connection, a failed or slow exchange, or
/// an answer that's too long or isn't HTTP/1.1.
pub async fn get(
    address: SocketAddr,
    path: &str,
    timeout: Duration,
) -> Result<HttpResponse, ProbeError> {
    if !path.starts_with('/') || !path.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(ProbeError::Path);
    }
    tokio::time::timeout(timeout, exchange(address, path))
        .await
        .map_err(|_elapsed| ProbeError::TimedOut { address, timeout })?
}

/// The request and its answer, without a timeout of their own: [`get`]
/// bounds them.
async fn exchange(address: SocketAddr, path: &str) -> Result<HttpResponse, ProbeError> {
    let io = |error| ProbeError::Io { address, error };
    let mut stream = TcpStream::connect(address)
        .await
        .map_err(|error| ProbeError::Connect { address, error })?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.map_err(io)?;
    let mut answer = Vec::new();
    // One byte past the cap, to tell a full answer from a cut one.
    let limit = u64::try_from(MAX_RESPONSE_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    (&mut stream)
        .take(limit)
        .read_to_end(&mut answer)
        .await
        .map_err(io)?;
    if answer.len() > MAX_RESPONSE_BYTES {
        return Err(ProbeError::TooLarge { address });
    }
    parse(&answer).ok_or(ProbeError::Malformed { address })
}

/// The status and body of an HTTP/1.1 answer, or `None` if it has no
/// `HTTP/1.1 NNN` status line or no end of headers.
fn parse(answer: &[u8]) -> Option<HttpResponse> {
    let end = answer.windows(4).position(|window| window == b"\r\n\r\n")?;
    let head = answer.get(..end)?;
    let body = answer.get(end + 4..)?.to_vec();
    let line_end = head
        .windows(2)
        .position(|window| window == b"\r\n")
        .unwrap_or(head.len());
    let status_line = head.get(..line_end)?;
    let rest = status_line.strip_prefix(b"HTTP/1.1 ")?;
    let digits = rest.get(..3)?;
    if !digits.iter().all(u8::is_ascii_digit) || rest.get(3).is_some_and(|byte| *byte != b' ') {
        return None;
    }
    let status = core::str::from_utf8(digits).ok()?.parse().ok()?;
    Some(HttpResponse { status, body })
}

/// What the healthcheck found.
#[derive(Debug)]
pub enum Health {
    /// `GET /health/ready` answered 200 with an empty body.
    Healthy {
        /// The address asked.
        address: SocketAddr,
    },
    /// It answered another status.
    Unready {
        /// The address asked.
        address: SocketAddr,
        /// The status it answered.
        status: u16,
    },
    /// It answered 200 with a body, which `/health/ready` never does.
    Unexpected {
        /// The address asked.
        address: SocketAddr,
    },
    /// No answer could be read.
    Unreachable(ProbeError),
}

impl Health {
    /// Whether the server is ready.
    #[must_use]
    pub const fn is_healthy(&self) -> bool {
        matches!(self, Self::Healthy { .. })
    }

    /// The process's exit code: 0 when healthy, 1 otherwise.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        if self.is_healthy() { 0 } else { 1 }
    }
}

impl fmt::Display for Health {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Healthy { address } => write!(f, "healthy: {PATH} on {address} answered 200"),
            Self::Unready { address, status } => {
                write!(f, "unhealthy: {PATH} on {address} answered {status}")
            }
            Self::Unexpected { address } => {
                write!(f, "unhealthy: {PATH} on {address} answered 200 with a body")
            }
            Self::Unreachable(error) => write!(f, "unhealthy: {error}"),
        }
    }
}

/// Asks the server `http` configures whether it's ready.
pub async fn check(http: &HttpConfig) -> Health {
    let address = probe_address(http.bind);
    match get(address, PATH, TIMEOUT).await {
        Ok(HttpResponse { status: 200, body }) if body.is_empty() => Health::Healthy { address },
        Ok(HttpResponse { status: 200, .. }) => Health::Unexpected { address },
        Ok(HttpResponse { status, .. }) => Health::Unready { address, status },
        Err(error) => Health::Unreachable(error),
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use tokio::net::TcpListener;

    use super::*;

    #[rstest]
    #[case::unspecified_v4("0.0.0.0:8080", "127.0.0.1:8080")]
    #[case::unspecified_v6("[::]:8443", "[::1]:8443")]
    #[case::loopback_v4("127.0.0.1:8080", "127.0.0.1:8080")]
    #[case::loopback_v6("[::1]:8080", "[::1]:8080")]
    #[case::a_specific_address("192.0.2.10:9000", "192.0.2.10:9000")]
    fn the_probe_asks_loopback_for_an_unspecified_bind(#[case] bind: &str, #[case] probed: &str) {
        let bind: SocketAddr = bind.parse().unwrap();

        assert_eq!(probe_address(bind), probed.parse::<SocketAddr>().unwrap());
    }

    #[rstest]
    #[case::empty_200(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n", 200, b"")]
    #[case::a_body(
        b"HTTP/1.1 503 Service Unavailable\r\nretry-after: 1\r\n\r\n{}",
        503,
        b"{}"
    )]
    #[case::no_reason(b"HTTP/1.1 204\r\n\r\n", 204, b"")]
    #[case::no_headers(b"HTTP/1.1 200 OK\r\n\r\n", 200, b"")]
    fn an_http_1_1_answer_parses_into_its_status_and_body(
        #[case] answer: &[u8],
        #[case] status: u16,
        #[case] body: &[u8],
    ) {
        assert_eq!(
            parse(answer),
            Some(HttpResponse {
                status,
                body: body.to_vec()
            })
        );
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::not_http(b"SSH-2.0-OpenSSH\r\n\r\n")]
    #[case::http_1_0(b"HTTP/1.0 200 OK\r\n\r\n")]
    #[case::no_end_of_headers(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n")]
    #[case::short_status(b"HTTP/1.1 20\r\n\r\n")]
    #[case::long_status(b"HTTP/1.1 2000 OK\r\n\r\n")]
    #[case::letters(b"HTTP/1.1 2x0 OK\r\n\r\n")]
    fn anything_else_doesnt_parse(#[case] answer: &[u8]) {
        assert_eq!(parse(answer), None);
    }

    /// A server on a fresh loopback port that answers one connection with
    /// `answer`, after reading the request, then closes it.
    async fn answering(answer: &'static [u8]) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await.unwrap();
            stream.write_all(answer).await.unwrap();
        });
        address
    }

    /// A loopback port nothing listens on.
    async fn closed_port() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap()
    }

    #[tokio::test]
    async fn get_reads_the_status_and_body() {
        let address = answering(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok").await;

        let answer = get(address, PATH, TIMEOUT).await.unwrap();

        assert_eq!(
            answer,
            HttpResponse {
                status: 200,
                body: b"ok".to_vec()
            }
        );
    }

    #[tokio::test]
    async fn get_sends_one_closing_get_request_naming_the_host() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 256];
            while !request.ends_with(b"\r\n\r\n") {
                let read = stream.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..read]);
            }
            stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
            String::from_utf8(request).unwrap()
        });

        get(address, "/health/ready", TIMEOUT).await.unwrap();

        assert_eq!(
            server.await.unwrap(),
            format!("GET /health/ready HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
        );
    }

    /// Long enough for Windows, which takes about 2 s to report that nothing
    /// listens on a loopback port.
    const PATIENT: Duration = Duration::from_secs(30);

    #[tokio::test]
    async fn nothing_listening_is_a_connect_error() {
        let address = closed_port().await;

        let error = get(address, PATH, PATIENT).await.unwrap_err();

        assert!(
            matches!(error, ProbeError::Connect { address: a, .. } if a == address),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_server_that_never_answers_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let _server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            core::future::pending::<()>().await;
            drop(stream);
        });
        let timeout = Duration::from_millis(300);

        let error = get(address, PATH, timeout).await.unwrap_err();

        assert!(
            matches!(error, ProbeError::TimedOut { address: a, timeout: t } if a == address && t == timeout),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            format!("{address} didn't answer within 300 ms")
        );
    }

    #[tokio::test]
    async fn an_answer_past_the_cap_is_too_large() {
        static HUGE: [u8; MAX_RESPONSE_BYTES + 1] = [b'x'; MAX_RESPONSE_BYTES + 1];
        let address = answering(&HUGE).await;

        let error = get(address, PATH, TIMEOUT).await.unwrap_err();

        assert!(matches!(error, ProbeError::TooLarge { .. }), "{error:?}");
    }

    #[tokio::test]
    async fn an_answer_at_the_cap_is_read() {
        static AT_CAP: [u8; MAX_RESPONSE_BYTES] = {
            let mut answer = [b'x'; MAX_RESPONSE_BYTES];
            let head = b"HTTP/1.1 200 OK\r\n\r\n";
            let mut index = 0;
            while index < head.len() {
                answer[index] = head[index];
                index += 1;
            }
            answer
        };
        let address = answering(&AT_CAP).await;

        let answer = get(address, PATH, TIMEOUT).await.unwrap();

        assert_eq!(answer.status, 200);
    }

    #[tokio::test]
    async fn an_answer_that_isnt_http_is_malformed() {
        let address = answering(b"220 smtp.example.com ESMTP\r\n").await;

        let error = get(address, PATH, TIMEOUT).await.unwrap_err();

        assert!(matches!(error, ProbeError::Malformed { .. }), "{error:?}");
    }

    #[rstest]
    #[case::relative("health/ready")]
    #[case::a_space("/health ready")]
    #[case::a_line_break("/health\r\nX-Injected: 1")]
    #[case::not_ascii("/gesundheit/\u{e9}")]
    #[tokio::test]
    async fn a_path_that_could_break_the_request_is_refused(#[case] path: &str) {
        let address = closed_port().await;

        let error = get(address, path, TIMEOUT).await.unwrap_err();

        assert!(matches!(error, ProbeError::Path), "{error:?}");
    }

    /// The HTTP config of a server bound to `address`.
    fn bound_to(address: SocketAddr) -> HttpConfig {
        HttpConfig {
            bind: address,
            request_timeout: Duration::from_secs(15),
            max_body_bytes: 65_536,
        }
    }

    #[tokio::test]
    async fn an_empty_200_is_healthy() {
        let address = answering(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n").await;

        let health = check(&bound_to(address)).await;

        assert!(health.is_healthy());
        assert_eq!(health.exit_code(), 0);
        assert_eq!(
            health.to_string(),
            format!("healthy: /health/ready on {address} answered 200")
        );
    }

    #[tokio::test]
    async fn another_status_is_unhealthy() {
        let address = answering(b"HTTP/1.1 503 Service Unavailable\r\n\r\n{}").await;

        let health = check(&bound_to(address)).await;

        assert_eq!(health.exit_code(), 1);
        assert_eq!(
            health.to_string(),
            format!("unhealthy: /health/ready on {address} answered 503")
        );
    }

    #[tokio::test]
    async fn a_200_with_a_body_is_unhealthy() {
        let address = answering(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok").await;

        let health = check(&bound_to(address)).await;

        assert_eq!(health.exit_code(), 1);
        assert_eq!(
            health.to_string(),
            format!("unhealthy: /health/ready on {address} answered 200 with a body")
        );
    }

    #[tokio::test]
    async fn no_answer_is_unhealthy_with_the_reason() {
        let address = closed_port().await;

        let health = check(&bound_to(address)).await;

        // Refused on Linux; on Windows the refusal can take about 2 s, so the
        // 3 s timeout may come first. Both name the address.
        assert_eq!(health.exit_code(), 1);
        assert!(matches!(health, Health::Unreachable(_)), "{health:?}");
        assert!(health.to_string().starts_with("unhealthy: "), "{health}");
        assert!(
            health.to_string().contains(&address.to_string()),
            "{health}"
        );
    }
}
