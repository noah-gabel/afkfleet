//! `afkfleet-server pki init` and `pki server-cert` (Plan.md P10.2;
//! ADR-0019, S11, S12): the control plane's PKI, made on the server's host.
//!
//! - **`init`** creates the CA and the gRPC listener's server certificate:
//!   the four files `[grpc]` names, keys first and the CA certificate last.
//! - **`server-cert`** issues a new server key and certificate from the
//!   existing CA, for the yearly renewal; it never touches the CA's files.
//! - **Neither overwrites a file** (a new CA would lock out every agent): a
//!   target or a leftover temp file that exists stops the command before
//!   anything is written, naming each one. To renew the server certificate,
//!   move the old pair away first.
//! - **Neither opens the database or reads the vault's keys,** and neither
//!   needs an async runtime.
//!
//! The report goes to stdout: each file created, the CA's fingerprint (the
//! pin an enrollment bundle carries, as openssl prints it), the server
//! certificate's names and expiry. A key or a PEM is never printed. A
//! refusal or failure is one `afkfleet-server: …` line on stderr per
//! problem. On Windows, each key written gets a note that only its folder's
//! permissions protect it (S20).

use std::io::Write;
use std::net::IpAddr;
use std::path::Path;

use chrono::SecondsFormat;
use fleet_core::system::{Clock, SecureRandom};
use tracing::{error, info};

use crate::config::{PkiPaths, ServerConfig};
use crate::http::error::chain;
use crate::infra::pki::files::{self, NewFile};
use crate::infra::pki::{
    CertificateAuthority, Ed25519Key, PkiError, ServerNames, Validity, load_ca,
};

/// The exit codes of `afkfleet-server pki init`, shown in its help.
pub const INIT_EXIT_CODES: &str = "\
Exit codes:
  0  the CA and the server certificate are created
  1  nothing was created: a file exists, a folder is missing, the names are
     invalid, there's no [grpc] section, or the config or logging failed
  2  usage error";

/// The exit codes of `afkfleet-server pki server-cert`, shown in its help.
pub const SERVER_CERT_EXIT_CODES: &str = "\
Exit codes:
  0  the new server certificate and key are created
  1  nothing was created: a server file exists, the CA can't be read, the
     names are invalid, there's no [grpc] section, or the config or logging
     failed
  2  usage error";

/// The temp files' prefix of `pki init`.
const INIT_PREFIX: &str = "pki-init-tmp-";

/// The temp files' prefix of `pki server-cert`.
const SERVER_CERT_PREFIX: &str = "pki-server-cert-tmp-";

/// How a `pki` command ended, which decides the process's exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The files are created.
    Created,
    /// Nothing was created; stderr says why.
    Failed,
}

impl Exit {
    /// The process's exit code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Created => 0,
            Self::Failed => 1,
        }
    }
}

