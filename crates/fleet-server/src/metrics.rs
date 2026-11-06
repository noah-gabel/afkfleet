//! The server's key metrics (Plan.md P12.4; ADR-0022, S9–S12), served in
//! Prometheus' text format on `[metrics] bind`, an internal port.
//!
//! - **One registry per server** ([`Metrics::prometheus`]), never a global
//!   recorder: in-process tests run several servers, each counts its own,
//!   and a restart starts at 0, as a new process does.
//! - **Recording** ([`ServerMetrics`]) takes typed values only. Every label
//!   is a fixed name from an exhaustive match, never text from a request, a
//!   row or the config: no username, account or player name, address, ID,
//!   token or chat text. Totals only: no per-agent, per-bot or per-user
//!   label (S11); housekeeping's minute line names the agent.
//! - **Every series exists from the start**, at 0 and described, so an
//!   alert sees a counter before its first event.
//! - **No histograms:** a duration, a size or a time is a gauge of the last
//!   value, so the recorder needs no upkeep. No idle timeout is set either:
//!   it would drop the series still at 0.
//!
//! | Metric | Type | Labels |
//! |---|---|---|
//! | `afkfleet_refresh_token_reuses_total` | counter | none |
//! | `afkfleet_rate_limit_refusals_total` | counter | `limit` ([`Limit`]) |
//! | `afkfleet_login_lockout_refusals_total` | counter | `reason` ([`LockoutRefusal`]) |
//! | `afkfleet_agent_connects_refused_total` | counter | `reason` ([`ConnectRefused`]) |
//! | `afkfleet_agent_identity_conflicts_total` | counter | none |
//! | `afkfleet_agent_reports_dropped_total` | counter | none |
//! | `afkfleet_session_grants_total` | counter | `kind` ([`GrantKind`]), `fresh` |
//! | `afkfleet_session_denials_total` | counter | `reason` ([`SessionDenial`]) |
//! | `afkfleet_ingest_chat_dropped_total` | counter | `reason` ([`NotTaken`]) |
//! | `afkfleet_ingest_rows_total` | counter | `outcome`: `stored`, `skipped`, `lost` |
//! | `afkfleet_retention_rows_deleted_total` | counter | `kind`: each kind of [`Deleted`] |
//! | `afkfleet_retention_runs_total` | counter | `outcome`: `success`, `failed`, `stopped` |
//! | `afkfleet_retention_phase_failures_total` | counter | `phase` ([`Phase`]) |
//! | `afkfleet_retention_last_run_duration_seconds` | gauge | none |
//! | `afkfleet_retention_last_run_batches` | gauge | none |
//! | `afkfleet_retention_last_success_timestamp_seconds` | gauge | none |
//! | `afkfleet_backup_last_success_timestamp_seconds` | gauge | none |
//! | `afkfleet_backup_failures_total` | counter | none |
//! | `afkfleet_backup_prune_failures_total` | counter | none |
//! | `afkfleet_backup_last_duration_seconds` | gauge | none |
//! | `afkfleet_backup_last_size_bytes` | gauge | none |

use core::fmt;
use core::time::Duration;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::agent::ConnectRefusal;
use fleet_core::placement::Unplaced;
use metrics::{NoopRecorder, Recorder, counter, describe_counter, describe_gauge, gauge};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

use crate::app::retention::{Deleted, Phase};
use crate::ports::ingest::NotTaken;

/// Refresh tokens presented again after their rotation.
const REFRESH_TOKEN_REUSES: &str = "afkfleet_refresh_token_reuses_total";
/// Requests a rate limit refused.
const RATE_LIMIT_REFUSALS: &str = "afkfleet_rate_limit_refusals_total";
/// Logins the lockout refused.
const LOCKOUT_REFUSALS: &str = "afkfleet_login_lockout_refusals_total";
/// `Connect` calls refused for their certificate.
const CONNECTS_REFUSED: &str = "afkfleet_agent_connects_refused_total";
/// Two processes found holding one agent's identity.
const IDENTITY_CONFLICTS: &str = "afkfleet_agent_identity_conflicts_total";
/// Reports about bots their agent doesn't hold.
const REPORTS_DROPPED: &str = "afkfleet_agent_reports_dropped_total";
/// Minecraft sessions granted to agents.
const SESSION_GRANTS: &str = "afkfleet_session_grants_total";
/// Session requests denied.
const SESSION_DENIALS: &str = "afkfleet_session_denials_total";
/// Chat lines the ingest didn't take.
const CHAT_DROPPED: &str = "afkfleet_ingest_chat_dropped_total";
/// Chat rows the ingest's batches stored, skipped or lost.
const INGEST_ROWS: &str = "afkfleet_ingest_rows_total";
/// Rows the retention deleted.
const RETENTION_DELETED: &str = "afkfleet_retention_rows_deleted_total";
/// Retention runs, by how they ended.
const RETENTION_RUNS: &str = "afkfleet_retention_runs_total";
/// Retention phases that failed.
const RETENTION_PHASE_FAILURES: &str = "afkfleet_retention_phase_failures_total";
/// How long the last retention run took.
const RETENTION_LAST_DURATION: &str = "afkfleet_retention_last_run_duration_seconds";
/// How many batches the last retention run committed.
const RETENTION_LAST_BATCHES: &str = "afkfleet_retention_last_run_batches";
/// When the last successful retention run ended.
const RETENTION_LAST_SUCCESS: &str = "afkfleet_retention_last_success_timestamp_seconds";
/// When the newest finished backup was taken.
const BACKUP_LAST_SUCCESS: &str = "afkfleet_backup_last_success_timestamp_seconds";
/// Backups that failed.
const BACKUP_FAILURES: &str = "afkfleet_backup_failures_total";
/// Backups whose pruning failed.
const BACKUP_PRUNE_FAILURES: &str = "afkfleet_backup_prune_failures_total";
/// How long the last backup took.
const BACKUP_LAST_DURATION: &str = "afkfleet_backup_last_duration_seconds";
/// How large the last backup is.
const BACKUP_LAST_SIZE: &str = "afkfleet_backup_last_size_bytes";

