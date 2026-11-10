//! A certificate's renewal, end to end (Plan.md P13.6; ADR-0027): the real
//! agent reaches two thirds of its certificate's lifetime while its bot is
//! online, asks the real server for the next one over its stream's
//! connection, stores it, and connects again with it. The bot runs on, once;
//! the new certificate's hello revokes the old one.

use core::time::Duration;

use fleet_core::audit::{AuditAction, AuditOutcome, RecordedValue};
use fleet_core::system::Clock as _;
use fleet_server::infra::system::SystemClock;
use fleet_server::ports::audit::AuditRecord;

use crate::harness::agent::RunOptions;
use crate::harness::server::ServerOptions;
use crate::harness::world::World;

/// How long after its enrollment the agent's renewal is due: long enough
/// for its bot to be online first, on a slow machine too.
const DUE_AFTER: Duration = Duration::from_secs(20);

/// How much further back the server's clock starts, for the Owner's login
/// to use up.
const SLACK: Duration = Duration::from_mins(10);

/// How far back the server's clock is when the agent enrolls. A 60-day certificate, valid from
/// an hour before its issue, is due for renewal 40 days less 20 minutes
/// after it; so one issued this far back is due [`DUE_AFTER`] from now.
fn clock_behind() -> Duration {
    Duration::from_hours(40 * 24) - Duration::from_mins(20) - DUE_AFTER
}

fn text(record: &AuditRecord, key: &str) -> Option<String> {
    record
        .metadata
        .entries()
        .iter()
        .find(|(found, _)| found == key)
        .map(|(_, value)| match value {
            RecordedValue::Text(text) => text.clone(),
            RecordedValue::Int(n) => n.to_string(),
            other => format!("{other:?}"),
        })
}

#[tokio::test]
async fn an_agent_renews_its_certificate_while_its_bot_runs_on() {
    let behind = clock_behind();
    let world = World::new(ServerOptions {
        clock_behind: behind + SLACK,
        ..ServerOptions::default()
    })
    .await;
    // The Owner's last calls: the clock moves past their step-up next, and
    // past their session after that.
    let bot = world.owner.bot("AfkBot1").await;
    world.owner.start(bot).await;
    // The Owner's login moved the clock by its TOTP steps: from here it's
    // exactly `behind` back, so the renewal is due when DUE_AFTER says.
    let back = fleet_core::time::sub(SystemClock.now(), behind);
    let short = back
        .signed_duration_since(world.server.clock().now())
        .to_std()
        .expect("the login moves the clock by less than SLACK");
    world.server.clock().advance(short);
    let mut agent = world.agent("agent-a").await;
    let enrolled = agent.certificate();
    // From here on the server's clock is the real time, which TLS checks
    // the next certificate against.
    world.server.clock().advance(behind);

    agent.start(&world.ledger, RunOptions::capacity(2));
    let first = world.online(&agent, bot, 0).await;
    assert_eq!(
        world
            .server
            .entries(&AuditAction::AGENT_CERT_RENEW)
            .await
            .len(),
        0,
        "the bot should be online before the renewal is due; a machine this slow needs a later DUE_AFTER"
    );
    // The renewal comes due: the agent asks, stores, and connects again.
    world.entries(&AuditAction::AGENT_CERT_RENEW, 1).await;
    let second = world.online(&agent, bot, first).await;

    assert!(second > first, "a new stream");
    assert_eq!(
        world.ledger.sessions(bot),
        1,
        "the bot never left its server"
    );
    let renewals = world.server.entries(&AuditAction::AGENT_CERT_RENEW).await;
    assert_eq!(renewals.len(), 1, "{renewals:?}");
    assert_eq!(renewals[0].outcome, AuditOutcome::Success);
    let renewed = text(&renewals[0], "fingerprint").unwrap();
    // The live stream presents the new certificate, which is the agent's
    // file now…
    let live = world.server.registry().live_certificates();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].fingerprint.to_string(), renewed);
    assert_ne!(agent.certificate(), enrolled);
    // …and its hello revoked the one it replaced.
    let certs = world.server.certs(agent.id).await;
    assert_eq!(certs.len(), 2, "{certs:?}");
    assert!(certs[0].revoked_at.is_some(), "{certs:?}");
    assert_eq!(certs[1].revoked_at, None);
    assert_eq!(certs[1].fingerprint.to_string(), renewed);
    let connects: Vec<_> = world
        .server
        .entries(&AuditAction::AGENT_CONNECT)
        .await
        .into_iter()
        .filter(|entry| entry.outcome == AuditOutcome::Success)
        .collect();
    assert_eq!(connects.len(), 2, "{connects:?}");
    assert_eq!(text(&connects[0], "certificates_revoked"), None);
    assert_eq!(
        text(&connects[1], "certificates_revoked").as_deref(),
        Some("1")
    );
    assert_eq!(text(&connects[1], "fingerprint"), Some(renewed));
    assert_eq!(
        world
            .server
            .entries(&AuditAction::AGENT_CONNECT)
            .await
            .iter()
            .filter(|entry| entry.outcome == AuditOutcome::Failure)
            .count(),
        0,
        "no connection was refused on the way"
    );
    agent.stop().await;
    world.end(vec![agent]).await;
}
