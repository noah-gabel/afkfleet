//! Reading the PKI at serve's startup (S12): the four files of `[grpc]`,
//! each through fleet-startup's `secret_file` (regular file, owner-only on
//! Unix, at most 64 KiB), and the checks that they belong together.
//!
//! - **One section per file:** one `CERTIFICATE` or one Ed25519
//!   `PRIVATE KEY`.
//! - **The CA certificate is a CA,** and the CA key is its key: a probe
//!   certificate the key signs must verify against the certificate with
//!   rustls's client verifier, exactly as agents' certificates will (group
//!   C).
//! - **The server key is the server certificate's,** and the server
//!   certificate is one the CA signed for a server, checked with rustls's
//!   server verifier within its validity.
//! - **An expired server certificate doesn't stop serve.** Only agents need
//!   it, and the HTTP API keeps working; serve logs an `error` instead
//!   ([`LoadedPki::server_not_after`]).
//!
//! Errors name the file and the problem, and never wrap a parser's error,
//! which could quote the file's content.

use core::fmt;
use core::time::Duration as StdDuration;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use fleet_core::id::AgentId;
use fleet_core::pki::CertFingerprint;
use fleet_startup::secret_file::{self, SecretFileError};
use rcgen::SerialNumber;
use rustls::RootCertStore;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::ServerCertVerifier as _;
use rustls::crypto::aws_lc_rs;
use rustls::pki_types::pem::SectionKind;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::WebPkiClientVerifier;
use rustls::sign::CertifiedKey;
use secrecy::ExposeSecret as _;
use x509_parser::prelude::{FromDer as _, GeneralName, X509Certificate};
use zeroize::Zeroizing;

use super::authority::{CertificateAuthority, fingerprint};
use super::keys::{Ed25519Key, KeyError};
use super::pem::single_section;
use crate::config::PkiPaths;

/// Why the PKI couldn't be read. Each variant names its file.
#[derive(Debug, thiserror::Error)]
pub enum PkiLoadError {
    /// The file couldn't be read, or isn't an owner-only regular file of
    /// 1 B to 64 KiB.
    #[error("a file of the PKI can't be used; `afkfleet-server pki init` creates the PKI")]
    File(#[source] SecretFileError),
    /// The file doesn't hold exactly one certificate.
    #[error("the certificate file {} must hold exactly one certificate", path.display())]
    NotOneCertificate {
        /// The file.
        path: PathBuf,
    },
    /// The file doesn't hold exactly one Ed25519 key.
    #[error("the key file {} {source}", path.display())]
    Key {
        /// The file.
        path: PathBuf,
        /// What's wrong with it.
        source: KeyError,
    },
    /// The CA certificate isn't a CA's.
    #[error("the CA certificate {} isn't a CA certificate", path.display())]
    NotACa {
        /// The file.
        path: PathBuf,
    },
    /// The CA key doesn't sign for the CA certificate.
    #[error("the CA key {} isn't the key of the CA certificate", path.display())]
    CaKeyMismatch {
        /// The key file.
        path: PathBuf,
    },
    /// The server key isn't the server certificate's.
    #[error("the server key {} isn't the key of the server certificate", path.display())]
    ServerKeyMismatch {
        /// The key file.
        path: PathBuf,
    },
    /// The server certificate isn't a server certificate the CA signed.
    #[error("the server certificate {} isn't a server certificate the CA signed", path.display())]
    ServerCertRefused {
        /// The file.
        path: PathBuf,
    },
}

/// The PKI as serve uses it.
pub struct LoadedPki {
    /// The CA, which signs agents' certificates.
    pub ca: CertificateAuthority,
    /// The CA certificate's PEM, the gRPC listener's client root.
    pub ca_cert_pem: Vec<u8>,
    /// The server certificate's PEM, the listener's identity.
    pub server_cert_pem: Vec<u8>,
    /// The server key's PEM, the listener's identity; wiped when dropped.
    pub server_key_pem: Zeroizing<Vec<u8>>,
    /// The server certificate's notAfter.
    pub server_not_after: DateTime<Utc>,
}

impl fmt::Debug for LoadedPki {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadedPki")
            .field("ca", &self.ca)
            .field("server_not_after", &self.server_not_after)
            .finish_non_exhaustive()
    }
}

