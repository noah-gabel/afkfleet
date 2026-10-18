//! A test PKI made at run time from seeded bytes (ADR-0019, S10): a CA, a
//! managed agent's identity files, and a server certificate for a fake
//! control port. Never `KeyPair::generate`.

use std::fs;
use std::path::{Path, PathBuf};

use fleet_agent::config::ControlPlaneConfig;
use fleet_agent::enroll::key::AgentKey;
use fleet_core::id::AgentId;
use fleet_core::net::PrivateTargets;
use fleet_testkit::system::SeededRandom;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    PKCS_ED25519, SanType,
};
use rustls::pki_types::PrivatePkcs8KeyDer;
use secrecy::ExposeSecret as _;

/// The Ed25519 key pair of `seed`.
pub(crate) fn pair(seed: u8) -> KeyPair {
    let der = fleet_core::pki::ed25519_pkcs8(&[seed; 32]);
    KeyPair::from_pkcs8_der_and_sign_algo(
        &PrivatePkcs8KeyDer::from(der.expose_secret().as_slice()),
        &PKCS_ED25519,
    )
    .unwrap()
}

/// The agent's key of `seed`, as enroll makes it.
pub(crate) fn agent_key(seed: u64) -> AgentKey {
    AgentKey::generate(&SeededRandom::new(seed)).unwrap()
}

/// When a certificate is valid, as rcgen's dates.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Validity {
    pub(crate) from: (i32, u8, u8),
    pub(crate) until: (i32, u8, u8),
}

/// From 2020 to 2090: valid whenever these tests run.
pub(crate) const ALWAYS: Validity = Validity {
    from: (2020, 1, 1),
    until: (2090, 1, 1),
};

/// A CA, the server's agent CA as `pki init` makes it.
pub(crate) struct TestCa {
    issuer: Issuer<'static, KeyPair>,
    /// Its certificate's PEM.
    pub(crate) pem: String,
    /// Its certificate's DER.
    pub(crate) der: Vec<u8>,
}

impl TestCa {
    /// The CA of `seed`.
    pub(crate) fn new(seed: u8) -> Self {
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params
            .distinguished_name
            .push(DnType::CommonName, format!("afkfleet test CA {seed}"));
        let key = pair(seed);
        let cert = params.self_signed(&key).unwrap();
        Self {
            pem: cert.pem(),
            der: cert.der().to_vec(),
            issuer: Issuer::new(params, key),
        }
    }

    /// A client certificate for `key`, as the server issues one to agent
    /// `agent`, valid as `validity` says: its PEM.
    pub(crate) fn agent_certificate(
        &self,
        key: &AgentKey,
        agent: AgentId,
        validity: Validity,
    ) -> String {
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
        params.not_before = rcgen::date_time_ymd(validity.from.0, validity.from.1, validity.from.2);
        params.not_after =
            rcgen::date_time_ymd(validity.until.0, validity.until.1, validity.until.2);
        params.signed_by(&public, &self.issuer).unwrap().pem()
    }

    /// A server certificate for `localhost` and `127.0.0.1`, of key `seed`:
    /// its PEM and the key's.
    pub(crate) fn server_certificate(&self, seed: u8) -> (String, String) {
        let key = pair(seed);
        let mut params = CertificateParams::default();
        params.subject_alt_names = vec![
            SanType::DnsName("localhost".try_into().unwrap()),
            SanType::IpAddress([127, 0, 0, 1].into()),
        ];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let cert = params.signed_by(&key, &self.issuer).unwrap();
        (cert.pem(), key.serialize_pem())
    }
}

/// A fresh, empty folder for test `name` under cargo's temp folder.
pub(crate) fn test_dir(group: &str, name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(group)
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes `bytes` to `path`, owner-only on Unix, as enroll writes them.
pub(crate) fn write_owner_only(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

/// A managed agent's files in `dir`: the CA, its key and its certificate,
/// as `pki init` and `enroll` leave them, and the config naming them for
/// the server at `url`.
pub(crate) fn identity_files(
    dir: &Path,
    url: &str,
    ca: &TestCa,
    key: &AgentKey,
    certificate: &str,
) -> ControlPlaneConfig {
    let plane = ControlPlaneConfig {
        url: url.to_owned(),
        ca_cert_file: dir.join("fleet_ca.crt"),
        cert_file: dir.join("agent.crt"),
        key_file: dir.join("agent.key"),
        private_targets: PrivateTargets::default(),
    };
    write_owner_only(&plane.ca_cert_file, ca.pem.as_bytes());
    write_owner_only(&plane.key_file, key.to_pem().as_bytes());
    write_owner_only(&plane.cert_file, certificate.as_bytes());
    plane
}

/// The agent ID of `n`.
pub(crate) fn agent_id(n: u8) -> AgentId {
    AgentId::new_v7(chrono::DateTime::UNIX_EPOCH, [n; 10]).unwrap()
}
