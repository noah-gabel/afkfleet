//! Tests for the agents' list and disabling (Plan.md P10.10; ADR-0019, S27,
//! S31): each agent's status, stream and last sign of life; one
//! transaction that disables an agent, revokes its certificates and records
//! it, then ends its stream; the re-checks; the host's disable.
//!
//! Each test gets a fresh database from `#[sqlx::test]` and opens it with
//! `Database::connect`; real time, a `ManualClock` (CLAUDE.md). The live
//! streams are the real `AgentRegistry`'s, claimed directly.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::net::IpAddr;
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use fleet_core::agent::AgentStatus;
use fleet_core::audit::{
    AuditAction, AuditMetadata, AuditOutcome, AuditTarget, RecordedMetadata, RecordedTarget,
};
use fleet_core::authz::{Actor, Role};
use fleet_core::id::{AgentId, RequestId, SessionId, UserId};
use fleet_core::pki::CertFingerprint;
use fleet_core::system::Clock as _;
use fleet_core::value::{AgentName, Username};
use fleet_proto::control::AgentMessage;
use fleet_server::app::agents::{AgentService, DisableAgentError, Disabler};
use fleet_server::app::users::ActingUser;
use fleet_server::grpc::registry::{AgentRegistry, Halt, Lease};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::agent_connections::{Admitted, StreamEnd};
use fleet_server::ports::agents::{NewAgent, NewAgentCert};
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditRecord, NewAuditEntry};
use fleet_server::ports::page::{Page, PageLimit};
use fleet_server::ports::sessions::NewSession;
use fleet_server::ports::store::Store as _;
use fleet_server::ports::users::{NewUser, PasswordHash};
use fleet_testkit::system::ManualClock;
use secrecy::SecretString;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{mpsc, oneshot};

/// A stored hash; nothing checks it here.
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
const T0: &str = "2027-01-15T08:00:00Z";
const SETUP: &str = "test.agents_setup";
const ADMIN_IP: [u8; 4] = [192, 0, 2, 10];

fn at(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

struct Setup {
    db: Database,
    clock: ManualClock,
    registry: AgentRegistry,
    agents: AgentService,
}

async fn setup(options: SqliteConnectOptions) -> Setup {
    let db = Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap();
    let clock = ManualClock::new(at(T0));
    let registry = AgentRegistry::new(Arc::new(clock.clone()));
    let agents = AgentService::new(
        Arc::new(clock.clone()),
        Arc::new(db.clone()),
        Arc::new(registry.clone()),
    );
    Setup {
        db,
        clock,
        registry,
        agents,
    }
}

fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from(SETUP).unwrap(),
        AuditOutcome::Success,
    )
}

fn page(limit: u32) -> Page<AgentId> {
    Page {
        before: None,
        limit: PageLimit::new(limit).unwrap(),
    }
}

