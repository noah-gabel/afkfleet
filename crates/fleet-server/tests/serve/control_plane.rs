//! The control plane in `serve` (Plan.md P10.3, P10.4; ADR-0019, S12, S13,
//! S22): the gRPC port beside the API while `[grpc]` is configured, an
//! agent enrolled over it with a token the database holds, both stopping at
//! one signal; an enrolled agent's stream, which that signal ends without
//! holding up the drain;
//! the start's lines with the port on and off; an expired server
//! certificate logged without stopping the start; and the binary refusing a
//! PKI it can't read.

use core::time::Duration;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use fleet_core::system::Clock as _;
use fleet_proto::PROTOCOL_VERSION;
use fleet_proto::control::ServerMessage;
use fleet_proto::generated as wire;
use fleet_proto::generated::EnrollRequest;
use fleet_proto::generated::agent_control_client::AgentControlClient;
use fleet_proto::generated::agent_enrollment_client::AgentEnrollmentClient;
use fleet_server::app::enrollment::{Creator, EnrollmentTokenService};
use fleet_server::cli::healthcheck;
use fleet_server::cli::serve::{self, ControlPlane, Exit};
use fleet_server::config::{GrpcConfig, PkiPaths, ServerConfig};
use fleet_server::grpc::control::SHUTTING_DOWN;
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::infra::pki::{CertificateAuthority, Ed25519Key, LoadedPki, ServerNames};
use fleet_server::infra::sqlite::{self, DatabaseOptions};
use fleet_server::infra::system::{OsRandom, SystemClock};
use fleet_server::ports::store::Store as _;
use fleet_startup::signals::Signal;
use fleet_testkit::system::SeededRandom;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
use rustls::pki_types::PrivatePkcs8KeyDer;
use secrecy::ExposeSecret as _;
use tokio::net::TcpListener;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Code;
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Identity};
use zeroize::Zeroizing;

use crate::support::{
    ChannelSignals, capture, closed_port, config_body, finish, json_lines, lines_with, listener,
    path_arg, server_config, test_dir, toml_path, vault_keys, write_config,
};

/// What a start logs without `[grpc]`.
const OFF: &str =
    "the control plane is off: [grpc] isn't configured, so no agent can enroll or connect";
/// What a start logs with an expired server certificate.
const EXPIRED: &str = "the server certificate has expired: no agent can connect until `afkfleet-server pki server-cert` replaces it";

/// The longest a test waits for anything.
const PATIENCE: Duration = Duration::from_secs(30);

/// A PKI whose certificates start at `at`: the CA, and the server's
/// certificate for localhost and 127.0.0.1.
fn pki(at: DateTime<Utc>) -> LoadedPki {
    let random = SeededRandom::new(41);
    let (ca, ca_cert) = CertificateAuthority::create(&random, at).unwrap();
    let key = Ed25519Key::generate(&random).unwrap();
    let names =
        ServerNames::try_new(&["localhost".to_owned()], &[IpAddr::from([127, 0, 0, 1])]).unwrap();
    let server = ca.issue_server(&key, &names, &random, at).unwrap();
    LoadedPki {
        ca,
        ca_cert_pem: ca_cert.pem.into_bytes(),
        server_cert_pem: server.pem.into_bytes(),
        server_key_pem: Zeroizing::new(key.to_pem().as_bytes().to_vec()),
        server_not_after: server.validity.not_after,
    }
}

/// `config` with `[grpc]` on `bind`; its file paths are never read here,
/// since `run` gets the PKI itself.
fn with_grpc(mut config: ServerConfig, dir: &Path, bind: std::net::SocketAddr) -> ServerConfig {
    config.grpc = Some(GrpcConfig {
        bind,
        pki: PkiPaths {
            ca_cert_file: dir.join("ca.crt"),
            ca_key_file: dir.join("ca.key"),
            server_cert_file: dir.join("server.crt"),
            server_key_file: dir.join("server.key"),
        },
        agent_cert_lifetime: TimeDelta::days(30),
        heartbeats: fleet_server::config::Heartbeats::default(),
    });
    config
}

fn agent_key() -> KeyPair {
    let der = fleet_core::pki::ed25519_pkcs8(&[9; 32]);
    KeyPair::from_pkcs8_der_and_sign_algo(
        &PrivatePkcs8KeyDer::from(der.expose_secret().as_slice()),
        &PKCS_ED25519,
    )
    .unwrap()
}

