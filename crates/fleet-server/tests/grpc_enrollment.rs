//! `AgentEnrollment.Enroll` over the gRPC port (Plan.md P10.3; ADR-0019,
//! S13, S14, S21): the real listener on `127.0.0.1:0` with a fresh PKI,
//! called by tonic's client over TLS.
//!
//! - A call without a client certificate reaches the handler and enrolls;
//!   a client certificate from another CA fails the handshake, and a
//!   plaintext connection gets nothing.
//! - The gate: the request ID on every answer, and the sixth call in a
//!   minute from one address refused before its body is read.
//! - The answers: one for every token problem, a malformed request, a
//!   taken name, a refused CSR, an oversized request, the deadline.
//! - Nothing secret in any log line of any target.
//!
//! Tests over real sockets run on real time, with short bounds and never a
//! sleep (CLAUDE.md); the services' clock is a `ManualClock` that starts at
//! the real time, so the certificates the client checks are valid.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::fmt::Write as _;
use core::time::Duration;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, mpsc};

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::id::{AgentId, RequestId};
use fleet_core::pki::ed25519_pkcs8;
use fleet_core::system::Clock as _;
use fleet_core::value::AgentName;
use fleet_proto::enrollment::{MAX_REQUEST_BYTES, SERVER_DEADLINE};
use fleet_proto::generated::EnrollRequest;
use fleet_proto::generated::agent_enrollment_client::AgentEnrollmentClient;
use fleet_server::app::agents::AgentConnectionService;
use fleet_server::app::enrollment::{Creator, EnrollmentService, EnrollmentTokenService};
use fleet_server::config::Heartbeats;
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::grpc::control::{ControlTimings, Sessions, control_service};
use fleet_server::grpc::enrollment::{
    INVALID_CSR, INVALID_REQUEST, INVALID_TOKEN, NAME_TAKEN, RATE_LIMITED, REQUEST_ID, RETRY_AFTER,
    TOO_SLOW, enrollment_service,
};
use fleet_server::grpc::listener;
use fleet_server::grpc::registry::AgentRegistry;
use fleet_server::http::middleware::RequestIds;
use fleet_server::http::rate_limit::RateLimits;
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::infra::pki::{
    AgentIssuer, CertificateAuthority, Ed25519Key, LoadedPki, ServerNames, X509AgentNames,
    fingerprint,
};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::infra::system::SystemClock;
use fleet_server::ports::agents::NewAgent;
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditRecord, NewAuditEntry};
use fleet_server::ports::pki::{AgentCertificates, IssueError, IssuedCertificate};
use fleet_server::ports::store::Store as _;
use fleet_server::ports::tokens::{TokenKind, Tokens as _};
use fleet_testkit::system::{ManualClock, SeededRandom};
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use secrecy::{ExposeSecret as _, SecretString};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tonic::{Code, Status};

/// How long a test waits for anything over the loopback.
const BOUND: Duration = Duration::from_secs(5);

/// A running port, its database, and what a client needs.
struct Port {
    address: SocketAddr,
    db: Database,
    clock: ManualClock,
    tokens: EnrollmentTokenService,
    ca_pem: String,
    stop: CancellationToken,
    server: JoinSet<()>,
}

impl Drop for Port {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// The CA's signer, or one that waits for `release` before signing.
#[derive(Debug)]
struct Signer {
    issuer: AgentIssuer,
    release: Option<Mutex<mpsc::Receiver<()>>>,
}

impl AgentCertificates for Signer {
    fn issue(
        &self,
        csr_der: &[u8],
        agent: AgentId,
        now: DateTime<Utc>,
    ) -> Result<IssuedCertificate, IssueError> {
        if let Some(release) = &self.release {
            let _ = release.lock().unwrap().recv_timeout(BOUND);
        }
        self.issuer.issue(csr_der, agent, now)
    }

