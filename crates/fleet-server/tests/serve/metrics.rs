//! The metrics port (Plan.md P12.4; ADR-0022, S9, S10): `serve` answers
//! `GET /metrics` on `[metrics] bind` only, beside the API, never on the
//! API's port, and stops it with the rest. Real sockets and real time, as
//! `boot`'s tests.

use core::time::Duration;
use std::net::SocketAddr;

use fleet_server::cli::healthcheck;
use fleet_server::cli::serve::{self, Exit, METRICS_LISTENING, METRICS_OFF};
use fleet_server::config::{Rate, ServerConfig};
use fleet_startup::signals::Signal;
use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::sync::mpsc::Sender;
use tokio::task::JoinHandle;

use crate::support::{
    ChannelSignals, DEADLINE, Running, capture, closed_port, config_body, drain, listener,
    path_arg, server, server_config, test_dir, vault_keys, write_config,
};

/// The longest a test waits for anything.
const PATIENCE: Duration = Duration::from_secs(30);

/// A server running in-process with its metrics port.
struct Serving {
    api: SocketAddr,
    metrics: SocketAddr,
    stop: Sender<Signal>,
    server: JoinHandle<Exit>,
}

impl Serving {
    /// Stops it by a signal; how it ended.
    async fn stop(self) -> Exit {
        self.stop.send(Signal::Terminate).await.unwrap();
        tokio::time::timeout(PATIENCE, self.server)
            .await
            .expect("the server should stop within its deadline")
            .unwrap()
    }
}

/// `serve::run` on listeners of its own, with its database in `dir` and
/// `config`'s changes.
async fn serving(dir: &str, change: impl FnOnce(&mut ServerConfig)) -> Serving {
    let dir = test_dir(dir);
    let (api_listener, api) = listener().await;
    let (metrics_listener, metrics) = listener().await;
    let mut config = server_config(&dir, api, false);
    config.metrics = Some(metrics);
    change(&mut config);
    let (stop, signals) = ChannelSignals::new();
    let keys = vault_keys(&config);
    let server = tokio::spawn(async move {
        serve::run(
            &config,
            api_listener,
            None,
            Some(metrics_listener),
            signals,
            keys,
        )
        .await
    });
    Serving {
        api,
        metrics,
        stop,
        server,
    }
}

/// Sends `request` as it is to `address` and reads the whole answer, which
/// the request's `Connection: close` ends.
async fn exchange(address: SocketAddr, request: &str) -> String {
    tokio::time::timeout(PATIENCE, async {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).await.unwrap();
        answer
    })
    .await
    .expect("the answer should come within the patience")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_metrics_port_serves_every_series_in_prometheus_text_format() {
    let serving = serving("metrics_served", |_| {}).await;

    let answer = exchange(
        serving.metrics,
        "GET /metrics HTTP/1.1\r\nHost: metrics\r\nConnection: close\r\n\r\n",
    )
    .await;

    let (head, body) = answer.split_once("\r\n\r\n").unwrap();
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
    assert!(
        head.to_ascii_lowercase()
            .contains("content-type: text/plain; version=0.0.4; charset=utf-8"),
        "{head}"
    );
    assert!(
        body.contains("# TYPE afkfleet_refresh_token_reuses_total counter"),
        "{body}"
    );
    assert!(
        body.contains("afkfleet_rate_limit_refusals_total{limit=\"login\"} 0"),
        "{body}"
    );
    assert_eq!(serving.stop().await, Exit::Stopped);
}

