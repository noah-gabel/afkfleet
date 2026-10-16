//! The certificate the server answers, checked before it's stored
//! (Plan.md P10.3; ADR-0019, R10). The agent stores only a certificate it
//! can use:
//!
//! - **One PEM section and nothing around it:** the text starts with its
//!   `BEGIN` line and ends after its `END` line (one line break allowed),
//!   and holds exactly one section, a certificate. The file is exactly the
//!   text that was checked.
//! - **Signed by the pinned CA, for client authentication,** at the current
//!   time: rustls's client verifier with the pinned CA as its one root, the
//!   check the server's port makes when the agent connects (group C).
//! - **For the agent's key.**

use std::sync::Arc;

use rustls::RootCertStore;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, UnixTime};
use rustls::server::WebPkiClientVerifier;
use rustls::sign::CertifiedKey;

use super::key::AgentKey;
use super::pin::one_certificate;

/// A certificate's PEM begins with this line.
const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
/// And ends with this one.
const END: &str = "-----END CERTIFICATE-----";

/// Why the agent won't store the certificate it got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CertificateProblem {
    /// The text isn't one certificate's PEM, and nothing else.
    #[error("the certificate isn't one PEM certificate")]
    NotOnePem,
    /// The pinned CA didn't sign it for client authentication, or it isn't
    /// valid now.
    #[error("the certificate isn't a client certificate the pinned CA signed, valid now")]
    NotFromTheCa,
    /// It's for another key.
    #[error("the certificate isn't for this agent's key")]
    NotOurKey,
}

/// Checks `pem` against the pinned CA's `ca_der` and the agent's `key` at
/// `now`; see the [module docs](self).
///
/// # Errors
/// The first [`CertificateProblem`].
pub fn check(
    pem: &str,
    ca_der: &[u8],
    key: &AgentKey,
    now: UnixTime,
) -> Result<(), CertificateProblem> {
    let framed = pem
        .strip_suffix("\r\n")
        .or_else(|| pem.strip_suffix('\n'))
        .unwrap_or(pem);
    if !framed.starts_with(BEGIN) || !framed.ends_with(END) {
        return Err(CertificateProblem::NotOnePem);
    }
    let der = one_certificate(pem.as_bytes()).ok_or(CertificateProblem::NotOnePem)?;
    let certificate = CertificateDer::from(der);
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(ca_der.to_vec()))
        .map_err(|_| CertificateProblem::NotFromTheCa)?;
    let verifier =
        WebPkiClientVerifier::builder_with_provider(Arc::new(roots), Arc::clone(&provider))
            .build()
            .map_err(|_| CertificateProblem::NotFromTheCa)?;
    verifier
        .verify_client_cert(&certificate, &[], now)
        .map_err(|_| CertificateProblem::NotFromTheCa)?;
    let key_der = PrivateKeyDer::Pkcs8(key.pkcs8_der().to_vec().into());
    CertifiedKey::from_der(vec![certificate], key_der, &provider)
        .and_then(|certified| certified.keys_match())
        .map_err(|_| CertificateProblem::NotOurKey)
}

#[cfg(test)]
mod tests {
    use fleet_core::id::AgentId;
    use fleet_testkit::system::SeededRandom;
    use rcgen::{
        BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
        PKCS_ED25519, SanType,
    };
    use rustls::pki_types::PrivatePkcs8KeyDer;
    use secrecy::ExposeSecret as _;

    use super::*;

    fn pair(seed: u8) -> KeyPair {
        let der = fleet_core::pki::ed25519_pkcs8(&[seed; 32]);
        KeyPair::from_pkcs8_der_and_sign_algo(
            &PrivatePkcs8KeyDer::from(der.expose_secret().as_slice()),
            &PKCS_ED25519,
        )
        .unwrap()
    }

    /// A CA from `seed`: its certificate's DER and its issuer.
    fn ca(seed: u8) -> (Vec<u8>, Issuer<'static, KeyPair>) {
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        let key = pair(seed);
        let cert = params.self_signed(&key).unwrap();
        (cert.der().to_vec(), Issuer::new(params, key))
    }

    /// A certificate for `key` that `issuer` signs, for `usage`, as PEM.
    fn issued(
        key: &AgentKey,
        issuer: &Issuer<'static, KeyPair>,
        usage: ExtendedKeyUsagePurpose,
    ) -> String {
        let agent = AgentId::new_v7(chrono::DateTime::UNIX_EPOCH, [1; 10]).unwrap();
        let mut params = CertificateParams::default();
        params.subject_alt_names = vec![SanType::URI(
            format!("urn:uuid:{agent}").try_into().unwrap(),
        )];
        params.extended_key_usages = vec![usage];
        let public = pair_of(key);
        params.signed_by(&public, issuer).unwrap().pem()
    }

    /// The key pair behind `key`, for signing its certificate in a test.
    fn pair_of(key: &AgentKey) -> KeyPair {
        KeyPair::from_pkcs8_der_and_sign_algo(
            &PrivatePkcs8KeyDer::from(key.pkcs8_der()),
            &PKCS_ED25519,
        )
        .unwrap()
    }

    fn agent_key(seed: u64) -> AgentKey {
        AgentKey::generate(&SeededRandom::new(seed)).unwrap()
    }

    #[test]
    fn a_client_certificate_the_ca_signed_for_the_key_is_kept() {
        let (ca_der, issuer) = ca(1);
        let key = agent_key(1);
        let pem = issued(&key, &issuer, ExtendedKeyUsagePurpose::ClientAuth);

        let checked = check(&pem, &ca_der, &key, UnixTime::now());

        assert_eq!(checked, Ok(()));
    }

    #[test]
    fn text_around_the_section_or_a_second_one_is_refused() {
        let (ca_der, issuer) = ca(1);
        let key = agent_key(1);
        let pem = issued(&key, &issuer, ExtendedKeyUsagePurpose::ClientAuth);

        for text in [
            format!("a comment\n{pem}"),
            format!("{pem}trailing"),
            format!("{pem}{pem}"),
            format!("{pem}\n\n"),
            String::new(),
            pem.replace("CERTIFICATE", "PRIVATE KEY"),
        ] {
            assert_eq!(
                check(&text, &ca_der, &key, UnixTime::now()),
                Err(CertificateProblem::NotOnePem),
                "{text:?}"
            );
        }
    }

    #[test]
    fn another_cas_certificate_is_refused() {
        let (ca_der, _) = ca(1);
        let (_, other) = ca(2);
        let key = agent_key(1);
        let pem = issued(&key, &other, ExtendedKeyUsagePurpose::ClientAuth);

        let checked = check(&pem, &ca_der, &key, UnixTime::now());

        assert_eq!(checked, Err(CertificateProblem::NotFromTheCa));
    }

    #[test]
    fn a_server_certificate_is_refused() {
        let (ca_der, issuer) = ca(1);
        let key = agent_key(1);
        let pem = issued(&key, &issuer, ExtendedKeyUsagePurpose::ServerAuth);

        let checked = check(&pem, &ca_der, &key, UnixTime::now());

        assert_eq!(checked, Err(CertificateProblem::NotFromTheCa));
    }

    #[test]
    fn a_certificate_for_another_key_is_refused() {
        let (ca_der, issuer) = ca(1);
        let key = agent_key(1);
        let pem = issued(&agent_key(2), &issuer, ExtendedKeyUsagePurpose::ClientAuth);

        let checked = check(&pem, &ca_der, &key, UnixTime::now());

        assert_eq!(checked, Err(CertificateProblem::NotOurKey));
    }
}
