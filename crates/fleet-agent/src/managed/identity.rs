//! [`ManagedIdentity`]: a managed agent's key, certificate and CA, loaded
//! and checked once at start (Plan.md P10.7; ADR-0019, S68).
//!
//! main.rs loads it right after logging and the TLS provider, before the
//! recorder, the runtime and the signals: blocking file IO before any
//! runtime, as the server's serve does. Every file goes through
//! fleet-startup's `secret_file` (a regular, owner-only file on Unix, not
//! empty, bounded), and the certificate passes enroll's own check: one PEM
//! certificate the CA signed for client authentication, for the key, valid
//! now. Any problem is a startup error (exit 1) whose text names the files
//! and says what to do, never their content.
//!
//! The identity is consumed into tonic's TLS config for the control
//! stream; what's kept is the certificate and the CA without the key
//! ([`KeptCertificate`]), for the check before each attempt (R52).
//!
//! **Residual (recorded in ADR-0019):** tonic's `Identity` and
//! `ClientTlsConfig` hold the key's PEM unwiped for the process's life, and
//! their `Debug` would print it: neither is ever formatted.

use core::fmt;
use std::path::{Path, PathBuf};

use fleet_core::pki::CertFingerprint;
use fleet_startup::secret_file::{self, SecretFileError};
use rustls::pki_types::UnixTime;
use secrecy::ExposeSecret as _;
use tonic::transport::{Certificate, ClientTlsConfig, Identity};
use tracing::info;
use zeroize::Zeroizing;

use crate::config::ControlPlaneConfig;
use crate::enroll::certificate::{self, CertificateProblem};
use crate::enroll::client::HANDSHAKE_TIMEOUT;
use crate::enroll::key::{AgentKey, KeyError};
use crate::enroll::pin::{fingerprint, one_certificate};

/// Why the identity can't be used; each text names the files and the fix.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// One of the three files can't be read as a secret file.
    #[error("{source}; {}", file_hint(*.file))]
    File {
        /// Which file.
        file: IdentityFile,
        /// Why.
        #[source]
        source: SecretFileError,
    },
    /// The CA file isn't one certificate.
    #[error(
        "the CA certificate {} must hold exactly one certificate: the server's agent CA from `afkfleet-server pki init`, as enroll pinned it",
        path.display()
    )]
    CaNotOneCertificate {
        /// `ca_cert_file`.
        path: PathBuf,
    },
    /// The key file isn't the Ed25519 key enroll wrote.
    #[error(
        "the key {} {}; it must be the key `afkfleet-agent enroll` wrote",
        path.display(),
        key_text(source)
    )]
    Key {
        /// `key_file`.
        path: PathBuf,
        /// Why.
        source: KeyError,
    },
    /// The certificate failed enroll's check.
    #[error("{}", certificate_text(paths, *problem))]
    Certificate {
        /// The three files.
        paths: Paths,
        /// Why.
        problem: CertificateProblem,
    },
}

/// One of the identity's files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityFile {
    /// `ca_cert_file`.
    Ca,
    /// `cert_file`.
    Certificate,
    /// `key_file`.
    Key,
}

/// The identity's files, as the config names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `ca_cert_file`.
    pub ca: PathBuf,
    /// `cert_file`.
    pub certificate: PathBuf,
    /// `key_file`.
    pub key: PathBuf,
}

/// What to do when a file can't be read.
const fn file_hint(file: IdentityFile) -> &'static str {
    match file {
        IdentityFile::Ca => {
            "[control_plane] ca_cert_file is the server's agent CA from `afkfleet-server pki init`"
        }
        IdentityFile::Certificate | IdentityFile::Key => {
            "an agent that isn't enrolled yet runs `afkfleet-agent enroll` first"
        }
    }
}

/// The key's problem, in the words of a file that's read.
const fn key_text(error: &KeyError) -> &'static str {
    match error {
        KeyError::NotOnePem => "must hold exactly one PKCS#8 private key",
        KeyError::Key | KeyError::Random(_) | KeyError::Csr => "isn't an Ed25519 key",
    }
}

/// The certificate's problem, with its files and the fix.
fn certificate_text(paths: &Paths, problem: CertificateProblem) -> String {
    let certificate = paths.certificate.display();
    match problem {
        CertificateProblem::NotOnePem => format!(
            "the certificate {certificate} isn't one PEM certificate, as `afkfleet-agent enroll` writes it"
        ),
        CertificateProblem::NotFromTheCa => format!(
            "the certificate {certificate} isn't a client certificate the CA in {} signed: check [control_plane] ca_cert_file, or disable this agent on the server and enroll it again",
            paths.ca.display()
        ),
        CertificateProblem::NotOurKey => format!(
            "the certificate {certificate} isn't for the key in {}: the two files come from different enrollments; disable this agent on the server and enroll it again",
            paths.key.display()
        ),
        CertificateProblem::Expired { not_after, now } => format!(
            "the certificate {certificate} expired at {not_after}, and it's {now} by this host's clock: disable this agent on the server and enroll it again; if the clock is wrong, fix it (NTP)"
        ),
        CertificateProblem::NotYetValid => format!(
            "the certificate {certificate} isn't valid yet by this host's clock: fix the clock (NTP)"
        ),
    }
}

