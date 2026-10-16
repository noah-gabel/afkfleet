//! [`CertificateAuthority`]: the CA's key and certificate, signing the
//! server's certificate and agents' certificates (P10.2); and
//! [`AgentIssuer`], the CA behind the enrollment service's
//! [`AgentCertificates`] port.

use core::fmt;
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use fleet_core::id::AgentId;
use fleet_core::pki::CertFingerprint;
use fleet_core::system::SecureRandom;
use rcgen::{Certificate, Issuer, PublicKeyData, SerialNumber};
use rustls::pki_types::CertificateDer;
use sha2::{Digest as _, Sha256};

pub use super::profiles::PkiError;

use super::csr::check_csr;
use super::keys::Ed25519Key;
use super::profiles::{
    CA_LIFETIME, SERVER_LIFETIME, ServerNames, Validity, agent_params, ca_params, serial,
    server_params,
};
use crate::ports::pki::{AgentCertificates, IssueError, IssuedCertificate};

/// The SHA-256 of a certificate's DER.
#[must_use]
pub fn fingerprint(der: &[u8]) -> CertFingerprint {
    CertFingerprint::from_digest(Sha256::digest(der).into())
}

/// A certificate the CA signed.
#[derive(Clone, PartialEq, Eq)]
pub struct Issued {
    /// The certificate, DER-encoded.
    pub der: CertificateDer<'static>,
    /// The certificate as a PEM file's text.
    pub pem: String,
    /// SHA-256 of `der`.
    pub fingerprint: CertFingerprint,
    /// When it's valid.
    pub validity: Validity,
}

impl Issued {
    /// `cert`, valid for `validity`.
    fn new(cert: &Certificate, validity: Validity) -> Self {
        let der = cert.der().clone();
        Self {
            fingerprint: fingerprint(&der),
            pem: cert.pem(),
            der,
            validity,
        }
    }
}

impl fmt::Debug for Issued {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Issued")
            .field("fingerprint", &self.fingerprint)
            .field("validity", &self.validity)
            .finish_non_exhaustive()
    }
}

/// The CA: its key, which signs, and its certificate, which agents trust.
pub struct CertificateAuthority {
    issuer: Issuer<'static, Ed25519Key>,
    cert_der: CertificateDer<'static>,
    fingerprint: CertFingerprint,
}

impl CertificateAuthority {
    /// A new CA: a key from `random` and a self-signed certificate valid for
    /// [`CA_LIFETIME`] from `now`, which is returned with it.
    ///
    /// # Errors
    /// [`PkiError`] when the key, the serial or the signature fails, or the
    /// validity is out of range.
    pub fn create(
        random: &dyn SecureRandom,
        now: DateTime<Utc>,
    ) -> Result<(Self, Issued), PkiError> {
        let key = Ed25519Key::generate(random)?;
        let validity = Validity::starting(now, CA_LIFETIME)?;
        let params = ca_params(&validity, serial(random)?)?;
        let cert = params.self_signed(&key).map_err(PkiError::Signing)?;
        let issued = Issued::new(&cert, validity);
        let ca = Self {
            issuer: Issuer::new(params, key),
            cert_der: issued.der.clone(),
            fingerprint: issued.fingerprint,
        };
        Ok((ca, issued))
    }

    /// The CA of an existing certificate and its key, as `pki init` wrote
    /// them. Whether the key belongs to the certificate is the caller's
    /// check (`load`).
    ///
    /// # Errors
    /// [`PkiError::Signing`] when rcgen can't read the certificate.
    pub fn from_parts(
        cert_der: CertificateDer<'static>,
        key: Ed25519Key,
    ) -> Result<Self, PkiError> {
        let issuer = Issuer::from_ca_cert_der(&cert_der, key).map_err(PkiError::Signing)?;
        Ok(Self {
            issuer,
            fingerprint: fingerprint(&cert_der),
            cert_der,
        })
    }

