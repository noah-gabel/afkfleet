//! Checking an agent's CSR (S10, P10.2): it proves the agent holds its key,
//! and only that key is taken.
//!
//! rcgen reads the CSR with x509-parser and verifies its self-signature with
//! aws-lc (`verify-aws`). rcgen takes the key's algorithm from the
//! signature's algorithm and the key's bytes from the request, and certifies
//! exactly that pair, which the signature covered; so the check that the
//! algorithm is Ed25519 and the key 32 bytes is the check of the key type.
//! A request that asks for any extension is refused, and so is one that
//! carries any attribute: rcgen reads extensions through x509-parser's
//! `requested_extensions()`, which sees only the first extension request,
//! so a second one could ask for what the check never sees. Its subject is
//! ignored, since nothing but the key is used.

use rcgen::{
    CertificateSigningRequestParams, IsCa, PKCS_ED25519, PublicKey, PublicKeyData,
    SignatureAlgorithm,
};
use rustls::pki_types::CertificateSigningRequestDer;
use x509_parser::certification_request::X509CertificationRequest;
use x509_parser::prelude::FromDer as _;

use crate::ports::pki::CsrRefusal;

/// The length of an Ed25519 public key.
const ED25519_KEY_LEN: usize = 32;

/// A CSR's public key, after [`check_csr`] accepted it.
#[derive(Debug, Clone)]
pub struct CheckedCsr(PublicKey);

impl PublicKeyData for CheckedCsr {
    fn der_bytes(&self) -> &[u8] {
        self.0.der_bytes()
    }

    fn algorithm(&self) -> &'static SignatureAlgorithm {
        PublicKeyData::algorithm(&self.0)
    }
}

/// The public key of `der`, a DER-encoded PKCS#10 request, when its
/// signature verifies, its key is Ed25519 and it asks for no extension and
/// carries no attribute.
///
/// # Errors
/// The [`CsrRefusal`] that applies first: its size, then whether it reads,
/// its signature, its key type, its extensions and its attributes.
pub fn check_csr(der: &[u8]) -> Result<CheckedCsr, CsrRefusal> {
    if der.len() > CsrRefusal::MAX_BYTES {
        return Err(CsrRefusal::TooLarge);
    }
    let request =
        CertificateSigningRequestParams::from_der(&CertificateSigningRequestDer::from(der))
            .map_err(|error| match error {
                rcgen::Error::InvalidCertificationRequestSignature => CsrRefusal::Signature,
                rcgen::Error::UnsupportedExtension => CsrRefusal::Extensions,
                rcgen::Error::UnsupportedSignatureAlgorithm => CsrRefusal::KeyType,
                _ => CsrRefusal::Unparsable,
            })?;
    let key = &request.public_key;
    if *PublicKeyData::algorithm(key) != PKCS_ED25519 || key.der_bytes().len() != ED25519_KEY_LEN {
        return Err(CsrRefusal::KeyType);
    }
    // rcgen reads the extensions it knows into these fields and refuses any
    // other; a request asking for none leaves them as they start.
    let asked = &request.params;
    if !asked.subject_alt_names.is_empty()
        || !asked.key_usages.is_empty()
        || !asked.extended_key_usages.is_empty()
        || !matches!(asked.is_ca, IsCa::NoCa)
    {
        return Err(CsrRefusal::Extensions);
    }
    // rcgen sees only the first extension request (x509-parser's
    // `requested_extensions()`), so a second could hide behind it: no
    // attribute at all is accepted. The agent's CSR carries none.
    let (_, parsed) =
        X509CertificationRequest::from_der(der).map_err(|_| CsrRefusal::Unparsable)?;
    if !parsed.certification_request_info.attributes().is_empty() {
        return Err(CsrRefusal::Attributes);
    }
    Ok(CheckedCsr(request.public_key))
}

#[cfg(test)]
mod tests {
    use fleet_core::pki::ed25519_pkcs8;
    use rcgen::{
        CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256, PKCS_ECDSA_P384_SHA384, PKCS_ED25519,
        PKCS_RSA_SHA256, RsaKeySize,
    };
    use rustls::pki_types::PrivatePkcs8KeyDer;
    use secrecy::ExposeSecret as _;

    use super::*;

    /// An Ed25519 key from a fixed seed, as the agent makes one.
    fn ed25519(seed: u8) -> KeyPair {
        let der = ed25519_pkcs8(&[seed; 32]);
        KeyPair::from_pkcs8_der_and_sign_algo(
            &PrivatePkcs8KeyDer::from(der.expose_secret().as_slice()),
            &PKCS_ED25519,
        )
        .unwrap()
    }

    /// A CSR for `key`, asking for nothing, as the agent sends one.
    fn csr(key: &KeyPair) -> Vec<u8> {
        CertificateParams::default()
            .serialize_request(key)
            .unwrap()
            .der()
            .to_vec()
    }

    #[test]
    fn an_ed25519_csr_gives_its_public_key() {
        let key = ed25519(7);

        let checked = check_csr(&csr(&key)).unwrap();

        assert_eq!(checked.der_bytes(), key.public_key_raw());
        assert_eq!(*checked.algorithm(), PKCS_ED25519);
    }