/// The certificate and the CA without the key: the check before each
/// attempt to connect (ADR-0019, R52).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptCertificate {
    certificate_der: Vec<u8>,
    ca_der: Vec<u8>,
}

impl KeptCertificate {
    /// Whether the certificate is still valid at `now`.
    ///
    /// # Errors
    /// [`CertificateProblem::Expired`] or
    /// [`CertificateProblem::NotYetValid`]; `NotFromTheCa` only if the files
    /// changed under a running agent, which never re-reads them.
    pub fn validity(&self, now: UnixTime) -> Result<(), CertificateProblem> {
        certificate::validity(&self.certificate_der, &self.ca_der, now)
    }
}

/// A managed agent's identity, checked; see the [module docs](self). It
/// isn't `Clone`, and its `Debug` shows only the two fingerprints.
pub struct ManagedIdentity {
    key_pem: Zeroizing<String>,
    certificate_pem: Vec<u8>,
    ca_pem: Vec<u8>,
    kept: KeptCertificate,
    certificate: CertFingerprint,
    ca: CertFingerprint,
}

impl fmt::Debug for ManagedIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManagedIdentity")
            .field("certificate", &self.certificate)
            .field("ca", &self.ca)
            .finish_non_exhaustive()
    }
}

impl ManagedIdentity {
    /// Loads `plane`'s three files and checks them at `now`.
    ///
    /// # Errors
    /// [`IdentityError`], the first problem found: the CA, then the key,
    /// then the certificate.
    pub fn load(plane: &ControlPlaneConfig, now: UnixTime) -> Result<Self, IdentityError> {
        let paths = Paths {
            ca: plane.ca_cert_file.clone(),
            certificate: plane.cert_file.clone(),
            key: plane.key_file.clone(),
        };
        let ca_pem = read(&paths.ca, IdentityFile::Ca)?;
        let ca_der =
            one_certificate(&ca_pem).ok_or_else(|| IdentityError::CaNotOneCertificate {
                path: paths.ca.clone(),
            })?;
        let key_pem = read(&paths.key, IdentityFile::Key)?;
        let key = AgentKey::from_pem(&key_pem).map_err(|source| IdentityError::Key {
            path: paths.key.clone(),
            source,
        })?;
        let certificate_pem = read(&paths.certificate, IdentityFile::Certificate)?;
        let checked = core::str::from_utf8(&certificate_pem)
            .map_err(|_| CertificateProblem::NotOnePem)
            .and_then(|text| certificate::check(text, &ca_der, &key, now));
        let certificate_der = match checked {
            Ok(der) => der,
            Err(problem) => return Err(IdentityError::Certificate { paths, problem }),
        };
        Ok(Self {
            key_pem: Zeroizing::new(String::from(key.to_pem().as_str())),
            certificate_pem: certificate_pem.to_vec(),
            ca_pem: ca_pem.to_vec(),
            certificate: fingerprint(&certificate_der),
            ca: fingerprint(&ca_der),
            kept: KeptCertificate {
                certificate_der,
                ca_der,
            },
        })
    }

    /// The certificate's fingerprint and the CA's.
    #[must_use]
    pub const fn fingerprints(&self) -> (CertFingerprint, CertFingerprint) {
        (self.certificate, self.ca)
    }

    /// Logs the identity's one start line: both fingerprints, never the key.
    pub fn log_loaded(&self) {
        info!(
            certificate_sha256 = %self.certificate,
            ca_sha256 = %self.ca,
            "the agent's identity is loaded"
        );
    }

    /// Hands the identity to tonic's TLS config for `domain`, the server
    /// URL's host, trusting only the CA, with enroll's handshake bound; what
    /// stays is the [`KeptCertificate`].
    #[must_use]
    pub fn into_tls(self, domain: String) -> (ClientTlsConfig, KeptCertificate) {
        let identity = Identity::from_pem(&self.certificate_pem, self.key_pem.as_bytes());
        let tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(&self.ca_pem))
            .domain_name(domain)
            .identity(identity)
            .timeout(HANDSHAKE_TIMEOUT);
        (tls, self.kept)
    }
}

/// `path`'s bytes through `secret_file`, as `file`.
fn read(path: &Path, file: IdentityFile) -> Result<Zeroizing<Vec<u8>>, IdentityError> {
    secret_file::read(path)
        .map(|bytes| Zeroizing::new(bytes.expose_secret().clone()))
        .map_err(|source| IdentityError::File { file, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_hint_says_what_to_do() {
        assert!(file_hint(IdentityFile::Key).contains("afkfleet-agent enroll"));
        assert!(file_hint(IdentityFile::Certificate).contains("afkfleet-agent enroll"));
        assert!(file_hint(IdentityFile::Ca).contains("pki init"));
        assert_eq!(key_text(&KeyError::Csr), "isn't an Ed25519 key");
    }
}
