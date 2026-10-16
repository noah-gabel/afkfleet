//! The CA pin (Plan.md P10.3; ADR-0019, S15): `[control_plane]
//! ca_cert_file` must be exactly the CA the bundle names, by the SHA-256 of
//! its DER, before anything is sent. Only then is it trusted, as the TLS
//! client's one root.
//!
//! The file is read through fleet-startup's `secret_file`, like every file
//! the agent trusts, and must hold exactly one PEM section, a certificate.
//! Errors never pass on the PEM reader's own, which can quote the file.

use std::path::{Path, PathBuf};

use fleet_core::pki::CertFingerprint;
use fleet_startup::secret_file::{self, SecretFileError};
use rustls::pki_types::pem::{SectionKind, SliceIter};
use secrecy::ExposeSecret as _;
use sha2::{Digest as _, Sha256};

/// The pinned CA: its certificate file's PEM, for the TLS client, and the
/// certificate's DER, for checking the agent's certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedCa {
    /// The file's PEM.
    pub pem: Vec<u8>,
    /// The certificate's DER.
    pub der: Vec<u8>,
}

/// Why the CA file isn't the pinned CA.
#[derive(Debug, thiserror::Error)]
pub enum PinError {
    /// The file couldn't be read, or isn't an owner-only regular file.
    #[error("the CA certificate can't be used")]
    File(#[source] SecretFileError),
    /// The file doesn't hold exactly one certificate.
    #[error("the CA certificate {} must hold exactly one certificate", path.display())]
    NotOneCertificate {
        /// The file.
        path: PathBuf,
    },
    /// The file is another CA than the bundle's.
    #[error(
        "the CA certificate {} isn't the CA the bundle names: its fingerprint is {found}, the bundle's is {expected}; nothing was sent",
        path.display()
    )]
    Mismatch {
        /// The file.
        path: PathBuf,
        /// The file's fingerprint.
        found: CertFingerprint,
        /// The bundle's.
        expected: CertFingerprint,
    },
}

/// The SHA-256 of a certificate's DER.
#[must_use]
pub fn fingerprint(der: &[u8]) -> CertFingerprint {
    CertFingerprint::from_digest(Sha256::digest(der).into())
}

/// The CA certificate at `path`, read through `secret_file`, when its
/// fingerprint is `expected`.
///
/// # Errors
/// [`PinError`].
pub fn pinned_ca(path: &Path, expected: CertFingerprint) -> Result<PinnedCa, PinError> {
    let file = secret_file::read(path).map_err(PinError::File)?;
    let pem = file.expose_secret();
    let der = check_pin(pem, path, expected)?;
    Ok(PinnedCa {
        pem: pem.clone(),
        der,
    })
}

/// The certificate's DER, when `pem`, the file at `path`, is one
/// certificate whose fingerprint is `expected`.
///
/// # Errors
/// [`PinError::NotOneCertificate`] and [`PinError::Mismatch`].
pub fn check_pin(pem: &[u8], path: &Path, expected: CertFingerprint) -> Result<Vec<u8>, PinError> {
    let der = one_certificate(pem).ok_or_else(|| PinError::NotOneCertificate {
        path: path.to_owned(),
    })?;
    let found = fingerprint(&der);
    if found != expected {
        return Err(PinError::Mismatch {
            path: path.to_owned(),
            found,
            expected,
        });
    }
    Ok(der)
}

/// The DER of the one section in `pem`, when there's exactly one section
/// the PEM reader knows and it's a certificate. The reader's own errors are
/// dropped: they can quote a line of the text.
pub(crate) fn one_certificate(pem: &[u8]) -> Option<Vec<u8>> {
    let mut sections = SliceIter::<(SectionKind, Vec<u8>)>::new(pem);
    let (kind, der) = sections.next()?.ok()?;
    // A second section of any kind, or a broken one, refuses the text.
    (kind == SectionKind::Certificate && sections.next().is_none()).then_some(der)
}

#[cfg(test)]
mod tests {
    use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
    use rustls::pki_types::PrivatePkcs8KeyDer;

    use super::*;

    /// A self-signed CA certificate from `seed`, as PEM and DER.
    fn ca(seed: u8) -> (String, Vec<u8>) {
        let der = fleet_core::pki::ed25519_pkcs8(&[seed; 32]);
        let key = KeyPair::from_pkcs8_der_and_sign_algo(
            &PrivatePkcs8KeyDer::from(der.expose_secret().as_slice()),
            &PKCS_ED25519,
        )
        .unwrap();
        let mut params = CertificateParams::default();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
        let cert = params.self_signed(&key).unwrap();
        (cert.pem(), cert.der().to_vec())
    }

    fn path() -> PathBuf {
        PathBuf::from("ca.crt")
    }

    #[test]
    fn the_fingerprint_is_the_sha256_of_the_der_in_openssls_form() {
        let shown = fingerprint(b"abc").to_string();

        assert_eq!(
            shown,
            "BA:78:16:BF:8F:01:CF:EA:41:41:40:DE:5D:AE:22:23:B0:03:61:A3:96:17:7A:9C:B4:10:FF:61:F2:00:15:AD"
        );
    }

    #[test]
    fn the_ca_the_bundle_names_is_pinned() {
        let (pem, der) = ca(1);

        let checked = check_pin(pem.as_bytes(), &path(), fingerprint(&der));

        assert!(checked.is_ok(), "{checked:?}");
    }

    #[test]
    fn another_ca_is_refused_with_both_fingerprints() {
        let (pem, der) = ca(1);
        let (_, other) = ca(2);

        let refused = check_pin(pem.as_bytes(), &path(), fingerprint(&other)).unwrap_err();

        let shown = refused.to_string();
        assert!(matches!(refused, PinError::Mismatch { .. }), "{refused:?}");
        assert!(shown.contains(&fingerprint(&der).to_string()), "{shown}");
        assert!(shown.contains(&fingerprint(&other).to_string()), "{shown}");
        assert!(shown.ends_with("nothing was sent"), "{shown}");
    }

    #[test]
    fn anything_but_one_certificate_is_refused() {
        let (pem, der) = ca(1);
        for text in [
            format!("{pem}{pem}"),
            "not a certificate".to_owned(),
            "-----BEGIN PRIVATE KEY-----\nAAEC\n-----END PRIVATE KEY-----\n".to_owned(),
            "-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----\n".to_owned(),
        ] {
            let refused = check_pin(text.as_bytes(), &path(), fingerprint(&der)).unwrap_err();

            assert!(
                matches!(refused, PinError::NotOneCertificate { .. }),
                "{text}: {refused:?}"
            );
        }
    }
}