/// A request limit, by its name in housekeeping's minute line (ADR-0016,
/// ADR-0019, ADR-0020).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    /// The global per-IP limit.
    PerIp,
    /// The login routes' shared per-IP limit.
    Login,
    /// The refresh route's per-IP limit.
    Refresh,
    /// The register route's per-IP limit.
    Register,
    /// The per-user limit of authed requests.
    PerUser,
    /// The per-user limit of grants created by a username.
    GrantsByName,
    /// The per-user limit of reads of "Share with"'s suggestions.
    GrantCandidates,
    /// The per-IP limit of `Enroll` calls.
    Enroll,
    /// The per-certificate limit of `Connect` calls.
    Connect,
    /// The per-bot limit of actions that make its account join a server.
    BotActions,
    /// The per-bot limit of fresh session requests.
    FreshSessions,
    /// The per-user limit of chat lines sent through the API.
    ChatPerUser,
    /// The per-bot limit of chat lines sent through the API.
    ChatPerBot,
    /// The per-user cap on open live events sockets.
    WsConnections,
}

impl Limit {
    /// Every limit.
    pub const ALL: [Self; 14] = [
        Self::PerIp,
        Self::Login,
        Self::Refresh,
        Self::Register,
        Self::PerUser,
        Self::GrantsByName,
        Self::GrantCandidates,
        Self::Enroll,
        Self::Connect,
        Self::BotActions,
        Self::FreshSessions,
        Self::ChatPerUser,
        Self::ChatPerBot,
        Self::WsConnections,
    ];

    /// The `limit` label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::PerIp => "per_ip",
            Self::Login => "login",
            Self::Refresh => "refresh",
            Self::Register => "register",
            Self::PerUser => "per_user",
            Self::GrantsByName => "grants_by_name",
            Self::GrantCandidates => "grant_candidates",
            Self::Enroll => "enroll",
            Self::Connect => "connect",
            Self::BotActions => "bot_actions",
            Self::FreshSessions => "fresh_sessions",
            Self::ChatPerUser => "chat_per_user",
            Self::ChatPerBot => "chat_per_bot",
            Self::WsConnections => "ws_connections",
        }
    }
}

/// Why the lockout refused a login (ADR-0016).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockoutRefusal {
    /// The name is locked.
    Locked,
    /// Another attempt for the name is in flight.
    InFlight,
}

impl LockoutRefusal {
    /// Every reason.
    pub const ALL: [Self; 2] = [Self::Locked, Self::InFlight];

    /// The `reason` label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Locked => "locked",
            Self::InFlight => "in_flight",
        }
    }
}

/// Why a `Connect` call was refused for its certificate (ADR-0019, S24,
/// S26, S30). A rate-limited call counts as [`Limit::Connect`] instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectRefused {
    /// A certificate the server knows that doesn't admit its agent.
    Refused(ConnectRefusal),
    /// A certificate the CA signed that the server doesn't know.
    UnknownCertificate,
    /// A call without a client certificate.
    NoCertificate,
    /// A second stream of an agent whose stream lives.
    AlreadyConnected,
}

impl ConnectRefused {
    /// Every reason: each of fleet-core's [`ConnectRefusal::ALL`], so a new
    /// one there counts from 0 here too, then the server's own.
    pub fn all() -> impl Iterator<Item = Self> {
        ConnectRefusal::ALL.into_iter().map(Self::Refused).chain([
            Self::UnknownCertificate,
            Self::NoCertificate,
            Self::AlreadyConnected,
        ])
    }

    /// The `reason` label: the audit entry's for a refused certificate.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Refused(refusal) => refusal.as_str(),
            Self::UnknownCertificate => "unknown_certificate",
            Self::NoCertificate => "no_certificate",
            Self::AlreadyConnected => "already_connected",
        }
    }
}

/// What a session grant hands out (ADR-0019, S61).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantKind {
    /// An online account's Minecraft session.
    Online,
    /// An offline account's name.
    Offline,
}

