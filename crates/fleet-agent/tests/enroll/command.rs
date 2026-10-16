//! The whole command against a fake gRPC port over real TLS (ADR-0019,
//! S15, S18, S19, R10): the pin before anything is sent, an identity that
//! exists, each refusal's fixed text, the certificate's checks, what's
//! written, exit 3, and the token in no log.
//!
//! The agent's key comes from a seeded source, so the fake port can sign a
//! certificate for it without reading the CSR. Each test runs the port on
//! a runtime of its own and `enroll` on another, as main.rs does.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fleet_agent::config::ControlPlaneConfig;
use fleet_agent::enroll::key::AgentKey;
use fleet_agent::enroll::pin::fingerprint;
use fleet_agent::enroll::{Enrollment, Exit, enroll};
use fleet_core::enrollment::EnrollmentBundle;
use fleet_core::id::AgentId;
use fleet_core::value::AgentName;
use fleet_proto::generated::agent_enrollment_server::{AgentEnrollment, AgentEnrollmentServer};
use fleet_proto::generated::{EnrollRequest, EnrollResponse};
use fleet_testkit::system::SeededRandom;
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    PKCS_ED25519, SanType,
};
use rstest::rstest;
use rustls::pki_types::PrivatePkcs8KeyDer;
use secrecy::{ExposeSecret as _, SecretString};
use tokio::runtime::Runtime;
use tonic::metadata::{AsciiMetadataValue, MetadataMap};
use tonic::transport::server::TcpIncoming;
use tonic::transport::{Identity, Server, ServerTlsConfig};
use tonic::{Code, Request, Response, Status};

use crate::support::{assert_owner_only, names, test_dir};

/// The seed of the agent's key in every test.
const SEED: u64 = 5;
/// The token every test sends.
const TOKEN: &str = "afk_et_TOKENTOKENTOKENTOKENTOKENTOKENTOKENTOKENTOK";
/// What the fake port says when it refuses; the agent never shows it.
const SERVER_MESSAGE: &str = "the server's own words";

fn pair(seed: u8) -> KeyPair {
    let der = fleet_core::pki::ed25519_pkcs8(&[seed; 32]);
    KeyPair::from_pkcs8_der_and_sign_algo(
        &PrivatePkcs8KeyDer::from(der.expose_secret().as_slice()),
        &PKCS_ED25519,
    )
    .unwrap()
}

/// A CA from `seed`: its certificate's PEM and DER, and its issuer.
struct Ca {
    pem: String,
    der: Vec<u8>,
    issuer: Issuer<'static, KeyPair>,
}

fn new_ca(seed: u8) -> Ca {
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    let key = pair(seed);
    let cert = params.self_signed(&key).unwrap();
    Ca {
        pem: cert.pem(),
        der: cert.der().to_vec(),
        issuer: Issuer::new(params, key),
    }
}

/// The fake port's identity for 127.0.0.1, signed by `ca`.
fn server_identity(ca: &Ca) -> Identity {
    let key = pair(200);
    let mut params = CertificateParams::default();
    params.subject_alt_names = vec![SanType::IpAddress(IpAddr::from([127, 0, 0, 1]))];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let cert = params.signed_by(&key, &ca.issuer).unwrap();
    Identity::from_pem(cert.pem(), key.serialize_pem())
}

/// The key the agent makes in every test.
fn agent_key() -> AgentKey {
    AgentKey::generate(&SeededRandom::new(SEED)).unwrap()
}

/// A client certificate for `key` that `ca` signs, naming `agent`.
fn client_certificate(key: &AgentKey, ca: &Ca, agent: AgentId) -> String {
    let public = KeyPair::from_pkcs8_der_and_sign_algo(
        &PrivatePkcs8KeyDer::from(key.pkcs8_der()),
        &PKCS_ED25519,
    )
    .unwrap();
    let mut params = CertificateParams::default();
    params.subject_alt_names = vec![SanType::URI(
        format!("urn:uuid:{agent}").try_into().unwrap(),
    )];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    params.signed_by(&public, &ca.issuer).unwrap().pem()
}

fn agent() -> AgentId {
    AgentId::new_v7(chrono::DateTime::UNIX_EPOCH, [7; 10]).unwrap()
}