/// Reads the PKI in `paths` and checks it, at `now`.
///
/// # Errors
/// The first [`PkiLoadError`]: [`load_ca`]'s, then the server certificate's
/// and the server key's files, then their checks.
pub fn load(paths: &PkiPaths, now: DateTime<Utc>) -> Result<LoadedPki, PkiLoadError> {
    let (ca, ca_cert_pem) = load_ca(paths, now)?;
    let (server_der, server_cert_pem) = read_certificate(&paths.server_cert_file)?;
    let (server_key, server_key_pem) = read_key(&paths.server_key_file)?;
    let provider = Arc::new(aws_lc_rs::default_provider());
    let roots = roots(&ca, &paths.ca_cert_file)?;

    let server_key_mismatch = || PkiLoadError::ServerKeyMismatch {
        path: paths.server_key_file.clone(),
    };
    let server_key_der = PrivateKeyDer::Pkcs8(server_key.pkcs8_der().to_vec().into());
    CertifiedKey::from_der(vec![server_der.clone()], server_key_der, &provider)
        .and_then(|certified| certified.keys_match())
        .map_err(|_| server_key_mismatch())?;

    let refused = || PkiLoadError::ServerCertRefused {
        path: paths.server_cert_file.clone(),
    };
    let server = parse(&server_der).ok_or_else(|| PkiLoadError::NotOneCertificate {
        path: paths.server_cert_file.clone(),
    })?;
    let validity = server.validity();
    let not_before =
        DateTime::from_timestamp(validity.not_before.timestamp(), 0).ok_or_else(refused)?;
    let server_not_after =
        DateTime::from_timestamp(validity.not_after.timestamp(), 0).ok_or_else(refused)?;
    let name = first_name(&server).ok_or_else(refused)?;
    // Checked at its first valid second, so an expired certificate is still
    // checked for what it is; serve logs its expiry.
    let signed_by_ca = WebPkiServerVerifier::builder_with_provider(roots, provider)
        .build()
        .ok()
        .zip(unix_time(not_before))
        .is_some_and(|(verifier, at)| {
            verifier
                .verify_server_cert(&server_der, &[], &name, &[], at)
                .is_ok()
        });
    if !signed_by_ca {
        return Err(refused());
    }

    Ok(LoadedPki {
        ca,
        ca_cert_pem,
        server_cert_pem,
        server_key_pem,
        server_not_after,
    })
}

/// The fingerprint of the CA certificate at `path`, which must be one CA
/// certificate. The CA key isn't read: `agent create-enrollment-token`
/// needs only the bundle's pin.
///
/// # Errors
/// [`PkiLoadError::File`], [`PkiLoadError::NotOneCertificate`] and
/// [`PkiLoadError::NotACa`].
pub fn ca_fingerprint(path: &Path) -> Result<CertFingerprint, PkiLoadError> {
    let (der, _) = read_certificate(path)?;
    let parsed = parse(&der).ok_or_else(|| PkiLoadError::NotOneCertificate {
        path: path.to_owned(),
    })?;
    if !parsed.is_ca() {
        return Err(PkiLoadError::NotACa {
            path: path.to_owned(),
        });
    }
    Ok(fingerprint(&der))
}

/// Reads the CA in `paths` and checks it at `now`: its certificate is a CA's
/// and its key signs for it. Returns it with its certificate file's PEM.
/// `pki server-cert` needs only this.
///
/// # Errors
/// The first [`PkiLoadError`]: the CA certificate's file, the CA key's,
/// then the checks.
pub fn load_ca(
    paths: &PkiPaths,
    now: DateTime<Utc>,
) -> Result<(CertificateAuthority, Vec<u8>), PkiLoadError> {
    let (ca_der, ca_cert_pem) = read_certificate(&paths.ca_cert_file)?;
    let (ca_key, _) = read_key(&paths.ca_key_file)?;
    let not_one = || PkiLoadError::NotOneCertificate {
        path: paths.ca_cert_file.clone(),
    };
    if !parse(&ca_der).ok_or_else(not_one)?.is_ca() {
        return Err(PkiLoadError::NotACa {
            path: paths.ca_cert_file.clone(),
        });
    }
    let ca = CertificateAuthority::from_parts(ca_der, ca_key).map_err(|_| not_one())?;
    let key_mismatch = || PkiLoadError::CaKeyMismatch {
        path: paths.ca_key_file.clone(),
    };
    // The probe certifies the CA key's own public key; only its signature
    // matters, and it never leaves the process.
    let probe_agent = AgentId::new_v7(now, [0; 10]).map_err(|_| key_mismatch())?;
    let probe = ca
        .issue_agent_with(
            ca.key(),
            probe_agent,
            PROBE_LIFETIME,
            SerialNumber::from(vec![0x40; 16]),
            now,
        )
        .map_err(|_| key_mismatch())?;
    let provider = Arc::new(aws_lc_rs::default_provider());
    let accepted =
        WebPkiClientVerifier::builder_with_provider(roots(&ca, &paths.ca_cert_file)?, provider)
            .build()
            .ok()
            .zip(unix_time(now))
            .is_some_and(|(verifier, now)| {
                verifier.verify_client_cert(&probe.der, &[], now).is_ok()
            });
    if !accepted {
        return Err(key_mismatch());
    }
    Ok((ca, ca_cert_pem))
}

