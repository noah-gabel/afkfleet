//! The suite's port, its agents and their calls: the real listener on
//! `127.0.0.1:0` with a fresh PKI and database, the control service over
//! the real `AgentConnectionService`, and tonic's client over TLS.

use core::time::Duration;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget, RecordedValue};
use fleet_core::id::{AgentId, RequestId};
use fleet_core::pki::CertFingerprint;
use fleet_core::system::Clock as _;
use fleet_core::value::AgentName;
use fleet_proto::PROTOCOL_VERSION;
use fleet_proto::control::{RejectReason, ServerMessage, Welcome};
use fleet_proto::generated as wire;
use fleet_proto::generated::agent_control_client::AgentControlClient;
use fleet_server::app::agents::{AgentConnectionService, AgentService, Disabler};
use fleet_server::app::enrollment::EnrollmentService;
use fleet_server::config::{Heartbeats, RateLimitConfig};
use fleet_server::grpc::control::{ControlTimings, REQUEST_ID, Sessions, control_service};
use fleet_server::grpc::enrollment::enrollment_service;
use fleet_server::grpc::listener;
use fleet_server::grpc::registry::{AgentRegistry, SWEEP_INTERVAL};
use fleet_server::http::middleware::RequestIds;
use fleet_server::http::rate_limit::RateLimits;
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::infra::pki::{
    AgentIssuer, CertificateAuthority, Ed25519Key, LoadedPki, ServerNames, X509AgentNames,
};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::infra::system::SystemClock;
use fleet_server::ports::agent_connections::AgentConnections;
use fleet_server::ports::agents::{Agent as AgentRow, NewAgent, NewAgentCert};
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditRecord, NewAuditEntry};
use fleet_server::ports::store::Store as _;
use fleet_testkit::system::{ManualClock, SeededRandom};
use sqlx::sqlite::SqliteConnectOptions;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::metadata::MetadataMap;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tonic::{Status, Streaming};

/// How long a test waits for anything over the loopback.
pub(crate) const BOUND: Duration = Duration::from_secs(5);
/// The setup's own audit entries, which the checks leave out.
const SETUP: &str = "test.control_setup";
/// The version every hello of the suite names.
pub(crate) const AGENT_VERSION: &str = "0.1.0";
/// The capacity every hello of the suite names.
pub(crate) const CAPACITY: u32 = 10;

/// How a test's port is set up: production values, except what a test
/// lowers (ADR-0019, R20).
pub(crate) struct Settings {
    pub(crate) limits: RateLimitConfig,
    pub(crate) timings: ControlTimings,
    pub(crate) age: Duration,
    pub(crate) sweep: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            limits: RateLimitConfig::default(),
            timings: ControlTimings::new(Heartbeats::default()),
            age: listener::CONNECTION_AGE,
            sweep: SWEEP_INTERVAL,
        }
    }
}

impl Settings {
    /// Production values with heartbeats of `interval` and `timeout`
    /// seconds.
    pub(crate) fn heartbeats(interval: u32, timeout: u32) -> Self {
        Self {
            timings: ControlTimings::new(Heartbeats::try_from_secs(interval, timeout).unwrap()),
            ..Self::default()
        }
    }
}

/// A running port, its database, and what a client needs.
pub(crate) struct Port {
    pub(crate) address: SocketAddr,
    pub(crate) db: Database,
    pub(crate) clock: ManualClock,
    pub(crate) sessions: Sessions,
    pub(crate) registry: AgentRegistry,
    ca: CertificateAuthority,
    ca_pem: String,
    random: SeededRandom,
    stop: CancellationToken,
    server: JoinSet<()>,
}