impl GrantKind {
    /// Every kind.
    pub const ALL: [Self; 2] = [Self::Online, Self::Offline];

    /// The `kind` label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Offline => "offline",
        }
    }
}

/// Why a session request was denied (ADR-0019, S57, S60). The fresh-session
/// limit's refusals count as [`Limit::FreshSessions`] instead (R43), and a
/// request whose stream ended before its answer isn't a denial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionDenial {
    /// The stream's certificate is unknown, names another agent, or its
    /// agent is gone.
    UnknownCertificate,
    /// The stream's certificate doesn't admit its agent any more.
    NotAdmitted(ConnectRefusal),
    /// The bot is gone.
    NoBot,
    /// The bot's row names another agent, or none.
    AssignedElsewhere,
    /// The bot may not run.
    Unplaced(Unplaced),
    /// The bot's account changed while its session was minted.
    AccountChanged,
    /// The stream doesn't hold the bot assigned any more.
    NotHeld,
    /// The grant's own read failed.
    Store,
    /// The session-token service refused.
    SessionToken,
    /// The grant couldn't be written for the wire.
    Unsendable,
    /// The registry denied it before any grant was worked out: the stream
    /// doesn't hold the bot assigned.
    NotAssigned,
    /// The stream's grant queue was full.
    GrantQueueFull,
}

impl SessionDenial {
    /// Every reason: fleet-core's [`ConnectRefusal::ALL`] and
    /// [`Unplaced::ALL`], so a new one there counts from 0 here too, beside
    /// the server's own.
    pub fn all() -> impl Iterator<Item = Self> {
        [Self::UnknownCertificate]
            .into_iter()
            .chain(ConnectRefusal::ALL.into_iter().map(Self::NotAdmitted))
            .chain([Self::NoBot, Self::AssignedElsewhere])
            .chain(Unplaced::ALL.into_iter().map(Self::Unplaced))
            .chain([
                Self::AccountChanged,
                Self::NotHeld,
                Self::Store,
                Self::SessionToken,
                Self::Unsendable,
                Self::NotAssigned,
                Self::GrantQueueFull,
            ])
    }

    /// The `reason` label: the denial's log line's for the grant service's
    /// own reasons.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::UnknownCertificate => "unknown_certificate",
            Self::NotAdmitted(refusal) => refusal.as_str(),
            Self::NoBot => "no_bot",
            Self::AssignedElsewhere => "assigned_elsewhere",
            Self::Unplaced(why) => why.as_str(),
            Self::AccountChanged => "account_changed",
            Self::NotHeld => "not_held",
            Self::Store => "store",
            Self::SessionToken => "session_token",
            Self::Unsendable => "unsendable",
            Self::NotAssigned => "not_assigned",
            Self::GrantQueueFull => "grant_queue_full",
        }
    }
}

/// Every reason the ingest refuses a chat row.
const NOT_TAKEN: [NotTaken; 3] = [NotTaken::StreamFull, NotTaken::QueueFull, NotTaken::Closed];

/// The `reason` label of a chat row the ingest didn't take.
const fn not_taken_label(why: NotTaken) -> &'static str {
    match why {
        NotTaken::StreamFull => "stream_full",
        NotTaken::QueueFull => "queue_full",
        NotTaken::Closed => "closed",
    }
}

/// Every row count of `deleted` under its `kind` label. The binding names
/// every field, so a new kind doesn't compile until it has a label.
const fn deleted_kinds(deleted: &Deleted) -> [(&'static str, u64); 7] {
    let Deleted {
        chat_by_age,
        chat_by_size,
        sessions,
        rotations,
        audit_entries,
        invites,
        enrollments,
    } = *deleted;
    [
        ("chat_by_age", chat_by_age),
        ("chat_by_size", chat_by_size),
        ("sessions", sessions),
        ("rotations", rotations),
        ("audit_entries", audit_entries),
        ("invites", invites),
        ("enrollments", enrollments),
    ]
}

/// One retention run, as its metrics see it.
#[derive(Debug, Clone, Copy)]
pub struct RetentionRun<'a> {
    /// What it deleted.
    pub deleted: &'a Deleted,
    /// The phases that failed.
    pub failed: &'a [Phase],
    /// Whether the stop ended it before its last phase.
    pub stopped: bool,
    /// The batches it committed.
    pub batches: u64,
    /// How long it took.
    pub took: Duration,
    /// When it ended.
    pub ended_at: DateTime<Utc>,
}

/// What the server records its metrics with; clones share its recorder.
/// See the [module docs](self).
#[derive(Clone)]
pub struct ServerMetrics {
    recorder: Arc<dyn Recorder + Send + Sync>,
}

impl fmt::Debug for ServerMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerMetrics").finish_non_exhaustive()
    }
}

impl ServerMetrics {
    /// The metrics recorded into `recorder`, every series described and
    /// registered at 0 there now.
    #[must_use]
    pub fn new(recorder: Arc<dyn Recorder + Send + Sync>) -> Self {
        let metrics = Self { recorder };
        metrics.record(register);
        metrics
    }

