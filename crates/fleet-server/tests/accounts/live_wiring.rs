//! serve's wiring of the live events' hints (ADR-0020, S42; PR #67's
//! review): every service that tells the sockets what changed is handed
//! the app's own bus by `app_with`. Each test subscribes to `App.events`,
//! makes one change through serve's app, and waits for that change's hint;
//! taking any one service's `live_hints` out of `cli/serve.rs` turns its
//! test red. The bots' and the grants' services are fleet-client's serve
//! table's.

use core::time::Duration;
use std::collections::BTreeMap;
use std::sync::Arc;

use axum::http::Method;
use fleet_core::account::AccountStatus;
use fleet_core::authz::Role;
use fleet_core::id::{AccountId, AgentId, BotId, ModeId, RequestId};
use fleet_core::value::{AgentCapacity, AgentName, AgentVersion};
use fleet_proto::control::{ServerMessage, Welcome};
use fleet_proto::generated as wire;
use fleet_server::app::events::BusEvent;
use fleet_server::grpc::registry::{Activation, Halt};
use fleet_server::infra::crypto::vault::{VaultKeys, XChaChaVault};
use fleet_server::infra::pki::{CertificateAuthority, Ed25519Key};
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::agent_connections::Presented;
use fleet_server::ports::agents::NewAgentCert;
use fleet_server::ports::msauth::{PollOutcome, RefreshError};
use fleet_server::ports::store::Store as _;
use fleet_server::ports::vault::{Vault as _, VaultPurpose};
use fleet_testkit::system::SeededRandom;
use secrecy::SecretBox;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::support::http::HttpSetup;
use crate::support::link::USER_CODE;
use crate::support::msauth::{Script, device_flow, session, tokens};
use crate::support::service::{DAY, T0, at, bounded, profile, setup_entry};

/// How long a hint may take: the app tells after its commit, at once.
const PATIENCE: Duration = Duration::from_secs(10);

