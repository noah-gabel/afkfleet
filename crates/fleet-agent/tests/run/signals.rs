//! The graceful shutdown on a signal (Plan.md P5.4).

use core::time::Duration;

use fleet_agent::run::{Exit, Outcome};
use fleet_agent::signals::Signal;
use fleet_runtime::ShutdownReport;
use tokio::time::Instant;

use crate::harness::{Agent, advance, afk, secs};

const SHUTTING_DOWN: &str = "shutting down";
const ALREADY: &str = "already shutting down";

fn stopped(stopped: usize, aborted: usize) -> Outcome {
    Outcome {
        exit: Exit::Stopped,
        report: Some(ShutdownReport {
            stopped,
            aborted,
            crashed: 0,
        }),
    }
}

#[tokio::test(start_paused = true)]
async fn a_signal_shuts_the_fleet_down_and_exits_stopped_with_the_report() {
    let agent = Agent::start(afk(&["AfkBot1"])).await;
    agent.join(1).await;
    let session = agent.session(0).await;
    let capture = agent.capture.clone();
    let started = Instant::now();

    agent.signal(Signal::Terminate).await;
    let outcome = agent.outcome().await;

    assert_eq!(outcome, stopped(1, 0));
    assert_eq!(started.elapsed(), Duration::ZERO);
    assert!(session.is_torn_down());
    let lines = crate::harness::lines_with(&capture, SHUTTING_DOWN);
    assert_eq!(lines.len(), 1, "{}", capture.text());
    assert_eq!(lines[0]["level"], "INFO");
    assert_eq!(lines[0]["signal"], "SIGTERM");
}

#[tokio::test(start_paused = true)]
async fn a_bot_slower_than_the_shutdown_timeout_is_aborted_and_the_exit_is_still_stopped() {
    let agent = Agent::start(afk(&["AfkBot1"])).await;
    agent.join(1).await;
    agent.session(0).await.delay_disconnect(secs(60));
    let started = Instant::now();

    agent.signal(Signal::Interrupt).await;
    let outcome = agent.outcome().await;

    assert_eq!(outcome, stopped(0, 1));
    assert_eq!(started.elapsed(), secs(10));
}

#[tokio::test(start_paused = true)]
async fn a_second_signal_during_the_shutdown_is_only_logged_and_does_not_change_its_timing() {
    let agent = Agent::start(afk(&["AfkBot1"])).await;
    agent.join(1).await;
    agent.session(0).await.delay_disconnect(secs(8));
    let capture = agent.capture.clone();
    let started = Instant::now();

    agent.signal(Signal::Terminate).await;
    advance(secs(1)).await;
    agent.signal(Signal::Interrupt).await;
    let outcome = agent.outcome().await;

    assert_eq!(outcome, stopped(1, 0));
    assert_eq!(started.elapsed(), secs(8));
    assert_eq!(crate::harness::lines_with(&capture, SHUTTING_DOWN).len(), 1);
    let repeated = crate::harness::lines_with(&capture, ALREADY);
    assert_eq!(repeated.len(), 1, "{}", capture.text());
    assert_eq!(repeated[0]["level"], "INFO");
    assert_eq!(repeated[0]["signal"], "SIGINT");
}

#[tokio::test(start_paused = true)]
async fn a_signal_sent_before_the_start_is_handled_once_the_agent_runs() {
    let agent = Agent::start_with(afk(&["AfkBot1"]), Some(Signal::CtrlC)).await;
    let capture = agent.capture.clone();

    let outcome = agent.outcome().await;

    assert_eq!(outcome, stopped(1, 0));
    let messages: Vec<String> = capture
        .json_lines()
        .unwrap()
        .iter()
        .filter_map(|line| line["message"].as_str().map(str::to_owned))
        .filter(|message| message == "the agent is running" || message == SHUTTING_DOWN)
        .collect();
    assert_eq!(messages, ["the agent is running", SHUTTING_DOWN]);
    let line = &crate::harness::lines_with(&capture, SHUTTING_DOWN)[0];
    assert_eq!(line["signal"], "Ctrl+C");
}

#[tokio::test(start_paused = true)]
async fn a_signal_source_that_ends_never_stops_the_agent() {
    let mut agent = Agent::start(afk(&["AfkBot1"])).await;
    agent.join(1).await;

    agent.drop_signals();
    advance(secs(60)).await;

    assert!(!agent.has_ended());
    assert!(!agent.session(0).await.is_torn_down());
}
