//! Certificates are always checked (Plan.md §7.5, ADR-0017): against the
//! system's roots, or only against a configured CA. Each test makes a CA and
//! a server certificate at run time with rcgen, since no key is ever
//! committed, and serves one answer from a rustls server on a thread; the
//! live events socket's three connections each present their own (P11.8;
//! ADR-0020, S55).

use core::time::Duration;
use std::io::{Read, Write as _};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use fleet_client::{
    Client, ClientConfig, ClientError, CustomCa, LiveEnd, LiveUpdate, MemoryStore, RefetchReason,
    ServerUrl, SessionStore as _, Trust,
};
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, DnType, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use secrecy::SecretString;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

/// How long the test server waits for the client before giving up.
const SOCKET_WAIT: Duration = Duration::from_secs(5);
/// How long a test waits for the live socket.
const PATIENCE: Duration = Duration::from_secs(10);

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

/// A TLS server's config that presents `certificate`.
fn server_config(
    certificate: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
) -> Arc<rustls::ServerConfig> {
    Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .unwrap(),
    )
}

/// A TLS server on a thread that presents `certificate` and answers one
/// request with an empty 200, the way `/health/live` does. A client that
/// refuses the certificate ends the handshake; the thread then just ends.
fn tls_server(certificate: CertificateDer<'static>, key: PrivateKeyDer<'static>) -> SocketAddr {
    let config = server_config(certificate, key);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        stream.set_read_timeout(Some(SOCKET_WAIT)).unwrap();
        stream.set_write_timeout(Some(SOCKET_WAIT)).unwrap();
        let connection = rustls::ServerConnection::new(config).unwrap();
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

/// The refresh's answer the live server gives on its first connection.
const REFRESHED: &str = r#"{"access_token":"afk_at_tls","refresh_token":"afk_rt_tls2","access_expires_in":900,"refresh_expires_in":604800}"#;
/// The ticket's answer, on its second.
const TICKET: &str = r#"{"ticket":"afk_wt_tls","expires_in":30}"#;
/// `ready`, as the server's unmasked text frame.
const READY_FRAME: &[u8] = b"\x81\x10{\"type\":\"ready\"}";

/// One request's head off `tls`, its body read and dropped, or `None` once
/// the connection ends first: a refused certificate ends it in the
/// handshake.
fn request_head(tls: &mut impl Read) -> Option<String> {
    let mut read = Vec::new();
    let mut buffer = [0; 1024];
    let end = loop {
        if let Some(at) = read.windows(4).position(|window| window == b"\r\n\r\n") {
            break at + 4;
        }
        match tls.read(&mut buffer) {
            Ok(0) | Err(_) => return None,
            Ok(n) => read.extend_from_slice(&buffer[..n]),
        }
    };
    let head = String::from_utf8_lossy(&read[..end]).into_owned();
    let length: usize = header(&head, "content-length").map_or(0, |length| length.parse().unwrap());
    while read.len() < end + length {
        match tls.read(&mut buffer) {
            Ok(0) | Err(_) => return None,
            Ok(n) => read.extend_from_slice(&buffer[..n]),
        }
    }
    Some(head)
}

/// The value of `head`'s header `name`, whose case doesn't matter.
fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

/// A 200 with `body` that ends its connection.
fn answer_json(
    tls: &mut rustls::StreamOwned<rustls::ServerConnection, std::net::TcpStream>,
    body: &str,
) {
    let answer = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = tls.write_all(answer.as_bytes());
    let _ = tls.flush();
    tls.conn.send_close_notify();
    let _ = tls.flush();
}

/// The live events socket's server: three connections over TLS, each
/// presenting its own of `certificates`, in order: the refresh, the ticket,
/// then the upgrade, whose 101 says `ready` and reads until the client
/// leaves. The first two answers end their connections, so the client opens
/// each next one, and a certificate refused on the third is the socket's own
/// (ADR-0020, S55).
fn live_server(certificates: Vec<(CertificateDer<'static>, PrivateKeyDer<'static>)>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        for (n, (certificate, key)) in certificates.into_iter().enumerate() {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            stream.set_read_timeout(Some(SOCKET_WAIT)).unwrap();
            stream.set_write_timeout(Some(SOCKET_WAIT)).unwrap();
            let connection =
                rustls::ServerConnection::new(server_config(certificate, key)).unwrap();
            let mut tls = rustls::StreamOwned::new(connection, stream);
            let Some(head) = request_head(&mut tls) else {
                return;
            };
            match n {
                0 => answer_json(&mut tls, REFRESHED),
                1 => answer_json(&mut tls, TICKET),
                _ => {
                    let key = header(&head, "sec-websocket-key").unwrap();
                    let accept = derive_accept_key(key.as_bytes());
                    let switched = format!(
                        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                    );
                    let _ = tls.write_all(switched.as_bytes());
                    let _ = tls.write_all(READY_FRAME);
                    let _ = tls.flush();
                    let mut buffer = [0; 1024];
                    while matches!(tls.read(&mut buffer), Ok(n) if n > 0) {}
                }
            }
        }
    });
    address
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

/// A client of `address` trusting `trust`, with a stored refresh token, so
/// its first authenticated call refreshes.
async fn signed_in(address: SocketAddr, trust: Trust) -> Client<MemoryStore> {
    let store = MemoryStore::new();
    store
        .save(&SecretString::from("afk_rt_tls1"))
        .await
        .unwrap();
    Client::new(
        ClientConfig {
            server: ServerUrl::parse(&format!("https://{address}")).unwrap(),
            trust,
        },
        store,
    )
    .unwrap()
}

#[tokio::test]
async fn the_live_socket_checks_its_own_connection_against_the_configured_ca() {
    let issuer = ca("afkfleet test CA");
    let certificates = (0..3).map(|_| leaf(&issuer, &["127.0.0.1"])).collect();
    let address = live_server(certificates);
    let client = signed_in(address, only(&issuer)).await;
    let mut live = client.live_events();

    let first = timeout(PATIENCE, live.recv()).await.unwrap();

    assert!(
        matches!(first, Ok(LiveUpdate::Refetch(RefetchReason::Ready))),
        "{first:?}"
    );
    live.close().await;
}

#[tokio::test]
async fn a_certificate_refused_on_the_upgrade_ends_the_live_socket_with_no_wait() {
    let issuer = ca("afkfleet test CA");
    let other = ca("another CA");
    let certificates = vec![
        leaf(&issuer, &["127.0.0.1"]),
        leaf(&issuer, &["127.0.0.1"]),
        leaf(&other, &["127.0.0.1"]),
    ];
    let address = live_server(certificates);
    let client = signed_in(address, only(&issuer)).await;
    let mut live = client.live_events();

    let ended = timeout(PATIENCE, live.recv()).await.unwrap();

    assert!(matches!(ended, Err(LiveEnd::Certificate)), "{ended:?}");
    assert!(matches!(live.recv().await, Err(LiveEnd::Certificate)));
}
