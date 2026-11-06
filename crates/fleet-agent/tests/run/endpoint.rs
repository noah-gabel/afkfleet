//! The metrics endpoint in the run (Plan.md P12.4; ADR-0022, S9, S10): its
//! startup line, and the run's own series on a real socket until the run
//! ends.

use core::net::SocketAddr;

use fleet_agent::metrics_endpoint::{Endpoint, METRICS_LISTENING, METRICS_OFF};
use fleet_agent::run::Exit;
use fleet_agent::signals::Signal;
use metrics_exporter_prometheus::PrometheusBuilder;
use serde_json::Value;
use tokio::net::TcpListener;

use crate::harness::{Agent, afk};
use crate::scrape::{has_series, is_closed, scrape};

/// A listener on a free loopback port, and its address.
async fn listener() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    (listener, address)
}

#[tokio::test(start_paused = true)]
async fn without_an_endpoint_the_run_logs_that_metrics_are_off() {
    let agent = Agent::start(afk(&["AfkBot1"])).await;

    let off = agent.lines(METRICS_OFF);
    assert_eq!(off.len(), 1, "{}", agent.capture.text());
    assert_eq!(off[0]["level"], "INFO");
    assert_eq!(agent.lines(METRICS_LISTENING), Vec::<Value>::new());
}

#[tokio::test(start_paused = true)]
async fn with_an_endpoint_the_run_logs_where_it_listens() {
    let (listener, address) = listener().await;
    let render = PrometheusBuilder::new().build_recorder().handle();

    let agent = Agent::serving(afk(&["AfkBot1"]), Endpoint { listener, render }).await;

    let listening = agent.lines(METRICS_LISTENING);
    assert_eq!(listening.len(), 1, "{}", agent.capture.text());
    assert_eq!(listening[0]["level"], "INFO");
    assert_eq!(listening[0]["address"], address.to_string());
    assert_eq!(agent.lines(METRICS_OFF), Vec::<Value>::new());
}

/// On real time: the socket is real (`.claude/rules/testing.md`).
#[tokio::test]
async fn the_endpoint_serves_the_runs_own_series_until_the_run_ends() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let render = recorder.handle();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let (listener, address) = listener().await;
    let agent = Agent::serving(afk(&["AfkBot1"]), Endpoint { listener, render }).await;
    agent.join(1).await;

    let answer = scrape(address).await;
    agent.signal(Signal::Terminate).await;
    let outcome = agent.outcome().await;
    let closed = is_closed(address).await;

    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    for series in [
        "afkfleet_bots{state=\"online\"} 1",
        "afkfleet_mc_worlds 0",
        "afkfleet_outbox_chat_dropped_total 0",
        "afkfleet_outbox_refused_total 0",
        "afkfleet_outbox_session_requests_dropped_total 0",
    ] {
        assert!(has_series(&answer, series), "{series}: {answer}");
    }
    assert_eq!(outcome.exit, Exit::Stopped);
    assert!(closed, "the run's end closed the listener");
}
