//! The session grants over the real session-token service and the Microsoft
//! fake (P10.6; ADR-0019, S59, S60, U14; Phase 9 group E's note): a bot held
//! by a live stream of the registry, its answers read off that stream's
//! queue as its agent would read them.

use core::num::NonZeroU64;
use core::time::Duration;
use std::sync::Arc;

use fleet_core::account::AccountStatus;
use fleet_core::bot::DesiredRunState;
use fleet_core::id::{AccountId, AgentId, BotId, RequestId};
use fleet_core::mc::SessionCredentials;
use fleet_core::mode::ModePreset;
use fleet_core::pki::CertFingerprint;
use fleet_core::value::{AgentCapacity, AgentName, ServerAddress};
use fleet_proto::control::ServerMessage;
use fleet_proto::generated as wire;
use fleet_server::app::agents::SessionGrantService;
use fleet_server::grpc::registry::{Activation, AgentRegistry, Halt, Hold, HoldEntry, Lease};
use fleet_server::ports::agent_connections::Admitted;
use fleet_server::ports::agents::{NewAgent, NewAgentCert};
use fleet_server::ports::bots::NewBot;
use fleet_server::ports::session_grants::{GrantRequest, GrantStream, SessionGrants as _};
use fleet_server::ports::store::Store as _;
use secrecy::ExposeSecret as _;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{mpsc, oneshot};

use crate::support::msauth::{Call, Script, gated};
use crate::support::service::{
    DAY, PROFILE, Setup, T0, at, bounded, mc_session, ms_tokens, setup_entry,
};

/// The bot, its agent's live stream, and the grants that answer on it.
struct Rig {
    grants: SessionGrantService,
    stream: GrantStream,
    queued: mpsc::Receiver<wire::ServerMessage>,
    /// Keeps the stream's slot.
    _lease: Lease,
    _ended: oneshot::Receiver<fleet_server::ports::agent_connections::StreamEnd>,
}

impl Rig {
    /// The next answer the stream's agent reads.
    async fn answer(&mut self) -> ServerMessage {
        let message = bounded(self.queued.recv()).await.unwrap();
        ServerMessage::try_from(message).unwrap()
    }

    async fn grant(&self, number: u64, fresh: bool) {
        let request = GrantRequest {
            number: NonZeroU64::new(number).unwrap(),
            bot: bot(),
            fresh,
        };
        self.grants.grant(self.stream, request).await;
    }
}

fn bot() -> BotId {
    BotId::new_v7(at(T0), [8; 10]).unwrap()
}

/// The rig's agent.
fn agent() -> AgentId {
    AgentId::new_v7(at(T0), [7; 10]).unwrap()
}

/// The certificate the rig's agent connects with.
const FINGERPRINT: [u8; 32] = [7; 32];

/// When the rig's agent's certificate expires.
fn not_after() -> chrono::DateTime<chrono::Utc> {
    at(T0 + 60 * DAY)
}

/// The rig's rows: an enrolled agent, and `account`'s bot, set to run and
/// assigned to it.
async fn rows(setup: &Setup, account: AccountId) {
    let now = at(T0);
    let agent = agent();
    let fingerprint = CertFingerprint::from_digest(FINGERPRINT);
    let not_after = not_after();
    let mut tx = setup.db.write().await.unwrap();
    tx.agents()
        .insert(&NewAgent {
            id: agent,
            name: AgentName::try_from("agent-1").unwrap(),
            created_at: now,
        })
        .await
        .unwrap();
    tx.agents()
        .insert_cert(&NewAgentCert {
            fingerprint,
            agent,
            not_after,
            created_at: now,
        })
        .await
        .unwrap();
    tx.bots()
        .insert(&NewBot {
            id: bot(),
            account,
            mode: ModePreset::Afk.id(),
        })
        .await
        .unwrap();
    let server = ServerAddress::try_from("play.example.com").unwrap();
    assert!(tx.bots().set_server(bot(), &server).await.unwrap());
    assert!(
        tx.bots()
            .set_desired(bot(), DesiredRunState::Running)
            .await
            .unwrap()
    );
    assert!(tx.bots().assign(bot(), agent).await.unwrap());
    tx.commit(setup_entry()).await.unwrap();
}

