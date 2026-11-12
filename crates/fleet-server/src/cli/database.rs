//! `afkfleet-server database check` (Plan.md P13.5, ADR-0033): SQLite's own
//! integrity check over the database file, from the server's host, beside a
//! running `serve` or with it stopped. What an operator asks after a crash,
//! a full disk or a restore.
//!
//! - **It reads and changes nothing:** a read-only connection of its own
//!   ([`integrity::check`]). The database is never created or migrated, and
//!   the vault's keys are never read.
//! - **Its output:** one line on stdout when the database is sound. Damage is
//!   one line on stderr saying so, then SQLite's findings, one a line, which
//!   name tables, indexes, pages and row numbers, never a row's content; and
//!   one `error` in the log with their count.
//! - **Damage is exit 3,** apart from a check that couldn't run (exit 1), so
//!   a script can tell "restore a backup" from "try again".

use std::io::Write;

use tracing::{error, info};

use crate::config::ServerConfig;
use crate::http::error::chain;
use crate::infra::sqlite::DatabaseOptions;
use crate::infra::sqlite::integrity::{self, Integrity};

/// The exit codes of `afkfleet-server database check`, shown in its help.
pub const CHECK_EXIT_CODES: &str = "\
Exit codes:
  0  the database passes SQLite's integrity check
  1  it couldn't be checked: the config, logging, a missing database, or a
     file that can't be read
  2  usage error
  3  the database is damaged: SQLite's findings are on stderr; restore a
     backup (docs/runbook.md)";

/// What stdout says when the database is sound.
pub const SOUND: &str = "the database passes SQLite's integrity check";

/// How `afkfleet-server database check` ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// SQLite found nothing.
    Sound,
    /// The check couldn't run; stderr (and the log) says why.
    Failed,
    /// SQLite found damage; stderr lists it.
    Damaged,
}

impl Exit {
    /// The process's exit code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Sound => 0,
            Self::Failed => 1,
            Self::Damaged => 3,
        }
    }
}

