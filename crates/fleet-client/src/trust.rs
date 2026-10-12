//! [`Trust`]: which certificates the client accepts (Plan.md §7.5,
//! ADR-0007, ADR-0017). Invalid certificates are never accepted.
//!
//! - [`Trust::System`] checks the server's certificate against the system's
//!   roots through the platform verifier (Windows' own on Windows).
//! - [`Trust::CustomCa`] checks it against the configured CA **only**: a
//!   profile with its own CA trusts nothing else, so a public certificate
//!   mis-issued for that host is refused too.
//!
//! [`CustomCa::from_pem`] is strict about what it reads, since the PEM comes
//! from a file the user picks (P8.4's server profile reuses it): at most
//! [`CustomCa::MAX_BYTES`], every PEM section a `CERTIFICATE` (a private key
//! picked by mistake never passes), 1 to [`CustomCa::MAX_CERTIFICATES`]
//! certificates, each loading as a rustls trust anchor. Text outside the
//! sections, such as OpenSSL's `subject=` lines, is ignored. Errors name the
//! problem and the position, never any content.

use core::fmt;

use reqwest::Certificate;
use rustls::RootCertStore;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject as _;
use thiserror::Error;

/// Which certificates the client accepts; see the [module docs](self).
#[derive(Debug, Clone)]
pub enum Trust {
    /// The system's roots, through the platform verifier.
    System,
    /// Only this CA's certificates.
    CustomCa(CustomCa),
}

/// The certificates of a custom CA, checked; see the [module docs](self).
#[derive(Clone)]
pub struct CustomCa {
    certificates: Vec<Certificate>,
}

/// Why a PEM text isn't a custom CA. The message never repeats the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CaError {
    /// It's larger than [`CustomCa::MAX_BYTES`].
    #[error("is larger than 64 KiB")]
    TooLarge,
    /// A PEM section isn't a certificate: a key, a request, or anything else.
    #[error("PEM section {position} isn't a certificate")]
    NotACertificate {
        /// The section's position, from 1.
        position: usize,
    },
    /// Its PEM can't be read: a section without its end, or bad base64.
    #[error("isn't readable PEM")]
    Unreadable,
    /// It holds no certificate.
    #[error("holds no certificate")]
    NoCertificate,
    /// It holds more than [`CustomCa::MAX_CERTIFICATES`] certificates.
    #[error("holds more than 16 certificates")]
    TooMany,
    /// A certificate can't be a trust anchor.
    #[error("certificate {position} isn't a valid certificate")]
    InvalidCertificate {
        /// The certificate's position, from 1.
        position: usize,
    },
}

impl CustomCa {
    /// The largest PEM text read: 64 KiB.
    pub const MAX_BYTES: usize = 64 * 1024;
    /// The most certificates one CA file holds.
    pub const MAX_CERTIFICATES: usize = 16;

    /// Reads the CA's certificates from `pem`.
    ///
    /// # Errors
    /// [`CaError`] names the first rule `pem` breaks, checked in this order:
    /// its size, each section's label, the PEM itself, the number of
    /// certificates, then each certificate.
    pub fn from_pem(pem: &str) -> Result<Self, CaError> {
        if pem.len() > Self::MAX_BYTES {
            return Err(CaError::TooLarge);
        }
        // rustls-pki-types skips sections whose label it doesn't know, so
        // the labels are checked here, every one of them.
        for (index, line) in pem
            .lines()
            .filter_map(|line| line.trim().strip_prefix("-----BEGIN "))
            .enumerate()
        {
            if line != "CERTIFICATE-----" {
                return Err(CaError::NotACertificate {
                    position: index.saturating_add(1),
                });
            }
        }
        let ders = CertificateDer::pem_slice_iter(pem.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| CaError::Unreadable)?;
        if ders.is_empty() {
            return Err(CaError::NoCertificate);
        }
        if ders.len() > Self::MAX_CERTIFICATES {
            return Err(CaError::TooMany);
        }
        let certificates = ders
            .into_iter()
            .enumerate()
            .map(|(index, der)| {
                let invalid = CaError::InvalidCertificate {
                    position: index.saturating_add(1),
                };
                let certificate = Certificate::from_der(&der).map_err(|_| invalid)?;
                RootCertStore::empty().add(der).map_err(|_| invalid)?;
                Ok(certificate)
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { certificates })
    }

    /// How many certificates the CA holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.certificates.len()
    }

    /// Whether it holds none; never true for one [`CustomCa::from_pem`]
    /// made.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.certificates.is_empty()
    }

    /// The certificates, for reqwest.
    pub(crate) fn certificates(&self) -> impl Iterator<Item = Certificate> + '_ {
        self.certificates.iter().cloned()
    }
}

