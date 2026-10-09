//! When the run beats (Plan.md P5.5): at once once the agent runs, then
//! every 10 s, until its shutdown starts.

use fleet_agent::run::Exit;
use fleet_agent::signals::Signal;

use crate::harness::{Agent, advance, afk, secs};

#[tokio::test(start_paused = true)]
async fn the_first_beat_comes_once_the_agent_runs_and_then_one_every_ten_seconds() {
    let agent = Agent::start(afk(&["AfkBot1"])).await;

    assert_eq!(agent.lines("the agent is running").len(), 1);
    assert_eq!(agent.heartbeat.beats(), 1);
    advance(secs(9)).await;
    assert_eq!(agent.heartbeat.beats(), 1);
    advance(secs(1)).await;
    assert_eq!(agent.heartbeat.beats(), 2);
    advance(secs(10)).await;
    assert_eq!(agent.heartbeat.beats(), 3);
}

#[tokio::test(start_paused = true)]
async fn no_beat_comes_once_the_shutdown_starts() {
    let agent = Agent::start(afk(&["AfkBot1"])).await;
    agent.join(1).await;
    // The bot takes the whole shutdown timeout to stop, so a beat would be
    // due during the shutdown.
    agent.session(0).await.delay_disconnect(secs(60));
    let heartbeat = agent.heartbeat.clone();
    assert_eq!(heartbeat.beats(), 1);

    agent.signal(Signal::Terminate).await;
    let outcome = agent.outcome().await;

    assert_eq!(outcome.exit, Exit::Stopped);
    assert_eq!(heartbeat.beats(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_start_that_fails_never_beats() {
    // The fleet refuses the second bot: it uses the first one's account.
    let agent = Agent::start(afk(&["AfkBot1", "afkbot1"])).await;
    let heartbeat = agent.heartbeat.clone();

    let outcome = agent.outcome().await;

    assert_eq!(outcome.exit, Exit::StartupFailed);
    assert_eq!(heartbeat.beats(), 0);
}