impl Drop for Port {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// An agent of the suite: its ID, the name it enrolled with and the
/// certificate it connects with.
#[derive(Clone)]
pub(crate) struct Agent {
    pub(crate) id: AgentId,
    pub(crate) name: String,
    pub(crate) identity: Identity,
    pub(crate) fingerprint: CertFingerprint,
    pub(crate) der: Vec<u8>,
    pub(crate) pem: String,
    pub(crate) issued: DateTime<Utc>,
    pub(crate) not_after: DateTime<Utc>,
}

/// A fresh PKI at `now`: the CA, and the server's certificate for
/// localhost and 127.0.0.1.
pub(crate) fn pki(random: &SeededRandom, now: DateTime<Utc>) -> LoadedPki {
    let (ca, ca_cert) = CertificateAuthority::create(random, now).unwrap();
    let key = Ed25519Key::generate(random).unwrap();
    let names =
        ServerNames::try_new(&["localhost".to_owned()], &[IpAddr::from([127, 0, 0, 1])]).unwrap();
    let server = ca.issue_server(&key, &names, random, now).unwrap();
    LoadedPki {
        ca,
        ca_cert_pem: ca_cert.pem.into_bytes(),
        server_cert_pem: server.pem.into_bytes(),
        server_key_pem: zeroize::Zeroizing::new(key.to_pem().as_bytes().to_vec()),
        server_not_after: server.validity.not_after,
    }
}

/// A second handle on `ca`: the test signs with one, the enrollment with
/// the other.
pub(crate) fn copy(ca: &CertificateAuthority) -> CertificateAuthority {
    CertificateAuthority::from_parts(
        ca.cert_der().clone(),
        Ed25519Key::from_pem(ca.key().to_pem().as_bytes()).unwrap(),
    )
    .unwrap()
}

/// `issued` as an agent's identity, with `key`.
pub(crate) fn identity(pem: &str, key: &Ed25519Key) -> Identity {
    Identity::from_pem(pem, key.to_pem().as_bytes())
}

pub(crate) async fn start(options: SqliteConnectOptions, settings: Settings) -> Port {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let db = Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap();
    // The certificates' validity is checked against the real time by TLS,
    // and against this clock by the server. It starts on a whole
    // millisecond, which is what the database keeps.
    let clock = ManualClock::new(
        DateTime::from_timestamp_millis(SystemClock.now().timestamp_millis()).unwrap(),
    );
    let random = SeededRandom::new(41);
    let pki = pki(&random, clock.now());
    let ca_pem = String::from_utf8(pki.ca_cert_pem.clone()).unwrap();
    let tls = listener::tls(&pki);
    let ca = copy(&pki.ca);
    let issuer = AgentIssuer::new(pki.ca, Arc::new(random.clone()), TimeDelta::days(60));
    let limits = RateLimits::new(&settings.limits, Arc::new(clock.clone()));
    let enrollment = enrollment_service(
        EnrollmentService::new(
            Arc::new(clock.clone()),
            Arc::new(random.clone()),
            Arc::new(db.clone()),
            Arc::new(Sha256Tokens::new(Arc::new(random.clone()))),
            Arc::new(issuer),
        ),
        limits.clone(),
        RequestIds::new(Arc::new(clock.clone()), Arc::new(random.clone())),
        fleet_proto::enrollment::SERVER_DEADLINE,
    );
    let sessions = Sessions::new(CancellationToken::new());
    let registry = AgentRegistry::new(Arc::new(clock.clone()));
    let connections: Arc<dyn AgentConnections> = Arc::new(AgentConnectionService::new(
        Arc::new(clock.clone()),
        Arc::new(db.clone()),
        Arc::new(X509AgentNames),
    ));
    let control = control_service(
        Arc::clone(&connections),
        Arc::new(clock.clone()),
        limits,
        RequestIds::new(Arc::new(clock.clone()), Arc::new(random.clone())),
        settings.timings,
        sessions.clone(),
        registry.clone(),
    );
    let router = listener::router(tls, enrollment, control, settings.timings.heartbeats).unwrap();
    let bound = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = bound.local_addr().unwrap();
    let stop = CancellationToken::new();
    let mut server = JoinSet::new();
    let serving = stop.child_token();
    let age = settings.age;
    server.spawn(async move {
        listener::serve(router, bound, age, serving).await.unwrap();
    });
    server.spawn(
        registry
            .clone()
            .sweep_every(connections, settings.sweep, stop.child_token()),
    );
    Port {
        address,
        db,
        clock,
        sessions,
        registry,
        ca,
        ca_pem,
        random,
        stop,
        server,
    }
}

fn setup_entry(now: DateTime<Utc>) -> NewAuditEntry {
    NewAuditEntry::new(
        now,
        AuditAction::try_from(SETUP).unwrap(),
        AuditOutcome::Success,
    )
}

impl Port {
    /// A certificate of this CA for agent `named`, issued at `issued` for
    /// `lifetime`, with a key from the port's randomness.
    pub(crate) fn certificate(
        &self,
        named: AgentId,
        issued: DateTime<Utc>,
        lifetime: TimeDelta,
    ) -> Agent {
        let key = Ed25519Key::generate(&self.random).unwrap();
        let cert = self
            .ca
            .issue_agent(&key, named, lifetime, &self.random, issued)
            .unwrap();
        Agent {
            id: named,
            name: String::new(),
            identity: identity(&cert.pem, &key),
            fingerprint: cert.fingerprint,
            der: cert.der.to_vec(),
            pem: cert.pem,
            issued,
            not_after: cert.validity.not_after,
        }
    }

