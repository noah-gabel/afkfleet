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

use chrono::{DateTime, Utc};
use rustls::RootCertStore;

use rustls::CertificateError;
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
    /// The pinned CA signed it for the key, and it expired: it was valid
    /// until `not_after`, and it's `now` by this host's clock.
    #[error("the certificate expired at {not_after}; it's {now} by this host's clock")]
    Expired {
        /// Its end.
        not_after: DateTime<Utc>,
        /// The time it was checked at.
        now: DateTime<Utc>,
    },
    /// The pinned CA signed it for the key, and it isn't valid yet by this
    /// host's clock.
    #[error("the certificate isn't valid yet by this host's clock")]
    NotYetValid,
}

/// Checks `pem` against the pinned CA's `ca_der` and the agent's `key` at
/// `now`, and returns the certificate's DER; see the [module docs](self).
/// The time is judged last: a certificate that's only out of its validity
/// is checked again at the bound it crossed, so another CA's certificate
/// or another key's is never reported as expired (ADR-0019, S68).
///
/// # Errors
/// The first [`CertificateProblem`], in the order `NotOnePem`,
/// `NotFromTheCa`, `NotOurKey`, then `Expired` or `NotYetValid`.
pub fn check(
    pem: &str,
    ca_der: &[u8],
    key: &AgentKey,
    now: UnixTime,
) -> Result<Vec<u8>, CertificateProblem> {
    let framed = pem
        .strip_suffix("\r\n")
        .or_else(|| pem.strip_suffix('\n'))
        .unwrap_or(pem);
    if !framed.starts_with(BEGIN) || !framed.ends_with(END) {
        return Err(CertificateProblem::NotOnePem);
    }
    let der = one_certificate(pem.as_bytes()).ok_or(CertificateProblem::NotOnePem)?;
    let timing = chain_at(&der, ca_der, now)?;
    let provider = rustls::crypto::aws_lc_rs::default_provider();
    let key_der = PrivateKeyDer::Pkcs8(key.pkcs8_der().to_vec().into());
    CertifiedKey::from_der(vec![CertificateDer::from(der.clone())], key_der, &provider)
        .and_then(|certified| certified.keys_match())
        .map_err(|_| CertificateProblem::NotOurKey)?;
    timing.map_or(Ok(der), Err)
}

/// Whether `cert_der` is a client certificate `ca_der` signed, valid at
/// `now`: the managed agent's check of its kept certificate before each
/// attempt to connect, without the key (ADR-0019, R52).
///
/// # Errors
/// [`CertificateProblem::NotFromTheCa`], then `Expired` or `NotYetValid`.
pub fn validity(cert_der: &[u8], ca_der: &[u8], now: UnixTime) -> Result<(), CertificateProblem> {
    chain_at(cert_der, ca_der, now)?.map_or(Ok(()), Err)
}

/// rustls's client verifier, `ca_der` its one root: the check the server's
/// port makes (group C) at `now`. Anything but the time is
/// [`CertificateProblem::NotFromTheCa`]; a certificate out of its validity
/// alone is checked again at the bound it crossed, and comes back as its
/// time problem.
fn chain_at(
    cert_der: &[u8],
    ca_der: &[u8],
    now: UnixTime,
) -> Result<Option<CertificateProblem>, CertificateProblem> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(ca_der.to_vec()))
        .map_err(|_| CertificateProblem::NotFromTheCa)?;
    let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
        .build()
        .map_err(|_| CertificateProblem::NotFromTheCa)?;
    let certificate = CertificateDer::from(cert_der);
    let verify = |time| {
        verifier
            .verify_client_cert(&certificate, &[], time)
            .map(drop)
    };
    let (bound, problem) = match verify(now) {
        Ok(()) => return Ok(None),
        Err(rustls::Error::InvalidCertificate(CertificateError::ExpiredContext {
            not_after,
            ..
        })) => (
            not_after,
            CertificateProblem::Expired {
                not_after: date(not_after),
                now: date(now),
            },
        ),
        Err(rustls::Error::InvalidCertificate(CertificateError::NotValidYetContext {
            not_before,
            ..
        })) => (not_before, CertificateProblem::NotYetValid),
        Err(_) => return Err(CertificateProblem::NotFromTheCa),
    };
    // Valid at its bound, both of which count: only its time is wrong.
    verify(bound).map_err(|_| CertificateProblem::NotFromTheCa)?;
    Ok(Some(problem))
}

