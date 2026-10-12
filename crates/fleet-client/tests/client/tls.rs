//! Certificates are always checked (Plan.md §7.5, ADR-0017): against the
//! system's roots, or only against a configured CA. Each test makes a CA and
//! a server certificate at run time with rcgen, since no key is ever
//! committed, and serves one answer from a rustls server on a thread.

use core::time::Duration;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use fleet_client::{Client, ClientConfig, ClientError, CustomCa, MemoryStore, ServerUrl, Trust};
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, DnType, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// How long the test server waits for the client before giving up.
const SOCKET_WAIT: Duration = Duration::from_secs(5);

/// A CA with its common name `name`.
fn ca(name: &str) -> CertifiedIssuer<'static, KeyPair> {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.distinguished_name.push(DnType::CommonName, name);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap()
}

/// A server certificate from `ca` for `names` (IP addresses or DNS names),
/// and its key.
fn leaf(
    ca: &CertifiedIssuer<'static, KeyPair>,
    names: &[&str],
) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    let mut params = CertificateParams::new(names).unwrap();
    params
        .distinguished_name
        .push(DnType::CommonName, "afkfleet test server");
    let key = KeyPair::generate().unwrap();
    let certificate = params.signed_by(&key, &**ca).unwrap();
    (
        certificate.der().clone(),
        PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
    )
}

/// A TLS server on a thread that presents `certificate` and answers one
/// request with an empty 200, the way `/health/live` does. A client that
/// refuses the certificate ends the handshake; the thread then just ends.
fn tls_server(certificate: CertificateDer<'static>, key: PrivateKeyDer<'static>) -> SocketAddr {
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![certificate], key)
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        stream.set_read_timeout(Some(SOCKET_WAIT)).unwrap();
        stream.set_write_timeout(Some(SOCKET_WAIT)).unwrap();
        let connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut tls = rustls::StreamOwned::new(connection, stream);
        let mut head = Vec::new();
        let mut buffer = [0; 1024];
        while !head.windows(4).any(|window| window == b"\r\n\r\n") {
            match tls.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(read) => head.extend_from_slice(&buffer[..read]),
            }
        }
        let _ = tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let _ = tls.flush();
        tls.conn.send_close_notify();
        let _ = tls.flush();
    });
    address
}

fn client(address: SocketAddr, trust: Trust) -> Client<MemoryStore> {
    Client::new(
        ClientConfig {
            server: ServerUrl::parse(&format!("https://{address}")).unwrap(),
            trust,
        },
        MemoryStore::new(),
    )
    .unwrap()
}

fn only(ca: &CertifiedIssuer<'static, KeyPair>) -> Trust {
    Trust::CustomCa(CustomCa::from_pem(&ca.pem()).unwrap())
}

#[tokio::test]
async fn the_configured_ca_is_trusted() {
    let issuer = ca("afkfleet test CA");
    let (certificate, key) = leaf(&issuer, &["127.0.0.1"]);
    let address = tls_server(certificate, key);

    let answer = client(address, only(&issuer)).health_live().await;

    assert!(answer.is_ok(), "{answer:?}");
}

#[tokio::test]
async fn the_system_roots_refuse_a_private_ca() {
    let issuer = ca("afkfleet test CA");
    let (certificate, key) = leaf(&issuer, &["127.0.0.1"]);
    let address = tls_server(certificate, key);

    let refused = client(address, Trust::System)
        .health_live()
        .await
        .unwrap_err();

    assert!(
        matches!(refused, ClientError::Certificate(_)),
        "{refused:?}"
    );
}

#[tokio::test]
async fn another_ca_is_refused() {
    let issuer = ca("afkfleet test CA");
    let other = ca("another CA");
    let (certificate, key) = leaf(&issuer, &["127.0.0.1"]);
    let address = tls_server(certificate, key);

    let refused = client(address, only(&other))
        .health_live()
        .await
        .unwrap_err();

    assert!(
        matches!(refused, ClientError::Certificate(_)),
        "{refused:?}"
    );
}

#[tokio::test]
async fn a_certificate_for_another_host_is_refused() {
    let issuer = ca("afkfleet test CA");
    let (certificate, key) = leaf(&issuer, &["example.com"]);
    let address = tls_server(certificate, key);

    let refused = client(address, only(&issuer))
        .health_live()
        .await
        .unwrap_err();

    assert!(
        matches!(refused, ClientError::Certificate(_)),
        "{refused:?}"
    );
}