/// Only the number of certificates.
impl fmt::Debug for CustomCa {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CustomCa")
            .field("certificates", &self.certificates.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use rcgen::{CertificateParams, CertifiedIssuer, KeyPair};

    use super::*;

    /// A self-signed CA certificate as PEM, with its common name `name`.
    fn ca_pem(name: &str) -> String {
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, name);
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = KeyPair::generate().unwrap();
        let issuer = CertifiedIssuer::self_signed(params, key).unwrap();
        issuer.pem()
    }

    /// A PEM section with `label` around some base64.
    fn section(label: &str) -> String {
        format!("-----BEGIN {label}-----\nAAAA\n-----END {label}-----\n")
    }

    #[test]
    fn one_ca_certificate_is_read() {
        let ca = CustomCa::from_pem(&ca_pem("Test CA")).unwrap();

        assert_eq!(ca.len(), 1);
        assert!(!ca.is_empty());
    }

    #[test]
    fn a_bundle_with_text_between_its_certificates_is_read() {
        let pem = format!(
            "subject=CN = Test CA 1\n{}\nsubject=CN = Test CA 2\n{}",
            ca_pem("Test CA 1"),
            ca_pem("Test CA 2")
        );

        let ca = CustomCa::from_pem(&pem).unwrap();

        assert_eq!(ca.len(), 2);
    }

    #[test]
    fn more_than_64_kib_is_refused_before_anything_else() {
        let pem = format!("{}{}", ca_pem("Test CA"), " ".repeat(CustomCa::MAX_BYTES));

        assert_eq!(CustomCa::from_pem(&pem).unwrap_err(), CaError::TooLarge);
    }

    #[test]
    fn a_private_key_a_request_or_an_unknown_section_is_refused_by_position() {
        let ca = ca_pem("Test CA");
        for label in [
            "PRIVATE KEY",
            "RSA PRIVATE KEY",
            "EC PRIVATE KEY",
            "CERTIFICATE REQUEST",
            "X509 CRL",
            "TRUSTED CERTIFICATE",
            "FOO",
        ] {
            let pem = format!("{ca}{}", section(label));

            assert_eq!(
                CustomCa::from_pem(&pem).unwrap_err(),
                CaError::NotACertificate { position: 2 },
                "{label}"
            );
        }
    }

    #[test]
    fn a_certificate_without_its_end_is_unreadable() {
        let ca = ca_pem("Test CA");
        let cut = ca.trim_end().trim_end_matches("-----END CERTIFICATE-----");

        assert_eq!(CustomCa::from_pem(cut).unwrap_err(), CaError::Unreadable);
    }

    #[test]
    fn no_certificate_is_refused() {
        for pem in ["", "just text\n"] {
            assert_eq!(
                CustomCa::from_pem(pem).unwrap_err(),
                CaError::NoCertificate,
                "{pem:?}"
            );
        }
    }

    #[test]
    fn seventeen_certificates_are_too_many() {
        let one = ca_pem("Test CA");
        let sixteen = one.repeat(16);
        let seventeen = one.repeat(17);

        assert_eq!(CustomCa::from_pem(&sixteen).unwrap().len(), 16);
        assert_eq!(
            CustomCa::from_pem(&seventeen).unwrap_err(),
            CaError::TooMany
        );
    }

    #[test]
    fn a_certificate_that_isnt_one_is_refused_by_position() {
        let pem = format!("{}{}", ca_pem("Test CA"), section("CERTIFICATE"));

        assert_eq!(
            CustomCa::from_pem(&pem).unwrap_err(),
            CaError::InvalidCertificate { position: 2 }
        );
    }

    #[test]
    fn no_error_repeats_any_content() {
        let errors = [
            CaError::TooLarge,
            CaError::NotACertificate { position: 2 },
            CaError::Unreadable,
            CaError::NoCertificate,
            CaError::TooMany,
            CaError::InvalidCertificate { position: 3 },
        ];

        let texts: Vec<String> = errors.iter().map(ToString::to_string).collect();

        assert_eq!(
            texts,
            [
                "is larger than 64 KiB",
                "PEM section 2 isn't a certificate",
                "isn't readable PEM",
                "holds no certificate",
                "holds more than 16 certificates",
                "certificate 3 isn't a valid certificate",
            ]
        );
    }

    #[test]
    fn debug_shows_only_the_number_of_certificates() {
        let ca = CustomCa::from_pem(&ca_pem("Test CA")).unwrap();

        assert_eq!(format!("{ca:?}"), "CustomCa { certificates: 1 }");
    }
}
