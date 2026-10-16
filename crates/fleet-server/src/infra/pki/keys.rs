//! [`Ed25519Key`]: the PKI's private keys, made from the [`SecureRandom`]
//! port and read from and written to PEM files.
//!
//! A key's 32 bytes come from the port and are wrapped by
//! [`fleet_core::pki::ed25519_pkcs8`], never drawn by rcgen or aws-lc
//! (S10, security rule 4). rcgen's key pair is held in a [`Zeroizing`], which
//! wipes its PKCS#8 copy when the key is dropped.

use core::fmt;

use fleet_core::pki::{ED25519_SEED_LEN, ed25519_pkcs8};
use fleet_core::system::{RandomError, SecureRandom};
use rcgen::{KeyPair, PKCS_ED25519, PublicKeyData, SignatureAlgorithm, SigningKey};
use rustls::pki_types::PrivatePkcs8KeyDer;
use rustls::pki_types::pem::SectionKind;
use secrecy::ExposeSecret as _;
use zeroize::Zeroizing;

use super::pem::single_section;

/// Why a key couldn't be made or read. No variant holds any part of the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// The random source failed.
    #[error("the key's random bytes couldn't be drawn")]
    Random(#[from] RandomError),
    /// The file doesn't hold exactly one `PRIVATE KEY` (PKCS#8) section and
    /// nothing else the PEM reader knows.
    #[error("must hold exactly one PKCS#8 private key")]
    NotOneKey,
    /// The key isn't an Ed25519 key.
    #[error("must be an Ed25519 key")]
    NotEd25519,
}

/// An Ed25519 private key. Its `Debug` never shows it.
pub struct Ed25519Key(Zeroizing<KeyPair>);

impl Ed25519Key {
    /// A new key from 32 bytes of `random`.
    ///
    /// # Errors
    /// [`KeyError::Random`] when the random source fails.
    pub fn generate(random: &dyn SecureRandom) -> Result<Self, KeyError> {
        let mut seed = Zeroizing::new([0; ED25519_SEED_LEN]);
        random.fill(seed.as_mut_slice())?;
        Self::from_seed(&seed)
    }

    /// The key with this seed.
    fn from_seed(seed: &[u8; ED25519_SEED_LEN]) -> Result<Self, KeyError> {
        Self::from_pkcs8(ed25519_pkcs8(seed).expose_secret())
    }

    /// The Ed25519 key in `der`, a PKCS#8 private key.
    fn from_pkcs8(der: &[u8]) -> Result<Self, KeyError> {
        KeyPair::from_pkcs8_der_and_sign_algo(&PrivatePkcs8KeyDer::from(der), &PKCS_ED25519)
            .map(|key| Self(Zeroizing::new(key)))
            .map_err(|_| KeyError::NotEd25519)
    }

    /// The key in a PEM file's bytes: exactly one `PRIVATE KEY` section,
    /// holding an Ed25519 key.
    ///
    /// # Errors
    /// [`KeyError::NotOneKey`] or [`KeyError::NotEd25519`].
    pub fn from_pem(pem: &[u8]) -> Result<Self, KeyError> {
        let der = single_section(pem, SectionKind::PrivateKey).ok_or(KeyError::NotOneKey)?;
        Self::from_pkcs8(&der)
    }

    /// The key as a PEM file's text: one `PRIVATE KEY` section, PKCS#8.
    #[must_use]
    pub fn to_pem(&self) -> Zeroizing<String> {
        Zeroizing::new(self.0.serialize_pem())
    }

    /// The key as PKCS#8 DER, for a TLS identity.
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

impl fmt::Debug for Ed25519Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Ed25519Key(..)")
    }
}

impl PublicKeyData for Ed25519Key {
    fn der_bytes(&self) -> &[u8] {
        self.0.der_bytes()
    }

    fn algorithm(&self) -> &'static SignatureAlgorithm {
        self.0.algorithm()
    }
}