/// The API's port never serves the metrics, and the metrics port serves
/// nothing else (S9).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_port_serves_only_its_own_routes() {
    let serving = serving("metrics_apart", |_| {}).await;

    let on_the_api = healthcheck::get(serving.api, "/metrics", PATIENCE)
        .await
        .unwrap();
    let health_on_metrics = healthcheck::get(serving.metrics, "/health/live", PATIENCE)
        .await
        .unwrap();
    let health_on_the_api = healthcheck::get(serving.api, "/health/live", PATIENCE)
        .await
        .unwrap();

    assert_eq!(on_the_api.status, 404);
    assert_eq!(health_on_metrics.status, 404);
    assert_eq!(health_on_the_api.status, 200);
    assert_eq!(serving.stop().await, Exit::Stopped);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_method_on_the_metrics_route_is_405() {
    let serving = serving("metrics_method", |_| {}).await;

    let answer = exchange(
        serving.metrics,
        "POST /metrics HTTP/1.1\r\nHost: metrics\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    )
    .await;

    assert!(
        answer.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"),
        "{answer}"
    );
    assert!(
        answer.contains("\"code\":\"method_not_allowed\""),
        "{answer}"
    );
    assert_eq!(serving.stop().await, Exit::Stopped);
}

/// The status line and whether the body holds the series, for `method`
/// and `target` on the metrics port at `address`.
async fn answered(address: SocketAddr, method: &str, target: &str) -> (String, bool) {
    let answer = exchange(
        address,
        &format!("{method} {target} HTTP/1.1\r\nHost: metrics\r\nConnection: close\r\n\r\n"),
    )
    .await;
    let (head, body) = answer.split_once("\r\n\r\n").unwrap_or((&answer, ""));
    let status = head.lines().next().unwrap_or_default().to_owned();
    (status, body.contains("afkfleet_refresh_token_reuses_total"))
}

/// A query or a fragment on `/metrics` is ignored, as the agent's
/// responder ignores it (ADR-0022, S20; PR #96's review): Prometheus'
/// `params` may add one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_query_on_the_metrics_route_is_ignored() {
    let serving = serving("metrics_query", |_| {}).await;

    let mut found = Vec::new();
    for (method, target) in [
        ("GET", "/metrics?x=1"),
        ("GET", "/metrics?"),
        ("GET", "/metrics#x"),
        ("POST", "/metrics?x=1"),
    ] {
        found.push((target, answered(serving.metrics, method, target).await));
    }

    assert_eq!(
        found,
        [
            ("/metrics?x=1", ("HTTP/1.1 200 OK".to_owned(), true)),
            ("/metrics?", ("HTTP/1.1 200 OK".to_owned(), true)),
            ("/metrics#x", ("HTTP/1.1 200 OK".to_owned(), true)),
            (
                "/metrics?x=1",
                ("HTTP/1.1 405 Method Not Allowed".to_owned(), false)
            ),
        ]
    );
    assert_eq!(serving.stop().await, Exit::Stopped);
}

/// Only the exact path is the metrics route: a trailing slash, a doubled
/// slash or `/metrics` inside another path's query is 404 (S20).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_exact_path_is_the_metrics_route() {
    let serving = serving("metrics_exact", |_| {}).await;

    let mut found = Vec::new();
    for target in ["/metrics/", "//metrics", "/x?y=/metrics"] {
        found.push((target, answered(serving.metrics, "GET", target).await));
    }

    let not_found = ("HTTP/1.1 404 Not Found".to_owned(), false);
    assert_eq!(
        found,
        [
            ("/metrics/", not_found.clone()),
            ("//metrics", not_found.clone()),
            ("/x?y=/metrics", not_found),
        ]
    );
    assert_eq!(serving.stop().await, Exit::Stopped);
}

/// `HEAD /metrics` answers 200 with the head alone, as the agent's
/// responder does (ADR-0022, U23).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn head_on_the_metrics_route_answers_without_a_body() {
    let serving = serving("metrics_head", |_| {}).await;

    let answer = exchange(
        serving.metrics,
        "HEAD /metrics HTTP/1.1\r\nHost: metrics\r\nConnection: close\r\n\r\n",
    )
    .await;

    let (head, body) = answer.split_once("\r\n\r\n").unwrap();
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
    assert_eq!(body, "");
    assert_eq!(serving.stop().await, Exit::Stopped);
}

/// The metrics port spends the per-IP limit, as every request does: a
/// scraper that floods it is refused (rule 3).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_metrics_port_spends_the_per_ip_limit() {
    let serving = serving("metrics_per_ip", |config| {
        let one = core::num::NonZeroU32::MIN;
        config.rate_limits.per_ip = Rate::per_second(one, one.saturating_add(1));
    })
    .await;

    let mut statuses = Vec::new();
    // A burst of 2 refilled once a second: the third of these quick
    // requests is refused, unless each took a second.
    for _ in 0..20 {
        let answer = healthcheck::get(serving.metrics, "/metrics", PATIENCE)
            .await
            .unwrap();
        statuses.push(answer.status);
        if answer.status != 200 {
            break;
        }
    }

    assert_eq!(statuses.first(), Some(&200), "{statuses:?}");
    assert_eq!(statuses.last(), Some(&429), "{statuses:?}");
    assert_eq!(serving.stop().await, Exit::Stopped);
}

#[tokio::test]
async fn the_start_says_where_the_metrics_listen() {
    let (logs, _guard) = capture();
    let serving = serving("metrics_line_on", |_| {}).await;
    let metrics = serving.metrics;

    // Once the API answers, the start's lines are written.
    let ready = healthcheck::get(serving.api, "/health/ready", PATIENCE)
        .await
        .unwrap();
    let exit = serving.stop().await;

    assert_eq!(ready.status, 200);
    assert_eq!(exit, Exit::Stopped);
    let listening = logs.lines_with(METRICS_LISTENING).unwrap();
    assert_eq!(listening.len(), 1, "{}", logs.text());
    assert_eq!(listening[0]["level"], "INFO");
    assert_eq!(listening[0]["address"], metrics.to_string());
    assert_eq!(
        logs.lines_with(METRICS_OFF).unwrap().len(),
        0,
        "{}",
        logs.text()
    );
}