    /// The CA certificate, DER-encoded.
    #[must_use]
    pub fn cert_der(&self) -> &CertificateDer<'static> {
        &self.cert_der
    }

    /// The CA certificate's fingerprint.
    #[must_use]
    pub fn fingerprint(&self) -> CertFingerprint {
        self.fingerprint
    }

    /// The CA's key.
    #[must_use]
    pub fn key(&self) -> &Ed25519Key {
        self.issuer.key()
    }

    /// The server's certificate for `key`, valid for `names` and
    /// [`SERVER_LIFETIME`] from `now`.
    ///
    /// # Errors
    /// [`PkiError`] when the serial or the signature fails, or the validity
    /// is out of range.
    pub fn issue_server(
        &self,
        key: &Ed25519Key,
        names: &ServerNames,
        random: &dyn SecureRandom,
        now: DateTime<Utc>,
    ) -> Result<Issued, PkiError> {
        let validity = Validity::starting(now, SERVER_LIFETIME)?;
        let params = server_params(names, &validity, serial(random)?)?;
        let cert = params
            .signed_by(key, &self.issuer)
            .map_err(PkiError::Signing)?;
        Ok(Issued::new(&cert, validity))
    }

    /// An agent's certificate for `public_key`, naming `agent`, valid for
    /// `lifetime` from `now`.
    ///
    /// # Errors
    /// [`PkiError`] when the serial or the signature fails, or the validity
    /// is out of range.
    pub fn issue_agent(
        &self,
        public_key: &impl PublicKeyData,
        agent: AgentId,
        lifetime: TimeDelta,
        random: &dyn SecureRandom,
        now: DateTime<Utc>,
    ) -> Result<Issued, PkiError> {
        self.issue_agent_with(public_key, agent, lifetime, serial(random)?, now)
    }

    /// [`CertificateAuthority::issue_agent`] with a given serial number:
    /// the startup check's probe, which never leaves the process.
    pub(super) fn issue_agent_with(
        &self,
        public_key: &impl PublicKeyData,
        agent: AgentId,
        lifetime: TimeDelta,
        serial: SerialNumber,
        now: DateTime<Utc>,
    ) -> Result<Issued, PkiError> {
        let validity = Validity::starting(now, lifetime)?;
        let params = agent_params(agent, &validity, serial)?;
        let cert = params
            .signed_by(public_key, &self.issuer)
            .map_err(PkiError::Signing)?;
        Ok(Issued::new(&cert, validity))
    }
}

impl fmt::Debug for CertificateAuthority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CertificateAuthority")
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

/// The CA behind the enrollment service's [`AgentCertificates`]: it checks
/// an agent's CSR and signs its certificate with `[grpc]
/// agent_cert_lifetime_days`, serials from the [`SecureRandom`] port.
pub struct AgentIssuer {
    ca: CertificateAuthority,
    random: Arc<dyn SecureRandom>,
    lifetime: TimeDelta,
}

impl AgentIssuer {
    /// The issuer signing with `ca`, for `lifetime`.
    #[must_use]
    pub fn new(
        ca: CertificateAuthority,
        random: Arc<dyn SecureRandom>,
        lifetime: TimeDelta,
    ) -> Self {
        Self {
            ca,
            random,
            lifetime,
        }
    }
}

impl fmt::Debug for AgentIssuer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentIssuer")
            .field("ca", &self.ca)
            .field("lifetime", &self.lifetime)
            .finish_non_exhaustive()
    }
}

impl AgentCertificates for AgentIssuer {
    fn issue(
        &self,
        csr_der: &[u8],
        agent: AgentId,
        now: DateTime<Utc>,
    ) -> Result<IssuedCertificate, IssueError> {
        let csr = check_csr(csr_der)?;
        let issued = self
            .ca
            .issue_agent(&csr, agent, self.lifetime, self.random.as_ref(), now)
            .map_err(|error| match error {
                PkiError::Random(error) => IssueError::Random(error),
                PkiError::OutOfRange(_) => IssueError::OutOfRange,
                other => IssueError::Signing(Box::new(other)),
            })?;
        Ok(IssuedCertificate {
            der: issued.der.to_vec(),
            pem: issued.pem,
            fingerprint: issued.fingerprint,
            not_before: issued.validity.not_before,
            not_after: issued.validity.not_after,
        })
    }

    fn ca_fingerprint(&self) -> CertFingerprint {
        self.ca.fingerprint
    }
}