impl SigningKey for Ed25519Key {
    fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, rcgen::Error> {
        self.0.sign(msg)
    }
}

#[cfg(test)]
mod tests {
    use fleet_testkit::system::SeededRandom;
    use rcgen::{PKCS_ECDSA_P256_SHA256, PKCS_ECDSA_P384_SHA384};

    use super::*;

    /// RFC 8032 §7.1, TEST 1: the secret key and its public key.
    const RFC_8032_SECRET: [u8; 32] = [
        0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec, 0x2c,
        0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03, 0x1c, 0xae,
        0x7f, 0x60,
    ];
    const RFC_8032_PUBLIC: [u8; 32] = [
        0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64, 0x07,
        0x3a, 0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68, 0xf7, 0x07,
        0x51, 0x1a,
    ];

    #[test]
    fn rfc_8032_s_test_1_secret_gives_its_public_key() {
        let key = Ed25519Key::from_seed(&RFC_8032_SECRET).unwrap();

        assert_eq!(key.public_key_raw(), RFC_8032_PUBLIC.as_slice());
        assert_eq!(*key.algorithm(), PKCS_ED25519);
    }

    #[test]
    fn one_seed_makes_one_key_and_another_another() {
        let one = Ed25519Key::generate(&SeededRandom::new(5)).unwrap();
        let same = Ed25519Key::generate(&SeededRandom::new(5)).unwrap();
        let other = Ed25519Key::generate(&SeededRandom::new(6)).unwrap();

        assert_eq!(one.public_key_raw(), same.public_key_raw());
        assert_ne!(one.public_key_raw(), other.public_key_raw());
    }

    #[test]
    fn a_failing_random_source_makes_no_key() {
        let random = SeededRandom::new(5);
        random.fail_next(RandomError::Unavailable);

        assert_eq!(
            Ed25519Key::generate(&random).map(|_| ()),
            Err(KeyError::Random(RandomError::Unavailable))
        );
    }

    #[test]
    fn a_key_written_as_pem_reads_back() {
        let key = Ed25519Key::generate(&SeededRandom::new(5)).unwrap();

        let pem = key.to_pem();
        let again = Ed25519Key::from_pem(pem.as_bytes()).unwrap();

        assert!(pem.starts_with("-----BEGIN PRIVATE KEY-----"));
        assert_eq!(again.public_key_raw(), key.public_key_raw());
        assert_eq!(again.pkcs8_der(), key.pkcs8_der());
    }

    #[test]
    fn another_key_type_is_refused() {
        for algorithm in [&PKCS_ECDSA_P256_SHA256, &PKCS_ECDSA_P384_SHA384] {
            let other = KeyPair::generate_for(algorithm).unwrap();

            let refused = Ed25519Key::from_pem(other.serialize_pem().as_bytes());

            assert_eq!(refused.map(|_| ()), Err(KeyError::NotEd25519));
        }
    }

    #[test]
    fn a_file_without_exactly_one_key_is_refused() {
        let key = Ed25519Key::generate(&SeededRandom::new(5))
            .unwrap()
            .to_pem();
        let other = Ed25519Key::generate(&SeededRandom::new(6))
            .unwrap()
            .to_pem();
        let cert = rcgen::generate_simple_self_signed(["localhost".to_owned()])
            .unwrap()
            .cert
            .pem();

        for file in [
            String::new(),
            "no key here".to_owned(),
            format!("{}{}", key.as_str(), other.as_str()),
            cert.clone(),
            format!("{}{cert}", key.as_str()),
        ] {
            assert_eq!(
                Ed25519Key::from_pem(file.as_bytes()).map(|_| ()),
                Err(KeyError::NotOneKey),
                "{file}"
            );
        }
    }

    #[test]
    fn debug_never_shows_the_key() {
        let key = Ed25519Key::generate(&SeededRandom::new(5)).unwrap();

        assert_eq!(format!("{key:?}"), "Ed25519Key(..)");
    }
}
