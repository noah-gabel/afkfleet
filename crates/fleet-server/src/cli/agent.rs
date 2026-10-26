//! `afkfleet-server agent …` (Plan.md P10.3, P10.10; ADR-0019, U3, U8,
//! S31): the server's host operator manages agents without the API, until
//! the app's agents screen (P11.9), and beside it afterwards. Each command
//! opens the database with [`sqlite::open_migrated`]: never created or
//! migrated, so it's safe while `serve` runs, its writes waiting for the
//! server's on SQLite's lock.
//!
//! `agent list` prints every agent, newest first, one tab-separated line
//! each after a header ([`LIST_HEADER`]), `-` for nothing. Its state is
//! `enabled`, `expired` or `disabled`: the host can't see the streams.
//!
//! `agent disable --id …` disables an agent for good, as the API does: every
//! certificate revoked, its name freed, `agent.disable` with `via` = `cli`
//! and no actor. It works without `[grpc]`. A running server ends the
//! agent's stream within one 5 s sweep, or two when the disable lands as a
//! stream is being welcomed (U8). An agent disabled already is
//! no change and no entry.
//!
//! `agent release --id …` releases a disabled agent's bots (S32, R25): one
//! transaction clears every bot assigned to it and every release record
//! naming it, with a `bot.detach` per bot and `agent.release` with `via` =
//! `cli`, no actor. It's refused while the agent is enabled. The operator
//! vouches that the agent's host and its bots are stopped: nothing can
//! prove it. A running server places the bots on its next pass, within
//! 30 s. An agent that holds no bot is no change and no entry.
//!
//! `agent create-enrollment-token`:
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

use core::fmt::Write as _;
use std::io::Write;
use std::sync::Arc;

use chrono::{DateTime, SecondsFormat, Utc};
use fleet_core::agent::AgentStatus;
use fleet_core::id::AgentId;
use fleet_core::system::{Clock, SecureRandom};
use secrecy::ExposeSecret as _;
use tracing::{error, warn};

use super::DATABASE_CLOSE;
use crate::app::agents::{
    AgentService, AgentView, DisableAgentError, DisabledAgent, Disabler, ReleaseAgentError,
    ReleasedAgent, Releaser,
};
use crate::app::enrollment::{
    CreateTokenError, CreatedEnrollmentToken, Creator, EnrollmentTokenService,
};
use crate::config::ServerConfig;
use crate::grpc::registry::AgentRegistry;
use crate::http::error::chain;
use crate::infra::crypto::tokens::Sha256Tokens;
use crate::infra::pki::ca_fingerprint;
use crate::infra::sqlite::{self, Database, DatabaseOptions};
use crate::ports::page::{Page, PageLimit};

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

/// The exit codes of `afkfleet-server agent list`, shown in its help.
pub const LIST_EXIT_CODES: &str = "\
Exit codes:
  0  the agents are listed on stdout
  1  nothing was listed: the config, logging or the database failed (a
     missing database and pending migrations included)
  2  usage error";

/// The exit codes of `afkfleet-server agent disable`, shown in its help.
pub const DISABLE_EXIT_CODES: &str = "\
Exit codes:
  0  the agent is disabled, now or before: every certificate it held is
     revoked, and a running server ends its stream within 10 s
  1  nothing was disabled: no agent has this ID, or the config, logging or
     the database failed (a missing database and pending migrations
     included)
  2  usage error";

/// The exit codes of `afkfleet-server agent release`, shown in its help.
pub const RELEASE_EXIT_CODES: &str = "\
Exit codes:
  0  the agent's bots are released, or it held none; a running server
     places them within 30 s
  1  nothing was released: no agent has this ID, the agent is enabled
     (disable it first), or the config, logging or the database failed (a
     missing database and pending migrations included)
  2  usage error";

/// The first line of `agent list`'s output: its columns.
pub const LIST_HEADER: &str =
    "id\tname\tstate\tbots\tcapacity\tversion\tlast_seen_at\tnot_after\tcreated_at";