    /// Makes the port's next draw of randomness fail.
    pub(crate) fn fail_next_random(&self) {
        self.random
            .fail_next(fleet_core::system::RandomError::Unavailable);
    }

    /// Agent `n`'s ID.
    pub(crate) fn agent_id(&self, n: u8) -> AgentId {
        AgentId::new_v7(self.clock.now(), [n; 10]).unwrap()
    }

    /// Stores `certificate` under a new agent `n` named `name`, both
    /// created when the certificate was issued.
    pub(crate) async fn store(&self, n: u8, name: &str, certificate: Agent) -> Agent {
        let id = self.agent_id(n);
        let now = certificate.issued;
        let mut tx = self.db.write().await.unwrap();
        tx.agents()
            .insert(&NewAgent {
                id,
                name: AgentName::try_from(name).unwrap(),
                created_at: now,
            })
            .await
            .unwrap();
        tx.agents()
            .insert_cert(&NewAgentCert {
                fingerprint: certificate.fingerprint,
                agent: id,
                not_after: certificate.not_after,
                created_at: now,
            })
            .await
            .unwrap();
        tx.commit(setup_entry(now)).await.unwrap();
        Agent {
            id,
            name: name.to_owned(),
            ..certificate
        }
    }

    /// Enrolls agent `n` named `name`: its row and a 60-day certificate
    /// naming it.
    pub(crate) async fn enroll(&self, n: u8, name: &str) -> Agent {
        let certificate = self.certificate(self.agent_id(n), self.clock.now(), TimeDelta::days(60));
        self.store(n, name, certificate).await
    }

    /// A certificate of this CA that no agent holds.
    pub(crate) fn stranger(&self, n: u8) -> Agent {
        self.certificate(self.agent_id(n), self.clock.now(), TimeDelta::days(60))
    }

    pub(crate) async fn revoke(&self, agent: &Agent) {
        let id = agent.id.as_uuid().hyphenated().to_string();
        sqlx::query!(
            "UPDATE agent_certs SET revoked_at = '2027-01-15T08:00:00.000Z' WHERE agent_id = ?",
            id
        )
        .execute(self.db.write_pool())
        .await
        .unwrap();
    }

    pub(crate) async fn disable(&self, agent: &Agent) {
        let id = agent.id.as_uuid().hyphenated().to_string();
        sqlx::query!("UPDATE agents SET enabled = 0 WHERE id = ?", id)
            .execute(self.db.write_pool())
            .await
            .unwrap();
    }

    /// Disables `agent` as `afkfleet-server agent disable` does beside the
    /// running server: through the agents' service, which revokes every
    /// certificate too, with a registry of its own that holds no stream, so
    /// only the port's sweep can end the stream.
    pub(crate) async fn host_disable(&self, agent: &Agent) {
        let clock = Arc::new(self.clock.clone());
        AgentService::new(
            clock.clone(),
            Arc::new(self.db.clone()),
            Arc::new(AgentRegistry::new(clock)),
        )
        .disable(Disabler::Host, agent.id)
        .await
        .unwrap();
    }

    /// Agent `agent`'s row.
    pub(crate) async fn row(&self, agent: &Agent) -> AgentRow {
        self.db.agents().find(agent.id).await.unwrap().unwrap()
    }