/// `account`'s bot, set to run and assigned to an enrolled agent whose live
/// stream holds it.
async fn rig(setup: &Setup, account: AccountId) -> Rig {
    rows(setup, account).await;
    let now = at(T0);
    let agent = agent();
    let fingerprint = CertFingerprint::from_digest(FINGERPRINT);
    let not_after = not_after();
    let registry = AgentRegistry::new(setup.clock.clone());
    let lease = registry
        .claim(&Admitted {
            request: RequestId::new_v7(now, [7; 10]).unwrap(),
            agent,
            name: AgentName::try_from("agent-1").unwrap(),
            fingerprint,
            not_after,
            peer: None,
        })
        .unwrap();
    let (queue, mut queued) = mpsc::channel(16);
    let ended = registry
        .activate(
            &lease,
            Activation {
                queue,
                halt: Halt::default(),
                connected_at: now,
                capacity: AgentCapacity::try_from(10_u32).unwrap(),
                running: Vec::new(),
                welcome: wire::ServerMessage::default(),
            },
        )
        .unwrap();
    registry
        .settle(
            agent,
            lease.generation(),
            vec![(
                bot(),
                HoldEntry {
                    hold: Hold::Assigned,
                    since: now,
                    sent: None,
                    stopping: None,
                },
            )],
            ServerMessage::Reconcile(Vec::new()),
        )
        .unwrap();
    // The welcome and the full reconcile.
    queued.try_recv().unwrap();
    queued.try_recv().unwrap();
    Rig {
        grants: SessionGrantService::new(
            setup.clock.clone(),
            setup.store.clone(),
            Arc::new(registry),
            Arc::new(setup.service.clone()),
            false,
        ),
        stream: GrantStream {
            agent,
            generation: lease.generation(),
            fingerprint,
            request: RequestId::new_v7(now, [9; 10]).unwrap(),
        },
        queued,
        _lease: lease,
        _ended: ended,
    }
}

fn token(answer: &ServerMessage) -> String {
    match answer {
        ServerMessage::SessionGrant(grant) => match &grant.credentials {
            SessionCredentials::Online { access_token, .. } => {
                access_token.expose_secret().to_owned()
            }
            SessionCredentials::Offline { .. } => panic!("an online session"),
        },
        other => panic!("a grant: {other:?}"),
    }
}

/// Phase 9 group E's note, end to end: an account linked again while a
/// grant's refresh runs answers `ChangedMeanwhile`, a retryable denial, and
/// the agent's next request gets a session minted from the new link's
/// token.
#[sqlx::test(migrations = false)]
async fn a_link_again_during_a_grants_refresh_is_a_retryable_denial_and_the_retry_uses_the_new_token(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let account = setup
        .link(1, "AfkOnline1", PROFILE, AccountStatus::Active, "refresh-0")
        .await;
    let mut rig = rig(&setup, account).await;
    let (script, mut gate) = gated();
    setup.fake.script_refresh(script);

    bounded(async {
        tokio::join!(rig.grant(1, false), async {
            gate.entered().await;
            setup.relink(account, "relinked").await;
            gate.release(Ok(ms_tokens("access-1", "refresh-1")));
        })
    })
    .await;
    let first = rig.answer().await;
    // Past any backoff the failed refresh started.
    setup.clock.advance(Duration::from_mins(10));
    setup
        .fake
        .script_refresh(Script::Answer(Ok(ms_tokens("access-2", "refresh-2"))));
    setup.fake.script_session(Script::Answer(Ok(mc_session(
        "token-from-the-new-link",
        PROFILE,
        "AfkOnline1",
    ))));
    bounded(rig.grant(2, false)).await;
    let second = rig.answer().await;

    assert!(
        matches!(&first, ServerMessage::SessionDenied(denied) if denied.retryable && denied.id.get().get() == 1),
        "{first:?}"
    );
    assert_eq!(token(&second), "token-from-the-new-link");
    assert_eq!(
        setup.fake.calls(),
        [
            Call::Refresh {
                refresh_token: "refresh-0".to_owned()
            },
            Call::Refresh {
                refresh_token: "relinked".to_owned()
            },
            Call::MinecraftSession {
                access_token: "access-2".to_owned()
            },
        ]
    );
}