/// How the command ended, which decides the process's exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The token is created, and its bundle printed.
    Created,
    /// The list or the disable is done, and its result printed.
    Done,
    /// Nothing was done; stderr (and the log) says why.
    Failed,
}

impl Exit {
    /// The process's exit code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Created | Self::Done => 0,
            Self::Failed => 1,
        }
    }
}

/// `afkfleet-server agent list`: prints every agent on `out`, with `clock`'s
/// time deciding what has expired; see the [module docs](self).
pub async fn list(
    config: &ServerConfig,
    clock: Arc<dyn Clock>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Exit {
    let database = match sqlite::open_migrated(&config.database, DatabaseOptions::default()).await {
        Ok(database) => database,
        Err(error) => return fail(err, "the database can't be opened", &error),
    };
    let agents = agent_service(&database, clock);
    let exit = match all_agents(&agents).await {
        Ok(views) => print_list(&views, out, err),
        Err(error) => fail(err, "the database failed", &error),
    };
    close(database, exit, err).await
}

/// The agents' service over `database`: the host sees no stream, so its
/// registry stays empty.
fn agent_service(database: &Database, clock: Arc<dyn Clock>) -> AgentService {
    let registry = AgentRegistry::new(Arc::clone(&clock));
    AgentService::new(clock, Arc::new(database.clone()), Arc::new(registry))
}

/// Every agent, newest first, a page at a time.
async fn all_agents(
    agents: &AgentService,
) -> Result<Vec<AgentView>, crate::ports::store::StoreError> {
    let mut views = Vec::new();
    let mut before = None;
    loop {
        let page = Page {
            before,
            limit: PageLimit::new(PageLimit::MAX.into())
                .map_err(|error| crate::ports::store::StoreError::Backend(Box::new(error)))?,
        };
        let listed = agents.list(page).await?;
        views.extend(listed.items);
        match listed.next {
            Some(next) => before = Some(next),
            None => return Ok(views),
        }
    }
}

/// Prints `views` as [`LIST_HEADER`]'s lines on `out`.
fn print_list(views: &[AgentView], out: &mut dyn Write, err: &mut dyn Write) -> Exit {
    let mut text = format!("{LIST_HEADER}\n");
    for view in views {
        let agent = &view.listed.agent;
        let state = match view.status {
            AgentStatus::Disabled => "disabled",
            AgentStatus::Expired => "expired",
            AgentStatus::Online | AgentStatus::Offline => "enabled",
        };
        // Writing to a String can't fail.
        let _ = writeln!(
            text,
            "{}\t{}\t{state}\t{}\t{}\t{}\t{}\t{}\t{}",
            agent.id,
            agent.name,
            view.listed.bots,
            agent
                .capacity
                .map_or_else(|| "-".to_owned(), |capacity| capacity.get().to_string()),
            agent
                .version
                .as_ref()
                .map_or("-", |version| version.as_str()),
            time(view.last_seen_at),
            time(view.listed.not_after),
            time(Some(agent.created_at)),
        );
    }
    if let Err(error) = out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        return fail(err, "the list couldn't be written to stdout", &error);
    }
    Exit::Done
}

/// `at` in the list's form, or `-`.
fn time(at: Option<DateTime<Utc>>) -> String {
    at.map_or_else(
        || "-".to_owned(),
        |at| at.to_rfc3339_opts(SecondsFormat::Secs, true),
    )
}