    fn ca_fingerprint(&self) -> fleet_core::pki::CertFingerprint {
        self.issuer.ca_fingerprint()
    }
}

/// How a test's port is set up.
struct Settings {
    limits: RateLimitConfig,
    deadline: Duration,
    release: Option<mpsc::Receiver<()>>,
}

impl Default for Settings {
    fn default() -> Self {
        let loose = Rate::per_second(
            core::num::NonZeroU32::new(1000).unwrap(),
            core::num::NonZeroU32::new(1000).unwrap(),
        );
        Self {
            limits: RateLimitConfig {
                enroll: loose,
                ..RateLimitConfig::default()
            },
            deadline: SERVER_DEADLINE,
            release: None,
        }
    }
}

/// A fresh PKI at `now`: the CA, and the server's certificate for
/// localhost and 127.0.0.1.
fn pki(random: &SeededRandom, now: DateTime<Utc>) -> LoadedPki {
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

async fn start(options: SqliteConnectOptions, settings: Settings) -> Port {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let db = Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap();
    let clock = ManualClock::new(SystemClock.now());
    let random = SeededRandom::new(31);
    let pki = pki(&random, clock.now());
    let ca_pem = String::from_utf8(pki.ca_cert_pem.clone()).unwrap();
    let tls = listener::tls(&pki);
    let signer = Signer {
        issuer: AgentIssuer::new(
            pki.ca,
            Arc::new(random.clone()),
            chrono::TimeDelta::days(60),
        ),
        release: settings.release.map(Mutex::new),
    };
    let token_port = Arc::new(Sha256Tokens::new(Arc::new(random.clone())));
    let tokens = EnrollmentTokenService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(db.clone()),
        token_port.clone(),
        signer.ca_fingerprint(),
    );
    let service = EnrollmentService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(db.clone()),
        token_port,
        Arc::new(signer),
    );
    let route = enrollment_service(
        service,
        RateLimits::new(&settings.limits, Arc::new(clock.clone())),
        RequestIds::new(Arc::new(clock.clone()), Arc::new(random.clone())),
        settings.deadline,
    );
    let control = control_service(
        Arc::new(AgentConnectionService::new(
            Arc::new(clock.clone()),
            Arc::new(db.clone()),
            Arc::new(X509AgentNames),
        )),
        Arc::new(clock.clone()),
        RateLimits::new(&settings.limits, Arc::new(clock.clone())),
        RequestIds::new(Arc::new(clock.clone()), Arc::new(random.clone())),
        ControlTimings::new(Heartbeats::default()),
        Sessions::new(CancellationToken::new()),
        AgentRegistry::new(Arc::new(clock.clone())),
    );
    let router = listener::router(tls, route, control, Heartbeats::default()).unwrap();
    let bound = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = bound.local_addr().unwrap();
    let stop = CancellationToken::new();
    let mut server = JoinSet::new();
    let serving = stop.child_token();
    server.spawn(async move {
        listener::serve(router, bound, listener::CONNECTION_AGE, serving)
            .await
            .unwrap();
    });
    Port {
        address,
        db,
        clock,
        tokens,
        ca_pem,
        stop,
        server,
    }
}