    /// Metrics that go nowhere: what a service records with until it's given
    /// the server's, and all the commands that serve nothing record with.
    #[must_use]
    pub fn off() -> Self {
        Self {
            recorder: Arc::new(NoopRecorder),
        }
    }

    /// Runs `record` against this server's recorder, on any thread.
    fn record(&self, record: impl FnOnce()) {
        metrics::with_local_recorder(&*self.recorder, record);
    }

    /// A refresh token came back after its rotation, and its session was
    /// revoked (ADR-0016).
    pub fn refresh_token_reused(&self) {
        self.record(|| counter!(REFRESH_TOKEN_REUSES).increment(1));
    }

    /// `limit` refused a request.
    pub fn rate_limit_refused(&self, limit: Limit) {
        self.record(|| counter!(RATE_LIMIT_REFUSALS, "limit" => limit.label()).increment(1));
    }

    /// The lockout refused a login, as `why` says.
    pub fn lockout_refused(&self, why: LockoutRefusal) {
        self.record(|| counter!(LOCKOUT_REFUSALS, "reason" => why.label()).increment(1));
    }

    /// A `Connect` call was refused, as `why` says.
    pub fn connect_refused(&self, why: ConnectRefused) {
        self.record(|| counter!(CONNECTS_REFUSED, "reason" => why.label()).increment(1));
    }

    /// Two processes were found holding one agent's identity.
    pub fn identity_conflict(&self) {
        self.record(|| counter!(IDENTITY_CONFLICTS).increment(1));
    }

    /// A report about a bot its agent doesn't hold was dropped.
    pub fn report_dropped(&self) {
        self.record(|| counter!(REPORTS_DROPPED).increment(1));
    }

    /// A session of `kind` was granted, `fresh` or not.
    pub fn session_granted(&self, kind: GrantKind, fresh: bool) {
        self.record(|| {
            counter!(SESSION_GRANTS, "kind" => kind.label(), "fresh" => fresh_label(fresh))
                .increment(1);
        });
    }

    /// A session request was denied, as `why` says.
    pub fn session_denied(&self, why: SessionDenial) {
        self.record(|| counter!(SESSION_DENIALS, "reason" => why.label()).increment(1));
    }

    /// The ingest didn't take a chat row, as `why` says.
    pub fn chat_dropped(&self, why: NotTaken) {
        self.record(|| counter!(CHAT_DROPPED, "reason" => not_taken_label(why)).increment(1));
    }

    /// An ingest batch committed: `stored` chat rows stored, `skipped` not,
    /// since their bot's row named another agent.
    pub fn ingest_written(&self, stored: usize, skipped: usize) {
        self.record(|| {
            counter!(INGEST_ROWS, "outcome" => "stored").increment(count(stored));
            counter!(INGEST_ROWS, "outcome" => "skipped").increment(count(skipped));
        });
    }

    /// An ingest batch of `rows` chat rows was lost.
    pub fn ingest_lost(&self, rows: usize) {
        self.record(|| counter!(INGEST_ROWS, "outcome" => "lost").increment(count(rows)));
    }

    /// A retention run ended: what it deleted, how, how long it took and
    /// how many batches it committed. One with no failed phase that wasn't
    /// stopped is a success, whether it deleted anything or not (S45).
    pub fn retention_ran(&self, run: &RetentionRun<'_>) {
        let outcome = if !run.failed.is_empty() {
            "failed"
        } else if run.stopped {
            "stopped"
        } else {
            "success"
        };
        self.record(|| {
            for (kind, rows) in deleted_kinds(run.deleted) {
                counter!(RETENTION_DELETED, "kind" => kind).increment(rows);
            }
            for phase in run.failed {
                counter!(RETENTION_PHASE_FAILURES, "phase" => phase.name()).increment(1);
            }
            counter!(RETENTION_RUNS, "outcome" => outcome).increment(1);
            gauge!(RETENTION_LAST_DURATION).set(run.took.as_secs_f64());
            gauge!(RETENTION_LAST_BATCHES).set(exact(run.batches));
            if outcome == "success" {
                gauge!(RETENTION_LAST_SUCCESS).set(seconds(run.ended_at));
            }
        });
    }

    /// The newest finished backup in the backups' folder was taken at
    /// `stamp`: `serve`'s own or the `backup` command's (R4).
    pub fn newest_backup(&self, stamp: DateTime<Utc>) {
        self.record(|| gauge!(BACKUP_LAST_SUCCESS).set(seconds(stamp)));
    }

    /// `serve` took a backup at `stamp`, `bytes` large, in `took`.
    pub fn backup_taken(&self, stamp: DateTime<Utc>, bytes: u64, took: Duration) {
        self.record(|| {
            gauge!(BACKUP_LAST_SUCCESS).set(seconds(stamp));
            gauge!(BACKUP_LAST_SIZE).set(exact(bytes));
            gauge!(BACKUP_LAST_DURATION).set(took.as_secs_f64());
        });
    }

