//! The agent's key and its certificate request (Plan.md P10.3; ADR-0019,
//! S10, S18).
//!
//! - **The key** is Ed25519 from 32 bytes of the [`SecureRandom`] port,
//!   wrapped as PKCS#8 (fleet-core's `ed25519_pkcs8`), never from rcgen's
//!   own generator. It's held in a key pair that's wiped when dropped, and
//!   it never leaves the host: only its CSR is sent.
//! - **The CSR** carries the public key under an empty subject and with no
//!   extension: the server takes only the key, and puts everything else in
//!   the certificate itself.
//! - [`OsRandom`] is the OS's randomness, which `enroll` runs on; tests use
//!   a seeded source.

use core::fmt;

use fleet_core::pki::{ED25519_SEED_LEN, ed25519_pkcs8};
use fleet_core::system::{RandomError, SecureRandom};
use rcgen::{CertificateParams, DistinguishedName, KeyPair, PKCS_ED25519};
use rustls::pki_types::PrivatePkcs8KeyDer;
use rustls::pki_types::pem::{SectionKind, SliceIter};
use secrecy::ExposeSecret as _;
use zeroize::Zeroizing;

/// The OS's secure randomness, through getrandom.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsRandom;

impl SecureRandom for OsRandom {
    fn fill(&self, dest: &mut [u8]) -> Result<(), RandomError> {
        getrandom::fill(dest).map_err(|error| RandomError::Os {
            code: error.raw_os_error().unwrap_or(0),
        })
    }
}

/// Why no key or CSR was made.
#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    /// The random source failed.
    #[error(transparent)]
    Random(#[from] RandomError),
    /// rcgen refused the key it was given.
    #[error("the key can't be made")]
    Key,
    /// rcgen couldn't write the CSR.
    #[error("the certificate request can't be made")]
    Csr,
    /// A key file's text isn't exactly one PKCS#8 `PRIVATE KEY` section.
    #[error(
        "must hold exactly one PKCS#8 private key (`BEGIN PRIVATE KEY`), as `afkfleet-agent enroll` writes it"
    )]
    NotOnePem,
}

/// The agent's Ed25519 key; see the [module docs](self).
pub struct AgentKey(Zeroizing<KeyPair>);

impl AgentKey {
    /// A new key from 32 bytes of `random`.
    ///
    /// # Errors
    /// [`KeyError::Random`] when `random` fails, and [`KeyError::Key`].
    pub fn generate(random: &dyn SecureRandom) -> Result<Self, KeyError> {
        let mut seed = Zeroizing::new([0; ED25519_SEED_LEN]);
        random.fill(seed.as_mut_slice())?;
        let der = ed25519_pkcs8(&seed);
        KeyPair::from_pkcs8_der_and_sign_algo(
            &PrivatePkcs8KeyDer::from(der.expose_secret().as_slice()),
            &PKCS_ED25519,
        )
        .map(|pair| Self(Zeroizing::new(pair)))
        .map_err(|_| KeyError::Key)
    }

    /// The key in `pem`, a key file's text: exactly one PKCS#8 `PRIVATE KEY`
    /// section of an Ed25519 key, as [`AgentKey::to_pem`] writes it (ADR-0019,
    /// S68). The DER is wiped once the key pair holds it.
    ///
    /// # Errors
    /// [`KeyError::NotOnePem`] for any other text, and [`KeyError::Key`] for a
    /// key that isn't Ed25519.
    pub fn from_pem(pem: &[u8]) -> Result<Self, KeyError> {
        let mut sections = SliceIter::<(SectionKind, Vec<u8>)>::new(pem);
        // Every section read is wiped, the refused ones too.
        let first = sections
            .next()
            .map(|section| section.map(|(kind, der)| (kind, Zeroizing::new(der))));
        let more = sections
            .next()
            .map(|section| section.map(|(kind, der)| (kind, Zeroizing::new(der))))
            .is_some();
        let der = match first {
            Some(Ok((SectionKind::PrivateKey, der))) if !more => der,
            _ => return Err(KeyError::NotOnePem),
        };
        KeyPair::from_pkcs8_der_and_sign_algo(
            &PrivatePkcs8KeyDer::from(der.as_slice()),
            &PKCS_ED25519,
        )
        .map(|pair| Self(Zeroizing::new(pair)))
        .map_err(|_| KeyError::Key)
    }

    /// The key's certificate request, DER-encoded.
    ///
    /// # Errors
    /// [`KeyError::Csr`].
    pub fn csr_der(&self) -> Result<Vec<u8>, KeyError> {
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params
            .serialize_request(&*self.0)
            .map(|csr| csr.der().to_vec())
            .map_err(|_| KeyError::Csr)
    }