    #[test]
    fn its_subject_is_ignored() {
        let key = ed25519(7);
        let mut params = CertificateParams::default();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "agent-claims-to-be-the-ca");

        let checked = check_csr(params.serialize_request(&key).unwrap().der()).unwrap();

        assert_eq!(checked.der_bytes(), key.public_key_raw());
    }

    #[test]
    fn another_key_type_is_refused() {
        let others = [
            KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap(),
            KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).unwrap(),
            KeyPair::generate_rsa_for(&PKCS_RSA_SHA256, RsaKeySize::_2048).unwrap(),
        ];

        for key in &others {
            assert_eq!(
                check_csr(&csr(key)).map(|_| ()),
                Err(CsrRefusal::KeyType),
                "{:?}",
                key.algorithm()
            );
        }
    }

    #[test]
    fn a_tampered_csr_is_refused() {
        let mut der = csr(&ed25519(7));
        let last = der.len() - 1;
        der[last] ^= 0x01;

        assert_eq!(check_csr(&der).map(|_| ()), Err(CsrRefusal::Signature));
    }

    #[test]
    fn a_csr_signed_by_another_key_is_refused() {
        let key = ed25519(7);
        let other = ed25519(8);
        let mine = csr(&key);
        let theirs = csr(&other);
        // The request's info from one CSR with the other's signature: both
        // are Ed25519 signatures of 64 bytes at the end.
        let mut mixed = mine[..mine.len() - 64].to_vec();
        mixed.extend_from_slice(&theirs[theirs.len() - 64..]);

        assert_eq!(check_csr(&mixed).map(|_| ()), Err(CsrRefusal::Signature));
    }

    #[test]
    fn a_csr_asking_for_an_extension_is_refused() {
        let key = ed25519(7);
        let asking = [
            CertificateParams::new(vec!["server.example.com".to_owned()]).unwrap(),
            {
                let mut params = CertificateParams::default();
                params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
                params
            },
            {
                let mut params = CertificateParams::default();
                params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
                params
            },
            {
                let mut params = CertificateParams::default();
                params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
                params
            },
        ];

        for params in asking {
            let der = params.serialize_request(&key).unwrap().der().to_vec();
            assert_eq!(check_csr(&der).map(|_| ()), Err(CsrRefusal::Extensions));
        }
    }

    /// PKCS#9's challengePassword.
    const CHALLENGE_PASSWORD: &[u64] = &[1, 2, 840, 113_549, 1, 9, 7];
    /// PKCS#9's extensionRequest.
    const EXTENSION_REQUEST: &[u64] = &[1, 2, 840, 113_549, 1, 9, 14];

    /// A CSR for `key` asking for nothing, carrying `attributes`.
    fn csr_with(key: &KeyPair, attributes: Vec<rcgen::Attribute>) -> Vec<u8> {
        CertificateParams::default()
            .serialize_request_with_attributes(key, attributes)
            .unwrap()
            .der()
            .to_vec()
    }

    #[test]
    fn a_csr_with_an_attribute_is_refused() {
        let key = ed25519(7);
        let der = csr_with(
            &key,
            vec![rcgen::Attribute {
                oid: CHALLENGE_PASSWORD,
                // SET { UTF8String "pass" }
                values: vec![0x31, 0x06, 0x0c, 0x04, b'p', b'a', b's', b's'],
            }],
        );

        assert_eq!(check_csr(&der).map(|_| ()), Err(CsrRefusal::Attributes));
    }

    #[test]
    fn a_second_extension_request_is_refused() {
        let key = ed25519(7);
        // basicConstraints, critical, CA: the request x509-parser's
        // requested_extensions() wouldn't see behind an empty first one.
        let ca_extension = [
            0x30, 0x0f, 0x06, 0x03, 0x55, 0x1d, 0x13, 0x01, 0x01, 0xff, 0x04, 0x05, 0x30, 0x03,
            0x01, 0x01, 0xff,
        ];
        let mut second = vec![0x31, 0x13, 0x30, 0x11];
        second.extend_from_slice(&ca_extension);
        let der = csr_with(
            &key,
            vec![
                rcgen::Attribute {
                    oid: EXTENSION_REQUEST,
                    // SET { SEQUENCE {} }: no extension
                    values: vec![0x31, 0x02, 0x30, 0x00],
                },
                rcgen::Attribute {
                    oid: EXTENSION_REQUEST,
                    values: second,
                },
            ],
        );

        assert_eq!(check_csr(&der).map(|_| ()), Err(CsrRefusal::Attributes));
    }

    #[test]
    fn what_isnt_a_csr_is_refused() {
        for der in [
            Vec::new(),
            vec![0x30, 0x03, 0x02, 0x01, 0x00],
            b"not der".to_vec(),
        ] {
            assert_eq!(check_csr(&der).map(|_| ()), Err(CsrRefusal::Unparsable));
        }
    }

    #[test]
    fn a_csr_over_the_limit_is_refused_before_it_is_read() {
        let mut der = csr(&ed25519(7));
        der.resize(CsrRefusal::MAX_BYTES + 1, 0);

        assert_eq!(check_csr(&der).map(|_| ()), Err(CsrRefusal::TooLarge));
    }
}