/// Waits on `bus` for a hint that names `bot`, past every other event.
async fn bots_hint(bus: &mut Receiver<Arc<BusEvent>>, bot: BotId) {
    let waiting = async {
        loop {
            match bus.recv().await {
                Ok(event) => {
                    if let BusEvent::Bots(bots) = &*event
                        && bots.contains(&bot)
                    {
                        return;
                    }
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => panic!("the bus is gone"),
            }
        }
    };
    timeout(PATIENCE, waiting)
        .await
        .expect("the bot's hint in time");
}

/// Waits on `bus` for a hint about the mode `id`.
async fn mode_hint(bus: &mut Receiver<Arc<BusEvent>>, id: ModeId) {
    let waiting = async {
        loop {
            match bus.recv().await {
                Ok(event) => {
                    if let BusEvent::Mode(mode) = &*event
                        && mode.id == id
                    {
                        return;
                    }
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => panic!("the bus is gone"),
            }
        }
    };
    timeout(PATIENCE, waiting)
        .await
        .expect("the mode's hint in time");
}

/// The app's bus, as every socket reads it.
fn bus(setup: &HttpSetup) -> Receiver<Arc<BusEvent>> {
    setup.app.as_ref().unwrap().events.subscribe()
}

#[sqlx::test(migrations = false)]
async fn an_approval_tells_the_bus_its_bot(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let admin = setup.stepped_up(1, Role::Admin).await;
    let member = setup.person(2, Role::Member).await;
    let id = setup
        .add_account(member.id, 3, Some(AccountStatus::Pending))
        .await;
    let bot = setup.add_bot(id, 3).await;
    let mut bus = bus(&setup);

    let approved = setup
        .api(
            Method::POST,
            &format!("/accounts/{id}/approve"),
            &admin,
            None,
        )
        .await;

    assert_eq!(approved.status, 200, "{}", approved.text);
    bots_hint(&mut bus, bot).await;
}

#[sqlx::test(migrations = false)]
async fn a_new_mode_tells_the_bus(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = HttpSetup::new(options).await;
    let member = setup.person(2, Role::Member).await;
    let mut bus = bus(&setup);

    let created = setup
        .api(
            Method::POST,
            "/modes",
            &member,
            Some(json!({
                "name": "wiring",
                "visibility": "private",
                "steps": [{
                    "action": { "type": "select_hotbar_slot", "slot": 2 },
                    "schedule": { "type": "at_start" },
                    "probability": 100
                }]
            })),
        )
        .await;

    assert_eq!(created.status, 201, "{}", created.text);
    let id: ModeId = created.body["id"].as_str().unwrap().parse().unwrap();
    mode_hint(&mut bus, id).await;
}

#[sqlx::test(migrations = false)]
async fn disabling_a_user_tells_the_bus_their_bots(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let owner = setup.stepped_up(1, Role::Owner).await;
    let member = setup.person(2, Role::Member).await;
    let id = setup.add_account(member.id, 3, None).await;
    let bot = setup.add_bot(id, 3).await;
    let mut bus = bus(&setup);

    let disabled = setup
        .api(
            Method::PATCH,
            &format!("/users/{}", member.id),
            &owner,
            Some(json!({ "disabled": true })),
        )
        .await;

    assert_eq!(disabled.status, 200, "{}", disabled.text);
    bots_hint(&mut bus, bot).await;
}

#[sqlx::test(migrations = false)]
async fn disabling_an_agent_tells_the_bus_its_bots(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let admin = setup.stepped_up(1, Role::Admin).await;
    let member = setup.person(2, Role::Member).await;
    let id = setup.add_account(member.id, 3, None).await;
    let bot = setup.add_bot(id, 3).await;
    let agent = setup.assign_to_agent(bot, 4).await;
    let mut bus = bus(&setup);

    let disabled = setup
        .api(
            Method::POST,
            &format!("/agents/{agent}/disable"),
            &admin,
            None,
        )
        .await;

    assert_eq!(disabled.status, 200, "{}", disabled.text);
    bots_hint(&mut bus, bot).await;
}

/// An agent disabled, then released over the API (ADR-0020, S49).
#[sqlx::test(migrations = false)]
async fn releasing_an_agent_tells_the_bus_its_bots(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let admin = setup.stepped_up(1, Role::Admin).await;
    let member = setup.person(2, Role::Member).await;
    let id = setup.add_account(member.id, 3, None).await;
    let bot = setup.add_bot(id, 3).await;
    let agent = setup.assign_to_agent(bot, 4).await;
    let disabled = setup
        .api(
            Method::POST,
            &format!("/agents/{agent}/disable"),
            &admin,
            None,
        )
        .await;
    assert_eq!(disabled.status, 200, "{}", disabled.text);
    let mut bus = bus(&setup);

    let released = setup
        .api(
            Method::POST,
            &format!("/agents/{agent}/release"),
            &admin,
            None,
        )
        .await;

    assert_eq!(released.status, 200, "{}", released.text);
    bots_hint(&mut bus, bot).await;
}

#[sqlx::test(migrations = false)]
async fn a_links_commit_tells_the_bus_its_new_bot(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let member = setup.person(2, Role::Member).await;
    setup.fake.script_start(Script::Answer(Ok(device_flow(
        "device-code-1",
        USER_CODE,
        at(T0 + 900_000),
    ))));
    setup
        .fake
        .script_poll(Script::Answer(Ok(PollOutcome::Authorized(tokens(
            "access-1",
            "refresh-1",
            at(T0 + 3_600_000),
        )))));
    setup.fake.script_session(Script::Answer(Ok(session(
        "minecraft-1",
        &profile(3),
        "AfkBot3",
        at(T0 + DAY),
    ))));
    let mut bus = bus(&setup);

    let started = setup.start(&member).await;
    assert_eq!(started.status, 201, "{}", started.text);
    let flow = started.body["flow_id"].as_str().unwrap().parse().unwrap();
    let account = bounded(async {
        loop {
            if let Some(fleet_server::app::link_flows::LinkFlowStatus::Linked { account, .. }) =
                setup.link_flows.status(flow).map(|view| view.status)
            {
                return account;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;

    let bot = setup.db.bots().of_account(account).await.unwrap().unwrap();
    bots_hint(&mut bus, bot.id).await;
}

/// The control plane's CA, and an agent `n`'s certificate from it: its DER
/// and fingerprint, for its row.
fn plane(n: u8) -> (CertificateAuthority, AgentId, AgentCert) {
    let random = SeededRandom::new(31);
    let (ca, _) = CertificateAuthority::create(&random, at(T0)).unwrap();
    let agent = AgentId::new_v7(at(T0), [n; 10]).unwrap();
    let key = Ed25519Key::generate(&random).unwrap();
    let issued = ca
        .issue_agent(&key, agent, chrono::TimeDelta::days(60), &random, at(T0))
        .unwrap();
    let cert = AgentCert {
        der: issued.der.to_vec(),
        fingerprint: issued.fingerprint,
        not_after: issued.validity.not_after,
    };
    (ca, agent, cert)
}

/// An agent's certificate as its row and its handshake hold it.
struct AgentCert {
    der: Vec<u8>,
    fingerprint: fleet_core::pki::CertFingerprint,
    not_after: chrono::DateTime<chrono::Utc>,
}

/// A Member's offline account and its bot on agent `agent`, whose row holds
/// `cert`; the setup has the control plane on.
async fn on_the_plane(setup: &HttpSetup, agent: AgentId, cert: &AgentCert) -> BotId {
    let member = setup.person(2, Role::Member).await;
    let id = setup.add_account(member.id, 3, None).await;
    let bot = setup.add_bot(id, 3).await;
    // assign_to_agent names agent n after its byte, as `plane` does.
    assert_eq!(setup.assign_to_agent(bot, 4).await, agent);
    let mut tx = setup.db.write().await.unwrap();
    tx.agents()
        .insert_cert(&NewAgentCert {
            fingerprint: cert.fingerprint,
            agent,
            not_after: cert.not_after,
            created_at: at(T0),
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    bot
}

#[sqlx::test(migrations = false)]
async fn an_agents_hello_tells_the_bus_its_bots(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (ca, agent, cert) = plane(4);
    let setup = HttpSetup::with_control_plane(options, ca).await;
    let bot = on_the_plane(&setup, agent, &cert).await;
    let connections = Arc::clone(
        &setup
            .app
            .as_ref()
            .unwrap()
            .grpc
            .as_ref()
            .unwrap()
            .sweep
            .connections,
    );
    let mut bus = bus(&setup);

    let admitted = connections
        .admit(Presented {
            request: RequestId::new_v7(at(T0), [1; 10]).unwrap(),
            fingerprint: cert.fingerprint,
            certificate: cert.der.clone(),
            peer: None,
        })
        .await
        .unwrap();
    connections
        .hello(
            &admitted,
            AgentCapacity::try_from(5_u32).unwrap(),
            &AgentVersion::try_from("0.1.0").unwrap(),
            &[],
        )
        .await
        .unwrap();

    bots_hint(&mut bus, bot).await;
}

#[sqlx::test(migrations = false)]
async fn a_reconciler_pass_tells_the_bus_the_bots_it_held(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (ca, agent, cert) = plane(4);
    let mut setup = HttpSetup::with_control_plane(options, ca).await;
    let bot = on_the_plane(&setup, agent, &cert).await;
    let routes = setup.app.as_mut().unwrap().grpc.take().unwrap();
    let registry = routes.sweep.registry.clone();
    let stop = CancellationToken::new();
    let running = tokio::spawn(routes.reconciler.run(stop.clone()));
    let mut bus = bus(&setup);

    // The agent's stream, welcomed: the reconciler's full reconcile holds
    // its bot, which nothing else tells.
    let lease = registry
        .claim(&fleet_server::ports::agent_connections::Admitted {
            request: RequestId::new_v7(at(T0), [1; 10]).unwrap(),
            agent,
            name: AgentName::try_from("agent-4").unwrap(),
            fingerprint: cert.fingerprint,
            not_after: cert.not_after,
            peer: None,
        })
        .unwrap();
    let (queue, _queued) = mpsc::channel(8);
    let welcome = wire::ServerMessage::try_from(ServerMessage::Welcome(Welcome {
        heartbeat_interval: Duration::from_secs(10),
        agent_id: agent,
    }))
    .unwrap();
    let _ended = registry
        .activate(
            &lease,
            Activation {
                queue,
                halt: Halt::default(),
                connected_at: at(T0),
                capacity: AgentCapacity::try_from(5_u32).unwrap(),
                running: Vec::new(),
                welcome,
            },
        )
        .unwrap();

    bots_hint(&mut bus, bot).await;
    stop.cancel();
    timeout(PATIENCE, running).await.unwrap().unwrap();
}

#[sqlx::test(migrations = false)]
async fn a_session_tokens_status_mark_tells_the_bus_its_bot(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (ca, _, _) = plane(4);
    let setup = HttpSetup::with_control_plane(options, ca).await;
    let member = setup.person(2, Role::Member).await;
    let id = AccountId::new_v7(at(T0), [5; 10]).unwrap();
    let vault = XChaChaVault::new(
        VaultKeys::from_keys(
            BTreeMap::from([(1, SecretBox::new(Box::new([0x21; 32])))]),
            1,
        )
        .unwrap(),
        Arc::new(SeededRandom::new(1)),
    );
    let envelope = vault
        .seal(VaultPurpose::MsRefreshToken, id.into(), b"refresh-0")
        .unwrap();
    let mut tx = setup.db.write().await.unwrap();
    tx.accounts()
        .insert(&McAccount {
            id,
            owner: member.id,
            name: "AfkBot5".parse().unwrap(),
            created_at: at(T0),
            credential: AccountCredential::Microsoft {
                uuid: profile(5).parse().unwrap(),
                refresh_envelope: envelope,
                status: AccountStatus::Active,
            },
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    let bot = setup.add_bot(id, 5).await;
    setup
        .fake
        .script_refresh(Script::Answer(Err(RefreshError::InvalidGrant)));
    let mut bus = bus(&setup);

    let refused = bounded(
        setup
            .app
            .as_ref()
            .unwrap()
            .grpc
            .as_ref()
            .unwrap()
            .session_tokens
            .session(id, false),
    )
    .await;

    assert!(refused.is_err(), "{refused:?}");
    bots_hint(&mut bus, bot).await;
}