    /// The key as a PEM file's text: one `PRIVATE KEY` section, PKCS#8.
    #[must_use]
    pub fn to_pem(&self) -> Zeroizing<String> {
        Zeroizing::new(self.0.serialize_pem())
    }

    /// The key as PKCS#8 DER.
    #[must_use]
    pub fn pkcs8_der(&self) -> &[u8] {
        self.0.serialized_der()
    }

    /// The 32 bytes of the public key.
    #[must_use]
    pub fn public_key_raw(&self) -> &[u8] {
        self.0.public_key_raw()
    }
}

impl fmt::Debug for AgentKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AgentKey(..)")
    }
}

#[cfg(test)]
mod tests {
    use fleet_testkit::system::SeededRandom;

    use super::*;

    /// The DER of the PKCS#9 extension request's OID, which a CSR with
    /// extensions carries.
    const EXTENSION_REQUEST_OID: [u8; 11] = [
        0x06, 0x09, 0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x09, 0x0E,
    ];

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    fn a_key_comes_from_the_random_source() {
        let one = AgentKey::generate(&SeededRandom::new(1)).unwrap();
        let again = AgentKey::generate(&SeededRandom::new(1)).unwrap();
        let two = AgentKey::generate(&SeededRandom::new(2)).unwrap();

        assert_eq!(one.public_key_raw(), again.public_key_raw());
        assert_ne!(one.public_key_raw(), two.public_key_raw());
        assert_eq!(one.public_key_raw().len(), 32);
    }

    #[test]
    fn a_failing_random_source_makes_no_key() {
        let random = SeededRandom::new(1);
        random.fail_next(RandomError::Os { code: 5 });

        let refused = AgentKey::generate(&random);

        assert!(matches!(refused, Err(KeyError::Random(_))), "{refused:?}");
    }

    #[test]
    fn the_os_source_gives_different_keys() {
        let one = AgentKey::generate(&OsRandom).unwrap();
        let two = AgentKey::generate(&OsRandom).unwrap();

        assert_ne!(one.public_key_raw(), two.public_key_raw());
    }

    #[test]
    fn the_csr_carries_the_public_key_and_no_extension_or_name() {
        let key = AgentKey::generate(&SeededRandom::new(1)).unwrap();

        let csr = key.csr_der().unwrap();

        assert!(contains(&csr, key.public_key_raw()));
        assert!(!contains(&csr, &EXTENSION_REQUEST_OID));
        assert!(!contains(&csr, b"rcgen"));
        assert!(!contains(&csr, key.pkcs8_der()));
    }

    #[test]
    fn the_pem_is_one_private_key_and_debug_shows_nothing() {
        let key = AgentKey::generate(&SeededRandom::new(1)).unwrap();

        let pem = key.to_pem();

        assert!(pem.starts_with("-----BEGIN PRIVATE KEY-----"), "PEM");
        assert_eq!(pem.matches("-----BEGIN").count(), 1);
        assert_eq!(format!("{key:?}"), "AgentKey(..)");
    }

    #[test]
    fn a_key_reads_back_from_its_pem() {
        let key = AgentKey::generate(&SeededRandom::new(1)).unwrap();

        let read = AgentKey::from_pem(key.to_pem().as_bytes()).unwrap();

        assert_eq!(read.public_key_raw(), key.public_key_raw());
        assert_eq!(read.pkcs8_der(), key.pkcs8_der());
    }

    #[test]
    fn a_pem_that_isnt_one_private_key_is_refused() {
        let key = AgentKey::generate(&SeededRandom::new(1)).unwrap();
        let other = AgentKey::generate(&SeededRandom::new(2)).unwrap();
        let two = format!("{}{}", key.to_pem().as_str(), other.to_pem().as_str());
        let certificate = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
        let sec1 = "-----BEGIN EC PRIVATE KEY-----\nAAAA\n-----END EC PRIVATE KEY-----\n";

        for text in [two.as_str(), certificate, sec1, "", "not a pem"] {
            let refused = AgentKey::from_pem(text.as_bytes());

            assert!(
                matches!(refused, Err(KeyError::NotOnePem)),
                "{text}: {refused:?}"
            );
        }
    }

    #[test]
    fn a_private_key_section_that_isnt_ed25519_is_refused() {
        let garbage = "-----BEGIN PRIVATE KEY-----\nAAECAwQFBgcICQ==\n-----END PRIVATE KEY-----\n";

        let refused = AgentKey::from_pem(garbage.as_bytes());

        assert!(matches!(refused, Err(KeyError::Key)), "{refused:?}");
    }

    #[test]
    fn no_refusal_names_the_text() {
        for error in [KeyError::NotOnePem, KeyError::Key] {
            assert!(!error.to_string().contains("AAEC"), "{error}");
        }
    }
}