impl Port {
    /// A client that trusts only the port's CA, presenting `identity` if
    /// given.
    async fn client(
        &self,
        identity: Option<Identity>,
    ) -> Result<AgentEnrollmentClient<Channel>, tonic::transport::Error> {
        let mut tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(&self.ca_pem))
            .domain_name("localhost")
            .timeout(BOUND);
        if let Some(identity) = identity {
            tls = tls.identity(identity);
        }
        let channel = Endpoint::from_shared(format!("https://127.0.0.1:{}", self.address.port()))?
            .tls_config(tls)?
            .connect_timeout(BOUND)
            .timeout(BOUND)
            .connect()
            .await?;
        Ok(AgentEnrollmentClient::new(channel))
    }

    /// A token the host made.
    async fn token(&self) -> SecretString {
        self.tokens
            .create(Creator::Host)
            .await
            .unwrap()
            .bundle
            .token()
            .clone()
    }

    /// `Enroll` with `token`, `name` and a CSR of the key from `seed`.
    async fn enroll(
        &self,
        token: &SecretString,
        name: &str,
        seed: u8,
    ) -> Result<tonic::Response<fleet_proto::generated::EnrollResponse>, Status> {
        self.send(request(token, name, csr(&agent_key(seed)))).await
    }

    async fn send(
        &self,
        request: EnrollRequest,
    ) -> Result<tonic::Response<fleet_proto::generated::EnrollResponse>, Status> {
        let mut client = self.client(None).await.unwrap();
        client.enroll(request).await
    }

    async fn agents(&self, name: &str) -> bool {
        self.db
            .agents()
            .name_taken(&AgentName::try_from(name).unwrap())
            .await
            .unwrap()
    }

    async fn audit(&self) -> Vec<AuditRecord> {
        self.db
            .audit()
            .list(AuditPage {
                before: None,
                limit: AuditLimit::new(100).unwrap(),
            })
            .await
            .unwrap()
    }

    async fn stop(mut self) {
        self.stop.cancel();
        while let Some(ended) = tokio::time::timeout(BOUND, self.server.join_next())
            .await
            .unwrap()
        {
            ended.unwrap();
        }
    }
}

fn agent_key(seed: u8) -> KeyPair {
    let der = ed25519_pkcs8(&[seed; 32]);
    KeyPair::from_pkcs8_der_and_sign_algo(
        &PrivatePkcs8KeyDer::from(der.expose_secret().as_slice()),
        &PKCS_ED25519,
    )
    .unwrap()
}

fn csr(key: &KeyPair) -> Vec<u8> {
    CertificateParams::default()
        .serialize_request(key)
        .unwrap()
        .der()
        .to_vec()
}

fn request(token: &SecretString, name: &str, csr_der: Vec<u8>) -> EnrollRequest {
    EnrollRequest {
        token: token.expose_secret().to_owned(),
        name: name.to_owned(),
        csr_der,
    }
}

/// The answer's request ID.
fn request_id(metadata: &tonic::metadata::MetadataMap) -> RequestId {
    metadata
        .get(REQUEST_ID)
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap()
}

/// A refusal's code and message.
fn refusal(
    answer: Result<tonic::Response<fleet_proto::generated::EnrollResponse>, Status>,
) -> Status {
    answer.unwrap_err()
}