/// The trust anchors of `ca`, whose certificate is the file at `path`.
fn roots(ca: &CertificateAuthority, path: &Path) -> Result<Arc<RootCertStore>, PkiLoadError> {
    let mut roots = RootCertStore::empty();
    roots
        .add(ca.cert_der().clone())
        .map_err(|_| PkiLoadError::NotOneCertificate {
            path: path.to_owned(),
        })?;
    Ok(Arc::new(roots))
}

/// How long the startup check's probe certificate is valid.
const PROBE_LIFETIME: TimeDelta = TimeDelta::hours(1);

/// The one certificate in the file at `path`, and the file's bytes.
fn read_certificate(path: &Path) -> Result<(CertificateDer<'static>, Vec<u8>), PkiLoadError> {
    let file = secret_file::read(path).map_err(PkiLoadError::File)?;
    let bytes = file.expose_secret();
    let der = single_section(bytes, SectionKind::Certificate).ok_or_else(|| {
        PkiLoadError::NotOneCertificate {
            path: path.to_owned(),
        }
    })?;
    Ok((CertificateDer::from(der.to_vec()), bytes.clone()))
}

/// The one Ed25519 key in the file at `path`, and the file's bytes, wiped
/// when dropped.
fn read_key(path: &Path) -> Result<(Ed25519Key, Zeroizing<Vec<u8>>), PkiLoadError> {
    let file = secret_file::read(path).map_err(PkiLoadError::File)?;
    let bytes = file.expose_secret();
    let key = Ed25519Key::from_pem(bytes).map_err(|source| PkiLoadError::Key {
        path: path.to_owned(),
        source,
    })?;
    Ok((key, Zeroizing::new(bytes.clone())))
}

/// `der` read as a certificate; `None` for anything else, whose error is
/// never passed on.
fn parse(der: &[u8]) -> Option<X509Certificate<'_>> {
    X509Certificate::from_der(der)
        .ok()
        .filter(|(rest, _)| rest.is_empty())
        .map(|(_, cert)| cert)
}

