//! A kill in the middle of a certificate's renewal (Plan.md P13.5 and
//! P13.6; ADR-0027, ADR-0033). Whatever dies and wherever, the agent comes
//! back on the certificate it enrolled with or on a renewed one, no
//! connection of its is ever refused, and in the end the server holds
//! exactly one certificate that isn't revoked: the one in the agent's file.
//!
//! The agent enrolls while the server's clock is 41 days back, so its 60-day
//! certificate is past two thirds of its life at the real time, and it asks
//! for the next one as soon as a stream welcomes it.
//!
//! - **The server dies between signing and its commit** (a commit hook on
//!   its write connection): nothing of the renewal is stored, and the agent,
//!   which never got an answer, asks again.
//! - **The server dies right after the commit, and the answer is lost:**
//!   the server holds a certificate the agent never saw. The agent's next
//!   renewal revokes it.
//! - **The agent dies once its certificate file is replaced,** before any
//!   connection with the new certificate: it starts on the new one, whose
//!   hello revokes the old.
//! - **The agent dies while it replaces the file:** it starts on the old
//!   certificate or the new one, whichever the kill left, and never on a
//!   part of either.

use core::time::Duration;

use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::id::BotId;
use fleet_core::pki::CertFingerprint;
use fleet_core::system::Clock as _;
use fleet_server::infra::sqlite::{self, DatabaseOptions};
use fleet_server::infra::system::SystemClock;
use fleet_server::ports::agents::AgentCert;
use fleet_server::ports::store::Store as _;

use super::child::Trip;
use super::{after_the_fault, assert_sound, metadata, run_in_child, serve_in_child};
use crate::harness::agent::{Agent, RunOptions};
use crate::harness::server::ServerOptions;
use crate::harness::world::World;

/// How far back the server's clock is when the agent enrolls. A 60-day
/// certificate is due for renewal 40 days less 20 minutes after it's
/// issued, so one issued this far back is due at the real time.
const BEHIND: Duration = Duration::from_hours(41 * 24);

/// How much further back the clock starts, for the Owner's login to use
/// up.
const SLACK: Duration = Duration::from_mins(10);

/// How soon the agent asks again after a renewal that failed: the hour of
/// production would outlast the case.
const RENEW_RETRY: Duration = Duration::from_secs(2);

fn options() -> ServerOptions {
    ServerOptions {
        clock_behind: BEHIND + SLACK,
        backups: true,
        ..ServerOptions::default()
    }
}

fn run_options() -> RunOptions {
    let mut options = RunOptions::capacity(2);
    options.timings.renew_retry = RENEW_RETRY;
    options
}

/// A world whose one agent is enrolled and due for its renewal, not yet
/// running, with one bot that should run; the server's clock at the real
/// time, which TLS checks every certificate against.
async fn due() -> (World, Agent, BotId) {
    let world = World::new(options()).await;
    let bot = world.owner.bot("AfkBot1").await;
    world.owner.start(bot).await;
    let agent = world.agent("agent-a").await;
    let ahead = SystemClock
        .now()
        .signed_duration_since(world.server.clock().now())
        .to_std()
        .unwrap();
    assert!(ahead >= BEHIND, "the agent enrolled {ahead:?} back");
    world.server.clock().advance(ahead);
    (world, agent, bot)
}

/// The agent's certificates that aren't revoked.
fn unrevoked(certs: &[AgentCert]) -> Vec<CertFingerprint> {
    certs
        .iter()
        .filter(|cert| cert.revoked_at.is_none())
        .map(|cert| cert.fingerprint)
        .collect()
}

/// The fingerprint each successful renewal answered with, oldest first.
async fn renewed(world: &World) -> Vec<String> {
    world
        .server
        .entries(&AuditAction::AGENT_CERT_RENEW)
        .await
        .iter()
        .filter(|entry| entry.outcome == AuditOutcome::Success)
        .map(|entry| metadata(entry, "fingerprint").unwrap())
        .collect()
}