    /// A backup, or the look at its folder, failed.
    pub fn backup_failed(&self) {
        self.record(|| counter!(BACKUP_FAILURES).increment(1));
    }

    /// A backup's pruning failed; the backup itself was written.
    pub fn backup_prune_failed(&self) {
        self.record(|| counter!(BACKUP_PRUNE_FAILURES).increment(1));
    }
}

/// Describes every metric and registers every series at 0, in the recorder
/// installed now.
fn register() {
    describe_counter!(
        REFRESH_TOKEN_REUSES,
        "Refresh tokens presented again after their rotation: each revoked its session. Any is a possible token theft."
    );
    counter!(REFRESH_TOKEN_REUSES).increment(0);
    describe_counter!(
        RATE_LIMIT_REFUSALS,
        "Requests a rate limit refused, by limit."
    );
    for limit in Limit::ALL {
        counter!(RATE_LIMIT_REFUSALS, "limit" => limit.label()).increment(0);
    }
    describe_counter!(
        LOCKOUT_REFUSALS,
        "Logins the per-username lockout refused, by reason."
    );
    for why in LockoutRefusal::ALL {
        counter!(LOCKOUT_REFUSALS, "reason" => why.label()).increment(0);
    }
    describe_counter!(
        CONNECTS_REFUSED,
        "Agent connections refused for their certificate, by reason, past the audit log's daily cap too."
    );
    for why in ConnectRefused::all() {
        counter!(CONNECTS_REFUSED, "reason" => why.label()).increment(0);
    }
    describe_counter!(
        IDENTITY_CONFLICTS,
        "Times two processes were found holding one agent's identity."
    );
    counter!(IDENTITY_CONFLICTS).increment(0);
    describe_counter!(
        REPORTS_DROPPED,
        "Agent reports about bots the agent doesn't hold, dropped."
    );
    counter!(REPORTS_DROPPED).increment(0);
    describe_counter!(
        SESSION_GRANTS,
        "Minecraft sessions granted to agents, by kind and whether a fresh one was asked for."
    );
    for kind in GrantKind::ALL {
        for fresh in [false, true] {
            counter!(SESSION_GRANTS, "kind" => kind.label(), "fresh" => fresh_label(fresh))
                .increment(0);
        }
    }
    describe_counter!(
        SESSION_DENIALS,
        "Session requests denied, by reason; the fresh-session limit's refusals are rate_limit_refusals with limit=\"fresh_sessions\"."
    );
    for why in SessionDenial::all() {
        counter!(SESSION_DENIALS, "reason" => why.label()).increment(0);
    }
    describe_counter!(
        CHAT_DROPPED,
        "Chat lines from agents the ingest didn't take, by reason."
    );
    for why in NOT_TAKEN {
        counter!(CHAT_DROPPED, "reason" => not_taken_label(why)).increment(0);
    }
    describe_counter!(
        INGEST_ROWS,
        "Chat rows the ingest's batches stored, skipped (their bot is assigned elsewhere) or lost (a batch that failed)."
    );
    for outcome in ["stored", "skipped", "lost"] {
        counter!(INGEST_ROWS, "outcome" => outcome).increment(0);
    }
    register_retention();
    register_backups();
}

/// [`register`]'s retention part.
fn register_retention() {
    describe_counter!(RETENTION_DELETED, "Rows the retention deleted, by kind.");
    for (kind, _) in deleted_kinds(&Deleted::default()) {
        counter!(RETENTION_DELETED, "kind" => kind).increment(0);
    }
    describe_counter!(
        RETENTION_RUNS,
        "Retention runs, by outcome: success (no phase failed, not stopped), failed or stopped."
    );
    for outcome in ["success", "failed", "stopped"] {
        counter!(RETENTION_RUNS, "outcome" => outcome).increment(0);
    }
    describe_counter!(
        RETENTION_PHASE_FAILURES,
        "Retention phases that failed, by phase."
    );
    for phase in Phase::ORDER {
        counter!(RETENTION_PHASE_FAILURES, "phase" => phase.name()).increment(0);
    }
    describe_gauge!(
        RETENTION_LAST_DURATION,
        "How long the last retention run took, in seconds."
    );
    gauge!(RETENTION_LAST_DURATION).set(0.0);
    describe_gauge!(
        RETENTION_LAST_BATCHES,
        "How many batches the last retention run committed."
    );
    gauge!(RETENTION_LAST_BATCHES).set(0.0);
    describe_gauge!(
        RETENTION_LAST_SUCCESS,
        "When the last successful retention run ended, in Unix seconds; 0 until the first. The retention isn't watched: an old value means it died or fails every run."
    );
    gauge!(RETENTION_LAST_SUCCESS).set(0.0);
}