fn csr() -> Vec<u8> {
    CertificateParams::default()
        .serialize_request(&agent_key())
        .unwrap()
        .der()
        .to_vec()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_enrolls_over_the_grpc_port_beside_the_api_and_both_stop_at_one_signal() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let dir = test_dir("control_plane_enroll");
    let (http, http_address) = listener().await;
    let grpc = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let grpc_address = grpc.local_addr().unwrap();
    let config = with_grpc(server_config(&dir, http_address, false), &dir, grpc_address);
    let pki = pki(SystemClock.now());
    let ca_pem = String::from_utf8(pki.ca_cert_pem.clone()).unwrap();
    let fingerprint = pki.ca.fingerprint();
    let (sender, signals) = ChannelSignals::new();
    let keys = vault_keys(&config);
    let server = tokio::spawn({
        let config = config.clone();
        let plane = ControlPlane {
            listener: grpc,
            pki,
        };
        async move { serve::run(&config, http, Some(plane), signals, keys).await }
    });
    let ready = healthcheck::get(http_address, "/health/ready", PATIENCE)
        .await
        .unwrap();
    assert_eq!(ready.status, 200);
    let database = sqlite::open_migrated(&config.database, DatabaseOptions::default())
        .await
        .unwrap();
    let random = Arc::new(OsRandom);
    let token = EnrollmentTokenService::new(
        Arc::new(SystemClock),
        random.clone(),
        Arc::new(database.clone()),
        Arc::new(Sha256Tokens::new(random)),
        fingerprint,
    )
    .create(Creator::Host)
    .await
    .unwrap()
    .bundle
    .token()
    .expose_secret()
    .to_owned();
    let channel = Endpoint::from_shared(format!("https://127.0.0.1:{}", grpc_address.port()))
        .unwrap()
        .tls_config(
            ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(&ca_pem))
                .domain_name("localhost"),
        )
        .unwrap()
        .connect_timeout(PATIENCE)
        .timeout(PATIENCE)
        .connect()
        .await
        .unwrap();
    let mut client = AgentEnrollmentClient::new(channel);

    let enrolled = client
        .enroll(EnrollRequest {
            token: token.clone(),
            name: "agent-1".to_owned(),
            csr_der: csr(),
        })
        .await
        .unwrap()
        .into_inner();
    let again = client
        .enroll(EnrollRequest {
            token,
            name: "agent-2".to_owned(),
            csr_der: csr(),
        })
        .await
        .unwrap_err();
    sender.send(Signal::Terminate).await.unwrap();
    let exit = tokio::time::timeout(PATIENCE, server)
        .await
        .expect("the server should stop within its deadline")
        .unwrap();

    let enrolled = fleet_proto::enrollment::EnrollResponse::try_from(enrolled).unwrap();
    let certs = database.agents().certs(enrolled.agent_id).await.unwrap();
    assert_eq!(certs.len(), 1);
    assert_eq!(certs[0].not_after, enrolled.not_after);
    let lifetime = enrolled.not_after - SystemClock.now();
    assert!(
        lifetime > TimeDelta::days(29) && lifetime <= TimeDelta::days(30),
        "[grpc] agent_cert_lifetime_days: {lifetime}"
    );
    assert_eq!(again.code(), Code::Unauthenticated, "{again:?}");
    assert_eq!(exit, Exit::Stopped);
    assert!(
        TcpListener::bind(grpc_address).await.is_ok(),
        "the port closed"
    );
    database.close().await;
}