/// Checks the database `config` names; see the [module docs](self). The
/// verdict of a sound database goes to `out`, everything else to `err`.
pub async fn check(config: &ServerConfig, out: &mut dyn Write, err: &mut dyn Write) -> Exit {
    match integrity::check(&config.database, &DatabaseOptions::default()).await {
        Ok(Integrity::Sound) => {
            info!("the database is sound");
            let _ = writeln!(out, "{SOUND}");
            Exit::Sound
        }
        Ok(Integrity::Damaged(findings)) => {
            let problems = findings.len();
            error!(problems, "the database is damaged");
            let _ = writeln!(
                err,
                "afkfleet-server: the database is damaged: SQLite's integrity check found {problems} problems; restore a backup"
            );
            for finding in findings.iter().flat_map(|finding| finding.lines()) {
                let _ = writeln!(err, "  {finding}");
            }
            Exit::Damaged
        }
        Err(failure) => {
            let chain = chain(&failure);
            error!(error = %chain, "the database can't be checked");
            let _ = writeln!(
                err,
                "afkfleet-server: the database can't be checked: {chain}"
            );
            Exit::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::time::Duration;

    use fleet_startup::config::{LogConfig, LogFormat};
    use fleet_startup::telemetry::{NoRules, layer_with};
    use fleet_testkit::log_buffer::LogBuffer;
    use tempfile::TempDir;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;
    use crate::config::{
        AccountsConfig, AuditConfig, AuthConfig, ChatConfig, DatabaseConfig, HttpConfig,
        RateLimitConfig, VaultConfig,
    };
    use crate::http::rate_limit::TrustedProxies;
    use crate::infra::sqlite;

    /// A config whose database lives in `dir`. The vault key is never read.
    fn config(dir: &Path) -> ServerConfig {
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
                backups: None,
            },
            log: LogConfig::default(),
            auth: AuthConfig::default(),
            rate_limits: RateLimitConfig::default(),
            vault: VaultConfig {
                key_files: BTreeMap::from([(1, dir.join("never-read"))]),
                active_key_id: 1,
            },
            accounts: AccountsConfig::default(),
            chat: ChatConfig::default(),
            audit: AuditConfig::default(),
            grpc: None,
            metrics: None,
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

    struct Run {
        exit: Exit,
        out: String,
        err: String,
    }

    async fn run(config: &ServerConfig) -> Run {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let exit = check(config, &mut out, &mut err).await;
        Run {
            exit,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    async fn migrated(dir: &Path) -> ServerConfig {
        let config = config(dir);
        sqlite::open(&config.database, DatabaseOptions::default())
            .await
            .unwrap()
            .close()
            .await;
        config
    }

    #[test]
    fn each_exit_has_its_code() {
        assert_eq!(Exit::Sound.code(), 0);
        assert_eq!(Exit::Failed.code(), 1);
        assert_eq!(Exit::Damaged.code(), 3);
    }

    // Real time: the database runs on sqlx's worker threads (CLAUDE.md).
    #[tokio::test]
    async fn a_sound_database_is_one_line_on_stdout_and_exit_0() {
        let dir = TempDir::new().unwrap();
        let config = migrated(dir.path()).await;
        let (logs, _guard) = capture();

        let run = run(&config).await;

        assert_eq!(run.exit, Exit::Sound, "{}", run.err);
        assert_eq!(run.out, format!("{SOUND}\n"));
        assert_eq!(run.err, "");
        let lines = logs.lines_with("the database is sound").unwrap();
        assert_eq!(lines.len(), 1, "{}", logs.text());
        assert_eq!(lines[0]["level"], "INFO");
    }

    #[tokio::test]
    async fn damage_is_exit_3_with_sqlite_s_findings_on_stderr_and_one_error_line() {
        let dir = TempDir::new().unwrap();
        let config = migrated(dir.path()).await;
        let mut bytes = std::fs::read(&config.database.path).unwrap();
        bytes[4096 * 3..4096 * 7].fill(0xAA);
        std::fs::write(&config.database.path, bytes).unwrap();
        let (logs, _guard) = capture();

        let run = run(&config).await;

        assert_eq!(run.exit, Exit::Damaged);
        assert_eq!(run.out, "");
        let mut lines = run.err.lines();
        let first = lines.next().unwrap();
        let findings: Vec<&str> = lines.collect();
        assert_ne!(findings.len(), 0, "{}", run.err);
        assert_eq!(
            first,
            format!(
                "afkfleet-server: the database is damaged: SQLite's integrity check found {} problems; restore a backup",
                findings.len()
            )
        );
        assert!(
            findings
                .iter()
                .all(|line| line.starts_with("  ") && line.trim() != ""),
            "{}",
            run.err
        );
        let logged = logs.lines_with("the database is damaged").unwrap();
        assert_eq!(logged.len(), 1, "{}", logs.text());
        assert_eq!(logged[0]["level"], "ERROR");
        assert_eq!(logged[0]["problems"], findings.len());
    }

    #[tokio::test]
    async fn a_missing_database_is_exit_1_and_nothing_is_created() {
        let dir = TempDir::new().unwrap();
        let config = config(dir.path());
        let (logs, _guard) = capture();

        let run = run(&config).await;

        assert_eq!(run.exit, Exit::Failed);
        assert_eq!(run.out, "");
        assert!(
            run.err.starts_with(
                "afkfleet-server: the database can't be checked: the database can't be opened: the database "
            ) && run
                .err
                .contains(" doesn't exist, and this command never creates one: "),
            "{}",
            run.err
        );
        assert_eq!(run.err.lines().count(), 1);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        let logged = logs.lines_with("the database can't be checked").unwrap();
        assert_eq!(logged.len(), 1, "{}", logs.text());
        assert_eq!(logged[0]["level"], "ERROR");
    }
}