impl Setup {
    /// Agent `n` named `agent-<n>`, enrolled at `T0` with certificate `n`…,
    /// valid for `days`.
    async fn enroll(&self, n: u8, days: i64) -> AgentId {
        let id = AgentId::new_v7(at(T0), [n; 10]).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.agents()
            .insert(&NewAgent {
                id,
                name: AgentName::try_from(format!("agent-{n}").as_str()).unwrap(),
                created_at: at(T0),
            })
            .await
            .unwrap();
        tx.agents()
            .insert_cert(&NewAgentCert {
                fingerprint: CertFingerprint::from_digest([n; 32]),
                agent: id,
                not_after: at(T0) + TimeDelta::days(days),
                created_at: at(T0),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        id
    }

    /// Another certificate for `agent`, `byte`….
    async fn add_cert(&self, agent: AgentId, byte: u8) {
        let mut tx = self.db.write().await.unwrap();
        tx.agents()
            .insert_cert(&NewAgentCert {
                fingerprint: CertFingerprint::from_digest([byte; 32]),
                agent,
                not_after: at(T0) + TimeDelta::days(60),
                created_at: at(T0),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
    }

    /// User `n` with `role` and a live session, acting.
    async fn user(&self, n: u8, role: Role) -> ActingUser {
        let id = UserId::new_v7(at(T0), [n; 10]).unwrap();
        let session = SessionId::new_v7(at(T0), [n; 10]).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.users()
            .insert(&NewUser {
                id,
                username: Username::try_from(format!("user{n}").as_str()).unwrap(),
                password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
                role,
                created_at: at(T0),
            })
            .await
            .unwrap();
        tx.sessions()
            .insert(&NewSession {
                id: session,
                user_id: id,
                access_hash: fleet_server::ports::tokens::TokenHash::from_bytes([n; 32]),
                refresh_hash: fleet_server::ports::tokens::TokenHash::from_bytes([n + 100; 32]),
                created_at: at(T0),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        ActingUser {
            actor: Actor { id, role },
            session_id: session,
            ip: IpAddr::from(ADMIN_IP),
        }
    }

    /// Claims agent `n`'s slot as a connecting session does.
    fn claim(&self, n: u8) -> Lease {
        let agent = AgentId::new_v7(at(T0), [n; 10]).unwrap();
        self.registry
            .claim(&Admitted {
                request: RequestId::new_v7(at(T0), [n; 10]).unwrap(),
                agent,
                name: AgentName::try_from(format!("agent-{n}").as_str()).unwrap(),
                fingerprint: CertFingerprint::from_digest([n; 32]),
                not_after: at(T0) + TimeDelta::days(60),
                peer: None,
            })
            .unwrap()
    }

    /// Agent `n`'s live stream, welcomed now.
    fn live(
        &self,
        n: u8,
    ) -> (
        Lease,
        mpsc::Receiver<fleet_proto::generated::ServerMessage>,
        oneshot::Receiver<StreamEnd>,
    ) {
        let lease = self.claim(n);
        let (queue, queued) = mpsc::channel(4);
        let ended = self
            .registry
            .activate(&lease, queue, Halt::default(), self.clock.now())
            .unwrap();
        (lease, queued, ended)
    }

    async fn entries(&self, action: &AuditAction) -> Vec<AuditRecord> {
        self.db
            .audit()
            .list(AuditPage {
                before: None,
                limit: AuditLimit::new(100).unwrap(),
            })
            .await
            .unwrap()
            .into_iter()
            .filter(|record| &record.action == action)
            .collect()
    }
}

#[sqlx::test(migrations = false)]
async fn the_list_shows_each_agents_status_stream_and_last_sign_of_life(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let offline = setup.enroll(1, 60).await;
    let online = setup.enroll(2, 60).await;
    let expiring = setup.enroll(3, 1).await;
    let disabled = setup.enroll(4, 60).await;
    sqlx::query!(
        "UPDATE agents SET enabled = 0 WHERE id = ?",
        disabled.to_string()
    )
    .execute(setup.db.write_pool())
    .await
    .unwrap();
    setup.clock.advance(Duration::from_hours(48));
    let (lease, _queued, _ended) = setup.live(2);
    setup.clock.advance(Duration::from_secs(30));
    let _ = setup.registry.admit(&lease, &AgentMessage::Heartbeat);

    let first = setup.agents.list(page(3)).await.unwrap();

    let shown: Vec<_> = first
        .items
        .iter()
        .map(|view| {
            (
                view.listed.agent.id,
                view.status,
                view.connected_since,
                view.last_seen_at,
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            (disabled, AgentStatus::Disabled, None, None),
            (expiring, AgentStatus::Expired, None, None),
            (
                online,
                AgentStatus::Online,
                Some(at("2027-01-17T08:00:00Z")),
                Some(at("2027-01-17T08:00:30Z")),
            ),
        ]
    );
    assert_eq!(first.next, Some(online));
    let rest = setup
        .agents
        .list(Page {
            before: first.next,
            limit: PageLimit::new(3).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(rest.items.len(), 1);
    assert_eq!(rest.items[0].listed.agent.id, offline);
    assert_eq!(rest.items[0].status, AgentStatus::Offline);
    assert_eq!(rest.next, None);
}

#[sqlx::test(migrations = false)]
async fn an_admin_disables_an_agent_revokes_its_certificates_and_ends_its_stream(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let agent = setup.enroll(1, 60).await;
    setup.add_cert(agent, 9).await;
    let admin = setup.user(20, Role::Admin).await;
    let (_lease, _queued, mut ended) = setup.live(1);

    let disabled = setup
        .agents
        .disable(Disabler::Admin(&admin), agent)
        .await
        .unwrap();

    assert_eq!(disabled.revoked, Some(2));
    let view = disabled.view;
    assert_eq!(view.status, AgentStatus::Disabled);
    assert!(!view.listed.agent.enabled);
    assert_eq!(view.listed.not_after, None, "every certificate is revoked");
    assert_eq!(ended.try_recv().unwrap(), StreamEnd::Disabled);
    let certs = setup.db.agents().certs(agent).await.unwrap();
    assert!(certs.iter().all(|cert| cert.revoked_at == Some(at(T0))));
    let entries = setup.entries(&AuditAction::AGENT_DISABLE).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    let entry = &entries[0];
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(admin.actor.id));
    assert_eq!(entry.ip, Some(IpAddr::from(ADMIN_IP)));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Agent(agent)))
    );
    assert_eq!(
        entry.metadata,
        RecordedMetadata::from(
            &AuditMetadata::new()
                .text("name", "agent-1")
                .unwrap()
                .int("certificates_revoked", 2)
                .unwrap()
                .text("via", "http")
                .unwrap()
        )
    );
}

#[sqlx::test(migrations = false)]
async fn a_disable_during_a_hello_keeps_the_stream_from_being_welcomed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let agent = setup.enroll(1, 60).await;
    let lease = setup.claim(1);

    setup.agents.disable(Disabler::Host, agent).await.unwrap();

    let (queue, _queued) = mpsc::channel(4);
    let activated = setup
        .registry
        .activate(&lease, queue, Halt::default(), setup.clock.now());
    assert_eq!(activated.unwrap_err(), StreamEnd::Disabled);
}

#[sqlx::test(migrations = false)]
async fn the_host_disables_without_an_actor_via_cli(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let agent = setup.enroll(1, 60).await;

    let disabled = setup.agents.disable(Disabler::Host, agent).await.unwrap();

    assert_eq!(disabled.view.status, AgentStatus::Disabled);
    assert_eq!(disabled.revoked, Some(1));
    let entries = setup.entries(&AuditAction::AGENT_DISABLE).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].actor, None);
    assert_eq!(entries[0].ip, None);
    assert_eq!(
        entries[0].metadata,
        RecordedMetadata::from(
            &AuditMetadata::new()
                .text("name", "agent-1")
                .unwrap()
                .int("certificates_revoked", 1)
                .unwrap()
                .text("via", "cli")
                .unwrap()
        )
    );
}

#[sqlx::test(migrations = false)]
async fn disabling_an_agent_disabled_already_changes_and_records_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let agent = setup.enroll(1, 60).await;
    setup.agents.disable(Disabler::Host, agent).await.unwrap();
    setup.add_cert(agent, 9).await;

    let again = setup.agents.disable(Disabler::Host, agent).await.unwrap();

    assert_eq!(again.view.status, AgentStatus::Disabled);
    assert_eq!(again.revoked, None);
    assert_eq!(setup.entries(&AuditAction::AGENT_DISABLE).await.len(), 1);
    let unrevoked = setup
        .db
        .agents()
        .certs(agent)
        .await
        .unwrap()
        .iter()
        .filter(|cert| cert.revoked_at.is_none())
        .count();
    assert_eq!(unrevoked, 1, "nothing changes, not even a later row");
}

#[sqlx::test(migrations = false)]
async fn an_unknown_agent_is_not_found(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let admin = setup.user(20, Role::Admin).await;

    let result = setup
        .agents
        .disable(
            Disabler::Admin(&admin),
            AgentId::new_v7(at(T0), [9; 10]).unwrap(),
        )
        .await;

    assert!(
        matches!(result, Err(DisableAgentError::NotFound)),
        "{result:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_session_that_ended_or_a_member_disables_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let agent = setup.enroll(1, 60).await;
    let admin = setup.user(20, Role::Admin).await;
    let member = setup.user(21, Role::Member).await;
    let mut tx = setup.db.write().await.unwrap();
    assert!(
        tx.sessions()
            .revoke(admin.session_id, at(T0))
            .await
            .unwrap()
    );
    tx.commit(setup_entry()).await.unwrap();

    let gone = setup.agents.disable(Disabler::Admin(&admin), agent).await;
    let denied = setup.agents.disable(Disabler::Admin(&member), agent).await;

    assert!(
        matches!(gone, Err(DisableAgentError::SessionGone)),
        "{gone:?}"
    );
    assert!(
        matches!(denied, Err(DisableAgentError::Denied(_))),
        "{denied:?}"
    );
    assert!(
        setup
            .db
            .agents()
            .find(agent)
            .await
            .unwrap()
            .unwrap()
            .enabled
    );
    assert_eq!(setup.entries(&AuditAction::AGENT_DISABLE).await, Vec::new());
}