/// Enrolls agent-1 with `token` over the port at `grpc`, whose CA is
/// `ca_pem`, then connects it: what it sends, and what it reads after
/// `Welcome` and the welcome's one full reconcile, which gives it no bots.
/// Nothing more is due on the stream after that, so whatever comes next is
/// the test's.
async fn welcomed_agent(
    grpc: std::net::SocketAddr,
    ca_pem: &str,
    token: String,
) -> (
    tokio::sync::mpsc::Sender<wire::AgentMessage>,
    tonic::Streaming<wire::ServerMessage>,
) {
    let endpoint = Endpoint::from_shared(format!("https://127.0.0.1:{}", grpc.port()))
        .unwrap()
        .connect_timeout(PATIENCE);
    let tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(ca_pem))
        .domain_name("localhost");
    let channel = endpoint.clone().tls_config(tls.clone()).unwrap();
    let enrolled = AgentEnrollmentClient::new(channel.connect().await.unwrap())
        .enroll(EnrollRequest {
            token,
            name: "agent-1".to_owned(),
            csr_der: csr(),
        })
        .await
        .unwrap()
        .into_inner();
    let identity = Identity::from_pem(&enrolled.certificate_pem, agent_key().serialize_pem());
    let channel = endpoint.tls_config(tls.identity(identity)).unwrap();
    let mut control = AgentControlClient::new(channel.connect().await.unwrap());
    let (agent, outbox) = tokio::sync::mpsc::channel(4);
    let mut answers = control
        .connect(ReceiverStream::new(outbox))
        .await
        .unwrap()
        .into_inner();
    let hello = wire::Hello {
        name: "agent-1".to_owned(),
        agent_version: "0.1.0".to_owned(),
        protocol_version: PROTOCOL_VERSION,
        capacity: 10,
        running: Vec::new(),
    };
    agent
        .send(wire::AgentMessage {
            msg: Some(wire::agent_message::Msg::Hello(hello)),
        })
        .await
        .unwrap();
    let welcome = tokio::time::timeout(PATIENCE, answers.message())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        ServerMessage::try_from(welcome),
        Ok(ServerMessage::Welcome(_))
    ));
    // Every welcome gets one full reconcile, an empty one too (ADR-0019,
    // S42); read it, or it races what the test sends next.
    let reconcile = tokio::time::timeout(PATIENCE, answers.message())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let reconcile = ServerMessage::try_from(reconcile);
    assert!(
        matches!(&reconcile, Ok(ServerMessage::Reconcile(bots)) if bots.is_empty()),
        "{reconcile:?}"
    );
    (agent, answers)
}

#[tokio::test]
async fn an_enrolled_agent_connects_and_one_signal_ends_its_stream_and_the_server() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let dir = test_dir("control_plane_connect");
    let (http, http_address) = listener().await;
    let grpc = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let grpc_address = grpc.local_addr().unwrap();
    let config = with_grpc(server_config(&dir, http_address, false), &dir, grpc_address);
    let pki = pki(SystemClock.now());
    let ca_pem = String::from_utf8(pki.ca_cert_pem.clone()).unwrap();
    let fingerprint = pki.ca.fingerprint();
    // One thread: the server's lines reach this test's log.
    let (logs, _guard) = capture();
    let (sender, signals) = ChannelSignals::new();
    let keys = vault_keys(&config);
    let server = tokio::spawn({
        let config = config.clone();
        let plane = ControlPlane {
            listener: grpc,
            pki,
        };
        async move { serve::run(&config, http, Some(plane), signals, keys).await }
    });
    let ready = healthcheck::get(http_address, "/health/ready", PATIENCE)
        .await
        .unwrap();
    assert_eq!(ready.status, 200);
    let database = sqlite::open_migrated(&config.database, DatabaseOptions::default())
        .await
        .unwrap();
    let random = Arc::new(OsRandom);
    let token = EnrollmentTokenService::new(
        Arc::new(SystemClock),
        random.clone(),
        Arc::new(database.clone()),
        Arc::new(Sha256Tokens::new(random)),
        fingerprint,
    )
    .create(Creator::Host)
    .await
    .unwrap()
    .bundle
    .token()
    .expose_secret()
    .to_owned();
    let (_agent, mut answers) = welcomed_agent(grpc_address, &ca_pem, token).await;

    sender.send(Signal::Terminate).await.unwrap();
    let ended = tokio::time::timeout(PATIENCE, answers.message())
        .await
        .unwrap()
        .unwrap_err();
    let exit = tokio::time::timeout(PATIENCE, server)
        .await
        .expect("the server should stop within its deadline")
        .unwrap();

    assert_eq!(ended.code(), Code::Unavailable, "{ended:?}");
    assert_eq!(ended.message(), SHUTTING_DOWN);
    assert_eq!(exit, Exit::Stopped, "{}", logs.text());
    assert_eq!(
        logs.lines_with(
            "connections were still open at the drain limit; the server stops waiting for them"
        )
        .unwrap()
        .len(),
        0,
        "{}",
        logs.text()
    );
    let ends = logs.lines_with("an agent's stream ended").unwrap();
    assert_eq!(ends.len(), 1, "{}", logs.text());
    assert_eq!(ends[0]["reason"], "server_shutdown");
    database.close().await;
}