/// [`register`]'s backups part.
fn register_backups() {
    describe_gauge!(
        BACKUP_LAST_SUCCESS,
        "When the newest finished backup in [database] backup_dir was taken, in Unix seconds, the backup command's included; 0 without one."
    );
    gauge!(BACKUP_LAST_SUCCESS).set(0.0);
    describe_counter!(
        BACKUP_FAILURES,
        "Scheduled backups that failed, or whose folder couldn't be read."
    );
    counter!(BACKUP_FAILURES).increment(0);
    describe_counter!(
        BACKUP_PRUNE_FAILURES,
        "Scheduled backups written whose pruning of older backups failed."
    );
    counter!(BACKUP_PRUNE_FAILURES).increment(0);
    describe_gauge!(
        BACKUP_LAST_DURATION,
        "How long the last scheduled backup took, in seconds."
    );
    gauge!(BACKUP_LAST_DURATION).set(0.0);
    describe_gauge!(
        BACKUP_LAST_SIZE,
        "How large the last scheduled backup is, in bytes."
    );
    gauge!(BACKUP_LAST_SIZE).set(0.0);
}

/// The `fresh` label.
const fn fresh_label(fresh: bool) -> &'static str {
    if fresh { "true" } else { "false" }
}

/// `n` as a counter's increment; a count above `u64::MAX` saturates.
fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// `value` as a gauge's: exact below 2^53, which no count, size or time
/// here reaches.
fn exact(value: u64) -> f64 {
    let high = u32::try_from(value >> 32).unwrap_or(u32::MAX);
    let low = u32::try_from(value & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

/// `at` in Unix seconds; a time before 1970 is 0.
fn seconds(at: DateTime<Utc>) -> f64 {
    u64::try_from(at.timestamp()).map_or(0.0, exact)
}

/// The server's metrics and what renders them: one Prometheus registry.
#[derive(Clone)]
pub struct Metrics {
    recording: ServerMetrics,
    render: MetricsRender,
}

impl fmt::Debug for Metrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Metrics").finish_non_exhaustive()
    }
}

impl Metrics {
    /// A registry of its own, every series registered at 0.
    #[must_use]
    pub fn prometheus() -> Self {
        // No upkeep task: it only drains histograms, and there are none.
        // No idle timeout: it would drop the series still at 0.
        let recorder = PrometheusBuilder::new().build_recorder();
        let render = MetricsRender(recorder.handle());
        Self {
            recording: ServerMetrics::new(Arc::new(recorder)),
            render,
        }
    }

    /// What the services record with.
    #[must_use]
    pub const fn recording(&self) -> &ServerMetrics {
        &self.recording
    }

    /// What `GET /metrics` renders.
    #[must_use]
    pub const fn render(&self) -> &MetricsRender {
        &self.render
    }
}

/// Renders a registry's series in Prometheus' text format; clones share it.
#[derive(Clone)]
pub struct MetricsRender(PrometheusHandle);

impl fmt::Debug for MetricsRender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MetricsRender").finish_non_exhaustive()
    }
}