#[cfg(test)]
mod tests {
    use fleet_core::pki::ed25519_pkcs8;
    use fleet_core::system::RandomError;
    use fleet_testkit::system::SeededRandom;
    use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
    use rustls::RootCertStore;
    use rustls::client::WebPkiServerVerifier;
    use rustls::client::danger::ServerCertVerifier as _;
    use rustls::crypto::aws_lc_rs;
    use rustls::pki_types::pem::PemObject as _;
    use rustls::pki_types::{PrivatePkcs8KeyDer, ServerName, UnixTime};
    use rustls::server::WebPkiClientVerifier;
    use secrecy::ExposeSecret as _;
    use x509_parser::prelude::{FromDer as _, GeneralName, X509Certificate};

    use super::*;
    use crate::ports::pki::CsrRefusal;

    /// A self-signed CA certificate made by openssl 3 (its key was never
    /// stored), and its fingerprint as `openssl x509 -noout -fingerprint
    /// -sha256` printed it: fleet-client's outside check of the hash
    /// (ADR-0017), repeated here for the server's.
    const FIXED_CA: &str = "-----BEGIN CERTIFICATE-----
MIIBjTCCATOgAwIBAgIULO/NN7VKM7pfDJO/Uo0nAFTrbg4wCgYIKoZIzj0EAwIw
GzEZMBcGA1UEAwwQYWZrZmxlZXQgdGVzdCBDQTAgFw0yNjEwMTIyMDU0NTlaGA8y
MTI2MDkxODIwNTQ1OVowGzEZMBcGA1UEAwwQYWZrZmxlZXQgdGVzdCBDQTBZMBMG
ByqGSM49AgEGCCqGSM49AwEHA0IABOJXQkDkyUrHWRh3vlx4o00GZ/msG2x/CABM
Bn5ZIjptno96IKg4bf5ERIrs1/nR5n+dfUxGa6nb7U1+WtBuYjajUzBRMB0GA1Ud
DgQWBBRoq5eNcQiDTSuK/WnAZGTO0TXL+DAfBgNVHSMEGDAWgBRoq5eNcQiDTSuK
/WnAZGTO0TXL+DAPBgNVHRMBAf8EBTADAQH/MAoGCCqGSM49BAMCA0gAMEUCIQDL
sOxjMINkK3TAT0v8m1sHV5Hfe1lu1oXJy5/5etF6cgIgGftIj7GG54P6yY5ZM933
03Ooy69l653HwlrIhUOnBRA=
-----END CERTIFICATE-----
";
    const FIXED_CA_SHA256: &str = "17:25:5B:0A:45:82:70:4B:AE:77:5D:9E:55:B4:EF:BD:F0:0E:72:76:7D:1F:B5:7E:1A:A2:7D:06:4D:3A:A1:3C";

    const NOW: &str = "2027-01-15T08:00:00.789Z";

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    fn ca() -> CertificateAuthority {
        CertificateAuthority::create(&SeededRandom::new(1), at(NOW))
            .unwrap()
            .0
    }

    fn agent() -> AgentId {
        "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap()
    }

    /// An agent's key, as the agent makes one.
    fn agent_key(seed: u8) -> KeyPair {
        let der = ed25519_pkcs8(&[seed; 32]);
        KeyPair::from_pkcs8_der_and_sign_algo(
            &PrivatePkcs8KeyDer::from(der.expose_secret().as_slice()),
            &PKCS_ED25519,
        )
        .unwrap()
    }

    fn issue_agent(ca: &CertificateAuthority, key: &KeyPair) -> Issued {
        ca.issue_agent(
            key,
            agent(),
            TimeDelta::days(60),
            &SeededRandom::new(2),
            at(NOW),
        )
        .unwrap()
    }

    fn unix(at: DateTime<Utc>) -> UnixTime {
        UnixTime::since_unix_epoch(core::time::Duration::from_secs(
            u64::try_from(at.timestamp()).unwrap(),
        ))
    }

    fn roots(ca: &CertificateAuthority) -> Arc<RootCertStore> {
        let mut roots = RootCertStore::empty();
        roots.add(ca.cert_der().clone()).unwrap();
        Arc::new(roots)
    }

    /// Whether rustls, as the gRPC listener will in group C, accepts
    /// `cert` as a client certificate at `when`.
    fn client_accepts(ca: &CertificateAuthority, cert: &Issued, when: DateTime<Utc>) -> bool {
        WebPkiClientVerifier::builder_with_provider(
            roots(ca),
            Arc::new(aws_lc_rs::default_provider()),
        )
        .build()
        .unwrap()
        .verify_client_cert(&cert.der, &[], unix(when))
        .is_ok()
    }

