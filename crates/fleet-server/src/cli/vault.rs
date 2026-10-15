//! `afkfleet-server vault status` and `vault rotate-key` (Plan.md P9.2,
//! §7.5, ADR-0018): what the vault's keys seal, and re-encrypting every
//! secret under the active key, from the server's host. There's no API for
//! either.
//!
//! - **Neither creates or migrates the database.** Both open it with
//!   [`sqlite::open_migrated`]: a missing file, or one with pending
//!   migrations, stops the command with exit 1 before anything else, so a
//!   key rotation never changes the schema as a side effect.
//! - **`status` reads no key file.** It reads the envelopes' headers and
//!   prints what each key ID seals; it exits 3 when a secret names a key that
//!   isn't configured or an envelope is malformed, so a script can check
//!   before removing a key file.
//! - **`rotate-key`** loads every configured key (off the runtime), then
//!   re-encrypts every secret under another key than `[vault]
//!   active_key_id` ([`VaultService::rotate`]), and prints the outcome and
//!   the status. There's no prompt: the rotation is one transaction, and the
//!   old key file undoes it as long as it's kept.
//!
//! The report goes to stdout; a refusal or failure is one
//! `afkfleet-server: …` line on stderr (a refused rotation adds one line per
//! secret that doesn't open). Both log per `[log]`.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::Write;
use std::sync::Arc;

use tracing::error;

use super::DATABASE_CLOSE;
use crate::app::vault::{
    BadEnvelope, BadReason, KeyUsage, PurposeCounts, RotateError, Rotation, VaultService,
    VaultStatus,
};
use crate::config::ServerConfig;
use crate::http::error::chain;
use crate::infra::crypto::vault::{VaultKeys, XChaChaVault};
use crate::infra::sqlite::{self, Database, DatabaseOptions};
use crate::infra::system::{OsRandom, SystemClock};
use crate::ports::vault::Vault as _;

/// The exit codes of `afkfleet-server vault status`, shown in its help.
pub const STATUS_EXIT_CODES: &str = "\
Exit codes:
  0  every secret's key is configured, and no envelope is malformed
  1  the config, logging or the database failed, the database doesn't exist,
     or it has pending migrations
  2  usage error
  3  a secret names a key that isn't configured, or an envelope is malformed";

/// The exit codes of `afkfleet-server vault rotate-key`, shown in its help.
pub const ROTATE_KEY_EXIT_CODES: &str = "\
Exit codes:
  0  the secrets are re-encrypted under the active key, or all of them were
     under it already
  1  nothing was re-encrypted: a secret doesn't open, something changed while
     rotating, or the config, logging, a key file or the database failed (a
     missing database and pending migrations included)
  2  usage error";

/// How a vault command ended, which decides the process's exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The report is printed, or the rotation is done.
    Done,
    /// Something was refused or failed; stderr (and the log) says what.
    Failed,
    /// (`status`) A secret names a key that isn't configured, or an envelope
    /// is malformed.
    Unhealthy,
}

impl Exit {
    /// The process's exit code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Done => 0,
            Self::Failed => 1,
            Self::Unhealthy => 3,
        }
    }
}

/// `afkfleet-server vault status`: prints what each key ID seals.
pub async fn status(config: &ServerConfig, out: &mut dyn Write, err: &mut dyn Write) -> Exit {
    let database = match sqlite::open_migrated(&config.database, DatabaseOptions::default()).await {
        Ok(database) => database,
        Err(error) => return fail(err, "the database can't be opened", &error),
    };
    let service = VaultService::new(Arc::new(SystemClock), Arc::new(database.clone()));
    let exit = match print_status(&service, config, out).await {
        Ok(status) if status.healthy() => Exit::Done,
        Ok(_) => Exit::Unhealthy,
        Err(error) => fail(err, "the database failed", &error),
    };
    close(database, exit, err).await
}

/// `afkfleet-server vault rotate-key`: re-encrypts every secret under the
/// active key, then prints the outcome and the status.
pub async fn rotate_key(config: &ServerConfig, out: &mut dyn Write, err: &mut dyn Write) -> Exit {
    let database = match sqlite::open_migrated(&config.database, DatabaseOptions::default()).await {
        Ok(database) => database,
        Err(error) => return fail(err, "the database can't be opened", &error),
    };
    let exit = match load_keys(config).await {
        Ok(keys) => {
            let vault = XChaChaVault::new(keys, Arc::new(OsRandom));
            let service = VaultService::new(Arc::new(SystemClock), Arc::new(database.clone()));
            rotate(&service, &vault, config, out, err).await
        }
        Err(LoadError::Keys(error)) => fail(err, "the vault's keys can't be loaded", &error),
        Err(LoadError::Interrupted(error)) => fail(err, "the vault's keys can't be loaded", &error),
    };
    close(database, exit, err).await
}