/// Waits until the agent's live stream presents its `count`-th renewed
/// certificate; that certificate's fingerprint.
async fn live_on_renewal(world: &World, count: usize) -> String {
    let mut newest = String::new();
    world
        .logs
        .until(&format!("a stream on renewal {count}"), async || {
            let renewals = renewed(world).await;
            let Some(fingerprint) = renewals.get(count.saturating_sub(1)) else {
                return false;
            };
            newest.clone_from(fingerprint);
            world
                .server
                .registry()
                .live_certificates()
                .iter()
                .any(|live| live.fingerprint.to_string() == *fingerprint)
        })
        .await;
    newest
}

/// What every case ends with: the server holds exactly one certificate of
/// the agent that isn't revoked, `live`, which its stream presents; no
/// connection and no renewal of the agent was ever refused; the bot runs,
/// once.
async fn assert_never_locked_out(world: &World, agent: &Agent, bot: BotId, live: &str) {
    world.online(agent, bot, 0).await;
    let certs = world.server.certs(agent.id).await;
    let left: Vec<String> = unrevoked(&certs).iter().map(ToString::to_string).collect();
    assert_eq!(left, [live.to_owned()], "{certs:#?}");
    for action in [AuditAction::AGENT_CONNECT, AuditAction::AGENT_CERT_RENEW] {
        let refused: Vec<_> = world
            .server
            .entries(&action)
            .await
            .into_iter()
            .filter(|entry| entry.outcome == AuditOutcome::Failure)
            .collect();
        assert_eq!(refused.len(), 0, "{refused:#?}");
    }
    assert!(world.ledger.live(bot));
    assert_eq!(world.ledger.sessions(bot), 1);
}

#[tokio::test]
async fn a_server_killed_between_signing_a_renewal_and_its_commit_locks_no_agent_out() {
    let (mut world, mut agent, bot) = due().await;
    let enrolled = agent.certificate();
    world.server.stop().await;
    let mut child = serve_in_child(
        &world,
        options(),
        Trip::Commit {
            table: "agent_certs".to_owned(),
            nth: 1,
            hold: false,
        },
        false,
    )
    .await;
    agent.start(&world.ledger, run_options());

    // Signed, written into its transaction, not committed: killed there.
    child.says("TRIPPED").await;
    child.kill(&world);

    assert_sound(&world.server.config().database).await;
    world.server.start_again().await;
    // Nothing of the renewal was stored, and the agent holds what it had.
    let certs = world.server.certs(agent.id).await;
    assert_eq!(certs.len(), 1, "{certs:#?}");
    assert_eq!(certs[0].revoked_at, None);
    assert_eq!(renewed(&world).await.len(), 0);
    assert_eq!(agent.certificate(), enrolled);
    // It comes back on that certificate and asks again.
    let live = live_on_renewal(&world, 1).await;

    assert_never_locked_out(&world, &agent, bot, &live).await;
    assert_ne!(agent.certificate(), enrolled);
    assert_eq!(world.server.certs(agent.id).await.len(), 2);
    after_the_fault(&mut world).await;
    agent.stop().await;
    world.end(vec![agent]).await;
}

#[tokio::test]
async fn a_server_killed_after_a_renewal_s_commit_whose_answer_is_lost_leaves_no_certificate_in_use()
 {
    let (mut world, mut agent, bot) = due().await;
    let enrolled = agent.certificate();
    world.server.stop().await;
    let mut child = serve_in_child(
        &world,
        options(),
        Trip::Commit {
            table: "agent_certs".to_owned(),
            nth: 1,
            hold: true,
        },
        false,
    )
    .await;
    agent.start(&world.ledger, run_options());
    child.says("TRIPPED").await;

    // The route dies before the commit: the answer never reaches the agent.
    agent.proxy.silence_open();
    child.go();
    child.says("RELEASED").await;
    // The commit is on disk once another process reads both certificates.
    let reader = sqlite::open_migrated(&world.server.config().database, DatabaseOptions::default())
        .await
        .unwrap();
    world
        .logs
        .until("the renewal's commit", async || {
            reader.agents().certs(agent.id).await.unwrap().len() == 2
        })
        .await;
    reader.close().await;
    child.kill(&world);
    agent.proxy.cut_open();

    assert_sound(&world.server.config().database).await;
    world.server.start_again().await;
    // The server holds a certificate the agent never saw; the agent's own
    // is still good.
    let certs = world.server.certs(agent.id).await;
    assert_eq!(unrevoked(&certs).len(), 2, "{certs:#?}");
    assert_eq!(agent.certificate(), enrolled);
    let lost = renewed(&world).await;
    assert_eq!(lost.len(), 1);
    // It comes back on its own certificate; its next renewal revokes the
    // lost one, and its hello with the new one revokes the old.
    let live = live_on_renewal(&world, 2).await;

    assert_never_locked_out(&world, &agent, bot, &live).await;
    assert_ne!(live, lost[0]);
    let certs = world.server.certs(agent.id).await;
    assert_eq!(certs.len(), 3, "{certs:#?}");
    let second = &world.server.entries(&AuditAction::AGENT_CERT_RENEW).await[1];
    assert_eq!(
        metadata(second, "certificates_revoked").as_deref(),
        Some("1")
    );
    after_the_fault(&mut world).await;
    agent.stop().await;
    world.end(vec![agent]).await;
}