    /// Whether rustls, as an agent will, accepts `cert` as the server's for
    /// `name` at `when`.
    fn server_accepts(
        ca: &CertificateAuthority,
        cert: &Issued,
        name: &str,
        when: DateTime<Utc>,
    ) -> bool {
        WebPkiServerVerifier::builder_with_provider(
            roots(ca),
            Arc::new(aws_lc_rs::default_provider()),
        )
        .build()
        .unwrap()
        .verify_server_cert(
            &cert.der,
            &[],
            &ServerName::try_from(name.to_owned()).unwrap(),
            &[],
            unix(when),
        )
        .is_ok()
    }

    fn names() -> ServerNames {
        ServerNames::try_new(&["localhost".to_owned()], &["127.0.0.1".parse().unwrap()]).unwrap()
    }

    /// `der` read back with x509-parser.
    fn parsed(der: &[u8]) -> X509Certificate<'_> {
        X509Certificate::from_der(der).unwrap().1
    }

    #[test]
    fn a_fingerprint_is_the_sha256_openssl_prints() {
        let der = CertificateDer::from_pem_slice(FIXED_CA.as_bytes()).unwrap();

        assert_eq!(fingerprint(&der).to_string(), FIXED_CA_SHA256);
    }

    #[test]
    fn a_new_ca_is_a_ca_that_one_seed_makes_once() {
        let (one, cert) = CertificateAuthority::create(&SeededRandom::new(1), at(NOW)).unwrap();
        let (same, same_cert) =
            CertificateAuthority::create(&SeededRandom::new(1), at(NOW)).unwrap();

        assert!(parsed(&cert.der).is_ca());
        assert_eq!(cert.der, *one.cert_der());
        assert_eq!(one.fingerprint(), fingerprint(&cert.der));
        assert_eq!(cert, same_cert);
        assert_eq!(one.key().public_key_raw(), same.key().public_key_raw());
        assert_eq!(cert.validity.not_before, at("2027-01-15T07:00:00Z"));
        assert_eq!(cert.validity.not_after, at("2037-01-12T08:00:00Z"));
        assert!(cert.pem.starts_with("-----BEGIN CERTIFICATE-----"));
    }

    #[test]
    fn a_ca_read_back_from_its_parts_signs_alike() {
        let ca = ca();
        let key = Ed25519Key::from_pem(ca.key().to_pem().as_bytes()).unwrap();

        let again = CertificateAuthority::from_parts(ca.cert_der().clone(), key).unwrap();
        let issued = issue_agent(&again, &agent_key(7));

        assert_eq!(again.fingerprint(), ca.fingerprint());
        assert!(client_accepts(&ca, &issued, at(NOW)));
    }

    #[test]
    fn an_agent_certificate_is_accepted_until_its_not_after_and_never_before() {
        let ca = ca();

        let issued = issue_agent(&ca, &agent_key(7));

        let Validity {
            not_before,
            not_after,
        } = issued.validity;
        assert_eq!(not_before, at("2027-01-15T07:00:00Z"));
        assert_eq!(not_after, at("2027-03-16T08:00:00Z"));
        assert!(client_accepts(&ca, &issued, not_before));
        assert!(client_accepts(&ca, &issued, not_after));
        assert!(!client_accepts(
            &ca,
            &issued,
            not_after + TimeDelta::seconds(1)
        ));
        assert!(!client_accepts(
            &ca,
            &issued,
            not_before - TimeDelta::seconds(1)
        ));
    }

    #[test]
    fn an_agent_certificate_names_the_agent_and_certifies_its_key() {
        let ca = ca();
        let key = agent_key(7);

        let issued = issue_agent(&ca, &key);
        let cert = parsed(&issued.der);

        assert_eq!(issued.fingerprint, fingerprint(&issued.der));
        assert_eq!(
            cert.subject_alternative_name()
                .unwrap()
                .unwrap()
                .value
                .general_names,
            [GeneralName::URI(
                "urn:uuid:018bcfe5-6800-7bab-abab-abababababab"
            )]
        );
        let usage = cert.extended_key_usage().unwrap().unwrap().value;
        assert!(usage.client_auth && !usage.server_auth && !usage.any);
        assert!(!cert.is_ca());
        // A client presenting it must prove this key, which rustls checks in
        // the handshake.
        assert_eq!(
            cert.public_key().subject_public_key.data.as_ref(),
            key.public_key_raw()
        );
    }

    #[test]
    fn an_agent_certificate_is_never_a_servers() {
        let ca = ca();

        let issued = issue_agent(&ca, &agent_key(7));

        for name in [
            "localhost",
            "127.0.0.1",
            "018bcfe5-6800-7bab-abab-abababababab",
        ] {
            assert!(!server_accepts(&ca, &issued, name, at(NOW)), "{name}");
        }
    }

    #[test]
    fn one_key_certified_twice_gets_two_serials() {
        let ca = ca();
        let random = SeededRandom::new(2);
        let issue = || {
            ca.issue_agent(
                &agent_key(7),
                agent(),
                TimeDelta::days(60),
                &random,
                at(NOW),
            )
            .unwrap()
        };

        let one = issue();
        let two = issue();

        assert_ne!(parsed(&one.der).raw_serial(), parsed(&two.der).raw_serial());
        assert_ne!(one.fingerprint, two.fingerprint);
    }

    #[test]
    fn a_server_certificate_is_valid_for_its_names_only_and_a_year() {
        let ca = ca();
        let key = Ed25519Key::generate(&SeededRandom::new(3)).unwrap();

        let issued = ca
            .issue_server(&key, &names(), &SeededRandom::new(4), at(NOW))
            .unwrap();

        assert_eq!(issued.validity.not_after, at("2028-01-15T08:00:00Z"));
        assert!(server_accepts(&ca, &issued, "localhost", at(NOW)));
        assert!(server_accepts(&ca, &issued, "127.0.0.1", at(NOW)));
        assert!(!server_accepts(&ca, &issued, "fleet.example.com", at(NOW)));
        assert!(!client_accepts(&ca, &issued, at(NOW)));
        assert_eq!(
            parsed(&issued.der)
                .public_key()
                .subject_public_key
                .data
                .as_ref(),
            key.public_key_raw()
        );
    }

    #[test]
    fn another_ca_s_certificates_are_refused() {
        let ca = ca();
        let other = CertificateAuthority::create(&SeededRandom::new(9), at(NOW))
            .unwrap()
            .0;

        let issued = issue_agent(&other, &agent_key(7));

        assert!(!client_accepts(&ca, &issued, at(NOW)));
    }

    #[test]
    fn a_failing_random_source_signs_nothing() {
        let ca = ca();
        let random = SeededRandom::new(2);
        random.fail_next(RandomError::Unavailable);

        let refused = ca.issue_agent(
            &agent_key(7),
            agent(),
            TimeDelta::days(60),
            &random,
            at(NOW),
        );

        assert!(matches!(
            refused,
            Err(PkiError::Random(RandomError::Unavailable))
        ));
    }

    #[test]
    fn the_issuer_checks_the_csr_and_signs_for_its_lifetime() {
        let ca = ca();
        let ca_fingerprint = ca.fingerprint();
        let issuer = AgentIssuer::new(ca, Arc::new(SeededRandom::new(2)), TimeDelta::days(30));
        let key = agent_key(7);
        let csr = CertificateParams::default()
            .serialize_request(&key)
            .unwrap()
            .der()
            .to_vec();

        let certificate = issuer.issue(&csr, agent(), at(NOW)).unwrap();
        let refused = issuer.issue(b"not a csr", agent(), at(NOW));

        assert_eq!(certificate.not_before, at("2027-01-15T07:00:00Z"));
        assert_eq!(certificate.not_after, at("2027-02-14T08:00:00Z"));
        assert_eq!(certificate.fingerprint, fingerprint(&certificate.der));
        assert_eq!(
            parsed(&certificate.der)
                .public_key()
                .subject_public_key
                .data
                .as_ref(),
            key.public_key_raw()
        );
        assert!(matches!(
            refused,
            Err(IssueError::Csr(CsrRefusal::Unparsable))
        ));
        assert_eq!(issuer.ca_fingerprint(), ca_fingerprint);
    }
}