    /// A client of the port at `address`, trusting only the port's CA,
    /// presenting `identity` if given.
    pub(crate) async fn client_at(
        &self,
        address: SocketAddr,
        identity: Option<&Identity>,
    ) -> Result<AgentControlClient<Channel>, tonic::transport::Error> {
        let mut tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(&self.ca_pem))
            .domain_name("localhost")
            .timeout(BOUND);
        if let Some(identity) = identity {
            tls = tls.identity(identity.clone());
        }
        let channel = Endpoint::from_shared(format!("https://127.0.0.1:{}", address.port()))?
            .tls_config(tls)?
            .connect_timeout(BOUND)
            .connect()
            .await?;
        Ok(AgentControlClient::new(channel))
    }

    /// A client of the port presenting `identity` if given.
    pub(crate) async fn client(
        &self,
        identity: Option<&Identity>,
    ) -> Result<AgentControlClient<Channel>, tonic::transport::Error> {
        self.client_at(self.address, identity).await
    }

    /// Opens a `Connect` stream with `agent`'s certificate.
    pub(crate) async fn open(&self, agent: &Agent) -> Result<Call, Status> {
        let client = self
            .client(Some(&agent.identity))
            .await
            .map_err(|error| Status::unavailable(error.to_string()))?;
        Call::open(client).await
    }

    /// Opens `agent`'s stream, sends its hello and reads `Welcome`.
    pub(crate) async fn welcomed(&self, agent: &Agent) -> (Call, Welcome) {
        let mut call = self.open(agent).await.unwrap();
        call.send(hello(&agent.name)).await;
        let welcome = call.welcome().await;
        (call, welcome)
    }

    /// Every audit entry but the setup's, newest first.
    pub(crate) async fn audit(&self) -> Vec<AuditRecord> {
        self.db
            .audit()
            .list(AuditPage {
                before: None,
                limit: AuditLimit::new(100).unwrap(),
            })
            .await
            .unwrap()
            .into_iter()
            .filter(|record| record.action.as_str() != SETUP)
            .collect()
    }

    /// The entries of `action`, newest first.
    pub(crate) async fn entries(&self, action: &AuditAction) -> Vec<AuditRecord> {
        self.audit()
            .await
            .into_iter()
            .filter(|record| &record.action == action)
            .collect()
    }