/// `time` as a date; one past chrono's range is its last date.
fn date(time: UnixTime) -> DateTime<Utc> {
    i64::try_from(time.as_secs())
        .ok()
        .and_then(|secs| DateTime::from_timestamp(secs, 0))
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
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

        assert_eq!(checked, Ok(one_certificate(pem.as_bytes()).unwrap()));
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

    // --- Its time (ADR-0019, S68, R52) ---

    /// 2026-01-01 and 2026-06-01, midnight UTC.
    const START: u64 = 1_767_225_600;
    const END: u64 = 1_780_272_000;

    fn at(secs: u64) -> UnixTime {
        UnixTime::since_unix_epoch(core::time::Duration::from_secs(secs))
    }

    fn date(secs: u64) -> DateTime<Utc> {
        DateTime::from_timestamp(i64::try_from(secs).unwrap(), 0).unwrap()
    }

    /// A client certificate for `key` that `issuer` signs, valid from START
    /// to END.
    fn dated(key: &AgentKey, issuer: &Issuer<'static, KeyPair>) -> String {
        let mut params = CertificateParams::default();
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        params.not_before = rcgen::date_time_ymd(2026, 1, 1);
        params.not_after = rcgen::date_time_ymd(2026, 6, 1);
        params.signed_by(&pair_of(key), issuer).unwrap().pem()
    }

    #[rstest::rstest]
    #[case::its_first_second(START)]
    #[case::inside(START + 86_400)]
    #[case::its_last_second(END)]
    fn a_certificate_is_valid_from_its_start_to_its_end_inclusive(#[case] now: u64) {
        let (ca_der, issuer) = ca(1);
        let key = agent_key(1);
        let pem = dated(&key, &issuer);

        assert!(check(&pem, &ca_der, &key, at(now)).is_ok());
        assert_eq!(
            validity(&one_certificate(pem.as_bytes()).unwrap(), &ca_der, at(now)),
            Ok(())
        );
    }

    #[test]
    fn an_expired_certificate_says_when_it_ended() {
        let (ca_der, issuer) = ca(1);
        let key = agent_key(1);
        let pem = dated(&key, &issuer);
        let expired = CertificateProblem::Expired {
            not_after: date(END),
            now: date(END + 1),
        };

        assert_eq!(check(&pem, &ca_der, &key, at(END + 1)), Err(expired));
        assert_eq!(
            validity(
                &one_certificate(pem.as_bytes()).unwrap(),
                &ca_der,
                at(END + 1)
            ),
            Err(expired)
        );
    }

    #[test]
    fn a_certificate_before_its_start_isnt_valid_yet() {
        let (ca_der, issuer) = ca(1);
        let key = agent_key(1);
        let pem = dated(&key, &issuer);

        assert_eq!(
            check(&pem, &ca_der, &key, at(START - 1)),
            Err(CertificateProblem::NotYetValid)
        );
        assert_eq!(
            validity(
                &one_certificate(pem.as_bytes()).unwrap(),
                &ca_der,
                at(START - 1)
            ),
            Err(CertificateProblem::NotYetValid)
        );
    }

    #[test]
    fn another_cas_or_another_keys_expired_certificate_is_refused_as_that() {
        let (ca_der, _) = ca(1);
        let (_, other) = ca(2);
        let (_, issuer) = ca(1);
        let key = agent_key(1);
        let foreign = dated(&key, &other);
        let anothers = dated(&agent_key(2), &issuer);

        assert_eq!(
            check(&foreign, &ca_der, &key, at(END + 1)),
            Err(CertificateProblem::NotFromTheCa)
        );
        assert_eq!(
            validity(
                &one_certificate(foreign.as_bytes()).unwrap(),
                &ca_der,
                at(END + 1)
            ),
            Err(CertificateProblem::NotFromTheCa)
        );
        assert_eq!(
            check(&anothers, &ca_der, &key, at(END + 1)),
            Err(CertificateProblem::NotOurKey)
        );
    }
}
