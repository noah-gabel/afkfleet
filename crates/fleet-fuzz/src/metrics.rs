//! The target over the agent's metrics responder (ADR-0022, S9): its
//! request-head reader and its router, through the endpoint's own `serve`.

use core::future::pending;
use std::io;
use std::sync::Arc;

use fleet_agent::metrics_endpoint::{Incoming, Limits, Render, serve};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, DuplexStream};
use tokio_util::sync::CancellationToken;

/// What the target's endpoint renders: a line no request holds by chance.
const SERIES: &str = "afkfleet_fuzz_series 1\n";

/// The statuses the responder answers.
const STATUSES: [&str; 4] = [
    "200 OK",
    "400 Bad Request",
    "404 Not Found",
    "405 Method Not Allowed",
];

/// The pipe's size each way: more than the largest answer, so the answer
/// never waits for its reader.
const PIPE_BYTES: usize = 64 * 1024;

#[derive(Debug)]
struct Series;

impl Render for Series {
    fn render(&self) -> String {
        SERIES.to_owned()
    }
}

/// One connection, then none.
#[derive(Debug)]
struct Once(Option<DuplexStream>);

impl Incoming for Once {
    type Stream = DuplexStream;

    async fn accept(&mut self) -> io::Result<DuplexStream> {
        match self.0.take() {
            Some(stream) => Ok(stream),
            None => pending().await,
        }
    }
}

/// What the endpoint answers a client that sends `request` and closes its
/// side: every byte until the endpoint closes its own. Time is paused, so a
/// head that never ends meets the endpoint's timeout at once.
fn exchange(request: &[u8]) -> Option<Vec<u8>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .ok()?;
    Some(runtime.block_on(async {
        let (mut client, server) = tokio::io::duplex(PIPE_BYTES);
        let stop = CancellationToken::new();
        let serving = serve(
            Once(Some(server)),
            Arc::new(Series),
            Limits::default(),
            stop.clone(),
        );
        let asking = async {
            // An endpoint that answered early and closed refuses the rest
            // of the request: the answer is still there to read.
            let _ = client.write_all(request).await;
            let _ = client.shutdown().await;
            let mut answer = Vec::new();
            let _ = client.read_to_end(&mut answer).await;
            stop.cancel();
            answer
        };
        let ((), answer) = tokio::join!(serving, asking);
        answer
    }))
}

/// What must hold of `answer` to `request`: nothing, or one whole response
/// with one of the responder's statuses; and the series only for a `GET` of
/// `/metrics`.
fn is_an_answer(request: &[u8], answer: &[u8]) {
    if answer.is_empty() {
        return;
    }
    let status = answer.strip_prefix(b"HTTP/1.1 ");
    assert!(
        status.is_some_and(|rest| STATUSES
            .iter()
            .any(|status| rest.starts_with(status.as_bytes()))),
        "an answer that isn't one of the responder's"
    );
    assert!(
        answer.windows(4).any(|window| window == b"\r\n\r\n"),
        "an answer whose head never ends"
    );
    let has_series = answer
        .windows(SERIES.len())
        .any(|window| window == SERIES.as_bytes());
    assert!(
        !has_series || request.starts_with(b"GET /metrics"),
        "the series were answered to something else than a GET of /metrics"
    );
}

/// The agent's metrics endpoint over any bytes as a request: no panic, and
/// the answer is nothing or one of the responder's own.
///
/// # Panics
/// When a property breaks.
pub fn agent_metrics(data: &[u8]) {
    let answer = exchange(data);
    assert!(answer.is_some(), "the target's runtime builds");
    if let Some(answer) = answer {
        is_an_answer(data, &answer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scrape_is_answered_the_series() {
        let answer = exchange(b"GET /metrics HTTP/1.1\r\nHost: agent\r\n\r\n").unwrap();

        let text = String::from_utf8(answer).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text}");
        assert!(text.ends_with(SERIES), "{text}");
    }

    #[test]
    fn a_head_that_never_ends_is_answered_nothing() {
        assert_eq!(exchange(b"GET /metrics HTTP/1.1\r\n").unwrap(), b"");
    }

    #[test]
    fn no_answer_and_the_responders_answers_pass() {
        is_an_answer(b"x", b"");
        is_an_answer(b"x", b"HTTP/1.1 400 Bad Request\r\n\r\nbad request\n");
    }

    #[test]
    #[should_panic(expected = "an answer that isn't one of the responder's")]
    fn another_status_fails() {
        is_an_answer(b"x", b"HTTP/1.1 500 Internal Server Error\r\n\r\n");
    }

    #[test]
    #[should_panic(expected = "an answer whose head never ends")]
    fn a_cut_answer_fails() {
        is_an_answer(b"x", b"HTTP/1.1 200 OK\r\n");
    }

    #[test]
    #[should_panic(expected = "the series were answered to something else")]
    fn the_series_for_another_request_fail() {
        let answer = format!("HTTP/1.1 200 OK\r\n\r\n{SERIES}");
        is_an_answer(b"POST /metrics HTTP/1.1\r\n\r\n", answer.as_bytes());
    }
}