#[sqlx::test(migrations = false)]
async fn a_call_without_a_client_certificate_enrolls_and_carries_its_request_id(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let token = port.token().await;

    let answer = port.enroll(&token, "agent-1", 7).await.unwrap();

    let _ = request_id(answer.metadata());
    let enrolled = answer.into_inner();
    let agent = fleet_proto::enrollment::EnrollResponse::try_from(enrolled.clone())
        .unwrap()
        .agent_id;
    let certs = port.db.agents().certs(agent).await.unwrap();
    assert_eq!(certs.len(), 1);
    let der = CertificateDer::from_pem_slice(enrolled.certificate_pem.as_bytes()).unwrap();
    assert_eq!(certs[0].fingerprint, fingerprint(&der));
    assert!(
        enrolled
            .certificate_pem
            .starts_with("-----BEGIN CERTIFICATE-----"),
        "{}",
        enrolled.certificate_pem
    );
    assert_eq!(
        enrolled.not_after_unix_ms,
        Some(certs[0].not_after.timestamp_millis())
    );
    assert!(port.agents("agent-1").await);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn every_token_problem_is_one_unauthenticated_answer(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let used = port.token().await;
    port.enroll(&used, "agent-1", 1).await.unwrap();
    let expired = port.token().await;
    port.clock.advance(Duration::from_mins(15));
    let unknown = SecretString::from(format!("afk_et_{}", "A".repeat(43)));
    let malformed = SecretString::from("not a token");

    let answers = [
        refusal(port.enroll(&unknown, "agent-2", 2).await),
        refusal(port.enroll(&malformed, "agent-2", 2).await),
        refusal(port.enroll(&used, "agent-2", 2).await),
        refusal(port.enroll(&expired, "agent-2", 2).await),
    ];

    for status in &answers {
        assert_eq!(status.code(), Code::Unauthenticated, "{status:?}");
        assert_eq!(status.message(), INVALID_TOKEN);
        let _ = request_id(status.metadata());
    }
    assert!(!port.agents("agent-2").await);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_malformed_request_is_invalid_argument_and_its_token_stays_unread(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let token = port.token().await;

    let bad_name = refusal(port.enroll(&token, "no spaces allowed", 1).await);
    let no_csr = refusal(port.send(request(&token, "agent-1", Vec::new())).await);

    for status in [&bad_name, &no_csr] {
        assert_eq!(status.code(), Code::InvalidArgument, "{status:?}");
        assert_eq!(status.message(), INVALID_REQUEST);
    }
    port.enroll(&token, "agent-1", 1).await.unwrap();
    let failures = port
        .audit()
        .await
        .into_iter()
        .filter(|entry| {
            entry.action == AuditAction::AGENT_ENROLL && entry.outcome != AuditOutcome::Success
        })
        .count();
    assert_eq!(failures, 0);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_taken_name_and_a_refused_csr_have_their_answers_and_keep_the_token(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let mut tx = port.db.write().await.unwrap();
    tx.agents()
        .insert(&NewAgent {
            id: AgentId::new_v7(port.clock.now(), [9; 10]).unwrap(),
            name: AgentName::try_from("taken").unwrap(),
            created_at: port.clock.now(),
        })
        .await
        .unwrap();
    tx.commit(NewAuditEntry::new(
        port.clock.now(),
        AuditAction::try_from("test.setup").unwrap(),
        AuditOutcome::Success,
    ))
    .await
    .unwrap();
    let token = port.token().await;

    let taken = refusal(port.enroll(&token, "Taken", 1).await);
    let not_a_csr = refusal(
        port.send(request(&token, "agent-1", vec![0x30, 0x03, 1, 2, 3]))
            .await,
    );

    assert_eq!(taken.code(), Code::AlreadyExists, "{taken:?}");
    assert_eq!(taken.message(), NAME_TAKEN);
    assert_eq!(not_a_csr.code(), Code::InvalidArgument, "{not_a_csr:?}");
    assert_eq!(not_a_csr.message(), INVALID_CSR);
    port.enroll(&token, "agent-1", 1).await.unwrap();
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn the_sixth_call_in_a_minute_is_refused_before_its_body_is_read(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(
        options,
        Settings {
            limits: RateLimitConfig::default(),
            ..Settings::default()
        },
    )
    .await;
    let unknown = SecretString::from(format!("afk_et_{}", "A".repeat(43)));
    for _ in 0..5 {
        let status = refusal(port.enroll(&unknown, "agent-1", 1).await);
        assert_eq!(status.code(), Code::Unauthenticated, "{status:?}");
    }

    let oversized = refusal(
        port.send(request(&unknown, "agent-1", vec![0; MAX_REQUEST_BYTES * 2]))
            .await,
    );

    assert_eq!(oversized.code(), Code::ResourceExhausted, "{oversized:?}");
    assert_eq!(oversized.message(), RATE_LIMITED);
    assert_eq!(
        oversized
            .metadata()
            .get(RETRY_AFTER)
            .unwrap()
            .to_str()
            .unwrap(),
        "12"
    );
    let _ = request_id(oversized.metadata());
    port.clock.advance(Duration::from_secs(12));
    let again = refusal(port.enroll(&unknown, "agent-1", 1).await);
    assert_eq!(again.code(), Code::Unauthenticated, "{again:?}");
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn an_oversized_request_is_out_of_range(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let token = port.token().await;

    let status = refusal(
        port.send(request(&token, "agent-1", vec![0; MAX_REQUEST_BYTES + 1]))
            .await,
    );

    assert_eq!(status.code(), Code::OutOfRange, "{status:?}");
    assert!(!port.agents("agent-1").await);
    port.enroll(&token, "agent-1", 1).await.unwrap();
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_call_past_its_deadline_is_deadline_exceeded_and_stores_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (release, released) = mpsc::channel();
    let port = start(
        options,
        Settings {
            deadline: Duration::from_millis(200),
            release: Some(released),
            ..Settings::default()
        },
    )
    .await;
    let token = port.token().await;

    let status = refusal(port.enroll(&token, "agent-1", 1).await);

    assert_eq!(status.code(), Code::DeadlineExceeded, "{status:?}");
    assert_eq!(status.message(), TOO_SLOW);
    release.send(()).unwrap();
    assert!(!port.agents("agent-1").await);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_client_certificate_from_another_ca_fails_the_handshake(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let token = port.token().await;
    let random = SeededRandom::new(77);
    let now = port.clock.now();
    let (other, _) = CertificateAuthority::create(&random, now).unwrap();
    let key = Ed25519Key::generate(&random).unwrap();
    let cert = other
        .issue_agent(
            &key,
            AgentId::new_v7(now, [3; 10]).unwrap(),
            chrono::TimeDelta::days(1),
            &random,
            now,
        )
        .unwrap();
    let identity = Identity::from_pem(cert.pem, key.to_pem().as_bytes());

    let answer = match port.client(Some(identity)).await {
        Ok(mut client) => client
            .enroll(request(&token, "agent-1", csr(&agent_key(1))))
            .await
            .map(drop),
        Err(error) => Err(Status::unavailable(error.to_string())),
    };

    let status = answer.unwrap_err();
    assert_ne!(status.code(), Code::Ok);
    assert!(status.metadata().get(REQUEST_ID).is_none(), "{status:?}");
    assert!(!port.agents("agent-1").await);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_plaintext_connection_gets_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let token = port.token().await;
    let channel = Endpoint::from_shared(format!("http://127.0.0.1:{}", port.address.port()))
        .unwrap()
        .connect_timeout(BOUND)
        .timeout(BOUND)
        .connect()
        .await;

    let answer = match channel {
        Ok(channel) => AgentEnrollmentClient::new(channel)
            .enroll(request(&token, "agent-1", csr(&agent_key(1))))
            .await
            .map(drop),
        Err(error) => Err(Status::unavailable(error.to_string())),
    };

    let status = answer.unwrap_err();
    assert!(status.metadata().get(REQUEST_ID).is_none(), "{status:?}");
    assert!(!port.agents("agent-1").await);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn no_log_line_of_any_target_holds_the_token_its_hash_the_csr_or_the_peer(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    fleet_testkit::log_capture::install().unwrap();
    let port = start(options, Settings::default()).await;
    let token = port.token().await;
    let csr_der = csr(&agent_key(5));
    let hash = Sha256Tokens::new(Arc::new(SeededRandom::new(1)))
        .hash_presented(TokenKind::Enrollment, token.expose_secret())
        .unwrap();

    port.send(request(&token, "agent-1", csr_der.clone()))
        .await
        .unwrap();
    let _ = port.send(request(&token, "agent-2", csr_der.clone())).await;
    let _ = port.enroll(&token, "no spaces", 1).await;
    port.stop().await;

    let hex = hash.as_bytes().iter().fold(String::new(), |mut hex, byte| {
        write!(hex, "{byte:02x}").unwrap();
        hex
    });
    let csr_text = format!("{csr_der:?}");
    for secret in [
        token.expose_secret(),
        &token.expose_secret()["afk_et_".len()..],
        hex.as_str(),
        &csr_text[1..60],
    ] {
        fleet_testkit::log_capture::check_absent(secret).unwrap();
    }
    // The test's own client logs the address it dials; no target of the
    // server's side does.
    let peer = fleet_testkit::log_capture::check_absent("127.0.0.1")
        .err()
        .map(|leak| leak.hits.into_keys().collect::<Vec<_>>())
        .unwrap_or_default();
    assert!(
        peer.iter()
            .all(|target| target == "hyper_util::client::legacy::connect::http"),
        "{peer:?}"
    );
    assert!(
        fleet_testkit::log_capture::count(
            "fleet_server::grpc::enrollment",
            "an enrollment finished"
        ) >= 3
    );
}