    /// The entries of `action` once there are `count`, waiting within
    /// [`BOUND`]: a stream's end is recorded after its last answer.
    pub(crate) async fn entries_once(
        &self,
        action: &AuditAction,
        count: usize,
    ) -> Vec<AuditRecord> {
        tokio::time::timeout(BOUND, async {
            loop {
                let entries = self.entries(action).await;
                if entries.len() >= count {
                    return entries;
                }
                // The session writes it on its own task; there's nothing
                // to wait on but the database.
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }

    /// Waits within [`BOUND`] until `agent.disconnect` is recorded for
    /// `agent`, and returns it. The session records it after its last
    /// answer, so a client sees the end first.
    pub(crate) async fn disconnect(&self, agent: &Agent) -> AuditRecord {
        tokio::time::timeout(BOUND, async {
            loop {
                let found = self
                    .entries(&AuditAction::AGENT_DISCONNECT)
                    .await
                    .into_iter()
                    .find(|record| on(record, agent));
                if let Some(found) = found {
                    return found;
                }
                // The session writes it on its own task; there's nothing
                // to wait on but the database.
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }

    /// Stops the port as serve does: the sessions end with the server's
    /// stop, so no open stream holds tonic's drain.
    pub(crate) async fn stop(mut self) {
        self.stop.cancel();
        tokio::time::timeout(BOUND, self.sessions.shutdown())
            .await
            .unwrap();
        while let Some(ended) = tokio::time::timeout(BOUND, self.server.join_next())
            .await
            .unwrap()
        {
            ended.unwrap();
        }
    }
}

/// Whether `record` is about `agent`.
pub(crate) fn on(record: &AuditRecord, agent: &Agent) -> bool {
    record.target == Some(RecordedTarget::Known(AuditTarget::Agent(agent.id)))
}

/// `record`'s metadata value under `key`, as text.
pub(crate) fn text(record: &AuditRecord, key: &str) -> Option<String> {
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

/// An open `Connect` stream: what the test sends, and what it reads. It
/// keeps its client, so the connection stays open from the client's side
/// after the stream ends.
pub(crate) struct Call {
    pub(crate) sender: Option<mpsc::Sender<wire::AgentMessage>>,
    pub(crate) answers: Streaming<wire::ServerMessage>,
    pub(crate) metadata: MetadataMap,
    _client: AgentControlClient<Channel>,
}

impl core::fmt::Debug for Call {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Call")
            .field("metadata", &self.metadata)
            .finish_non_exhaustive()
    }
}

impl Call {
    /// `Connect` on `client`; the stream's headers have arrived when it
    /// returns.
    pub(crate) async fn open(mut client: AgentControlClient<Channel>) -> Result<Self, Status> {
        let (sender, receiver) = mpsc::channel(16);
        let response = tokio::time::timeout(BOUND, client.connect(ReceiverStream::new(receiver)))
            .await
            .unwrap()?;
        let metadata = response.metadata().clone();
        Ok(Self {
            sender: Some(sender),
            answers: response.into_inner(),
            metadata,
            _client: client,
        })
    }

    /// The request ID in the stream's headers.
    pub(crate) fn request_id(&self) -> RequestId {
        request_id(&self.metadata)
    }

    pub(crate) async fn send(&self, message: wire::AgentMessage) {
        tokio::time::timeout(BOUND, self.sender.as_ref().unwrap().send(message))
            .await
            .unwrap()
            .unwrap();
    }

    /// Ends what the agent sends; the stream stays open the other way.
    pub(crate) fn close(&mut self) {
        self.sender = None;
    }

    /// The next answer, within [`BOUND`].
    pub(crate) async fn next(&mut self) -> Result<Option<ServerMessage>, Status> {
        let message = tokio::time::timeout(BOUND, self.answers.message())
            .await
            .unwrap()?;
        Ok(message.map(|message| ServerMessage::try_from(message).unwrap()))
    }

    pub(crate) async fn welcome(&mut self) -> Welcome {
        match self.next().await {
            Ok(Some(ServerMessage::Welcome(welcome))) => welcome,
            other => panic!("expected Welcome, got {other:?}"),
        }
    }

    /// The `Reject` the stream ends with; it must end OK after it.
    pub(crate) async fn rejected(&mut self) -> RejectReason {
        let reason = match self.next().await {
            Ok(Some(ServerMessage::Reject(reason))) => reason,
            other => panic!("expected Reject, got {other:?}"),
        };
        let after = self.next().await;
        assert!(matches!(after, Ok(None)), "{after:?}");
        reason
    }

    /// The status the stream ends with.
    pub(crate) async fn ended(&mut self) -> Status {
        match self.next().await {
            Err(status) => status,
            other => panic!("expected a status, got {other:?}"),
        }
    }
}

/// The request ID in `metadata`.
pub(crate) fn request_id(metadata: &MetadataMap) -> RequestId {
    metadata
        .get(REQUEST_ID)
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap()
}

/// A hello for `name`.
pub(crate) fn hello(name: &str) -> wire::AgentMessage {
    hello_with(name, PROTOCOL_VERSION, CAPACITY)
}

/// A hello for `name` speaking `protocol_version` with `capacity`.
pub(crate) fn hello_with(name: &str, protocol_version: u32, capacity: u32) -> wire::AgentMessage {
    wire::AgentMessage {
        msg: Some(wire::agent_message::Msg::Hello(wire::Hello {
            name: name.to_owned(),
            agent_version: AGENT_VERSION.to_owned(),
            protocol_version,
            capacity,
            running: Vec::new(),
        })),
    }
}

pub(crate) fn heartbeat() -> wire::AgentMessage {
    wire::AgentMessage {
        msg: Some(wire::agent_message::Msg::Heartbeat(wire::Heartbeat {})),
    }
}

/// A message of exactly `total` bytes on the wire: an event whose bot ID is
/// `total - 10` bytes, which converts to nothing.
pub(crate) fn sized(total: usize) -> wire::AgentMessage {
    // The event's field: 1 tag byte, a 4-byte length, the bytes. The
    // envelope's: 1 tag byte, a 4-byte length, the event. Both lengths take
    // 4 bytes from 2^21 to 2^28.
    let filler = total - 10;
    assert!((1 << 21..1 << 28).contains(&filler), "{total}");
    wire::AgentMessage {
        msg: Some(wire::agent_message::Msg::Event(wire::BotEvent {
            bot_id: vec![7; filler],
            ..wire::BotEvent::default()
        })),
    }
}