/// Starts `config`'s server with `plane`, stops it at once, and returns its
/// log.
async fn start_and_stop(
    config: &ServerConfig,
    plane: Option<ControlPlane>,
) -> fleet_testkit::log_buffer::LogBuffer {
    let (listener, _) = listener().await;
    let (logs, _guard) = capture();
    let (sender, signals) = ChannelSignals::new();
    sender.send(Signal::Terminate).await.unwrap();

    let exit = tokio::time::timeout(
        PATIENCE,
        serve::run(config, listener, plane, signals, vault_keys(config)),
    )
    .await
    .unwrap();

    assert_eq!(exit, Exit::Stopped, "{}", logs.text());
    logs
}

#[tokio::test]
async fn a_start_without_grpc_says_the_control_plane_is_off() {
    let dir = test_dir("control_plane_off");
    let (_, address) = listener().await;

    let logs = start_and_stop(&server_config(&dir, address, false), None).await;

    let off = logs.lines_with(OFF).unwrap();
    assert_eq!(off.len(), 1, "{}", logs.text());
    assert_eq!(off[0]["level"], "INFO");
    assert_eq!(
        logs.lines_with("the control plane is listening")
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn a_start_with_grpc_says_where_the_control_plane_listens() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let dir = test_dir("control_plane_on");
    let (_, address) = listener().await;
    let grpc = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let grpc_address = grpc.local_addr().unwrap();
    let config = with_grpc(server_config(&dir, address, false), &dir, grpc_address);
    let plane = ControlPlane {
        listener: grpc,
        pki: pki(SystemClock.now()),
    };

    let logs = start_and_stop(&config, Some(plane)).await;

    let on = logs.lines_with("the control plane is listening").unwrap();
    assert_eq!(on.len(), 1, "{}", logs.text());
    assert_eq!(on[0]["address"], grpc_address.to_string());
    assert_eq!(logs.lines_with(OFF).unwrap().len(), 0);
    assert_eq!(logs.lines_with(EXPIRED).unwrap().len(), 0);
}

#[tokio::test]
async fn an_expired_server_certificate_is_an_error_and_the_server_still_starts() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let dir = test_dir("control_plane_expired");
    let (_, address) = listener().await;
    let grpc = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let grpc_address = grpc.local_addr().unwrap();
    let config = with_grpc(server_config(&dir, address, false), &dir, grpc_address);
    let pki = pki(SystemClock.now() - TimeDelta::days(400));
    let not_after = pki.server_not_after;
    let plane = ControlPlane {
        listener: grpc,
        pki,
    };

    let logs = start_and_stop(&config, Some(plane)).await;

    let expired = logs.lines_with(EXPIRED).unwrap();
    assert_eq!(expired.len(), 1, "{}", logs.text());
    assert_eq!(expired[0]["level"], "ERROR");
    assert_eq!(
        expired[0]["not_after"],
        not_after.to_string(),
        "{}",
        logs.text()
    );
    assert_eq!(logs.lines_with("the server is listening").unwrap().len(), 1);
}

/// The PKI is read before anything else starts, like the vault's keys: a
/// missing file stops `serve` with exit 1 and an error naming the file and
/// `pki init` (ADR-0019, S12).
#[test]
fn serve_with_a_missing_pki_logs_the_file_and_exits_1() {
    let dir = test_dir("serve_missing_pki");
    let pki = dir.join("pki");
    let config = write_config(
        &dir,
        &format!(
            "{}[grpc]\nca_cert_file = {}\nca_key_file = {}\nserver_cert_file = {}\nserver_key_file = {}\n",
            config_body(&dir, closed_port()),
            toml_path(&pki.join("ca.crt")),
            toml_path(&pki.join("ca.key")),
            toml_path(&pki.join("server.crt")),
            toml_path(&pki.join("server.key")),
        ),
    );

    let finished = finish(&["serve", "--config", path_arg(&config)]);

    assert_eq!(finished.status.code(), Some(1), "{}", finished.stdout);
    let lines = json_lines(&finished.stdout);
    let failed = lines_with(&lines, "the server can't start");
    assert_eq!(failed.len(), 1, "{}", finished.stdout);
    let error = failed[0]["error"].as_str().unwrap();
    assert!(
        error.starts_with(
            "the control plane's PKI can't be used: a file of the PKI can't be used; \
             `afkfleet-server pki init` creates the PKI: the secret file "
        ),
        "{error}"
    );
    assert!(error.contains("ca.crt"), "{error}");
    assert_eq!(error.matches("the secret file").count(), 1, "{error}");
    assert!(!dir.join("afkfleet.db").exists(), "nothing was opened");
}