/// Why the keys couldn't be loaded.
enum LoadError {
    /// A key file can't be used.
    Keys(crate::infra::crypto::vault::VaultKeyError),
    /// The blocking task that read them ended without an answer.
    Interrupted(tokio::task::JoinError),
}

/// Every configured key, read off the runtime: the files are local, and a
/// slow disk mustn't block it. No timeout: `serve` reads them the same way,
/// before its runtime starts, and nothing else waits for this command.
async fn load_keys(config: &ServerConfig) -> Result<VaultKeys, LoadError> {
    let vault = config.vault.clone();
    tokio::task::spawn_blocking(move || VaultKeys::load(&vault))
        .await
        .map_err(LoadError::Interrupted)?
        .map_err(LoadError::Keys)
}

/// Rotates, then prints the outcome and the status.
async fn rotate(
    service: &VaultService,
    vault: &XChaChaVault,
    config: &ServerConfig,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Exit {
    match service.rotate(vault).await {
        Ok(Rotation::NothingToDo) => {
            let _ = writeln!(
                out,
                "nothing to re-encrypt: no secret is sealed under another key than key {} (active)",
                vault.active_key_id()
            );
        }
        Ok(Rotation::Rotated {
            key_id,
            totp_secrets,
            refresh_tokens,
        }) => {
            let _ = writeln!(
                out,
                "re-encrypted {} and {} under key {key_id}",
                count(totp_secrets, "TOTP secret"),
                count(refresh_tokens, "Microsoft refresh token")
            );
        }
        Ok(Rotation::Refused(bad)) => return refused(err, &bad),
        Ok(Rotation::ChangedMeanwhile) => {
            return refuse(
                err,
                "something changed while rotating, so nothing was written; stop the server and run it again",
            );
        }
        Err(RotateError::Store(error)) => return fail(err, "the database failed", &error),
        Err(error) => return fail(err, "nothing was written", &error),
    }
    match print_status(service, config, out).await {
        Ok(_) => Exit::Done,
        Err(error) => fail(err, "the database failed", &error),
    }
}

/// Reads the status and prints it.
async fn print_status(
    service: &VaultService,
    config: &ServerConfig,
    out: &mut dyn Write,
) -> Result<VaultStatus, crate::ports::store::StoreError> {
    let configured: BTreeSet<u32> = config.vault.key_files.keys().copied().collect();
    let status = service
        .status(&configured, config.vault.active_key_id)
        .await?;
    let _ = out.write_all(report(&status).as_bytes());
    Ok(status)
}

/// The status as `vault status` prints it: one line per key ID, then the
/// malformed envelopes if there are any.
#[must_use]
pub fn report(status: &VaultStatus) -> String {
    let mut report = String::new();
    for (&key_id, &KeyUsage { configured, counts }) in &status.keys {
        let active = key_id == status.active;
        let line = match (configured, active, counts.is_empty()) {
            (true, true, true) => format!("key {key_id} (active): seals nothing"),
            (true, false, true) => format!(
                "key {key_id}: seals nothing; it can leave [vault] master_key_files (keep its file while backups from before the rotation exist)"
            ),
            (true, true, false) => format!("key {key_id} (active): {}", counts_text(counts)),
            (true, false, false) => format!("key {key_id}: {}", counts_text(counts)),
            (false, _, _) => format!(
                "key {key_id} (not configured): {}; these can't be opened until key {key_id}'s file is back in [vault] master_key_files",
                counts_text(counts)
            ),
        };
        let _ = writeln!(report, "{line}");
    }
    if !status.malformed.is_empty() {
        let total = status
            .malformed
            .totp_secrets
            .saturating_add(status.malformed.refresh_tokens);
        let _ = writeln!(
            report,
            "{}: {}",
            count(total, "malformed envelope"),
            counts_text(status.malformed)
        );
    }
    report
}

/// `1 TOTP secret, 0 Microsoft refresh tokens`.
fn counts_text(counts: PurposeCounts) -> String {
    format!(
        "{}, {}",
        count(counts.totp_secrets, "TOTP secret"),
        count(counts.refresh_tokens, "Microsoft refresh token")
    )
}

/// `n` of `thing`, in the singular for 1.
fn count(n: u64, thing: &str) -> String {
    if n == 1 {
        format!("1 {thing}")
    } else {
        format!("{n} {thing}s")
    }
}

/// A refused rotation: the heading and one line per secret that doesn't
/// open on stderr, and exit 1. The service logged each.
fn refused(err: &mut dyn Write, bad: &[BadEnvelope]) -> Exit {
    let _ = writeln!(
        err,
        "afkfleet-server: {} don't open, so nothing was re-encrypted:",
        count(u64::try_from(bad.len()).unwrap_or(u64::MAX), "secret")
    );
    for envelope in bad {
        let why = match (envelope.reason, envelope.key_id) {
            (BadReason::UnknownKey, Some(key_id)) => format!("key {key_id} isn't configured"),
            (BadReason::Rejected, Some(key_id)) => format!("rejected under key {key_id}"),
            (BadReason::Malformed, _) | (BadReason::UnknownKey | BadReason::Rejected, None) => {
                "malformed".to_owned()
            }
        };
        let _ = writeln!(err, "  {}: {why}", envelope.record);
    }
    Exit::Failed
}

/// Closes `database` within [`DATABASE_CLOSE`], keeping `exit` unless the
/// close fails.
async fn close(database: Database, exit: Exit, err: &mut dyn Write) -> Exit {
    match database.close_within(DATABASE_CLOSE).await {
        Ok(()) => exit,
        Err(error) => fail(err, "the database can't be closed", &error),
    }
}

/// A refusal the service already logged: one line on stderr, and exit 1.
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
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use fleet_core::audit::AuditAction;
    use fleet_core::authz::Role;
    use fleet_core::id::UserId;
    use fleet_core::value::Username;
    use fleet_startup::config::{LogConfig, LogFormat};
    use fleet_startup::telemetry::{NoRules, layer_with};
    use fleet_testkit::log_buffer::LogBuffer;
    use fleet_testkit::system::SeededRandom;
    use secrecy::{SecretBox, SecretString};
    use tempfile::TempDir;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;
    use crate::app::vault::{KeyUsage, PurposeCounts};
    use crate::config::{
        AccountsConfig, AuthConfig, DatabaseConfig, HttpConfig, RateLimitConfig, VaultConfig,
    };
    use crate::http::rate_limit::TrustedProxies;
    use crate::infra::crypto::vault::{VaultKeys, XChaChaVault};
    use crate::infra::sqlite::{self, DatabaseOptions};
    use crate::ports::audit::{AuditLimit, AuditPage, NewAuditEntry};
    use crate::ports::envelopes::SealedRecord;
    use crate::ports::mfa::UserMfa;
    use crate::ports::store::Store as _;
    use crate::ports::users::{NewUser, PasswordHash};
    use crate::ports::vault::{VaultPurpose, key_id_of};

    /// A valid Argon2id PHC string (not a real hash).
    const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";

    fn at(millis: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp_millis(millis).unwrap()
    }

    /// Key `id`'s file in `dir`.
    fn key_file(dir: &Path, id: u32) -> PathBuf {
        dir.join(format!("vault-key-{id}"))
    }

    /// Writes key `id` (`[id; 32]`) into its file, as the docs say: standard
    /// base64 with padding and a line break, owner-only on Unix.
    fn write_key(dir: &Path, id: u8) {
        write_key_bytes(dir, id, id);
    }

    /// Writes `[byte; 32]` into key `id`'s file.
    fn write_key_bytes(dir: &Path, id: u8, byte: u8) {
        let mut buffer = [0_u8; 64];
        let len = STANDARD.encode_slice([byte; 32], &mut buffer).unwrap();
        let path = key_file(dir, u32::from(id));
        let mut text = buffer[..len].to_vec();
        text.push(b'\n');
        std::fs::write(&path, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    /// A config whose database lives in `dir`, listing the key files `ids`
    /// there (written or not), with `active` as the active key.
    fn config(dir: &Path, ids: &[u32], active: u32) -> ServerConfig {
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
                key_files: ids.iter().map(|&id| (id, key_file(dir, id))).collect(),
                active_key_id: active,
            },
            accounts: AccountsConfig::default(),
        }
    }

    /// A vault over the keys `ids` (key `n` is `[n; 32]`), sealing with
    /// `active`.
    fn vault(ids: &[u8], active: u8) -> XChaChaVault {
        let keys = ids
            .iter()
            .map(|&n| (u32::from(n), SecretBox::new(Box::new([n; 32]))))
            .collect::<BTreeMap<_, _>>();
        XChaChaVault::new(
            VaultKeys::from_keys(keys, u32::from(active)).unwrap(),
            Arc::new(SeededRandom::new(1)),
        )
    }

    /// Migrates the database in `dir` and adds, for each `(n, key)`, user `n`
    /// with a TOTP secret sealed under key `key` (`[key; 32]`).
    async fn seed(dir: &Path, users: &[(u8, u8)]) -> Vec<UserId> {
        let database = sqlite::open(
            &DatabaseConfig {
                path: dir.join("afkfleet.db"),
            },
            DatabaseOptions::default(),
        )
        .await
        .unwrap();
        let mut ids = Vec::new();
        for &(n, key) in users {
            let id = UserId::new_v7(at(1_800_000_000_000), [n; 10]).unwrap();
            let envelope = vault(&[key], key)
                .seal(VaultPurpose::TotpSecret, id.into(), &[n; 20])
                .unwrap();
            let mut tx = database.write().await.unwrap();
            tx.users()
                .insert(&NewUser {
                    id,
                    username: Username::try_from(format!("user{n}").as_str()).unwrap(),
                    password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
                    role: Role::Member,
                    created_at: at(1_800_000_000_000),
                })
                .await
                .unwrap();
            tx.mfa()
                .insert(&UserMfa {
                    user_id: id,
                    secret_envelope: envelope,
                    last_used_step: 0,
                    confirmed_at: at(1_800_000_000_000),
                })
                .await
                .unwrap();
            tx.commit(NewAuditEntry::new(
                at(1_800_000_000_000),
                AuditAction::try_from("test.vault_cli_setup").unwrap(),
                fleet_core::audit::AuditOutcome::Success,
            ))
            .await
            .unwrap();
            ids.push(id);
        }
        database.close().await;
        ids
    }

    /// Creates an empty database file in `dir`, owner-only on Unix, as the
    /// server would: the permission check refuses one other users can read.
    fn empty_database(dir: &Path) {
        let path = dir.join("afkfleet.db");
        std::fs::write(&path, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    /// The key every stored envelope names, by row, read straight from the
    /// database.
    async fn sealed_under(dir: &Path) -> BTreeMap<SealedRecord, u32> {
        let database = sqlite::open_migrated(
            &DatabaseConfig {
                path: dir.join("afkfleet.db"),
            },
            DatabaseOptions::default(),
        )
        .await
        .unwrap();
        let sealed = database
            .envelopes()
            .all()
            .await
            .unwrap()
            .into_iter()
            .map(|sealed| (sealed.record, key_id_of(&sealed.envelope).unwrap()))
            .collect();
        database.close().await;
        sealed
    }

    /// How many `vault.rotate_key` entries the log holds.
    async fn rotate_entries(dir: &Path) -> usize {
        let database = sqlite::open_migrated(
            &DatabaseConfig {
                path: dir.join("afkfleet.db"),
            },
            DatabaseOptions::default(),
        )
        .await
        .unwrap();
        let count = database
            .audit()
            .list(AuditPage {
                before: None,
                limit: AuditLimit::new(100).unwrap(),
            })
            .await
            .unwrap()
            .into_iter()
            .filter(|entry| entry.action == AuditAction::VAULT_ROTATE_KEY)
            .count();
        database.close().await;
        count
    }

    /// The `-wal` and `-shm` files next to the database, if any.
    fn leftovers(dir: &Path) -> (bool, bool) {
        (
            dir.join("afkfleet.db-wal").exists(),
            dir.join("afkfleet.db-shm").exists(),
        )
    }

    /// The outcome and what went to stdout and stderr.
    struct Run {
        exit: Exit,
        out: String,
        err: String,
    }

    async fn run_status(config: &ServerConfig) -> Run {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let exit = status(config, &mut out, &mut err).await;
        Run {
            exit,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    async fn run_rotate(config: &ServerConfig) -> Run {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let exit = rotate_key(config, &mut out, &mut err).await;
        Run {
            exit,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    /// The server's log layer at `info`, writing into a buffer, as this
    /// thread's default subscriber.
    fn capture() -> (LogBuffer, tracing::subscriber::DefaultGuard) {
        let buffer = LogBuffer::default();
        let config = LogConfig {
            format: LogFormat::Json,
            filter: "info".parse().unwrap(),
        };
        let subscriber = tracing_subscriber::registry().with(layer_with(
            &config,
            &NoRules,
            false,
            buffer.clone(),
        ));
        (buffer, tracing::subscriber::set_default(subscriber))
    }

    #[test]
    fn each_exit_has_its_code() {
        assert_eq!(Exit::Done.code(), 0);
        assert_eq!(Exit::Failed.code(), 1);
        assert_eq!(Exit::Unhealthy.code(), 3);
    }

    // --- The report ------------------------------------------------------------

    fn usage(configured: bool, totp_secrets: u64, refresh_tokens: u64) -> KeyUsage {
        KeyUsage {
            configured,
            counts: PurposeCounts {
                totp_secrets,
                refresh_tokens,
            },
        }
    }

    #[test]
    fn the_report_names_every_case_of_a_key() {
        let status = VaultStatus {
            active: 2,
            keys: BTreeMap::from([
                (1, usage(true, 0, 0)),
                (2, usage(true, 1, 0)),
                (3, usage(true, 2, 1)),
                (4, usage(false, 1, 2)),
            ]),
            malformed: PurposeCounts {
                totp_secrets: 1,
                refresh_tokens: 1,
            },
        };

        insta::assert_snapshot!(report(&status));
    }

    #[test]
    fn the_report_of_a_fresh_vault_shows_the_active_key_sealing_nothing() {
        let status = VaultStatus {
            active: 1,
            keys: BTreeMap::from([(1, usage(true, 0, 0))]),
            malformed: PurposeCounts::default(),
        };

        assert_eq!(report(&status), "key 1 (active): seals nothing\n");
    }

    // --- vault status ------------------------------------------------------------

    // Real time: the database runs on sqlx's worker threads (CLAUDE.md).
    #[tokio::test]
    async fn status_never_creates_a_missing_database() {
        let dir = TempDir::new().unwrap();

        let run = run_status(&config(dir.path(), &[1], 1)).await;

        assert_eq!(run.exit, Exit::Failed);
        assert_eq!(run.out, "");
        assert!(
            run.err
                .starts_with("afkfleet-server: the database can't be opened: the database ")
                && run
                    .err
                    .ends_with(" doesn't exist, and this command never creates one\n"),
            "{}",
            run.err
        );
        assert!(!dir.path().join("afkfleet.db").exists());
    }

    #[tokio::test]
    async fn status_refuses_pending_migrations_and_changes_nothing() {
        let dir = TempDir::new().unwrap();
        empty_database(dir.path());

        let run = run_status(&config(dir.path(), &[1], 1)).await;

        assert_eq!(run.exit, Exit::Failed);
        assert_eq!(
            run.err,
            "afkfleet-server: the database can't be opened: the database has pending migrations (the first is 1); run `afkfleet-server migrate` first\n"
        );
        assert_eq!(std::fs::read(dir.path().join("afkfleet.db")).unwrap(), b"");
    }

    #[tokio::test]
    async fn status_reads_no_key_file_and_reports_each_key() {
        let dir = TempDir::new().unwrap();
        seed(dir.path(), &[(1, 1), (2, 2)]).await;

        // No key file exists: status never reads one.
        let run = run_status(&config(dir.path(), &[1, 2], 2)).await;

        assert_eq!(run.exit, Exit::Done, "{}", run.err);
        assert_eq!(
            run.out,
            "key 1: 1 TOTP secret, 0 Microsoft refresh tokens\n\
             key 2 (active): 1 TOTP secret, 0 Microsoft refresh tokens\n"
        );
        assert_eq!(run.err, "");
        assert_eq!(leftovers(dir.path()), (false, false));
    }

    #[tokio::test]
    async fn status_exits_3_when_a_secret_names_a_key_that_isnt_configured() {
        let dir = TempDir::new().unwrap();
        seed(dir.path(), &[(1, 1), (2, 2)]).await;

        let run = run_status(&config(dir.path(), &[2], 2)).await;

        assert_eq!(run.exit, Exit::Unhealthy);
        assert!(
            run.out.starts_with("key 1 (not configured): 1 TOTP secret"),
            "{}",
            run.out
        );
    }

    // --- vault rotate-key ------------------------------------------------------------

    #[tokio::test]
    async fn rotate_key_refuses_pending_migrations_before_reading_a_key() {
        let dir = TempDir::new().unwrap();
        empty_database(dir.path());

        // Key 1's file doesn't exist: reading it would fail differently.
        let run = run_rotate(&config(dir.path(), &[1], 1)).await;

        assert_eq!(run.exit, Exit::Failed);
        assert_eq!(
            run.err,
            "afkfleet-server: the database can't be opened: the database has pending migrations (the first is 1); run `afkfleet-server migrate` first\n"
        );
    }

    #[tokio::test]
    async fn rotate_key_with_a_missing_key_file_changes_nothing() {
        let dir = TempDir::new().unwrap();
        let users = seed(dir.path(), &[(1, 1)]).await;
        write_key(dir.path(), 1);

        let run = run_rotate(&config(dir.path(), &[1, 2], 2)).await;

        assert_eq!(run.exit, Exit::Failed);
        assert!(
            run.err
                .starts_with("afkfleet-server: the vault's keys can't be loaded: vault key 2: "),
            "{}",
            run.err
        );
        assert_eq!(
            sealed_under(dir.path()).await,
            BTreeMap::from([(SealedRecord::TotpSecret(users[0]), 1)])
        );
    }

    #[tokio::test]
    async fn rotate_key_re_encrypts_reports_and_logs() {
        let dir = TempDir::new().unwrap();
        let users = seed(dir.path(), &[(1, 1), (2, 2)]).await;
        write_key(dir.path(), 1);
        write_key(dir.path(), 2);
        let (logs, _guard) = capture();

        let run = run_rotate(&config(dir.path(), &[1, 2], 2)).await;

        assert_eq!(run.exit, Exit::Done, "{}", run.err);
        assert_eq!(
            run.out,
            "re-encrypted 1 TOTP secret and 0 Microsoft refresh tokens under key 2\n\
             key 1: seals nothing; it can leave [vault] master_key_files (keep its file while backups from before the rotation exist)\n\
             key 2 (active): 2 TOTP secrets, 0 Microsoft refresh tokens\n"
        );
        assert_eq!(run.err, "");
        assert_eq!(
            sealed_under(dir.path()).await,
            BTreeMap::from([
                (SealedRecord::TotpSecret(users[0]), 2),
                (SealedRecord::TotpSecret(users[1]), 2),
            ])
        );
        assert_eq!(rotate_entries(dir.path()).await, 1);
        let done = logs
            .lines_with("the vault's secrets are re-encrypted under the active key")
            .unwrap();
        assert_eq!(done.len(), 1, "{}", logs.text());
        assert_eq!(done[0]["level"], "INFO");
        assert_eq!(leftovers(dir.path()), (false, false));
    }

    #[tokio::test]
    async fn rotate_key_with_nothing_under_another_key_writes_nothing() {
        let dir = TempDir::new().unwrap();
        seed(dir.path(), &[(1, 2)]).await;
        write_key(dir.path(), 1);
        write_key(dir.path(), 2);

        let run = run_rotate(&config(dir.path(), &[1, 2], 2)).await;

        assert_eq!(run.exit, Exit::Done, "{}", run.err);
        assert!(
            run.out.starts_with(
                "nothing to re-encrypt: no secret is sealed under another key than key 2 (active)\n"
            ),
            "{}",
            run.out
        );
        assert_eq!(rotate_entries(dir.path()).await, 0);
    }

    #[tokio::test]
    async fn a_refused_rotation_lists_each_secret_that_doesnt_open() {
        let dir = TempDir::new().unwrap();
        let users = seed(dir.path(), &[(1, 1), (2, 3)]).await;
        // Key 1's file holds another key than the one user 1's secret was
        // sealed with.
        write_key_bytes(dir.path(), 1, 9);
        write_key(dir.path(), 2);
        let (logs, _guard) = capture();

        let run = run_rotate(&config(dir.path(), &[1, 2], 2)).await;

        assert_eq!(run.exit, Exit::Failed);
        assert_eq!(run.out, "");
        assert_eq!(
            run.err,
            format!(
                "afkfleet-server: 2 secrets don't open, so nothing was re-encrypted:\n  \
                 user_mfa {}: rejected under key 1\n  \
                 user_mfa {}: key 3 isn't configured\n",
                users[0], users[1]
            )
        );
        assert_eq!(rotate_entries(dir.path()).await, 2);
        let refused = logs
            .lines_with("a secret doesn't open, so nothing was re-encrypted")
            .unwrap();
        assert_eq!(refused.len(), 2, "{}", logs.text());
        assert_eq!(refused[0]["level"], "ERROR");
    }
}