/// `afkfleet-server pki init`: creates the CA and the server certificate for
/// the names `dns` and `ips`.
pub fn init(
    config: &ServerConfig,
    dns: &[String],
    ips: &[IpAddr],
    clock: &dyn Clock,
    random: &dyn SecureRandom,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Exit {
    let Some(pki) = pki_paths(config, err) else {
        return Exit::Failed;
    };
    let names = match ServerNames::try_new(dns, ips) {
        Ok(names) => names,
        Err(error) => return refuse(err, &chain(&error)),
    };
    // Keys first, the CA certificate, which agents trust, last.
    let targets = [
        pki.ca_key_file.as_path(),
        pki.server_key_file.as_path(),
        pki.server_cert_file.as_path(),
        pki.ca_cert_file.as_path(),
    ];
    if !checked(&targets, INIT_PREFIX, err) {
        return Exit::Failed;
    }
    let now = clock.now();
    let created = CertificateAuthority::create(random, now).and_then(|(ca, ca_cert)| {
        let server_key = Ed25519Key::generate(random)?;
        let server = ca.issue_server(&server_key, &names, random, now)?;
        Ok((ca, ca_cert, server_key, server))
    });
    let (ca, ca_cert, server_key, server) = match created {
        Ok(created) => created,
        Err(error) => return fail(err, "the PKI couldn't be made", &error),
    };
    let ca_key_pem = ca.key().to_pem();
    let server_key_pem = server_key.to_pem();
    let files = [
        NewFile {
            path: &pki.ca_key_file,
            bytes: ca_key_pem.as_bytes(),
        },
        NewFile {
            path: &pki.server_key_file,
            bytes: server_key_pem.as_bytes(),
        },
        NewFile {
            path: &pki.server_cert_file,
            bytes: server.pem.as_bytes(),
        },
        NewFile {
            path: &pki.ca_cert_file,
            bytes: ca_cert.pem.as_bytes(),
        },
    ];
    if !published(&files, INIT_PREFIX, out, err) {
        return Exit::Failed;
    }
    let _ = writeln!(out, "CA fingerprint (SHA-256): {}", ca.fingerprint());
    report_server(&names, &server.validity, out);
    note_keys(&[&pki.ca_key_file, &pki.server_key_file], err);
    info!(fingerprint = %ca.fingerprint(), "the PKI is created");
    Exit::Created
}

/// `afkfleet-server pki server-cert`: issues a new server certificate and
/// key from the existing CA, for the names `dns` and `ips`.
pub fn server_cert(
    config: &ServerConfig,
    dns: &[String],
    ips: &[IpAddr],
    clock: &dyn Clock,
    random: &dyn SecureRandom,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Exit {
    let Some(pki) = pki_paths(config, err) else {
        return Exit::Failed;
    };
    let names = match ServerNames::try_new(dns, ips) {
        Ok(names) => names,
        Err(error) => return refuse(err, &chain(&error)),
    };
    let targets = [
        pki.server_key_file.as_path(),
        pki.server_cert_file.as_path(),
    ];
    if !checked(&targets, SERVER_CERT_PREFIX, err) {
        return Exit::Failed;
    }
    let now = clock.now();
    let ca = match load_ca(pki, now) {
        Ok((ca, _)) => ca,
        Err(error) => return refuse(err, &chain(&error)),
    };
    let issued = Ed25519Key::generate(random)
        .map_err(PkiError::from)
        .and_then(|key| Ok((ca.issue_server(&key, &names, random, now)?, key)));
    let (server, server_key) = match issued {
        Ok(issued) => issued,
        Err(error) => return fail(err, "the server certificate couldn't be made", &error),
    };
    let server_key_pem = server_key.to_pem();
    let files = [
        NewFile {
            path: &pki.server_key_file,
            bytes: server_key_pem.as_bytes(),
        },
        NewFile {
            path: &pki.server_cert_file,
            bytes: server.pem.as_bytes(),
        },
    ];
    if !published(&files, SERVER_CERT_PREFIX, out, err) {
        return Exit::Failed;
    }
    report_server(&names, &server.validity, out);
    note_keys(&[&pki.server_key_file], err);
    info!(not_after = %server.validity.not_after, "the server certificate is renewed");
    Exit::Created
}

/// The note that only its folder's permissions protect the key at `path`,
/// on Windows; `None` elsewhere, where the files are owner-only (S20).
#[must_use]
pub fn windows_note(path: &Path) -> Option<String> {
    cfg!(windows).then(|| {
        format!(
            "on Windows, only its folder's permissions protect {}: keep it in a folder only \
             you can open, such as one under your user profile",
            path.display()
        )
    })
}

/// The PKI's files, or a refusal when the control plane is off.
fn pki_paths<'a>(config: &'a ServerConfig, err: &mut dyn Write) -> Option<&'a PkiPaths> {
    let paths = config.grpc.as_ref().map(|grpc| &grpc.pki);
    if paths.is_none() {
        refuse(
            err,
            "the control plane is off: the config has no [grpc] section, which names the PKI's \
             files",
        );
    }
    paths
}

/// Whether `targets` can be written; otherwise one line per problem.
fn checked(targets: &[&Path], prefix: &str, err: &mut dyn Write) -> bool {
    let problems = files::check(targets, prefix);
    for problem in &problems {
        let _ = writeln!(err, "afkfleet-server: {problem}");
    }
    problems.is_empty()
}

