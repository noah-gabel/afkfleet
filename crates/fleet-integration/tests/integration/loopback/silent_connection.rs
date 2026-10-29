//! A connection that goes silent without closing, end to end (P10.4's and
//! P10.9's notes; ADR-0019, S23): a server crash or a NAT timeout, which no
//! error announces. The server frees the agent within its heartbeat
//! timeout, the agent notices within its keepalive bound, reconnects within
//! `RECONNECT_DELAY_MAX` and resyncs, and its bot runs on through it. The
//! one case that measures timings, so the one with short ones: the
//! server's as group C's liveness tests have them, the agent's as group
//! G's link tests.

use core::time::Duration;
use std::time::Instant;

use fleet_agent::managed::link::LinkTimings;
use fleet_core::audit::AuditAction;
use fleet_server::config::Heartbeats;

use crate::harness::agent::RunOptions;
use crate::harness::logs::RECONNECT_MAX;
use crate::harness::server::ServerOptions;
use crate::harness::world::World;

/// The server's heartbeat timeout.
const TIMEOUT: Duration = Duration::from_secs(2);

/// The agent's link: its welcome timeout and its keepalive.
const SHORT: LinkTimings = LinkTimings {
    welcome: Duration::from_secs(2),
    keepalive_interval: Duration::from_secs(1),
    keepalive_timeout: Duration::from_secs(2),
};

/// Slack for a slow machine on every bound.
const SLACK: Duration = Duration::from_secs(3);

#[tokio::test]
async fn a_silent_connection_frees_the_agent_and_it_reconnects_and_resyncs() {
    let world = World::new(ServerOptions {
        heartbeats: Heartbeats::try_from_secs(1, 2).unwrap(),
        ..ServerOptions::default()
    })
    .await;
    let mut agent = world.agent("agent-a").await;
    agent.start(
        &world.ledger,
        RunOptions {
            timings: SHORT,
            ..RunOptions::capacity(2)
        },
    );
    let bot = world.owner.bot("AfkBot1").await;
    world.owner.start(bot).await;
    let first = world.online(&agent, bot, 0).await;

    // Timed from the stream end after the silence, whatever ended before.
    let ended_before = world
        .server
        .entries(&AuditAction::AGENT_DISCONNECT)
        .await
        .len();
    agent.proxy.silence_open();
    let silenced = Instant::now();
    world
        .entries(&AuditAction::AGENT_DISCONNECT, ended_before + 1)
        .await;
    let freed = silenced.elapsed();
    let again = world.online(&agent, bot, first).await;
    let back = silenced.elapsed();

    // The server ended the stream of silence, within C's bound.
    let ended = world.server.entries(&AuditAction::AGENT_DISCONNECT).await;
    assert_eq!(
        crate::loopback::reason(&ended[ended_before]),
        Some("heartbeat_timeout".to_owned())
    );
    let freed_by = 2 * TIMEOUT + TIMEOUT / 2 + SLACK;
    assert!(freed <= freed_by, "freed after {freed:?} > {freed_by:?}");
    // The agent noticed within its keepalive, came back within its longest
    // backoff, and its hello listed the bot, which ran on: one session.
    let back_by = SHORT.keepalive_interval + SHORT.keepalive_timeout + RECONNECT_MAX + SLACK;
    assert!(back <= back_by, "back after {back:?} > {back_by:?}");
    assert!(again > first);
    assert_eq!(world.ledger.sessions(bot), 1, "the bot never reconnected");
    assert!(world.ledger.live(bot));
    world.end(vec![agent]).await;
}