#[tokio::test]
async fn an_agent_killed_once_its_certificate_file_is_replaced_starts_on_the_new_one() {
    let (mut world, mut agent, bot) = due().await;
    let enrolled = agent.certificate();
    // Its one connection passes, and none after it: the renewal is asked on
    // that connection, and the new certificate never reaches the server
    // before the kill.
    agent.proxy.close_gate_after(1);
    let mut child = run_in_child(
        &agent,
        2,
        Trip::Changes {
            file: agent.cert_file().to_owned(),
        },
    )
    .await;

    child.says("TRIPPED").await;
    child.kill(&world);

    // The file is the renewed certificate, whole; the server holds both,
    // and saw one hello, with the old one.
    world.entries(&AuditAction::AGENT_CERT_RENEW, 1).await;
    assert_ne!(agent.certificate(), enrolled);
    let certs = world.server.certs(agent.id).await;
    assert_eq!(unrevoked(&certs).len(), 2, "{certs:#?}");
    assert_eq!(
        world
            .server
            .entries(&AuditAction::AGENT_CONNECT)
            .await
            .len(),
        1
    );
    // The process starts again, from its files.
    agent.proxy.open_gate();
    agent.start(&world.ledger, run_options());
    let live = live_on_renewal(&world, 1).await;

    assert_never_locked_out(&world, &agent, bot, &live).await;
    assert_eq!(renewed(&world).await.len(), 1, "it never asked again");
    after_the_fault(&mut world).await;
    agent.stop().await;
    world.end(vec![agent]).await;
}

#[tokio::test]
async fn an_agent_killed_while_it_replaces_its_certificate_file_starts_on_the_old_or_the_new_one() {
    let (mut world, mut agent, bot) = due().await;
    let enrolled = agent.certificate();
    agent.proxy.close_gate_after(1);
    let mut child = run_in_child(
        &agent,
        2,
        Trip::Replaces {
            file: agent.cert_file().to_owned(),
            suffix: fleet_agent::enroll::files::RENEW_TEMP_SUFFIX.to_owned(),
        },
    )
    .await;

    // The temporary file is there, or was a moment ago: the kill lands
    // while it's written, or right after it replaced the certificate.
    child.says("TRIPPED").await;
    child.kill(&world);

    world.entries(&AuditAction::AGENT_CERT_RENEW, 1).await;
    // Whichever the kill left, the file is one certificate, whole.
    let kept = agent.certificate() == enrolled;
    agent.proxy.open_gate();
    agent.start(&world.ledger, run_options());
    // On the old one it asks again, which revokes the answer it lost.
    let live = live_on_renewal(&world, if kept { 2 } else { 1 }).await;

    assert_never_locked_out(&world, &agent, bot, &live).await;
    assert_ne!(agent.certificate(), enrolled);
    let left: Vec<String> = std::fs::read_dir(agent.dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(fleet_agent::enroll::files::RENEW_TEMP_SUFFIX))
        .collect();
    assert_eq!(left, [] as [&str; 0], "a renewal's temporary file stayed");
    after_the_fault(&mut world).await;
    agent.stop().await;
    world.end(vec![agent]).await;
}
