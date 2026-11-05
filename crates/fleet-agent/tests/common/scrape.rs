//! One `GET /metrics` over a real socket, for the tests of the agent's
//! metrics endpoint in the run (Plan.md P12.4; ADR-0022, S9).

use core::net::SocketAddr;
use core::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

/// A bound for each step over the real socket.
pub(crate) const STEP: Duration = Duration::from_secs(10);

/// `GET /metrics` at `address`: the whole answer, within [`STEP`].
pub(crate) async fn scrape(address: SocketAddr) -> String {
    tokio::time::timeout(STEP, async {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(b"GET /metrics HTTP/1.1\r\nHost: agent\r\n\r\n")
            .await
            .unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).await.unwrap();
        answer
    })
    .await
    .expect("the endpoint answers")
}

/// Whether nothing listens at `address` any more: a connection is refused
/// within [`STEP`].
pub(crate) async fn is_closed(address: SocketAddr) -> bool {
    tokio::time::timeout(STEP, TcpStream::connect(address))
        .await
        .expect("a closed port refuses within the bound")
        .is_err()
}

/// Whether the series line `series` is in `answer`'s body.
pub(crate) fn has_series(answer: &str, series: &str) -> bool {
    answer
        .split_once("\r\n\r\n")
        .is_some_and(|(_, body)| body.lines().any(|line| line == series))
}