/// Publishes `files` and names each on stdout; a failure is one line on
/// stderr, and a temp file left behind one line each.
fn published(
    files: &[NewFile<'_>],
    prefix: &str,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> bool {
    match files::publish(files, prefix) {
        Ok(left_over) => {
            for file in files {
                let _ = writeln!(out, "created {}", file.path.display());
            }
            for path in left_over {
                let _ = writeln!(
                    err,
                    "afkfleet-server: {} couldn't be removed; remove it",
                    path.display()
                );
            }
            true
        }
        Err(error) => {
            fail(err, "the files couldn't be written", &error);
            false
        }
    }
}

/// The server certificate's names and expiry, on stdout.
fn report_server(names: &ServerNames, validity: &Validity, out: &mut dyn Write) {
    let _ = writeln!(
        out,
        "server certificate for {names}, valid until {}",
        validity
            .not_after
            .to_rfc3339_opts(SecondsFormat::Secs, true)
    );
}

/// The Windows note for each of `keys`, on stderr.
fn note_keys(keys: &[&Path], err: &mut dyn Write) {
    for key in keys {
        if let Some(note) = windows_note(key) {
            let _ = writeln!(err, "afkfleet-server: note: {note}");
        }
    }
}

/// A refusal: one line on stderr, and exit 1.
fn refuse(err: &mut dyn Write, why: &str) -> Exit {
    let _ = writeln!(err, "afkfleet-server: {why}");
    Exit::Failed
}

/// Something failed: one `error` with the whole chain, one line on stderr,
/// and exit 1.
fn fail(err: &mut dyn Write, what: &str, error: &(dyn std::error::Error + 'static)) -> Exit {
    let chain = chain(error);
    error!(error = %chain, "{what}");
    let _ = writeln!(err, "afkfleet-server: {what}: {chain}");
    Exit::Failed
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    use chrono::{DateTime, Utc};
    use fleet_startup::config::LogConfig;
    use fleet_testkit::system::{ManualClock, SeededRandom};
    use tempfile::TempDir;

    use super::*;
    use crate::config::{
        AccountsConfig, AuthConfig, DatabaseConfig, GrpcConfig, HttpConfig, PkiPaths,
        RateLimitConfig, VaultConfig,
    };
    use crate::http::rate_limit::TrustedProxies;
    use crate::infra::pki::{self, load_ca};

    const NOW: &str = "2027-01-15T08:00:00.789Z";

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    fn paths(dir: &Path) -> PkiPaths {
        PkiPaths {
            ca_cert_file: dir.join("ca.crt"),
            ca_key_file: dir.join("ca.key"),
            server_cert_file: dir.join("server.crt"),
            server_key_file: dir.join("server.key"),
        }
    }

    /// A config whose `[grpc]` names files in `dir` (or no `[grpc]`).
    fn config(dir: &Path, grpc: bool) -> ServerConfig {
        ServerConfig {
            dev_mode: false,
            http: HttpConfig {
                bind: HttpConfig::DEFAULT_BIND,
                request_timeout: Duration::from_secs(15),
                max_body_bytes: 65_536,
                trusted_proxies: TrustedProxies::default(),
            },
            database: DatabaseConfig {
                path: dir.join("afkfleet.db"),
            },
            log: LogConfig::default(),
            auth: AuthConfig::default(),
            rate_limits: RateLimitConfig::default(),
            vault: VaultConfig {
                key_files: [(1, dir.join("vault-key-1"))].into_iter().collect(),
                active_key_id: 1,
            },
            accounts: AccountsConfig::default(),
            grpc: grpc.then(|| GrpcConfig {
                bind: GrpcConfig::DEFAULT_BIND,
                pki: paths(dir),
                agent_cert_lifetime: chrono::TimeDelta::days(60),
                heartbeats: crate::config::Heartbeats::default(),
            }),
        }
    }

    /// What a command printed.
    struct Ran {
        exit: Exit,
        out: String,
        err: String,
    }

    fn localhost() -> (Vec<String>, Vec<IpAddr>) {
        (
            vec!["localhost".to_owned()],
            vec!["127.0.0.1".parse().unwrap(), "::1".parse().unwrap()],
        )
    }

    fn run_init(config: &ServerConfig, seed: u64) -> Ran {
        let (dns, ips) = localhost();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let exit = init(
            config,
            &dns,
            &ips,
            &ManualClock::new(at(NOW)),
            &SeededRandom::new(seed),
            &mut out,
            &mut err,
        );
        Ran {
            exit,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    fn run_server_cert(config: &ServerConfig, seed: u64) -> Ran {
        let (dns, ips) = localhost();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let exit = server_cert(
            config,
            &dns,
            &ips,
            &ManualClock::new(at(NOW)),
            &SeededRandom::new(seed),
            &mut out,
            &mut err,
        );
        Ran {
            exit,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// The lines a run printed on stderr besides the Windows notes.
    fn refusals(ran: &Ran) -> Vec<&str> {
        ran.err
            .lines()
            .filter(|line| !line.contains("only its folder's permissions"))
            .collect()
    }

    fn assert_no_secret_printed(ran: &Ran) {
        for text in [&ran.out, &ran.err] {
            assert!(!text.contains("PRIVATE KEY"), "{text}");
            assert!(!text.contains("BEGIN"), "{text}");
        }
    }

    #[test]
    fn init_creates_the_four_files_and_reports_them() {
        let dir = TempDir::new().unwrap();
        let config = config(dir.path(), true);

        let ran = run_init(&config, 1);

        assert_eq!(ran.exit, Exit::Created, "{}", ran.err);
        assert_eq!(
            files(dir.path()),
            ["ca.crt", "ca.key", "server.crt", "server.key"]
        );
        let loaded = pki::load(&paths(dir.path()), at(NOW)).unwrap();
        let p = paths(dir.path());
        assert_eq!(
            ran.out,
            format!(
                "created {}\ncreated {}\ncreated {}\ncreated {}\n\
                 CA fingerprint (SHA-256): {}\n\
                 server certificate for localhost, 127.0.0.1, ::1, valid until 2028-01-15T08:00:00Z\n",
                p.ca_key_file.display(),
                p.server_key_file.display(),
                p.server_cert_file.display(),
                p.ca_cert_file.display(),
                loaded.ca.fingerprint(),
            )
        );
        assert_eq!(refusals(&ran), Vec::<&str>::new());
        assert_no_secret_printed(&ran);
        #[cfg(unix)]
        for name in ["ca.crt", "ca.key", "server.crt", "server.key"] {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(dir.path().join(name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{name}");
        }
    }

    #[test]
    fn init_never_overwrites_naming_every_file_in_the_way() {
        let dir = TempDir::new().unwrap();
        let config = config(dir.path(), true);
        fs::write(dir.path().join("ca.crt"), b"an older CA").unwrap();
        fs::write(dir.path().join("server.key"), b"an older key").unwrap();

        let ran = run_init(&config, 1);

        assert_eq!(ran.exit, Exit::Failed);
        let p = paths(dir.path());
        assert_eq!(
            refusals(&ran),
            [
                format!(
                    "afkfleet-server: {} exists already; it's never overwritten",
                    p.server_key_file.display()
                ),
                format!(
                    "afkfleet-server: {} exists already; it's never overwritten",
                    p.ca_cert_file.display()
                ),
            ]
        );
        assert_eq!(ran.out, "");
        assert_eq!(files(dir.path()), ["ca.crt", "server.key"]);
        assert_eq!(fs::read(dir.path().join("ca.crt")).unwrap(), b"an older CA");
    }

    #[test]
    fn init_without_a_grpc_section_is_refused() {
        let dir = TempDir::new().unwrap();

        let ran = run_init(&config(dir.path(), false), 1);

        assert_eq!(ran.exit, Exit::Failed);
        assert_eq!(
            refusals(&ran),
            [
                "afkfleet-server: the control plane is off: the config has no [grpc] section, \
              which names the PKI's files"
            ]
        );
        assert_eq!(files(dir.path()), Vec::<String>::new());
    }

    #[test]
    fn init_without_names_is_refused() {
        let dir = TempDir::new().unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());

        let exit = init(
            &config(dir.path(), true),
            &[],
            &[],
            &ManualClock::new(at(NOW)),
            &SeededRandom::new(1),
            &mut out,
            &mut err,
        );

        assert_eq!(exit, Exit::Failed);
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "afkfleet-server: the server certificate needs at least one name (--dns or --ip)\n"
        );
        assert_eq!(files(dir.path()), Vec::<String>::new());
    }

    #[test]
    fn server_cert_issues_a_new_pair_from_the_ca_and_leaves_the_ca_alone() {
        let dir = TempDir::new().unwrap();
        let config = config(dir.path(), true);
        assert_eq!(run_init(&config, 1).exit, Exit::Created);
        let p = paths(dir.path());
        let ca_cert = fs::read(&p.ca_cert_file).unwrap();
        let ca_key = fs::read(&p.ca_key_file).unwrap();
        let old_cert = fs::read(&p.server_cert_file).unwrap();
        let old = dir.path().join("old");
        fs::create_dir(&old).unwrap();
        fs::rename(&p.server_cert_file, old.join("server.crt")).unwrap();
        fs::rename(&p.server_key_file, old.join("server.key")).unwrap();

        let ran = run_server_cert(&config, 2);

        assert_eq!(ran.exit, Exit::Created, "{}", ran.err);
        assert_eq!(fs::read(&p.ca_cert_file).unwrap(), ca_cert);
        assert_eq!(fs::read(&p.ca_key_file).unwrap(), ca_key);
        assert_ne!(fs::read(&p.server_cert_file).unwrap(), old_cert);
        pki::load(&p, at(NOW)).unwrap();
        assert_eq!(
            ran.out,
            format!(
                "created {}\ncreated {}\n\
                 server certificate for localhost, 127.0.0.1, ::1, valid until 2028-01-15T08:00:00Z\n",
                p.server_key_file.display(),
                p.server_cert_file.display(),
            )
        );
        assert_no_secret_printed(&ran);
    }

    #[test]
    fn server_cert_never_overwrites_the_server_pair() {
        let dir = TempDir::new().unwrap();
        let config = config(dir.path(), true);
        assert_eq!(run_init(&config, 1).exit, Exit::Created);
        let p = paths(dir.path());
        let before = fs::read(&p.server_cert_file).unwrap();

        let ran = run_server_cert(&config, 2);

        assert_eq!(ran.exit, Exit::Failed);
        assert_eq!(refusals(&ran).len(), 2, "{}", ran.err);
        assert_eq!(fs::read(&p.server_cert_file).unwrap(), before);
    }

    #[test]
    fn server_cert_without_the_ca_is_refused_naming_its_file() {
        let dir = TempDir::new().unwrap();

        let ran = run_server_cert(&config(dir.path(), true), 2);

        assert_eq!(ran.exit, Exit::Failed);
        let shown = refusals(&ran).join("\n");
        assert!(
            shown.contains(&paths(dir.path()).ca_cert_file.display().to_string()),
            "{shown}"
        );
        assert_eq!(files(dir.path()), Vec::<String>::new());
        assert!(load_ca(&paths(dir.path()), at(NOW)).is_err());
    }

    #[test]
    fn the_windows_note_is_printed_for_each_key_on_windows_only() {
        let dir = TempDir::new().unwrap();
        let p = paths(dir.path());

        let ran = run_init(&config(dir.path(), true), 1);

        let notes: Vec<&str> = ran
            .err
            .lines()
            .filter(|line| line.contains("only its folder's permissions"))
            .collect();
        if cfg!(windows) {
            assert_eq!(
                notes,
                [
                    format!(
                        "afkfleet-server: note: {}",
                        windows_note(&p.ca_key_file).unwrap()
                    ),
                    format!(
                        "afkfleet-server: note: {}",
                        windows_note(&p.server_key_file).unwrap()
                    ),
                ]
            );
            assert_eq!(
                windows_note(Path::new("C:\\x\\ca.key")).as_deref(),
                Some(
                    "on Windows, only its folder's permissions protect C:\\x\\ca.key: keep it \
                     in a folder only you can open, such as one under your user profile"
                )
            );
        } else {
            assert_eq!(notes, Vec::<&str>::new());
            assert_eq!(windows_note(&p.ca_key_file), None);
        }
        let _: PathBuf = p.ca_key_file;
    }
}