#[tokio::test]
async fn the_start_says_once_when_metrics_are_off() {
    let (logs, _guard) = capture();
    let dir = test_dir("metrics_line_off");
    let (api_listener, api) = listener().await;
    let config = server_config(&dir, api, false);
    let (stop, signals) = ChannelSignals::new();
    let keys = vault_keys(&config);
    let server =
        tokio::spawn(
            async move { serve::run(&config, api_listener, None, None, signals, keys).await },
        );

    let ready = healthcheck::get(api, "/health/ready", PATIENCE)
        .await
        .unwrap();
    stop.send(Signal::Terminate).await.unwrap();
    let exit = tokio::time::timeout(PATIENCE, server)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(ready.status, 200);
    assert_eq!(exit, Exit::Stopped);
    let off = logs.lines_with(METRICS_OFF).unwrap();
    assert_eq!(off.len(), 1, "{}", logs.text());
    assert_eq!(off[0]["level"], "INFO");
    assert_eq!(
        logs.lines_with(METRICS_LISTENING).unwrap().len(),
        0,
        "{}",
        logs.text()
    );
}

/// A scrape still open at the signal doesn't hold the stop: the metrics
/// port drains with the API's, within the deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_connection_to_the_metrics_port_doesnt_hold_the_stop() {
    let serving = serving("metrics_silent", |config| {
        config.http.request_timeout = Duration::from_secs(1);
    })
    .await;
    // Connected, but never a whole request.
    let mut silent = TcpStream::connect(serving.metrics).await.unwrap();
    silent.write_all(b"GET /metr").await.unwrap();

    let exit = serving.stop().await;

    assert_eq!(exit, Exit::Stopped);
    drop(silent);
}

/// How many free ports the binary test tries: another process can take
/// the one it found before the server binds it.
const ATTEMPTS: usize = 3;

/// The real binary with `[metrics] bind` on a port that was free a moment
/// ago: the address it logged, or `None` when that port was taken by then.
fn start_binary(attempt: usize) -> Option<(Running, SocketAddr)> {
    let dir = test_dir(&format!("metrics_binary_{attempt}"));
    let (api, metrics) = (closed_port(), closed_port());
    if api.port() == metrics.port() {
        return None;
    }
    let body = format!(
        "{}[metrics]\nbind = \"{metrics}\"\n",
        config_body(&dir, api)
    );
    let config = write_config(&dir, &body);
    let mut child = server(&["serve", "--config", path_arg(&config)])
        .spawn()
        .unwrap();
    let stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    let stderr = drain(child.stderr.take().unwrap());
    let running = Running(Some(child));
    let (lines, seen) = std::sync::mpsc::sync_channel(256);
    std::thread::spawn(move || {
        use std::io::BufRead as _;
        for line in stdout.lines() {
            let Ok(line) = line else { break };
            if lines.send(line).is_err() {
                return;
            }
        }
    });
    loop {
        let Ok(line) = seen.recv_timeout(DEADLINE) else {
            // The output ended: the start failed, a taken port allowed.
            let text = stderr.recv_timeout(DEADLINE).unwrap_or_default();
            assert!(
                text.is_empty(),
                "the server ended before its metrics listened: {text}"
            );
            return None;
        };
        let line: Value = serde_json::from_str(&line).unwrap_or_default();
        if line["message"] == "the server can't start"
            && line["error"]
                .as_str()
                .is_some_and(|error| error.starts_with("can't listen on "))
        {
            return None;
        }
        if line["message"] == METRICS_LISTENING {
            assert_eq!(line["address"], metrics.to_string());
            return Some((running, metrics));
        }
        assert_ne!(
            line["message"], METRICS_OFF,
            "the binary didn't bind [metrics] bind"
        );
    }
}

/// `main.rs` binds `[metrics] bind` and hands it to `serve`.
#[test]
fn the_binary_serves_its_metrics_on_its_own_port() {
    let (running, metrics) = (1..=ATTEMPTS)
        .find_map(start_binary)
        .expect("free ports within the attempts");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let answer = runtime
        .block_on(healthcheck::get(metrics, "/metrics", PATIENCE))
        .unwrap();

    assert_eq!(answer.status, 200);
    let body = String::from_utf8(answer.body).unwrap();
    assert!(
        body.contains("afkfleet_backup_last_success_timestamp_seconds 0"),
        "{body}"
    );
    drop(running);
}