/// The certificate's first name, DNS or IP, as rustls checks it.
fn first_name(cert: &X509Certificate<'_>) -> Option<ServerName<'static>> {
    let names = cert.subject_alternative_name().ok()??;
    names
        .value
        .general_names
        .iter()
        .find_map(|name| match name {
            GeneralName::DNSName(dns) => ServerName::try_from((*dns).to_owned()).ok(),
            GeneralName::IPAddress(bytes) => match bytes.len() {
                4 => <[u8; 4]>::try_from(*bytes)
                    .ok()
                    .map(|octets| ServerName::IpAddress(IpAddr::from(octets).into())),
                16 => <[u8; 16]>::try_from(*bytes)
                    .ok()
                    .map(|octets| ServerName::IpAddress(IpAddr::from(octets).into())),
                _ => None,
            },
            _ => None,
        })
}

/// `at` as rustls's time; `None` before 1970.
fn unix_time(at: DateTime<Utc>) -> Option<UnixTime> {
    u64::try_from(at.timestamp())
        .ok()
        .map(|seconds| UnixTime::since_unix_epoch(StdDuration::from_secs(seconds)))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use chrono::TimeDelta;
    use fleet_testkit::system::SeededRandom;

    use super::super::authority::CertificateAuthority;
    use super::super::keys::Ed25519Key;
    use super::super::profiles::ServerNames;
    use super::*;

    const NOW: &str = "2027-01-15T08:00:00Z";

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    /// Writes `text` owner-only, as `pki init` does.
    fn write(path: &Path, text: &[u8]) {
        fs::write(path, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    /// A PKI written to a fresh folder, as `pki init` writes one, from
    /// `seed`.
    struct Written {
        _dir: tempfile::TempDir,
        paths: PkiPaths,
        ca: CertificateAuthority,
        server_not_after: DateTime<Utc>,
    }

    fn written(seed: u64) -> Written {
        let dir = tempfile::tempdir().unwrap();
        let paths = PkiPaths {
            ca_cert_file: dir.path().join("ca.crt"),
            ca_key_file: dir.path().join("ca.key"),
            server_cert_file: dir.path().join("server.crt"),
            server_key_file: dir.path().join("server.key"),
        };
        let random = SeededRandom::new(seed);
        let (ca, ca_cert) = CertificateAuthority::create(&random, at(NOW)).unwrap();
        let server_key = Ed25519Key::generate(&random).unwrap();
        let names = ServerNames::try_new(&["localhost".to_owned()], &[]).unwrap();
        let server = ca
            .issue_server(&server_key, &names, &random, at(NOW))
            .unwrap();
        write(&paths.ca_cert_file, ca_cert.pem.as_bytes());
        write(&paths.ca_key_file, ca.key().to_pem().as_bytes());
        write(&paths.server_cert_file, server.pem.as_bytes());
        write(&paths.server_key_file, server_key.to_pem().as_bytes());
        Written {
            _dir: dir,
            paths,
            ca,
            server_not_after: server.validity.not_after,
        }
    }

    fn refusal(paths: &PkiPaths) -> PkiLoadError {
        load(paths, at(NOW)).unwrap_err()
    }

    #[test]
    fn the_ca_fingerprint_is_read_from_the_certificate_alone() {
        let pki = written(1);
        fs::remove_file(&pki.paths.ca_key_file).unwrap();

        let read = ca_fingerprint(&pki.paths.ca_cert_file).unwrap();

        assert_eq!(read, pki.ca.fingerprint());
    }

    #[test]
    fn a_ca_fingerprint_is_only_read_from_a_ca_certificate() {
        let pki = written(1);
        let missing = pki.paths.ca_cert_file.with_extension("missing");

        let server = ca_fingerprint(&pki.paths.server_cert_file).unwrap_err();
        let key = ca_fingerprint(&pki.paths.ca_key_file).unwrap_err();
        let absent = ca_fingerprint(&missing).unwrap_err();

        assert!(matches!(server, PkiLoadError::NotACa { .. }), "{server:?}");
        assert!(
            matches!(key, PkiLoadError::NotOneCertificate { .. }),
            "{key:?}"
        );
        assert!(matches!(absent, PkiLoadError::File(_)), "{absent:?}");
    }

    #[test]
    fn a_written_pki_loads_back() {
        let pki = written(1);

        let loaded = load(&pki.paths, at(NOW)).unwrap();

        assert_eq!(loaded.ca.fingerprint(), pki.ca.fingerprint());
        assert_eq!(loaded.ca_cert_pem, as_read(&pki.paths.ca_cert_file));
        assert_eq!(loaded.server_cert_pem, as_read(&pki.paths.server_cert_file));
        assert_eq!(*loaded.server_key_pem, as_read(&pki.paths.server_key_file));
        assert_eq!(loaded.server_not_after, pki.server_not_after);
    }

    #[test]
    fn a_loaded_pkis_debug_never_shows_the_server_key() {
        let pki = written(1);
        let loaded = load(&pki.paths, at(NOW)).unwrap();

        let shown = format!("{loaded:?}");

        let key = String::from_utf8(loaded.server_key_pem.to_vec()).unwrap();
        let body = key.lines().nth(1).unwrap();
        assert!(!shown.contains(body), "{shown}");
        assert!(shown.contains("server_not_after"), "{shown}");
    }

    /// The file at `path` as `secret_file` reads it: one trailing line
    /// break removed.
    fn as_read(path: &Path) -> Vec<u8> {
        let mut bytes = fs::read(path).unwrap();
        if bytes.ends_with(b"\r\n") {
            bytes.truncate(bytes.len() - 2);
        } else if bytes.ends_with(b"\n") {
            bytes.truncate(bytes.len() - 1);
        }
        bytes
    }

    #[test]
    fn an_expired_server_certificate_still_loads_with_its_not_after() {
        let pki = written(1);
        let later = pki.server_not_after + TimeDelta::days(1);

        let loaded = load(&pki.paths, later).unwrap();

        assert_eq!(loaded.server_not_after, pki.server_not_after);
    }

    #[test]
    fn a_missing_file_is_named_with_the_command_that_makes_it() {
        let pki = written(1);
        fs::remove_file(&pki.paths.server_key_file).unwrap();

        let error = refusal(&pki.paths);

        assert!(matches!(error, PkiLoadError::File(_)), "{error:?}");
        let shown = crate::http::error::chain(&error);
        assert!(
            shown.contains(&pki.paths.server_key_file.display().to_string()),
            "{shown}"
        );
        assert!(shown.contains("afkfleet-server pki init"), "{shown}");
    }

    #[test]
    fn a_certificate_file_without_exactly_one_certificate_is_refused() {
        let pki = written(1);
        let ca = fs::read_to_string(&pki.paths.ca_cert_file).unwrap();
        write(&pki.paths.ca_cert_file, format!("{ca}{ca}").as_bytes());

        let error = refusal(&pki.paths);

        assert!(
            matches!(&error, PkiLoadError::NotOneCertificate { path } if *path == pki.paths.ca_cert_file),
            "{error:?}"
        );
    }

    #[test]
    fn a_key_file_holding_a_certificate_is_refused() {
        let pki = written(1);
        let cert = fs::read(&pki.paths.server_cert_file).unwrap();
        write(&pki.paths.server_key_file, &cert);

        let error = refusal(&pki.paths);

        assert!(
            matches!(&error, PkiLoadError::Key { path, source: KeyError::NotOneKey } if *path == pki.paths.server_key_file),
            "{error:?}"
        );
    }

    #[test]
    fn a_ca_certificate_that_isnt_a_ca_s_is_refused() {
        let pki = written(1);
        let server = fs::read(&pki.paths.server_cert_file).unwrap();
        write(&pki.paths.ca_cert_file, &server);

        let error = refusal(&pki.paths);

        assert!(
            matches!(&error, PkiLoadError::NotACa { path } if *path == pki.paths.ca_cert_file),
            "{error:?}"
        );
    }

    #[test]
    fn another_ca_s_key_is_refused() {
        let pki = written(1);
        let other = written(2);
        fs::copy(&other.paths.ca_key_file, &pki.paths.ca_key_file).unwrap();

        let error = refusal(&pki.paths);

        assert!(
            matches!(&error, PkiLoadError::CaKeyMismatch { path } if *path == pki.paths.ca_key_file),
            "{error:?}"
        );
    }

    #[test]
    fn another_server_key_is_refused() {
        let pki = written(1);
        let other = written(2);
        fs::copy(&other.paths.server_key_file, &pki.paths.server_key_file).unwrap();

        let error = refusal(&pki.paths);

        assert!(
            matches!(&error, PkiLoadError::ServerKeyMismatch { path } if *path == pki.paths.server_key_file),
            "{error:?}"
        );
    }

    #[test]
    fn a_server_certificate_of_another_ca_is_refused() {
        let pki = written(1);
        let other = written(2);
        fs::copy(&other.paths.server_cert_file, &pki.paths.server_cert_file).unwrap();
        fs::copy(&other.paths.server_key_file, &pki.paths.server_key_file).unwrap();

        let error = refusal(&pki.paths);

        assert!(
            matches!(&error, PkiLoadError::ServerCertRefused { path } if *path == pki.paths.server_cert_file),
            "{error:?}"
        );
    }

    #[test]
    fn errors_never_show_the_files_content() {
        let pki = written(1);
        let marker = "MARKER-7f3a";
        for path in [&pki.paths.ca_cert_file, &pki.paths.ca_key_file] {
            let mut text = fs::read_to_string(path).unwrap();
            text = text.replace("-----END", &format!("{marker}\n-----END"));
            write(path, text.as_bytes());
            let error = refusal(&pki.paths);
            let shown = format!("{error} {error:?}");
            assert!(!shown.contains(marker), "{shown}");
            assert!(!shown.contains("PRIVATE KEY"), "{shown}");
        }
    }
}
