//! `GET /agents` and `POST /agents/{id}/disable` (Plan.md P10.10, Appendix
//! B; ADR-0019, S31): every agent with its status for Admins and the Owner;
//! disabling one after a step-up, its certificates revoked and its live
//! stream ended before the answer; everyone else refused; the re-checks in
//! the transaction; the limits.

use core::num::NonZeroU32;

use chrono::TimeDelta;
use fleet_core::audit::{
    AuditAction, AuditMetadata, AuditOutcome, AuditTarget, RecordedMetadata, RecordedTarget,
};
use fleet_core::authz::Role;
use fleet_core::id::{AgentId, RequestId};
use fleet_core::pki::CertFingerprint;
use fleet_core::system::Clock as _;
use fleet_core::value::AgentName;
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::grpc::registry::{Halt, Lease};
use fleet_server::ports::agent_connections::{Admitted, StreamEnd};
use fleet_server::ports::agents::{NewAgent, NewAgentCert};
use fleet_server::ports::store::Store as _;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{mpsc, oneshot};

use super::support::{
    Meanwhile, Options, Person, Setup, T0, at, redacted, setup, setup_entry, setup_with,
};

const AGENTS: &str = "/api/v1/agents";

fn disable_path(id: AgentId) -> String {
    format!("{AGENTS}/{id}/disable")
}

impl Setup {
    /// Agent `n` named `agent-<n>`, enrolled at `T0` with a 60-day
    /// certificate `n`….
    async fn agent(&self, n: u8) -> AgentId {
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
                not_after: at(T0) + TimeDelta::days(60),
                created_at: at(T0),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        id
    }

    /// Agent `n`'s live stream, welcomed now.
    fn stream(
        &self,
        n: u8,
    ) -> (
        Lease,
        mpsc::Receiver<fleet_proto::generated::ServerMessage>,
        oneshot::Receiver<StreamEnd>,
    ) {
        let lease = self
            .registry
            .claim(&Admitted {
                request: RequestId::new_v7(at(T0), [n; 10]).unwrap(),
                agent: AgentId::new_v7(at(T0), [n; 10]).unwrap(),
                name: AgentName::try_from(format!("agent-{n}").as_str()).unwrap(),
                fingerprint: CertFingerprint::from_digest([n; 32]),
                not_after: at(T0) + TimeDelta::days(60),
                peer: None,
            })
            .unwrap();
        let (queue, queued) = mpsc::channel(4);
        let ended = self
            .registry
            .activate(
                &lease,
                fleet_server::grpc::registry::Activation {
                    queue,
                    halt: Halt::default(),
                    connected_at: self.clock.now(),
                    capacity: fleet_core::value::AgentCapacity::try_from(10_u32).unwrap(),
                    running: Vec::new(),
                    welcome: fleet_proto::generated::ServerMessage::default(),
                },
            )
            .unwrap();
        (lease, queued, ended)
    }

    async fn enabled(&self, id: AgentId) -> bool {
        self.db.agents().find(id).await.unwrap().unwrap().enabled
    }

    async fn disables(&self) -> usize {
        self.audit()
            .await
            .iter()
            .filter(|entry| entry.action == AuditAction::AGENT_DISABLE)
            .count()
    }

    async fn disable(&self, id: AgentId, by: &Person) -> super::support::Seen {
        self.post(&disable_path(id), Some(&by.token), None).await
    }
}

/// `role`'s person `n`, stepped up now.
async fn stepped_up(setup: &Setup, n: u8, name: &str, role: Role) -> Person {
    let person = setup.person(n, name, role).await;
    setup.stamp(person.session).await;
    person
}

#[sqlx::test(migrations = false)]
async fn admins_and_the_owner_list_every_agent_with_its_status(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let owner = setup.person(3, "owner", Role::Owner).await;
    let first = setup.agent(1).await;
    let second = setup.agent(2).await;
    let (_lease, _queued, _ended) = setup.stream(2);

    let page = setup
        .get(&format!("{AGENTS}?limit=1"), Some(&admin.token))
        .await;
    let rest = setup
        .get(
            &format!("{AGENTS}?before={second}&limit=1"),
            Some(&owner.token),
        )
        .await;

    assert_eq!(page.status, 200, "{}", page.text);
    assert_eq!(page.body["items"][0]["id"], json!(second.to_string()));
    assert_eq!(page.body["items"][0]["status"], json!("online"));
    assert_eq!(page.body["next"], json!(second.to_string()));
    assert_eq!(rest.status, 200, "{}", rest.text);
    assert_eq!(rest.body["items"][0]["status"], json!("offline"));
    assert!(rest.body.get("next").is_none(), "{}", rest.text);
    let all = setup.get(AGENTS, Some(&admin.token)).await;
    insta::assert_json_snapshot!(
        "agents_page",
        redacted(
            &all.body,
            &[
                (first.to_string(), "agent-1"),
                (second.to_string(), "agent-2"),
            ]
        )
    );
}

#[sqlx::test(migrations = false)]
async fn a_member_may_not_list_or_disable_agents(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let member = stepped_up(&setup, 4, "member", Role::Member).await;
    let agent = setup.agent(1).await;

    let listed = setup.get(AGENTS, Some(&member.token)).await;
    let disabled = setup.disable(agent, &member).await;

    for seen in [&listed, &disabled] {
        assert_eq!(seen.status, 403, "{}", seen.text);
        assert_eq!(seen.code(), "forbidden");
    }
    assert!(setup.enabled(agent).await);
}