/// The host's way to a token (U3): the real binary prints the bundle alone
/// on stdout, the expiry and the CA's fingerprint on stderr, and its help
/// lists the exit codes.
#[test]
fn the_binary_prints_a_bundle_that_pins_the_ca() {
    let dir = test_dir("agent_create_enrollment_token");
    let pki = dir.join("pki");
    std::fs::create_dir(&pki).unwrap();
    let config = write_config(
        &dir,
        &format!(
            "{}[grpc]\nca_cert_file = {}\nca_key_file = {}\nserver_cert_file = {}\nserver_key_file = {}\n",
            config_body(&dir, closed_port()),
            toml_path(&pki.join("ca.crt")),
            toml_path(&pki.join("ca.key")),
            toml_path(&pki.join("server.crt")),
            toml_path(&pki.join("server.key")),
        ),
    );
    let init = finish(&[
        "pki",
        "init",
        "--config",
        path_arg(&config),
        "--dns",
        "localhost",
    ]);
    assert_eq!(init.status.code(), Some(0), "{}", init.stderr);
    let migrated = finish(&["migrate", "--config", path_arg(&config)]);
    assert_eq!(migrated.status.code(), Some(0), "{}", migrated.stdout);
    let fingerprint = fleet_server::infra::pki::ca_fingerprint(&pki.join("ca.crt")).unwrap();

    let created = finish(&[
        "agent",
        "create-enrollment-token",
        "--config",
        path_arg(&config),
    ]);
    let help = finish(&["agent", "create-enrollment-token", "--help"]);

    assert_eq!(created.status.code(), Some(0), "{}", created.stderr);
    let lines: Vec<&str> = created.stdout.lines().collect();
    assert_eq!(lines.len(), 1, "{}", created.stdout);
    let bundle = fleet_core::enrollment::EnrollmentBundle::try_from(lines[0]).unwrap();
    assert_eq!(bundle.ca_fingerprint(), fingerprint);
    assert!(
        created
            .stderr
            .contains(&format!("CA fingerprint (SHA-256): {fingerprint}\n")),
        "{}",
        created.stderr
    );
    assert_eq!(help.status.code(), Some(0));
    assert!(
        help.stdout
            .contains(fleet_server::cli::agent::CREATE_ENROLLMENT_TOKEN_EXIT_CODES),
        "{}",
        help.stdout
    );
}

#[test]
fn the_binary_lists_agents_on_stdout_and_logs_on_stderr() {
    let dir = test_dir("agent_list");
    let config = write_config(&dir, &config_body(&dir, closed_port()));
    let migrated = finish(&["migrate", "--config", path_arg(&config)]);
    assert_eq!(migrated.status.code(), Some(0), "{}", migrated.stdout);

    let listed = finish(&["agent", "list", "--config", path_arg(&config)]);
    let unknown = finish(&[
        "agent",
        "disable",
        "--config",
        path_arg(&config),
        "--id",
        "019bcfe5-6800-7101-8101-010101010101",
    ]);

    assert_eq!(listed.status.code(), Some(0), "{}", listed.stderr);
    assert_eq!(
        listed.stdout,
        format!("{}\n", fleet_server::cli::agent::LIST_HEADER)
    );
    assert_eq!(unknown.status.code(), Some(1), "{}", unknown.stderr);
    assert_eq!(unknown.stdout, "");
    assert!(
        unknown
            .stderr
            .contains("afkfleet-server: no agent has the ID 019bcfe5-6800-7101-8101-010101010101"),
        "{}",
        unknown.stderr
    );
}
