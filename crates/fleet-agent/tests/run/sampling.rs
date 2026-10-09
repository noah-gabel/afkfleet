//! fleet-mc's diagnostics in the run: the abandoned-thread limit, the
//! sampling period and the metrics.

use fleet_agent::diagnostics::HostSample;
use fleet_agent::run::{Exit, Outcome};
use fleet_runtime::ShutdownReport;
use fleet_testkit::mc::SessionController;
use metrics_exporter_prometheus::PrometheusBuilder;

use crate::harness::{Agent, advance, afk, secs};

/// The value of the series `name` in a Prometheus render, if it's there.
fn value(render: &str, name: &str) -> Option<String> {
    render.lines().find_map(|line| {
        line.strip_prefix(name)
            .and_then(|rest| rest.strip_prefix(' '))
            .map(str::to_owned)
    })
}

#[tokio::test(start_paused = true)]
async fn reaching_the_abandoned_thread_limit_shuts_down_and_exits_with_the_report() {
    let agent = Agent::start(afk(&["AfkBot1", "AfkBot2"])).await;
    agent.join(2).await;
    let sessions = [agent.session(0).await, agent.session(1).await];
    let capture = agent.capture.clone();

    agent.diagnostics.set_abandoned(3);
    advance(secs(5)).await;
    let outcome = agent.outcome().await;

    assert_eq!(
        outcome,
        Outcome {
            exit: Exit::AbandonedLimit,
            report: Some(ShutdownReport {
                stopped: 2,
                aborted: 0,
                crashed: 0
            })
        }
    );
    assert!(sessions.iter().all(SessionController::is_torn_down));
    let lines = crate::harness::lines_with(
        &capture,
        "the abandoned-thread limit is reached; shutting down",
    );
    assert_eq!(lines.len(), 1, "{}", capture.text());
    assert_eq!(lines[0]["level"], "ERROR");
    assert_eq!(lines[0]["abandoned"], 3);
    assert_eq!(lines[0]["limit"], 3);
}

#[tokio::test(start_paused = true)]
async fn below_the_limit_the_agent_keeps_running() {
    let agent = Agent::start(afk(&["AfkBot1"])).await;
    agent.join(1).await;

    agent.diagnostics.set_abandoned(2);
    advance(secs(30)).await;

    assert!(!agent.has_ended());
    assert!(!agent.session(0).await.is_torn_down());
}

#[tokio::test(start_paused = true)]
async fn diagnostics_are_sampled_at_start_and_every_five_seconds() {
    let agent = Agent::start(afk(&["AfkBot1"])).await;
    agent.join(1).await;
    assert_eq!(agent.diagnostics.samples(), 1);

    advance(secs(4)).await;
    assert_eq!(agent.diagnostics.samples(), 1);
    advance(secs(1)).await;
    assert_eq!(agent.diagnostics.samples(), 2);
    advance(secs(5)).await;
    assert_eq!(agent.diagnostics.samples(), 3);
}

#[tokio::test(start_paused = true)]
async fn the_mc_metrics_start_at_zero_and_follow_each_sample() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let agent = Agent::start(afk(&["AfkBot1"])).await;
    agent.join(1).await;

    let render = handle.render();
    for name in [
        "afkfleet_mc_abandoned_threads_total",
        "afkfleet_mc_dropped_chat_total",
        "afkfleet_mc_ignored_action_bar_total",
        "afkfleet_mc_host_threads",
        "afkfleet_mc_worlds",
    ] {
        assert_eq!(value(&render, name).as_deref(), Some("0"), "{render}");
    }
    // The runtime's metrics land in the same recorder.
    assert_eq!(
        value(&render, "afkfleet_bots{state=\"online\"}").as_deref(),
        Some("1"),
        "{render}"
    );

    agent.diagnostics.set(HostSample {
        abandoned_threads: 1,
        host_threads: 2,
        worlds: 1,
        dropped_chat: 5,
        ignored_action_bar: 7,
    });
    advance(secs(5)).await;

    let render = handle.render();
    assert_eq!(
        value(&render, "afkfleet_mc_abandoned_threads_total").as_deref(),
        Some("1")
    );
    assert_eq!(
        value(&render, "afkfleet_mc_host_threads").as_deref(),
        Some("2")
    );
    assert_eq!(value(&render, "afkfleet_mc_worlds").as_deref(), Some("1"));
    assert_eq!(
        value(&render, "afkfleet_mc_dropped_chat_total").as_deref(),
        Some("5")
    );
    assert_eq!(
        value(&render, "afkfleet_mc_ignored_action_bar_total").as_deref(),
        Some("7")
    );
}