/// `afkfleet-server agent disable --id …`: disables agent `id` at
/// `clock`'s time, and says on `out` what changed; see the [module
/// docs](self).
pub async fn disable(
    config: &ServerConfig,
    clock: Arc<dyn Clock>,
    id: AgentId,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Exit {
    let database = match sqlite::open_migrated(&config.database, DatabaseOptions::default()).await {
        Ok(database) => database,
        Err(error) => return fail(err, "the database can't be opened", &error),
    };
    let agents = agent_service(&database, clock);
    let exit = match agents.disable(Disabler::Host, id).await {
        Ok(disabled) => print_disabled(&disabled, out, err),
        Err(DisableAgentError::NotFound) => refuse(err, &format!("no agent has the ID {id}")),
        Err(error) => fail(err, "the agent couldn't be disabled", &error),
    };
    close(database, exit, err).await
}

/// Says on `out` what disabling `disabled` changed.
fn print_disabled(disabled: &DisabledAgent, out: &mut dyn Write, err: &mut dyn Write) -> Exit {
    let agent = &disabled.view.listed.agent;
    let line = match disabled.revoked {
        Some(revoked) => format!(
            "The agent {} ({}) is disabled: {revoked} certificate{} revoked. A running server ends its stream within 10 s.",
            agent.name,
            agent.id,
            if revoked == 1 { "" } else { "s" }
        ),
        None => format!(
            "The agent {} ({}) was disabled already: nothing changed.",
            agent.name, agent.id
        ),
    };
    if let Err(error) = writeln!(out, "{line}").and_then(|()| out.flush()) {
        return fail(err, "the result couldn't be written to stdout", &error);
    }
    Exit::Done
}

/// `afkfleet-server agent release --id …`: releases the disabled agent
/// `id`'s bots, and says on `out` how many; see the [module docs](self).
pub async fn release(
    config: &ServerConfig,
    clock: Arc<dyn Clock>,
    id: AgentId,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Exit {
    let database = match sqlite::open_migrated(&config.database, DatabaseOptions::default()).await {
        Ok(database) => database,
        Err(error) => return fail(err, "the database can't be opened", &error),
    };
    let agents = agent_service(&database, clock);
    let exit = released_said(id, agents.release(Releaser::Host, id).await, out, err);
    close(database, exit, err).await
}

/// What `agent release` says of `outcome`, the release of agent `id`'s bots,
/// on `out` and `err`, and its exit.
fn released_said(
    id: AgentId,
    outcome: Result<ReleasedAgent, ReleaseAgentError>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Exit {
    match outcome {
        Ok(released) => print_released(id, released.bots.len(), out, err),
        Err(ReleaseAgentError::NotFound) => refuse(err, &format!("no agent has the ID {id}")),
        Err(ReleaseAgentError::Enabled) => refuse(
            err,
            &format!("the agent {id} is enabled: disable it first, and stop its host and its bots"),
        ),
        // Committed already: said as done, and the failed read after it.
        Err(ReleaseAgentError::ReleasedUnread { bots, error }) => {
            let exit = print_released(id, bots.len(), out, err);
            let chain = chain(&error);
            warn!(error = %chain, "the agent couldn't be read again after its bots were released");
            let _ = writeln!(
                err,
                "afkfleet-server: the bots were released, but the agent couldn't be read again: {chain}"
            );
            exit
        }
        Err(error) => fail(err, "the agent's bots couldn't be released", &error),
    }
}

/// Says on `out` how many of agent `id`'s bots were released.
fn print_released(id: AgentId, released: usize, out: &mut dyn Write, err: &mut dyn Write) -> Exit {
    let line = match released {
        0 => format!("The agent {id} holds no bots: nothing changed."),
        1 => format!("Released 1 bot of the agent {id}: a running server places it within 30 s."),
        n => format!(
            "Released {n} bots of the agent {id}: a running server places them within 30 s."
        ),
    };
    if let Err(error) = writeln!(out, "{line}").and_then(|()| out.flush()) {
        return fail(err, "the result couldn't be written to stdout", &error);
    }
    Exit::Done
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
    use fleet_core::authz::Role;
    use fleet_core::enrollment::{EnrollmentBundle, MAX_PENDING_TOKENS};
    use fleet_core::id::{AccountId, BotId, UserId};
    use fleet_core::mode::ModePreset;
    use fleet_core::pki::CertFingerprint;
    use fleet_core::value::Username;
    use fleet_startup::config::{LogConfig, LogFormat};
    use fleet_startup::telemetry::{NoRules, layer_with};
    use fleet_testkit::log_buffer::LogBuffer;
    use fleet_testkit::system::{ManualClock, SeededRandom};
    use tempfile::TempDir;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;
    use crate::config::{
        AccountsConfig, AuditConfig, AuthConfig, ChatConfig, DatabaseConfig, GrpcConfig,
        HttpConfig, PkiPaths, RateLimitConfig, VaultConfig,
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
            chat: ChatConfig::default(),
            audit: AuditConfig::default(),
            grpc: grpc.then(|| GrpcConfig {
                bind: GrpcConfig::DEFAULT_BIND,
                pki: PkiPaths {
                    ca_cert_file: dir.join("ca.crt"),
                    ca_key_file: dir.join("no-such-ca.key"),
                    server_cert_file: dir.join("server.crt"),
                    server_key_file: dir.join("server.key"),
                },
                agent_cert_lifetime: chrono::TimeDelta::days(60),
                heartbeats: crate::config::Heartbeats::default(),
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
    // --- agent list and agent disable (P10.10) ------------------------------------------

    /// Agent `n` (`agent-<n>`), enrolled at `NOW` with a certificate valid
    /// `days`, in `database`.
    async fn enroll(database: &Database, n: u8, days: i64) -> AgentId {
        use crate::ports::agents::{NewAgent, NewAgentCert};
        let id = AgentId::new_v7(at(NOW), [n; 10]).unwrap();
        let mut tx = database.write().await.unwrap();
        tx.agents()
            .insert(&NewAgent {
                id,
                name: fleet_core::value::AgentName::try_from(format!("agent-{n}").as_str())
                    .unwrap(),
                created_at: at(NOW),
            })
            .await
            .unwrap();
        tx.agents()
            .insert_cert(&NewAgentCert {
                fingerprint: CertFingerprint::from_digest([n; 32]),
                agent: id,
                not_after: at(NOW) + chrono::TimeDelta::days(days),
                created_at: at(NOW),
            })
            .await
            .unwrap();
        tx.commit(crate::ports::audit::NewAuditEntry::new(
            at(NOW),
            AuditAction::try_from("test.agents_setup").unwrap(),
            AuditOutcome::Success,
        ))
        .await
        .unwrap();
        id
    }

    async fn open(dir: &Path) -> Database {
        sqlite::open_migrated(&database_config(dir), DatabaseOptions::default())
            .await
            .unwrap()
    }

    /// Runs `agent list` over `config` at `now`.
    async fn run_list(config: &ServerConfig, now: i64) -> Ran {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let exit = list(
            config,
            Arc::new(ManualClock::new(at(now))),
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

    /// Runs `agent disable --id id` over `config`.
    async fn run_disable(config: &ServerConfig, id: AgentId) -> Ran {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let exit = disable(
            config,
            Arc::new(ManualClock::new(at(NOW))),
            id,
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

    #[tokio::test]
    async fn the_list_is_a_header_and_a_tab_separated_line_per_agent() {
        let dir = TempDir::new().unwrap();
        migrate(dir.path()).await;
        let database = open(dir.path()).await;
        let heard = enroll(&database, 1, 60).await;
        let expiring = enroll(&database, 2, 1).await;
        let disabled = enroll(&database, 3, 60).await;
        let mut tx = database.write().await.unwrap();
        tx.agents()
            .record_hello(&crate::ports::agents::AgentHello {
                agent: heard,
                capacity: fleet_core::value::AgentCapacity::try_from(10_u32).unwrap(),
                version: fleet_core::value::AgentVersion::try_from("0.1.0").unwrap(),
                at: at(NOW + 1_000),
            })
            .await
            .unwrap();
        assert!(tx.agents().disable(disabled).await.unwrap());
        tx.commit(crate::ports::audit::NewAuditEntry::new(
            at(NOW),
            AuditAction::try_from("test.agents_setup").unwrap(),
            AuditOutcome::Success,
        ))
        .await
        .unwrap();
        database.close().await;

        // Two days on: agent-2's certificate has expired.
        let ran = run_list(&config(dir.path(), false), NOW + 2 * 86_400_000).await;

        assert_eq!(ran.exit, Exit::Done, "{}", ran.err);
        assert_eq!(
            ran.out,
            format!(
                "{LIST_HEADER}\n\
                 {disabled}\tagent-3\tdisabled\t0\t-\t-\t-\t2027-03-16T08:00:00Z\t2027-01-15T08:00:00Z\n\
                 {expiring}\tagent-2\texpired\t0\t-\t-\t-\t2027-01-16T08:00:00Z\t2027-01-15T08:00:00Z\n\
                 {heard}\tagent-1\tenabled\t0\t10\t0.1.0\t2027-01-15T08:00:01Z\t2027-03-16T08:00:00Z\t2027-01-15T08:00:00Z\n"
            )
        );
        assert_eq!(ran.err, "");
    }

    #[tokio::test]
    async fn an_empty_list_is_its_header() {
        let dir = TempDir::new().unwrap();
        migrate(dir.path()).await;

        let ran = run_list(&config(dir.path(), true), NOW).await;

        assert_eq!(ran.exit, Exit::Done, "{}", ran.err);
        assert_eq!(ran.out, format!("{LIST_HEADER}\n"));
    }

    #[tokio::test]
    async fn neither_command_creates_a_missing_database() {
        let dir = TempDir::new().unwrap();
        let config = config(dir.path(), false);

        let listed = run_list(&config, NOW).await;
        let disabled = run_disable(&config, AgentId::new_v7(at(NOW), [1; 10]).unwrap()).await;

        for ran in [&listed, &disabled] {
            assert_eq!(ran.exit, Exit::Failed);
            assert_eq!(ran.out, "");
            assert!(
                ran.err
                    .starts_with("afkfleet-server: the database can't be opened: "),
                "{}",
                ran.err
            );
        }
        assert!(!dir.path().join("afkfleet.db").exists());
    }

    #[tokio::test]
    async fn disable_revokes_records_via_cli_and_says_what_changed() {
        let dir = TempDir::new().unwrap();
        migrate(dir.path()).await;
        let database = open(dir.path()).await;
        let agent = enroll(&database, 1, 60).await;
        database.close().await;
        // Without [grpc]: an agent enrolled before is still disabled.
        let config = config(dir.path(), false);

        let first = run_disable(&config, agent).await;
        let again = run_disable(&config, agent).await;

        assert_eq!(first.exit, Exit::Done, "{}", first.err);
        assert_eq!(
            first.out,
            format!(
                "The agent agent-1 ({agent}) is disabled: 1 certificate revoked. A running server ends its stream within 10 s.\n"
            )
        );
        assert_eq!(again.exit, Exit::Done, "{}", again.err);
        assert_eq!(
            again.out,
            format!("The agent agent-1 ({agent}) was disabled already: nothing changed.\n")
        );
        let database = open(dir.path()).await;
        let entries: Vec<_> = audit(&database)
            .await
            .into_iter()
            .filter(|entry| entry.action == AuditAction::AGENT_DISABLE)
            .collect();
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].actor, None);
        assert_eq!(entries[0].ip, None);
        assert_eq!(
            entries[0].target,
            Some(RecordedTarget::Known(AuditTarget::Agent(agent)))
        );
        database.close().await;
    }

    #[tokio::test]
    async fn disabling_an_unknown_agent_is_exit_1() {
        let dir = TempDir::new().unwrap();
        migrate(dir.path()).await;
        let unknown = AgentId::new_v7(at(NOW), [9; 10]).unwrap();

        let ran = run_disable(&config(dir.path(), true), unknown).await;

        assert_eq!(ran.exit, Exit::Failed);
        assert_eq!(ran.out, "");
        assert_eq!(
            ran.err,
            format!("afkfleet-server: no agent has the ID {unknown}\n")
        );
    }

    /// Runs `agent release --id id` over `config`.
    async fn run_release(config: &ServerConfig, id: AgentId) -> Ran {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let exit = release(
            config,
            Arc::new(ManualClock::new(at(NOW))),
            id,
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

    /// User `n`'s offline account `AfkBot<n>` and its bot, assigned to
    /// `agent`.
    async fn bot_on(database: &Database, agent: AgentId, n: u8) -> BotId {
        use crate::ports::accounts::{AccountCredential, McAccount};
        use crate::ports::bots::NewBot;
        use crate::ports::users::{NewUser, PasswordHash};
        let user = UserId::new_v7(at(NOW), [n; 10]).unwrap();
        let account = AccountId::new_v7(at(NOW), [n; 10]).unwrap();
        let bot = BotId::new_v7(at(NOW), [n; 10]).unwrap();
        let mut tx = database.write().await.unwrap();
        tx.users()
            .insert(&NewUser {
                id: user,
                username: Username::try_from(format!("user{n}").as_str()).unwrap(),
                password_hash: PasswordHash::new(secrecy::SecretString::from(
                    "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g",
                ))
                .unwrap(),
                role: Role::Member,
                created_at: at(NOW),
            })
            .await
            .unwrap();
        tx.accounts()
            .insert(&McAccount {
                id: account,
                owner: user,
                name: format!("AfkBot{n}").parse().unwrap(),
                created_at: at(NOW),
                credential: AccountCredential::Offline,
            })
            .await
            .unwrap();
        tx.bots()
            .insert(&NewBot {
                id: bot,
                account,
                mode: ModePreset::Afk.id(),
            })
            .await
            .unwrap();
        assert!(tx.bots().assign(bot, agent).await.unwrap());
        tx.commit(crate::ports::audit::NewAuditEntry::new(
            at(NOW),
            AuditAction::try_from("test.agents_setup").unwrap(),
            AuditOutcome::Success,
        ))
        .await
        .unwrap();
        bot
    }

    #[tokio::test]
    async fn release_frees_a_disabled_agents_bots_records_via_cli_and_says_how_many() {
        let dir = TempDir::new().unwrap();
        migrate(dir.path()).await;
        let database = open(dir.path()).await;
        let agent = enroll(&database, 1, 60).await;
        let bots = [
            bot_on(&database, agent, 1).await,
            bot_on(&database, agent, 2).await,
        ];
        database.close().await;
        let config = config(dir.path(), false);
        assert_eq!(run_disable(&config, agent).await.exit, Exit::Done);
        let before = run_list(&config, NOW).await;

        let ran = run_release(&config, agent).await;

        assert_eq!(ran.exit, Exit::Done, "{}", ran.err);
        assert_eq!(
            ran.out,
            format!(
                "Released 2 bots of the agent {agent}: a running server places them within 30 s.\n"
            )
        );
        assert_eq!(ran.err, "");
        assert!(before.out.contains("\tdisabled\t2\t"), "{}", before.out);
        let after = run_list(&config, NOW).await;
        assert!(after.out.contains("\tdisabled\t0\t"), "{}", after.out);
        let database = open(dir.path()).await;
        for bot in bots {
            assert_eq!(
                database.bots().find(bot).await.unwrap().unwrap().agent,
                None
            );
        }
        let entries = audit(&database).await;
        let released: Vec<_> = entries
            .iter()
            .filter(|entry| entry.action == AuditAction::AGENT_RELEASE)
            .collect();
        assert_eq!(released.len(), 1, "{entries:?}");
        assert_eq!(released[0].actor, None);
        assert_eq!(
            released[0].target,
            Some(RecordedTarget::Known(AuditTarget::Agent(agent)))
        );
        let detached = entries
            .iter()
            .filter(|entry| entry.action == AuditAction::BOT_DETACH)
            .count();
        assert_eq!(detached, 2);
        database.close().await;
    }

    #[tokio::test]
    async fn releasing_an_agent_that_holds_no_bot_changes_nothing() {
        let dir = TempDir::new().unwrap();
        migrate(dir.path()).await;
        let database = open(dir.path()).await;
        let agent = enroll(&database, 1, 60).await;
        database.close().await;
        let config = config(dir.path(), true);
        assert_eq!(run_disable(&config, agent).await.exit, Exit::Done);

        let ran = run_release(&config, agent).await;

        assert_eq!(ran.exit, Exit::Done, "{}", ran.err);
        assert_eq!(
            ran.out,
            format!("The agent {agent} holds no bots: nothing changed.\n")
        );
        let database = open(dir.path()).await;
        let released = audit(&database)
            .await
            .into_iter()
            .filter(|entry| entry.action == AuditAction::AGENT_RELEASE)
            .count();
        assert_eq!(released, 0);
        database.close().await;
    }

    #[tokio::test]
    async fn releasing_an_enabled_agent_is_refused_and_keeps_its_bots() {
        let dir = TempDir::new().unwrap();
        migrate(dir.path()).await;
        let database = open(dir.path()).await;
        let agent = enroll(&database, 1, 60).await;
        let bot = bot_on(&database, agent, 1).await;
        database.close().await;

        let ran = run_release(&config(dir.path(), true), agent).await;

        assert_eq!(ran.exit, Exit::Failed);
        assert_eq!(ran.out, "");
        assert_eq!(
            ran.err,
            format!(
                "afkfleet-server: the agent {agent} is enabled: disable it first, and stop its host and its bots\n"
            )
        );
        let database = open(dir.path()).await;
        assert_eq!(
            database.bots().find(bot).await.unwrap().unwrap().agent,
            Some(agent)
        );
        database.close().await;
    }

    #[tokio::test]
    async fn releasing_an_unknown_agent_or_without_a_database_is_exit_1() {
        let dir = TempDir::new().unwrap();
        let unknown = AgentId::new_v7(at(NOW), [9; 10]).unwrap();
        let config = config(dir.path(), true);

        let without = run_release(&config, unknown).await;
        migrate(dir.path()).await;
        let ran = run_release(&config, unknown).await;

        assert_eq!(without.exit, Exit::Failed);
        assert!(
            without
                .err
                .starts_with("afkfleet-server: the database can't be opened: "),
            "{}",
            without.err
        );
        assert_eq!(ran.exit, Exit::Failed);
        assert_eq!(ran.out, "");
        assert_eq!(
            ran.err,
            format!("afkfleet-server: no agent has the ID {unknown}\n")
        );
    }

    #[test]
    fn one_released_bot_is_said_in_the_singular() {
        let agent = AgentId::new_v7(at(NOW), [1; 10]).unwrap();
        let mut out = Vec::new();

        let exit = print_released(agent, 1, &mut out, &mut Vec::new());

        assert_eq!(exit, Exit::Done);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!(
                "Released 1 bot of the agent {agent}: a running server places it within 30 s.
"
            )
        );
    }

    /// The release committed, then reading the agent again failed: the
    /// command says the bots were released, as they were, and the failed
    /// read on stderr, and exits 0.
    #[test]
    fn a_release_whose_read_after_its_commit_failed_says_the_bots_went() {
        let agent = AgentId::new_v7(at(NOW), [1; 10]).unwrap();
        let bot = BotId::new_v7(at(NOW), [2; 10]).unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());

        let exit = released_said(
            agent,
            Err(ReleaseAgentError::ReleasedUnread {
                bots: vec![bot],
                error: crate::ports::store::StoreError::Busy,
            }),
            &mut out,
            &mut err,
        );

        assert_eq!(exit, Exit::Done);
        assert!(
            String::from_utf8(out)
                .unwrap()
                .starts_with(&format!("Released 1 bot of the agent {agent}")),
        );
        let err = String::from_utf8(err).unwrap();
        assert!(
            err.contains("the bots were released, but the agent couldn't be read again"),
            "{err}"
        );
        assert!(!err.contains("couldn't be released"), "{err}");
    }

    #[test]
    fn done_is_exit_0() {
        assert_eq!(Exit::Done.code(), 0);
        assert_eq!(Exit::Created.code(), 0);
        assert_eq!(Exit::Failed.code(), 1);
    }
}
