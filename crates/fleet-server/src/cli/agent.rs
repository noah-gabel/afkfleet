//! `afkfleet-server agent create-enrollment-token` (Plan.md P10.3;
//! ADR-0019, U3): the server's host operator creates an enrollment token
//! without the API, until the app's agents screen (P11.9), and beside it
//! afterwards.
//!
//! - **Refused while the control plane is off** (`[grpc]` isn't
//!   configured), before anything is read.
//! - **The CA certificate** gives the bundle its pin: it's read through
//!   `secret_file` and must be a CA's. The CA key isn't read.
//! - **The database** is opened with [`sqlite::open_migrated`]: never
//!   created or migrated, so it's safe while `serve` runs; the command's one
//!   write waits for the server's on SQLite's lock.
//! - **The token** is the Owner's kind: single-use, 15 minutes, stored only
//!   as its hash, at most 20 pending in all. Its audit entry,
//!   `enrollment_token.create`, has no actor and no IP, and `via` = `cli`.
//!   No 2FA guards it: whoever runs the binary against the database holds
//!   the CA key, the vault's keys and the database already (threat model).
//! - **Output:** the bundle alone on stdout, as one line, so a variable or a
//!   pipe takes it whole; its expiry and the CA's fingerprint on stderr. A
//!   refusal or failure is one `afkfleet-server: …` line on stderr. Logs
//!   per `[log]` never hold the token.

use std::io::Write;
use std::sync::Arc;

use chrono::SecondsFormat;
use fleet_core::system::{Clock, SecureRandom};
use secrecy::ExposeSecret as _;
use tracing::error;

use super::DATABASE_CLOSE;
use crate::app::enrollment::{
    CreateTokenError, CreatedEnrollmentToken, Creator, EnrollmentTokenService,
};
use crate::config::ServerConfig;
use crate::http::error::chain;
use crate::infra::crypto::tokens::Sha256Tokens;
use crate::infra::pki::ca_fingerprint;
use crate::infra::sqlite::{self, Database, DatabaseOptions};

