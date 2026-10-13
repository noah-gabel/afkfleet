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
//!
//! [`CustomCa::to_pem`] writes the certificates back as canonical PEM,
//! re-encoded from the DER `from_pem` parsed, never the text it was given:
//! the desktop app stores that (ADR-0017). `from_pem` refuses a CA whose
//! canonical PEM would be larger than [`CustomCa::MAX_BYTES`], so
//! `from_pem(&ca.to_pem())` always reads it back.
//! [`CustomCa::fingerprints`] are the certificates' SHA-256 hashes, which the
//! app shows.

use core::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use reqwest::Certificate;
use rustls::RootCertStore;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject as _;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

/// A canonical PEM section's first line.
const BEGIN: &str = "-----BEGIN CERTIFICATE-----\n";
/// A canonical PEM section's last line.
const END: &str = "-----END CERTIFICATE-----\n";
/// The base64 characters per line of canonical PEM.
const LINE: usize = 64;

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
    /// The same certificates' DER, in the same order, for [`CustomCa::to_pem`]
    /// and [`CustomCa::fingerprints`].
    ders: Vec<CertificateDer<'static>>,
}

/// Why a PEM text isn't a custom CA. The message never repeats the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CaError {
    /// It's larger than [`CustomCa::MAX_BYTES`], or its canonical PEM would
    /// be.
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
    /// certificates, the size of its canonical PEM, then each certificate.
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
        let canonical = ders
            .iter()
            .map(|der| canonical_len(der.len()))
            .fold(0_usize, usize::saturating_add);
        if canonical > Self::MAX_BYTES {
            return Err(CaError::TooLarge);
        }
        let certificates = ders
            .iter()
            .enumerate()
            .map(|(index, der)| {
                let invalid = CaError::InvalidCertificate {
                    position: index.saturating_add(1),
                };
                let certificate = Certificate::from_der(der).map_err(|_| invalid)?;
                RootCertStore::empty()
                    .add(der.clone())
                    .map_err(|_| invalid)?;
                Ok(certificate)
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { certificates, ders })
    }

    /// The certificates as canonical PEM, re-encoded from the DER
    /// [`CustomCa::from_pem`] parsed: one `CERTIFICATE` section each, in
    /// order, standard base64 in lines of 64, `\n` line ends, nothing else.
    #[must_use]
    pub fn to_pem(&self) -> String {
        let mut pem = String::new();
        for der in &self.ders {
            pem.push_str(BEGIN);
            for (index, character) in STANDARD.encode(der).chars().enumerate() {
                if index > 0 && index % LINE == 0 {
                    pem.push('\n');
                }
                pem.push(character);
            }
            pem.push('\n');
            pem.push_str(END);
        }
        pem
    }

    /// Each certificate's SHA-256 hash over its DER, in order.
    #[must_use]
    pub fn fingerprints(&self) -> Vec<[u8; 32]> {
        self.ders
            .iter()
            .map(|der| Sha256::digest(der).into())
            .collect()
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

/// The length of one certificate's canonical PEM section, for a DER of
/// `der_len` bytes: its first and last lines, the padded base64, and a line
/// break after every full or final line of it.
fn canonical_len(der_len: usize) -> usize {
    let base64 = der_len.div_ceil(3).saturating_mul(4);
    let breaks = base64.div_ceil(LINE);
    BEGIN
        .len()
        .saturating_add(base64)
        .saturating_add(breaks)
        .saturating_add(END.len())
}

/// Only the number of certificates: never their DER.
impl fmt::Debug for CustomCa {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CustomCa")
            .field("certificates", &self.certificates.len())
            .finish_non_exhaustive()
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

    /// A self-signed CA certificate made by openssl 3 (its key was never
    /// stored), and its fingerprint as `openssl x509 -noout -fingerprint
    /// -sha256` printed it: an outside check of the hash.
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

    /// A self-signed CA certificate as PEM whose DER holds a custom
    /// extension of `padding` bytes, so its size can be chosen.
    fn padded_ca_pem(padding: usize) -> String {
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params
            .custom_extensions
            .push(rcgen::CustomExtension::from_oid_content(
                &[1, 3, 6, 1, 4, 1, 55555, 1],
                vec![0x04; padding],
            ));
        let key = KeyPair::generate().unwrap();
        CertifiedIssuer::self_signed(params, key).unwrap().pem()
    }

    /// `pem` with each section's base64 on one line.
    fn unbroken(pem: &str) -> String {
        let mut out = String::new();
        for line in pem.lines() {
            if line.starts_with("-----") {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(line);
                out.push('\n');
            } else {
                out.push_str(line);
            }
        }
        out
    }

    /// A fingerprint as openssl prints it.
    fn shown(hash: &[u8; 32]) -> String {
        hash.iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(":")
    }

    #[test]
    fn the_canonical_pem_reads_back_as_the_same_certificates() {
        let ca =
            CustomCa::from_pem(&format!("{}{}", ca_pem("Test CA 1"), ca_pem("Test CA 2"))).unwrap();

        let again = CustomCa::from_pem(&ca.to_pem()).unwrap();

        assert_eq!(again.len(), 2);
        assert_eq!(again.to_pem(), ca.to_pem());
        assert_eq!(again.fingerprints(), ca.fingerprints());
    }

    #[test]
    fn the_canonical_pem_keeps_only_the_certificates_in_lines_of_64() {
        let plain = ca_pem("Test CA");
        let messy = format!(
            "subject=CN = Test CA\r\n{}",
            unbroken(&plain).replace('\n', "\r\n")
        );

        let canonical = CustomCa::from_pem(&messy).unwrap().to_pem();

        assert_eq!(canonical, CustomCa::from_pem(&plain).unwrap().to_pem());
        assert!(
            canonical.starts_with("-----BEGIN CERTIFICATE-----\n"),
            "{canonical}"
        );
        assert!(
            canonical.ends_with("\n-----END CERTIFICATE-----\n"),
            "{canonical}"
        );
        assert!(!canonical.contains('\r') && !canonical.contains("subject"));
        assert!(
            canonical.lines().all(|line| line.len() <= 64),
            "{canonical}"
        );
    }

    #[test]
    fn the_fixed_certificate_is_written_back_exactly() {
        let ca = CustomCa::from_pem(FIXED_CA).unwrap();

        assert_eq!(ca.to_pem(), FIXED_CA);
    }

    #[test]
    fn a_fingerprint_is_the_sha256_openssl_prints() {
        let ca = CustomCa::from_pem(FIXED_CA).unwrap();

        let fingerprints: Vec<String> = ca.fingerprints().iter().map(shown).collect();

        assert_eq!(fingerprints, [FIXED_CA_SHA256]);
    }

    #[test]
    fn the_fingerprints_follow_the_files_order() {
        let other = ca_pem("Test CA");
        let other_alone = CustomCa::from_pem(&other).unwrap().fingerprints();

        let both: Vec<String> = CustomCa::from_pem(&format!("{FIXED_CA}{other}"))
            .unwrap()
            .fingerprints()
            .iter()
            .map(shown)
            .collect();

        assert_eq!(
            both,
            [
                FIXED_CA_SHA256.to_owned(),
                other_alone.iter().map(shown).collect::<String>()
            ]
        );
    }

    #[test]
    fn a_ca_whose_canonical_pem_would_pass_64_kib_is_refused() {
        // One long base64 line grows by a line break every 64 characters once
        // re-encoded: find a padding where the input fits and the canonical
        // PEM doesn't. The key's signature varies by a byte or two, so search.
        let pem = (40_000..50_000)
            .step_by(48)
            .find_map(|padding| {
                let pem = unbroken(&padded_ca_pem(padding));
                let grown = pem.len() + pem.len() / 64;
                (pem.len() <= CustomCa::MAX_BYTES && grown > CustomCa::MAX_BYTES + 64)
                    .then_some(pem)
            })
            .expect("no padding put the PEM just under 64 KiB");

        assert_eq!(CustomCa::from_pem(&pem).unwrap_err(), CaError::TooLarge);
    }

    #[test]
    fn a_large_ca_whose_canonical_pem_fits_is_read() {
        let pem = padded_ca_pem(30_000);

        let ca = CustomCa::from_pem(&pem).unwrap();

        assert!(ca.to_pem().len() <= CustomCa::MAX_BYTES);
        assert_eq!(CustomCa::from_pem(&ca.to_pem()).unwrap().len(), 1);
    }

    #[test]
    fn the_size_check_counts_exactly_what_to_pem_writes() {
        for padding in [0, 1, 2, 47, 48, 49, 1000] {
            let ca = CustomCa::from_pem(&padded_ca_pem(padding)).unwrap();
            let der_len = ca.ders.first().unwrap().len();

            assert_eq!(canonical_len(der_len), ca.to_pem().len(), "{padding}");
        }
    }

    #[test]
    fn debug_shows_only_the_number_of_certificates() {
        let ca = CustomCa::from_pem(&ca_pem("Test CA")).unwrap();

        assert_eq!(format!("{ca:?}"), "CustomCa { certificates: 1, .. }");
    }
}