impl MetricsRender {
    /// Every series, now.
    #[must_use]
    pub fn text(&self) -> String {
        self.0.render()
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::*;

    /// The value of the series `series` (its name and labels as rendered),
    /// if it's there.
    fn value(render: &str, series: &str) -> Option<String> {
        render.lines().find_map(|line| {
            line.strip_prefix(series)
                .and_then(|rest| rest.strip_prefix(' '))
                .map(str::to_owned)
        })
    }

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text).unwrap().to_utc()
    }

    /// Every series line of `render`: its name, and its labels' keys and
    /// values.
    fn series(render: &str) -> Vec<(String, Vec<(String, String)>)> {
        render
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .map(|line| {
                let name_and_labels = line.rsplit_once(' ').unwrap().0;
                match name_and_labels.split_once('{') {
                    None => (name_and_labels.to_owned(), Vec::new()),
                    Some((name, labels)) => {
                        let labels = labels
                            .trim_end_matches('}')
                            .split(',')
                            .map(|pair| {
                                let (key, quoted) = pair.split_once('=').unwrap();
                                (key.to_owned(), quoted.trim_matches('"').to_owned())
                            })
                            .collect();
                        (name.to_owned(), labels)
                    }
                }
            })
            .collect()
    }

    /// `render` in a fixed order: the exporter writes its families and their
    /// series in hash order, so each family's lines are sorted (`# HELP`,
    /// `# TYPE`, then the series), and the families by their first line.
    fn sorted(render: &str) -> String {
        let mut families: Vec<String> = render
            .split("\n\n")
            .map(|family| {
                let mut lines: Vec<&str> = family.lines().filter(|line| !line.is_empty()).collect();
                lines.sort_unstable();
                lines.join("\n")
            })
            .filter(|family| !family.is_empty())
            .collect();
        families.sort_unstable();
        families.join("\n\n")
    }

    #[test]
    fn every_series_starts_at_zero_and_is_described() {
        let metrics = Metrics::prometheus();

        let render = metrics.render().text();

        insta::assert_snapshot!(sorted(&render));
    }

    /// Every reason fleet-core names, a new one too, is a series at 0 from
    /// the start (PR #96's review): the refusals and denials are built from
    /// [`ConnectRefusal::ALL`] and [`Unplaced::ALL`], never copied by hand.
    #[test]
    fn every_reason_fleet_core_names_starts_at_zero() {
        let render = Metrics::prometheus().render().text();

        for refusal in ConnectRefusal::ALL.map(ConnectRefusal::as_str) {
            for name in [CONNECTS_REFUSED, SESSION_DENIALS] {
                let series = format!("{name}{{reason=\"{refusal}\"}}");
                assert_eq!(value(&render, &series).as_deref(), Some("0"), "{series}");
            }
        }
        for why in Unplaced::ALL.map(Unplaced::as_str) {
            let series = format!("{SESSION_DENIALS}{{reason=\"{why}\"}}");
            assert_eq!(value(&render, &series).as_deref(), Some("0"), "{series}");
        }
    }

    /// No label holds anything but a fixed name of the server's (S11): the
    /// keys from one short list, the values lowercase words.
    #[test]
    fn every_label_is_a_fixed_name() {
        let render = Metrics::prometheus().render().text();
        let found = series(&render);

        assert!(found.len() > 50, "{render}");
        for (name, labels) in found {
            assert!(name.starts_with("afkfleet_"), "{name}");
            for (key, label) in labels {
                assert!(
                    ["limit", "reason", "kind", "fresh", "outcome", "phase"]
                        .contains(&key.as_str()),
                    "{name}: {key}"
                );
                assert!(
                    label.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                        && label
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                    "{name}: {key}={label}"
                );
            }
        }
    }

    #[test]
    fn each_label_set_names_each_value_once() {
        let distinct = |labels: Vec<&str>| {
            let mut sorted = labels.clone();
            sorted.sort_unstable();
            sorted.dedup();
            sorted.len() == labels.len()
        };

        assert!(distinct(Limit::ALL.map(Limit::label).into()));
        assert!(distinct(
            LockoutRefusal::ALL.map(LockoutRefusal::label).into()
        ));
        assert!(distinct(
            ConnectRefused::all().map(ConnectRefused::label).collect()
        ));
        assert!(distinct(
            SessionDenial::all().map(SessionDenial::label).collect()
        ));
        assert!(distinct(NOT_TAKEN.map(not_taken_label).into()));
    }

    #[test]
    fn metrics_that_are_off_record_nothing_and_never_fail() {
        let off = ServerMetrics::off();

        off.refresh_token_reused();
        off.rate_limit_refused(Limit::Login);
        off.backup_failed();
    }

    #[test]
    fn two_registries_count_apart() {
        let one = Metrics::prometheus();
        let two = Metrics::prometheus();

        one.recording().refresh_token_reused();

        assert_eq!(
            value(&one.render().text(), REFRESH_TOKEN_REUSES).as_deref(),
            Some("1")
        );
        assert_eq!(
            value(&two.render().text(), REFRESH_TOKEN_REUSES).as_deref(),
            Some("0")
        );
    }

    #[test]
    fn a_count_from_another_thread_reaches_the_registry() {
        let metrics = Metrics::prometheus();
        let recording = metrics.recording().clone();

        std::thread::spawn(move || recording.identity_conflict())
            .join()
            .unwrap();

        assert_eq!(
            value(&metrics.render().text(), IDENTITY_CONFLICTS).as_deref(),
            Some("1")
        );
    }

    #[test]
    fn each_counter_counts_under_its_labels() {
        let metrics = Metrics::prometheus();
        let recording = metrics.recording();

        recording.refresh_token_reused();
        recording.rate_limit_refused(Limit::FreshSessions);
        recording.lockout_refused(LockoutRefusal::InFlight);
        recording.connect_refused(ConnectRefused::Refused(ConnectRefusal::Revoked));
        recording.connect_refused(ConnectRefused::NoCertificate);
        recording.identity_conflict();
        recording.report_dropped();
        recording.session_granted(GrantKind::Online, true);
        recording.session_denied(SessionDenial::Unplaced(Unplaced::Pending));
        recording.session_denied(SessionDenial::GrantQueueFull);
        recording.chat_dropped(NotTaken::StreamFull);
        recording.ingest_written(5, 2);
        recording.ingest_lost(3);
        recording.backup_failed();
        recording.backup_prune_failed();

        let render = metrics.render().text();
        for (series, expected) in [
            ("afkfleet_refresh_token_reuses_total", "1"),
            (
                "afkfleet_rate_limit_refusals_total{limit=\"fresh_sessions\"}",
                "1",
            ),
            ("afkfleet_rate_limit_refusals_total{limit=\"login\"}", "0"),
            (
                "afkfleet_login_lockout_refusals_total{reason=\"in_flight\"}",
                "1",
            ),
            (
                "afkfleet_agent_connects_refused_total{reason=\"revoked\"}",
                "1",
            ),
            (
                "afkfleet_agent_connects_refused_total{reason=\"no_certificate\"}",
                "1",
            ),
            ("afkfleet_agent_identity_conflicts_total", "1"),
            ("afkfleet_agent_reports_dropped_total", "1"),
            (
                "afkfleet_session_grants_total{kind=\"online\",fresh=\"true\"}",
                "1",
            ),
            (
                "afkfleet_session_grants_total{kind=\"online\",fresh=\"false\"}",
                "0",
            ),
            ("afkfleet_session_denials_total{reason=\"pending\"}", "1"),
            (
                "afkfleet_session_denials_total{reason=\"grant_queue_full\"}",
                "1",
            ),
            (
                "afkfleet_ingest_chat_dropped_total{reason=\"stream_full\"}",
                "1",
            ),
            ("afkfleet_ingest_rows_total{outcome=\"stored\"}", "5"),
            ("afkfleet_ingest_rows_total{outcome=\"skipped\"}", "2"),
            ("afkfleet_ingest_rows_total{outcome=\"lost\"}", "3"),
            ("afkfleet_backup_failures_total", "1"),
            ("afkfleet_backup_prune_failures_total", "1"),
        ] {
            assert_eq!(
                value(&render, series).as_deref(),
                Some(expected),
                "{series}"
            );
        }
    }

    #[test]
    fn a_successful_retention_run_sets_its_time_and_a_failed_one_doesnt() {
        let metrics = Metrics::prometheus();
        let recording = metrics.recording();
        let deleted = Deleted {
            chat_by_age: 7,
            audit_entries: 2,
            ..Deleted::default()
        };
        let succeeded = at("2027-01-15T08:00:00Z");

        recording.retention_ran(&RetentionRun {
            deleted: &deleted,
            failed: &[],
            stopped: false,
            batches: 3,
            took: Duration::from_millis(1500),
            ended_at: succeeded,
        });
        recording.retention_ran(&RetentionRun {
            deleted: &Deleted::default(),
            failed: &[Phase::Audit, Phase::Invites],
            stopped: false,
            batches: 1,
            took: Duration::from_millis(250),
            ended_at: succeeded + TimeDelta::minutes(1),
        });
        recording.retention_ran(&RetentionRun {
            deleted: &Deleted::default(),
            failed: &[],
            stopped: true,
            batches: 0,
            took: Duration::ZERO,
            ended_at: succeeded + TimeDelta::minutes(2),
        });

        let render = metrics.render().text();
        for (series, expected) in [
            (
                "afkfleet_retention_rows_deleted_total{kind=\"chat_by_age\"}",
                "7",
            ),
            (
                "afkfleet_retention_rows_deleted_total{kind=\"audit_entries\"}",
                "2",
            ),
            (
                "afkfleet_retention_rows_deleted_total{kind=\"sessions\"}",
                "0",
            ),
            ("afkfleet_retention_runs_total{outcome=\"success\"}", "1"),
            ("afkfleet_retention_runs_total{outcome=\"failed\"}", "1"),
            ("afkfleet_retention_runs_total{outcome=\"stopped\"}", "1"),
            (
                "afkfleet_retention_phase_failures_total{phase=\"audit\"}",
                "1",
            ),
            (
                "afkfleet_retention_phase_failures_total{phase=\"invites\"}",
                "1",
            ),
            (
                "afkfleet_retention_phase_failures_total{phase=\"sessions\"}",
                "0",
            ),
            ("afkfleet_retention_last_run_duration_seconds", "0"),
            ("afkfleet_retention_last_run_batches", "0"),
            (
                "afkfleet_retention_last_success_timestamp_seconds",
                "1800000000",
            ),
        ] {
            assert_eq!(
                value(&render, series).as_deref(),
                Some(expected),
                "{series}"
            );
        }
    }

    #[test]
    fn a_backup_sets_its_time_size_and_duration_and_the_newest_moves_the_time() {
        let metrics = Metrics::prometheus();
        let recording = metrics.recording();

        recording.backup_taken(
            at("2027-01-15T08:00:00Z"),
            123_456_789,
            Duration::from_millis(2500),
        );
        let after_taken = metrics.render().text();
        recording.newest_backup(at("2027-01-15T09:00:00Z"));
        let after_newest = metrics.render().text();

        assert_eq!(
            value(&after_taken, BACKUP_LAST_SUCCESS).as_deref(),
            Some("1800000000")
        );
        assert_eq!(
            value(&after_taken, BACKUP_LAST_SIZE).as_deref(),
            Some("123456789")
        );
        assert_eq!(
            value(&after_taken, BACKUP_LAST_DURATION).as_deref(),
            Some("2.5")
        );
        assert_eq!(
            value(&after_newest, BACKUP_LAST_SUCCESS).as_deref(),
            Some("1800003600")
        );
    }

    #[test]
    fn values_convert_exactly() {
        assert_eq!(exact(0).to_bits(), 0.0_f64.to_bits());
        assert_eq!(exact(123_456_789).to_bits(), 123_456_789.0_f64.to_bits());
        assert_eq!(
            exact(1 << 52).to_bits(),
            4_503_599_627_370_496.0_f64.to_bits()
        );
        assert_eq!(
            seconds(at("1969-12-31T23:59:59Z")).to_bits(),
            0.0_f64.to_bits()
        );
        assert_eq!(count(7), 7);
    }
}