/// The exit codes of `afkfleet-server agent create-enrollment-token`, shown
/// in its help.
pub const CREATE_ENROLLMENT_TOKEN_EXIT_CODES: &str = "\
Exit codes:
  0  the token is created: its bundle is on stdout
  1  nothing was created: the control plane is off ([grpc] isn't configured),
     20 tokens are pending already, or the config, logging, the CA
     certificate or the database failed (a missing database and pending
     migrations included)
  2  usage error";

/// How the command ended, which decides the process's exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The token is created, and its bundle printed.
    Created,
    /// Nothing was created; stderr (and the log) says why.
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

/// `afkfleet-server agent create-enrollment-token`: creates a token with
/// `clock` and `random`, and prints its bundle on `out`; see the [module
/// docs](self).
pub async fn create_enrollment_token(
    config: &ServerConfig,
    clock: Arc<dyn Clock>,
    random: Arc<dyn SecureRandom>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Exit {
    let Some(grpc) = config.grpc.as_ref() else {
        return refuse(
            err,
            "the control plane is off: [grpc] isn't configured, so no agent can enroll",
        );
    };
    // A local file, read off the runtime. No timeout: `serve` reads it the
    // same way, before its runtime starts, and nothing else waits for this
    // command.
    let path = grpc.pki.ca_cert_file.clone();
    let fingerprint = match tokio::task::spawn_blocking(move || ca_fingerprint(&path)).await {
        Ok(Ok(fingerprint)) => fingerprint,
        Ok(Err(error)) => return fail(err, "the CA certificate can't be used", &error),
        Err(error) => return fail(err, "the CA certificate can't be used", &error),
    };
    let database = match sqlite::open_migrated(&config.database, DatabaseOptions::default()).await {
        Ok(database) => database,
        Err(error) => return fail(err, "the database can't be opened", &error),
    };
    let tokens = EnrollmentTokenService::new(
        clock,
        Arc::clone(&random),
        Arc::new(database.clone()),
        Arc::new(Sha256Tokens::new(random)),
        fingerprint,
    );
    let exit = match tokens.create(Creator::Host).await {
        Ok(created) => print(&created, out, err),
        Err(CreateTokenError::CapReached) => refuse(
            err,
            "20 enrollment tokens are pending already: use one, or wait until they expire",
        ),
        Err(CreateTokenError::Store(error)) => fail(err, "the database failed", &error),
        Err(error) => fail(err, "the token couldn't be created", &error),
    };
    close(database, exit, err).await
}

/// Prints the bundle of `created` on `out`, and its expiry and the CA's
/// fingerprint on `err`.
fn print(created: &CreatedEnrollmentToken, out: &mut dyn Write, err: &mut dyn Write) -> Exit {
    let bundle = created.bundle.to_text();
    if let Err(error) = writeln!(out, "{}", bundle.expose_secret()).and_then(|()| out.flush()) {
        return fail(err, "the bundle couldn't be written to stdout", &error);
    }
    let _ = writeln!(
        err,
        "The enrollment token expires at {}: pipe the bundle into `afkfleet-agent enroll` before then.\nCA fingerprint (SHA-256): {}",
        created
            .expires_at
            .to_rfc3339_opts(SecondsFormat::Secs, true),
        created.bundle.ca_fingerprint()
    );
    Exit::Created
}

/// Closes `database` within [`DATABASE_CLOSE`], keeping `exit` unless that
/// fails.
async fn close(database: Database, exit: Exit, err: &mut dyn Write) -> Exit {
    match database.close_within(DATABASE_CLOSE).await {
        Ok(()) => exit,
        Err(error) => fail(err, "the database can't be closed", &error),
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
    use core::time::Duration;
    use std::path::Path;

    use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget};
    use fleet_core::enrollment::{EnrollmentBundle, MAX_PENDING_TOKENS};
    use fleet_core::pki::CertFingerprint;
    use fleet_startup::config::{LogConfig, LogFormat};
    use fleet_startup::telemetry::{NoRules, layer_with};
    use fleet_testkit::log_buffer::LogBuffer;
    use fleet_testkit::system::{ManualClock, SeededRandom};
    use tempfile::TempDir;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;
    use crate::config::{
        AccountsConfig, AuthConfig, DatabaseConfig, GrpcConfig, HttpConfig, PkiPaths,
        RateLimitConfig, VaultConfig,
    };
    use crate::http::rate_limit::TrustedProxies;
    use crate::infra::crypto::tokens::Sha256Tokens;
    use crate::infra::pki::CertificateAuthority;
    use crate::infra::sqlite::{self, Database, DatabaseOptions};
    use crate::ports::audit::{AuditLimit, AuditPage, AuditRecord};
    use crate::ports::store::Store as _;
    use crate::ports::tokens::{TokenKind, Tokens as _};

    const NOW: i64 = 1_800_000_000_000;

    fn at(millis: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp_millis(millis).unwrap()
    }

    fn database_config(dir: &Path) -> DatabaseConfig {
        DatabaseConfig {
            path: dir.join("afkfleet.db"),
        }
    }

    /// A config whose database lives in `dir`, with `[grpc]` naming the CA
    /// certificate there when `grpc` is set. The CA key's file never exists.
    fn config(dir: &Path, grpc: bool) -> ServerConfig {
        ServerConfig {
            dev_mode: false,
            http: HttpConfig {
                bind: HttpConfig::DEFAULT_BIND,
                request_timeout: Duration::from_secs(15),
                max_body_bytes: 65_536,
                trusted_proxies: TrustedProxies::default(),
            },
            database: database_config(dir),
            log: LogConfig::default(),
            auth: AuthConfig::default(),
            rate_limits: RateLimitConfig::default(),
            vault: VaultConfig {
                key_files: std::collections::BTreeMap::new(),
                active_key_id: 1,
            },
            accounts: AccountsConfig::default(),
            grpc: grpc.then(|| GrpcConfig {
                bind: GrpcConfig::DEFAULT_BIND,
                pki: PkiPaths {
                    ca_cert_file: dir.join("ca.crt"),
                    ca_key_file: dir.join("no-such-ca.key"),
                    server_cert_file: dir.join("server.crt"),
                    server_key_file: dir.join("server.key"),
                },
                agent_cert_lifetime: chrono::TimeDelta::days(60),
            }),
        }
    }

    /// Writes a CA certificate into `dir`, and returns its fingerprint.
    fn write_ca(dir: &Path) -> CertFingerprint {
        let (ca, issued) = CertificateAuthority::create(&SeededRandom::new(3), at(NOW)).unwrap();
        let path = dir.join("ca.crt");
        std::fs::write(&path, issued.pem).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        ca.fingerprint()
    }

    /// A migrated database in `dir`, closed again.
    async fn migrate(dir: &Path) {
        sqlite::open(&database_config(dir), DatabaseOptions::default())
            .await
            .unwrap()
            .close()
            .await;
    }

    struct Ran {
        exit: Exit,
        out: String,
        err: String,
    }

    /// Runs the command over `config`, its randomness seeded with `seed`.
    async fn run(config: &ServerConfig, seed: u64) -> Ran {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let exit = create_enrollment_token(
            config,
            Arc::new(ManualClock::new(at(NOW))),
            Arc::new(SeededRandom::new(seed)),
            &mut out,
            &mut err,
        )
        .await;
        Ran {
            exit,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    async fn audit(database: &Database) -> Vec<AuditRecord> {
        database
            .audit()
            .list(AuditPage {
                before: None,
                limit: AuditLimit::new(100).unwrap(),
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_bundle_alone_goes_to_stdout_and_the_token_is_stored_hashed_and_audited() {
        let dir = TempDir::new().unwrap();
        let fingerprint = write_ca(dir.path());
        migrate(dir.path()).await;

        let ran = run(&config(dir.path(), true), 9).await;

        assert_eq!(ran.exit, Exit::Created, "{}", ran.err);
        let line = ran.out.strip_suffix('\n').unwrap();
        assert!(!line.contains('\n'), "{}", ran.out);
        let bundle = EnrollmentBundle::try_from(line).unwrap();
        assert_eq!(bundle.ca_fingerprint(), fingerprint);
        assert!(ran.err.contains(&fingerprint.to_string()), "{}", ran.err);
        assert!(ran.err.contains("2027-01-15T08:15:00Z"), "{}", ran.err);
        assert!(
            !ran.err.contains(bundle.token().expose_secret()),
            "{}",
            ran.err
        );
        let database =
            sqlite::open_migrated(&database_config(dir.path()), DatabaseOptions::default())
                .await
                .unwrap();
        let hash = Sha256Tokens::new(Arc::new(SeededRandom::new(1)))
            .hash_presented(TokenKind::Enrollment, bundle.token().expose_secret())
            .unwrap();
        let token = database
            .enrollment_tokens()
            .find_by_hash(&hash)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(token.created_by, None);
        let entries = audit(&database).await;
        let created: Vec<_> = entries
            .iter()
            .filter(|entry| entry.action == AuditAction::ENROLLMENT_TOKEN_CREATE)
            .collect();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].outcome, AuditOutcome::Success);
        assert_eq!(created[0].actor, None);
        assert_eq!(created[0].ip, None);
        assert_eq!(
            created[0].target,
            Some(RecordedTarget::Known(AuditTarget::EnrollmentToken(
                token.id
            )))
        );
        database.close().await;
    }

    #[tokio::test]
    async fn without_grpc_nothing_is_read_or_created() {
        let dir = TempDir::new().unwrap();

        let ran = run(&config(dir.path(), false), 9).await;

        assert_eq!(ran.exit, Exit::Failed);
        assert_eq!(ran.out, "");
        assert_eq!(
            ran.err,
            "afkfleet-server: the control plane is off: [grpc] isn't configured, so no agent can enroll\n"
        );
        assert!(!dir.path().join("afkfleet.db").exists());
    }

    #[tokio::test]
    async fn a_missing_database_is_never_created() {
        let dir = TempDir::new().unwrap();
        write_ca(dir.path());

        let ran = run(&config(dir.path(), true), 9).await;

        assert_eq!(ran.exit, Exit::Failed);
        assert_eq!(ran.out, "");
        assert!(
            ran.err
                .starts_with("afkfleet-server: the database can't be opened: "),
            "{}",
            ran.err
        );
        assert!(!dir.path().join("afkfleet.db").exists());
    }

    #[tokio::test]
    async fn a_missing_ca_certificate_is_named_with_the_command_that_makes_it() {
        let dir = TempDir::new().unwrap();
        migrate(dir.path()).await;

        let ran = run(&config(dir.path(), true), 9).await;

        assert_eq!(ran.exit, Exit::Failed);
        assert_eq!(ran.out, "");
        assert!(
            ran.err
                .starts_with("afkfleet-server: the CA certificate can't be used: "),
            "{}",
            ran.err
        );
        assert!(ran.err.contains("ca.crt"), "{}", ran.err);
        assert!(ran.err.contains("afkfleet-server pki init"), "{}", ran.err);
    }

    #[tokio::test]
    async fn the_twenty_first_pending_token_is_refused() {
        let dir = TempDir::new().unwrap();
        write_ca(dir.path());
        migrate(dir.path()).await;
        let config = config(dir.path(), true);
        for seed in 0..u64::from(MAX_PENDING_TOKENS) {
            assert_eq!(run(&config, seed).await.exit, Exit::Created);
        }

        let ran = run(&config, 99).await;

        assert_eq!(ran.exit, Exit::Failed);
        assert_eq!(ran.out, "");
        assert_eq!(
            ran.err,
            "afkfleet-server: 20 enrollment tokens are pending already: use one, or wait until they expire\n"
        );
    }

    #[tokio::test]
    async fn a_database_the_server_holds_open_takes_the_token_too() {
        let dir = TempDir::new().unwrap();
        write_ca(dir.path());
        let serving = sqlite::open(&database_config(dir.path()), DatabaseOptions::default())
            .await
            .unwrap();

        let ran = run(&config(dir.path(), true), 9).await;

        assert_eq!(ran.exit, Exit::Created, "{}", ran.err);
        let created = audit(&serving)
            .await
            .into_iter()
            .filter(|entry| entry.action == AuditAction::ENROLLMENT_TOKEN_CREATE)
            .count();
        assert_eq!(created, 1);
        serving.close().await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn no_log_line_holds_the_token() {
        let dir = TempDir::new().unwrap();
        write_ca(dir.path());
        migrate(dir.path()).await;
        let logs = LogBuffer::default();
        let log = LogConfig {
            format: LogFormat::Json,
            filter: "trace".parse().unwrap(),
        };
        let subscriber =
            tracing_subscriber::registry().with(layer_with(&log, &NoRules, false, logs.clone()));
        let _guard = tracing::subscriber::set_default(subscriber);

        let ran = run(&config(dir.path(), true), 9).await;

        assert_eq!(ran.exit, Exit::Created, "{}", ran.err);
        let line = ran.out.trim_end();
        let token = &line[..line.find('.').unwrap()];
        assert!(!logs.text().contains(token), "{}", logs.text());
        assert!(!logs.text().contains(&token["afk_et_".len()..]));
        assert_eq!(
            logs.lines_with("an enrollment token was created")
                .unwrap()
                .len(),
            1,
            "{}",
            logs.text()
        );
    }
}
