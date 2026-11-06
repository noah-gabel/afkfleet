//! A managed run's metrics endpoint (Plan.md P12.4; ADR-0022, S9): its own
//! series, the outbox's among them, on a real socket until the run ends.

use core::time::Duration;

use fleet_agent::metrics_endpoint::{Endpoint, METRICS_LISTENING};
use fleet_agent::run::Exit;
use metrics_exporter_prometheus::PrometheusBuilder;
use tokio::net::TcpListener;

use crate::agent::{Agent, agent};
use crate::pki::{TestCa, test_dir};
use crate::scrape::{has_series, is_closed, scrape};
use crate::server::FakeServer;

#[tokio::test]
async fn a_managed_run_serves_its_metrics_until_it_ends() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let render = recorder.handle();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let dir = test_dir("control", "metrics");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let running = Agent::serving(&dir, &server.url(), &ca, Endpoint { listener, render });
    let call = server.pending().await.accept();
    call.welcome(agent(), Duration::from_secs(10)).await;

    let answer = scrape(address).await;
    let listening = running.logs.lines_with(METRICS_LISTENING).unwrap();
    let outcome = running.stop().await;
    let closed = is_closed(address).await;

    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    for series in [
        "afkfleet_mc_worlds 0",
        "afkfleet_outbox_chat_dropped_total 0",
        "afkfleet_outbox_refused_total 0",
        "afkfleet_outbox_session_requests_dropped_total 0",
    ] {
        assert!(has_series(&answer, series), "{series}: {answer}");
    }
    assert_eq!(listening.len(), 1);
    assert_eq!(listening[0]["address"], address.to_string());
    assert_eq!(outcome.exit, Exit::Stopped);
    assert!(closed, "the run's end closed the listener");
    drop(call);
}