#[sqlx::test(migrations = false)]
async fn without_credentials_the_agents_routes_are_401(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let agent = setup.agent(1).await;

    let listed = setup.get(AGENTS, None).await;
    let disabled = setup.post(&disable_path(agent), None, None).await;

    assert_eq!(listed.status, 401, "{}", listed.text);
    assert_eq!(disabled.status, 401, "{}", disabled.text);
    assert!(setup.enabled(agent).await);
}

#[sqlx::test(migrations = false)]
async fn a_query_that_doesnt_parse_is_400_and_a_limit_out_of_range_422(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;

    let unparsed = setup
        .get(&format!("{AGENTS}?before=nonsense"), Some(&admin.token))
        .await;
    let zero = setup
        .get(&format!("{AGENTS}?limit=0"), Some(&admin.token))
        .await;
    let too_many = setup
        .get(&format!("{AGENTS}?limit=101"), Some(&admin.token))
        .await;

    assert_eq!(unparsed.status, 400, "{}", unparsed.text);
    for seen in [&zero, &too_many] {
        assert_eq!(seen.status, 422, "{}", seen.text);
    }
}

#[sqlx::test(migrations = false)]
async fn an_admin_disables_an_agent_whose_stream_ends_before_the_answer(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    // With the control plane off: an agent enrolled before is still
    // disabled.
    let setup = setup(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    let agent = setup.agent(1).await;
    let (_lease, _queued, mut ended) = setup.stream(1);

    let seen = setup.disable(agent, &admin).await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    assert_eq!(ended.try_recv().unwrap(), StreamEnd::Disabled);
    assert_eq!(seen.body["status"], json!("disabled"));
    assert_eq!(seen.body["enabled"], json!(false));
    insta::assert_json_snapshot!(
        "agent_disabled",
        redacted(&seen.body, &[(agent.to_string(), "agent-1")])
    );
    assert!(!setup.enabled(agent).await);
    let entry = setup.last(&AuditAction::AGENT_DISABLE).await;
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(admin.id));
    assert_eq!(entry.ip, Some("192.0.2.10".parse().unwrap()));
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
                .int("certificates_revoked", 1)
                .unwrap()
                .text("via", "http")
                .unwrap()
        )
    );
}

#[sqlx::test(migrations = false)]
async fn disabling_an_agent_that_never_connected_and_then_again_records_once(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        Options {
            control_plane: true,
            ..Options::default()
        },
    )
    .await;
    let owner = stepped_up(&setup, 3, "owner", Role::Owner).await;
    let agent = setup.agent(1).await;

    let first = setup.disable(agent, &owner).await;
    let again = setup.disable(agent, &owner).await;

    assert_eq!(first.status, 200, "{}", first.text);
    assert_eq!(again.status, 200, "{}", again.text);
    assert_eq!(again.body, first.body);
    assert_eq!(setup.disables().await, 1);
}

#[sqlx::test(migrations = false)]
async fn disabling_needs_a_fresh_step_up(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let admin = setup.person(2, "admin", Role::Admin).await;
    let agent = setup.agent(1).await;

    let seen = setup.disable(agent, &admin).await;

    assert_eq!(seen.status, 403, "{}", seen.text);
    assert_eq!(seen.code(), "step_up_required");
    assert!(setup.enabled(agent).await);
    assert_eq!(setup.disables().await, 0);
}

#[sqlx::test(migrations = false)]
async fn an_unknown_or_malformed_agent_is_404(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;

    let unknown = setup
        .disable(AgentId::new_v7(at(T0), [9; 10]).unwrap(), &admin)
        .await;
    let malformed = setup
        .post(
            &format!("{AGENTS}/not-an-id/disable"),
            Some(&admin.token),
            None,
        )
        .await;

    for seen in [&unknown, &malformed] {
        assert_eq!(seen.status, 404, "{}", seen.text);
        assert_eq!(seen.code(), "not_found");
    }
}

#[sqlx::test(migrations = false)]
async fn an_admin_demoted_or_logged_out_meanwhile_disables_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let demoted = stepped_up(&setup, 2, "admin", Role::Admin).await;
    let logged_out = stepped_up(&setup, 3, "other", Role::Admin).await;
    let agent = setup.agent(1).await;

    setup
        .hooked
        .queue(Meanwhile::SetRole(demoted.id, Role::Member));
    let refused = setup.disable(agent, &demoted).await;
    setup
        .hooked
        .queue(Meanwhile::RevokeSession(logged_out.session));
    let gone = setup.disable(agent, &logged_out).await;

    assert_eq!(refused.status, 403, "{}", refused.text);
    assert_eq!(refused.code(), "forbidden");
    assert_eq!(gone.status, 401, "{}", gone.text);
    assert!(setup.enabled(agent).await);
    assert_eq!(setup.disables().await, 0);
}

#[sqlx::test(migrations = false)]
async fn both_routes_spend_the_users_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        Options {
            limits: RateLimitConfig {
                per_user: Rate::per_second(NonZeroU32::MIN, NonZeroU32::MIN),
                ..RateLimitConfig::default()
            },
            ..Options::default()
        },
    )
    .await;
    let admin = stepped_up(&setup, 2, "admin", Role::Admin).await;
    let agent = setup.agent(1).await;
    assert_eq!(setup.get(AGENTS, Some(&admin.token)).await.status, 200);

    let listed = setup.get(AGENTS, Some(&admin.token)).await;
    let disabled = setup.disable(agent, &admin).await;

    assert_eq!(listed.status, 429, "{}", listed.text);
    assert_eq!(disabled.status, 429, "{}", disabled.text);
    assert!(setup.enabled(agent).await);
}