/// ADR-0018's margin through a grant: a cached session goes out while more
/// than 10 minutes are left, and no sooner a new one is minted.
#[sqlx::test(migrations = false)]
async fn a_cached_session_goes_out_only_with_more_than_ten_minutes_left(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let account = setup
        .link(1, "AfkOnline1", PROFILE, AccountStatus::Active, "refresh-0")
        .await;
    let mut rig = rig(&setup, account).await;
    for (access, refresh, minted) in [
        ("access-1", "refresh-1", "token-1"),
        ("access-2", "refresh-2", "token-2"),
    ] {
        setup
            .fake
            .script_refresh(Script::Answer(Ok(ms_tokens(access, refresh))));
        setup.fake.script_session(Script::Answer(Ok(mc_session(
            minted,
            PROFILE,
            "AfkOnline1",
        ))));
    }

    bounded(rig.grant(1, false)).await;
    let first = rig.answer().await;
    setup.clock.set(at(T0 + DAY - 600_001));
    bounded(rig.grant(2, false)).await;
    let cached = rig.answer().await;
    setup.clock.set(at(T0 + DAY - 600_000));
    bounded(rig.grant(3, false)).await;
    let renewed = rig.answer().await;

    assert_eq!(token(&first), "token-1");
    assert_eq!(token(&cached), "token-1", "10 min and 1 ms left");
    assert_eq!(token(&renewed), "token-2", "10 min left: a new one");
    assert_eq!(setup.fake.calls().len(), 4, "two refreshes, two sessions");
}

#[sqlx::test(migrations = false)]
async fn a_fresh_request_gets_a_session_minted_after_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let account = setup
        .link(1, "AfkOnline1", PROFILE, AccountStatus::Active, "refresh-0")
        .await;
    let mut rig = rig(&setup, account).await;
    for (access, refresh, minted) in [
        ("access-1", "refresh-1", "token-1"),
        ("access-2", "refresh-2", "token-2"),
    ] {
        setup
            .fake
            .script_refresh(Script::Answer(Ok(ms_tokens(access, refresh))));
        setup.fake.script_session(Script::Answer(Ok(mc_session(
            minted,
            PROFILE,
            "AfkOnline1",
        ))));
    }

    bounded(rig.grant(1, false)).await;
    let first = rig.answer().await;
    bounded(rig.grant(2, true)).await;
    let fresh = rig.answer().await;

    assert_eq!(token(&first), "token-1");
    assert_eq!(
        token(&fresh),
        "token-2",
        "the rejected session isn't reused"
    );
}

/// U14: the session-token service's flight reads the owner again once it
/// holds the account's lock, so an owner disabled while a grant waits comes
/// back as its `OwnerDisabled`, which a grant answers retryable, as it does
/// one it finds itself; the bot then comes back without a Reset.
#[sqlx::test(migrations = false)]
async fn an_owner_disabled_while_a_grant_waits_is_a_retryable_denial(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let account = setup
        .link(1, "AfkOnline1", PROFILE, AccountStatus::Active, "refresh-0")
        .await;
    let mut rig = rig(&setup, account).await;
    // The request's own read passes; its flight's waits.
    let mut gate = setup.store.gate_account_read_after(1);

    bounded(async {
        tokio::join!(rig.grant(1, false), async {
            gate.entered().await;
            setup.set_disabled(setup.owner, true).await;
            gate.release();
        })
    })
    .await;
    let answer = rig.answer().await;

    assert!(
        matches!(&answer, ServerMessage::SessionDenied(denied) if denied.retryable),
        "{answer:?}"
    );
    assert_eq!(setup.fake.calls(), [], "Microsoft was never asked");
}