/// What the fake port answers.
#[derive(Clone)]
enum Answer {
    /// A certificate with this PEM.
    Certificate(String),
    /// This code, with `retry-after` if given.
    Refuse(Code, Option<&'static str>),
    /// A certificate, after creating this file.
    CertificateAfterCreating(String, PathBuf),
}

/// The fake port: it records each request and answers `answer`.
#[derive(Clone)]
struct Fake {
    answer: Answer,
    seen: Arc<Mutex<Vec<EnrollRequest>>>,
}

#[tonic::async_trait]
impl AgentEnrollment for Fake {
    async fn enroll(
        &self,
        request: Request<EnrollRequest>,
    ) -> Result<Response<EnrollResponse>, Status> {
        self.seen.lock().unwrap().push(request.into_inner());
        let pem = match &self.answer {
            Answer::Refuse(code, retry_after) => {
                let mut metadata = MetadataMap::new();
                if let Some(seconds) = retry_after {
                    metadata.insert("retry-after", AsciiMetadataValue::from_static(seconds));
                }
                return Err(Status::with_metadata(*code, SERVER_MESSAGE, metadata));
            }
            Answer::Certificate(pem) => pem.clone(),
            Answer::CertificateAfterCreating(pem, path) => {
                std::fs::write(path, "someone else's").unwrap();
                pem.clone()
            }
        };
        let answer = fleet_proto::enrollment::EnrollResponse {
            agent_id: agent(),
            certificate_pem: pem,
            not_after: chrono::DateTime::from_timestamp(4_102_444_800, 0).unwrap(),
        };
        Ok(Response::new(EnrollResponse::try_from(answer).unwrap()))
    }
}

/// A fake port running on its own runtime until dropped.
struct Port {
    address: SocketAddr,
    seen: Arc<Mutex<Vec<EnrollRequest>>>,
    _runtime: Runtime,
}

/// Starts a fake port presenting a certificate from `server_ca`, answering
/// `answer`.
fn start(server_ca: &Ca, answer: Answer) -> Port {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let fake = Fake {
        answer,
        seen: Arc::clone(&seen),
    };
    let identity = server_identity(server_ca);
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let address = listener.local_addr().unwrap();
    runtime.spawn(async move {
        let _ = Server::builder()
            .tls_config(ServerTlsConfig::new().identity(identity))
            .unwrap()
            .add_service(AgentEnrollmentServer::new(fake))
            .serve_with_incoming(TcpIncoming::from(listener))
            .await;
    });
    Port {
        address,
        seen,
        _runtime: runtime,
    }
}

/// An agent's setup in folder `name`: the pinned CA's file and the config.
struct Agent {
    dir: PathBuf,
    plane: ControlPlaneConfig,
}

fn agent_in(name: &str, ca: &Ca, address: SocketAddr) -> Agent {
    let dir = test_dir(name);
    let ca_file = dir.join("fleet_ca.crt");
    std::fs::write(&ca_file, &ca.pem).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&ca_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let identity = dir.join("identity");
    std::fs::create_dir(&identity).unwrap();
    Agent {
        plane: ControlPlaneConfig {
            url: format!("https://127.0.0.1:{}", address.port()),
            ca_cert_file: ca_file,
            cert_file: identity.join("agent.crt"),
            key_file: identity.join("agent.key"),
        },
        dir,
    }
}

impl Agent {
    fn identity(&self) -> PathBuf {
        self.dir.join("identity")
    }
}

/// What a run of `enroll` left.
struct Ran {
    exit: Exit,
    out: String,
    err: String,
}

/// Runs `enroll` for `agent` with a bundle pinning `pinned`.
fn run(agent: &Agent, pinned: &[u8]) -> Ran {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let bundle = EnrollmentBundle::new(SecretString::from(TOKEN), fingerprint(pinned)).unwrap();
    let name = AgentName::try_from("agent-1").unwrap();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let exit = enroll(
        Enrollment {
            plane: &agent.plane,
            name: &name,
            bundle,
        },
        &SeededRandom::new(SEED),
        &runtime,
        &mut out,
        &mut err,
    );
    Ran {
        exit,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn an_agent_enrolls_and_stores_its_key_and_the_certificate_owner_only() {
    let ca = new_ca(1);
    let certificate = client_certificate(&agent_key(), &ca, agent());
    let port = start(&ca, Answer::Certificate(certificate.clone()));
    let agent_setup = agent_in("enrolls", &ca, port.address);

    let ran = run(&agent_setup, &ca.der);

    assert_eq!(ran.exit, Exit::Enrolled, "{}", ran.err);
    assert_eq!(names(&agent_setup.identity()), ["agent.crt", "agent.key"]);
    assert_eq!(
        std::fs::read_to_string(&agent_setup.plane.cert_file).unwrap(),
        certificate
    );
    let key = std::fs::read_to_string(&agent_setup.plane.key_file).unwrap();
    assert_eq!(key, agent_key().to_pem().as_str());
    assert_owner_only(&agent_setup.plane.key_file);
    assert_owner_only(&agent_setup.plane.cert_file);
    let seen = port.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].token, TOKEN);
    assert_eq!(seen[0].name, "agent-1");
    assert!(contains(&seen[0].csr_der, agent_key().public_key_raw()));
    assert!(!contains(&seen[0].csr_der, agent_key().pkcs8_der()));
    assert_eq!(
        ran.out,
        format!(
            "enrolled as agent {} (agent-1)\ncertificate valid until 2100-01-01T00:00:00Z\nkey: {}\ncertificate: {}\n",
            agent(),
            agent_setup.plane.key_file.display(),
            agent_setup.plane.cert_file.display()
        )
    );
    assert_eq!(
        ran.err.contains("only its folder's permissions protect"),
        cfg!(windows),
        "{}",
        ran.err
    );
    assert!(!ran.out.contains(TOKEN) && !ran.err.contains(TOKEN));
}

#[test]
fn a_ca_file_that_isnt_the_pinned_ca_sends_nothing() {
    let ca = new_ca(1);
    let other = new_ca(2);
    let port = start(&ca, Answer::Refuse(Code::Internal, None));
    let agent_setup = agent_in("wrong_ca", &ca, port.address);

    let ran = run(&agent_setup, &other.der);

    assert_eq!(ran.exit, Exit::Failed);
    assert_eq!(port.seen.lock().unwrap().len(), 0);
    assert!(
        ran.err.contains(&fingerprint(&ca.der).to_string()),
        "{}",
        ran.err
    );
    assert!(
        ran.err.contains(&fingerprint(&other.der).to_string()),
        "{}",
        ran.err
    );
    assert!(ran.err.contains("nothing was sent"), "{}", ran.err);
    assert_eq!(names(&agent_setup.identity()), Vec::<String>::new());
}

#[rstest]
#[case::a_key("agent.key")]
#[case::a_certificate("agent.crt")]
#[case::a_lock("agent.key.tmp")]
fn an_identity_that_exists_sends_nothing_and_makes_no_key(#[case] existing: &str) {
    let ca = new_ca(1);
    let port = start(&ca, Answer::Refuse(Code::Internal, None));
    let agent_setup = agent_in(&format!("exists_{existing}"), &ca, port.address);
    let path = agent_setup.identity().join(existing);
    std::fs::write(&path, "kept").unwrap();

    let ran = run(&agent_setup, &ca.der);

    assert_eq!(ran.exit, Exit::Failed);
    assert_eq!(port.seen.lock().unwrap().len(), 0);
    assert!(ran.err.contains(existing), "{}", ran.err);
    assert_eq!(names(&agent_setup.identity()), [existing]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "kept");
}

#[rstest]
#[case::an_invalid_token(Code::Unauthenticated, None, "unknown, used or expired")]
#[case::a_taken_name(Code::AlreadyExists, None, "named agent-1 already")]
#[case::rate_limited(Code::ResourceExhausted, Some("12"), "try again in 12 s")]
#[case::busy(Code::Unavailable, Some("1"), "busy")]
#[case::a_refused_request(Code::InvalidArgument, None, "refused the enrollment request")]
#[case::a_server_failure(Code::Internal, None, "see the server's log")]
fn each_refusal_has_its_own_text_and_leaves_nothing(
    #[case] code: Code,
    #[case] retry_after: Option<&'static str>,
    #[case] text: &str,
) {
    let ca = new_ca(1);
    let port = start(&ca, Answer::Refuse(code, retry_after));
    let agent_setup = agent_in(&format!("refused_{code:?}"), &ca, port.address);

    let ran = run(&agent_setup, &ca.der);

    assert_eq!(ran.exit, Exit::Failed);
    assert_eq!(port.seen.lock().unwrap().len(), 1);
    assert!(ran.err.contains(text), "{}", ran.err);
    assert!(!ran.err.contains(SERVER_MESSAGE), "{}", ran.err);
    assert_eq!(ran.out, "");
    assert_eq!(names(&agent_setup.identity()), Vec::<String>::new());
}

#[test]
fn a_server_whose_certificate_another_ca_signed_gets_nothing() {
    let pinned = new_ca(1);
    let other = new_ca(2);
    let port = start(&other, Answer::Refuse(Code::Internal, None));
    let agent_setup = agent_in("server_of_another_ca", &pinned, port.address);

    let ran = run(&agent_setup, &pinned.der);

    assert_eq!(ran.exit, Exit::Failed);
    assert_eq!(port.seen.lock().unwrap().len(), 0);
    assert!(ran.err.contains("pinned CA"), "{}", ran.err);
    assert_eq!(names(&agent_setup.identity()), Vec::<String>::new());
}

#[test]
fn a_server_that_isnt_there_is_unreachable() {
    let ca = new_ca(1);
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let agent_setup = agent_in("unreachable", &ca, closed);

    let ran = run(&agent_setup, &ca.der);

    assert_eq!(ran.exit, Exit::Failed);
    assert!(ran.err.contains("can't be reached"), "{}", ran.err);
    assert_eq!(names(&agent_setup.identity()), Vec::<String>::new());
}

#[rstest]
#[case::for_another_key("another_key")]
#[case::from_another_ca("another_ca")]
#[case::with_text_around_it("text_around")]
fn a_certificate_the_agent_cant_use_is_lost_and_nothing_stored(#[case] which: &str) {
    let ca = new_ca(1);
    let pem = match which {
        "another_key" => client_certificate(
            &AgentKey::generate(&SeededRandom::new(SEED + 1)).unwrap(),
            &ca,
            agent(),
        ),
        "another_ca" => client_certificate(&agent_key(), &new_ca(2), agent()),
        _ => format!("{}\nmore", client_certificate(&agent_key(), &ca, agent())),
    };
    let port = start(&ca, Answer::Certificate(pem));
    let agent_setup = agent_in(&format!("lost_{which}"), &ca, port.address);

    let ran = run(&agent_setup, &ca.der);

    assert_eq!(ran.exit, Exit::Lost, "{}", ran.err);
    assert!(ran.err.contains(&agent().to_string()), "{}", ran.err);
    assert!(ran.err.contains("enroll with a new token"), "{}", ran.err);
    assert_eq!(names(&agent_setup.identity()), Vec::<String>::new());
}

#[test]
fn a_certificate_file_that_appeared_during_the_call_is_kept_and_the_identity_lost() {
    let ca = new_ca(1);
    let certificate = client_certificate(&agent_key(), &ca, agent());
    let dir = test_dir("appeared");
    let cert_path = dir.join("identity").join("agent.crt");
    let port = start(
        &ca,
        Answer::CertificateAfterCreating(certificate, cert_path.clone()),
    );
    let agent_setup = agent_in("appeared", &ca, port.address);

    let ran = run(&agent_setup, &ca.der);

    assert_eq!(ran.exit, Exit::Lost, "{}", ran.err);
    assert_eq!(names(&agent_setup.identity()), ["agent.crt"]);
    assert_eq!(
        std::fs::read_to_string(&cert_path).unwrap(),
        "someone else's"
    );
}

/// One check of everything a run logs, at every level of every target.
#[test]
fn the_token_reaches_no_log() {
    fleet_testkit::log_capture::install().unwrap();
    let ca = new_ca(1);
    let certificate = client_certificate(&agent_key(), &ca, agent());
    let port = start(&ca, Answer::Certificate(certificate));
    let agent_setup = agent_in("no_token_in_logs", &ca, port.address);

    let ran = run(&agent_setup, &ca.der);

    assert_eq!(ran.exit, Exit::Enrolled, "{}", ran.err);
    fleet_testkit::log_capture::check_absent(TOKEN).unwrap();
    fleet_testkit::log_capture::check_absent(&TOKEN["afk_et_".len()..]).unwrap();
    let key = agent_key().to_pem();
    let body = key.lines().nth(1).unwrap();
    fleet_testkit::log_capture::check_absent(body).unwrap();
}
