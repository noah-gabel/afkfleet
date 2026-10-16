//! One run of the server's housekeeping (Plan.md P7.10, ADR-0016): the rate
//! limiters forget clients whose buckets are full again, the lockout's
//! expired streaks are purged, the login's expired challenges are dropped,
//! and one `info` line sums up the refusals, only when there were any. Its timing and its stop are tested on paused
//! time next to it; this needs a real database from `#[sqlx::test]`, so it
//! runs in real time with a `ManualClock` (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::num::NonZeroU32;
use core::time::Duration;
use std::net::IpAddr;
use std::sync::Arc;

use chrono::{TimeZone as _, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::auth::LockoutPolicy;
use fleet_core::authz::Role;
use fleet_core::bot::{BotSnapshot, BotState};
use fleet_core::id::{AgentId, BotId, LoginStepId, RequestId, UserId};
use fleet_core::pki::CertFingerprint;
use fleet_core::system::Clock as _;
use fleet_core::value::AgentName;
use fleet_core::value::Username;
use fleet_proto::control::AgentMessage;
use fleet_server::app::challenges::{ChallengeUser, Challenges, SetupPurpose};
use fleet_server::app::link_flows::LinkFlows;
use fleet_server::app::lockout::LockoutService;
use fleet_server::cli::housekeeping::Housekeeping;
use fleet_server::config::{LinkFlowLimits, LogConfig, LogFormat, Rate, RateLimitConfig};
use fleet_server::grpc::registry::AgentRegistry;
use fleet_server::http::rate_limit::RateLimits;
use fleet_server::http::routes::RateClass;
use fleet_server::infra::crypto::attempt_keys::HmacAttemptKeys;
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::agent_connections::Admitted;
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::login_attempts::{AttemptKeys as _, LoginAttempt};
use fleet_server::ports::store::Store as _;
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::log_buffer::LogBuffer;
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::SecretBox;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt as _;

/// The summary's message.
const SUMMARY: &str = "requests were refused since the last report";
/// The failed purge's message.
const PURGE_FAILED: &str =
    "the purge of expired login failure streaks failed; the next run tries again";

fn nonzero(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap()
}

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

fn name(text: &str) -> Username {
    Username::try_from(text).unwrap()
}

fn keys() -> HmacAttemptKeys {
    HmacAttemptKeys::new(SecretBox::new(Box::new([0x42; 32])))
}

struct Setup {
    housekeeping: Housekeeping,
    registry: AgentRegistry,
    limits: RateLimits,
    lockout: LockoutService,
    challenges: Challenges,
    clock: ManualClock,
    db: Database,
}

async fn setup(options: SqliteConnectOptions) -> Setup {
    let db = Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap();
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2027, 1, 15, 8, 0, 0).unwrap());
    let one = Rate::per_second(nonzero(1), nonzero(1));
    let limits = RateLimits::new(
        &RateLimitConfig {
            per_ip: one,
            login: one,
            refresh: one,
            register: one,
            per_user: one,
            link_flows: LinkFlowLimits::default(),
            grants_by_name: one,
            enroll: one,
            connect: one,
        },
        Arc::new(clock.clone()),
    );
    let lockout = LockoutService::new(
        Arc::new(clock.clone()),
        Arc::new(db.clone()),
        Arc::new(keys()),
        LockoutPolicy::default(),
    );
    let challenges = Challenges::new(
        Arc::new(clock.clone()),
        Arc::new(Sha256Tokens::new(Arc::new(SeededRandom::new(3)))),
    );
    let registry = AgentRegistry::new(Arc::new(clock.clone()));
    Setup {
        housekeeping: Housekeeping::new(
            limits.clone(),
            lockout.clone(),
            challenges.clone(),
            LinkFlows::new(Arc::new(clock.clone())),
            registry.clone(),
        ),
        registry,
        limits,
        lockout,
        challenges,
        clock,
        db,
    }
}

impl Setup {
    /// One failed login for `text`, committed.
    async fn fail(&self, text: &str) {
        let attempt = self.lockout.begin(&name(text)).await.unwrap();
        let mut tx = self.db.write().await.unwrap();
        attempt
            .record_failure(&mut *tx, None, ip("192.0.2.10"))
            .await
            .unwrap();
        tx.commit(NewAuditEntry::new(
            self.clock.now(),
            AuditAction::try_from("test.login_attempt").unwrap(),
            AuditOutcome::Failure,
        ))
        .await
        .unwrap();
    }

    /// `text`'s stored streak.
    async fn row(&self, text: &str) -> Option<LoginAttempt> {
        self.db
            .login_attempts()
            .find(&keys().key(&name(text)))
            .await
            .unwrap()
    }

    /// One run, with the server's log layer at `info` into a buffer.
    async fn run_once(&self) -> LogBuffer {
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
        let _guard = tracing::subscriber::set_default(subscriber);
        self.housekeeping.run_once(&CancellationToken::new()).await;
        buffer
    }
}

#[sqlx::test(migrations = false)]
async fn a_run_sums_up_the_refusals_since_the_last_one_in_one_line(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    for _ in 0..4 {
        let _ = setup.limits.check_ip(ip("192.0.2.10"));
    }
    for _ in 0..3 {
        let _ = setup.limits.check_class(RateClass::Login, ip("192.0.2.10"));
    }
    let grantor = fleet_core::id::UserId::new_v7(
        Utc.with_ymd_and_hms(2027, 1, 15, 8, 0, 0).unwrap(),
        [7; 10],
    )
    .unwrap();
    for _ in 0..2 {
        let _ = setup.limits.check_grant_by_name(grantor);
        let _ = setup.limits.check_enroll("192.0.2.30".parse().unwrap());
        let _ = setup
            .limits
            .check_connect(fleet_core::pki::CertFingerprint::from_digest([9; 32]));
    }
    for _ in 0..5 {
        setup.fail("alice").await;
    }
    let _ = setup.lockout.begin(&name("alice")).await;
    let held = setup.lockout.begin(&name("bob")).await.unwrap();
    let _ = setup.lockout.begin(&name("bob")).await;
    drop(held);

    let logs = setup.run_once().await;

    let lines = logs.lines_with(SUMMARY).unwrap();
    assert_eq!(lines.len(), 1, "{}", logs.text());
    let line = &lines[0];
    assert_eq!(line["level"], "INFO");
    assert_eq!(line["per_ip"], 3);
    assert_eq!(line["login"], 2);
    assert_eq!(line["refresh"], 0);
    assert_eq!(line["register"], 0);
    assert_eq!(line["per_user"], 0);
    assert_eq!(line["grants_by_name"], 1);
    assert_eq!(line["enroll"], 1);
    assert_eq!(line["connect"], 1);
    assert_eq!(line["locked"], 1);
    assert_eq!(line["in_flight"], 1);
    let again = setup.run_once().await;
    assert_eq!(again.text(), "", "nothing was refused since");
}

/// The agents' line's message.
const DROPPED: &str =
    "an agent's reports about bots it doesn't hold were dropped since the last report";

#[sqlx::test(migrations = false)]
async fn a_run_logs_one_line_per_agent_whose_reports_were_dropped(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let now = setup.clock.now();
    let admitted = Admitted {
        request: RequestId::new_v7(now, [1; 10]).unwrap(),
        agent: AgentId::new_v7(now, [1; 10]).unwrap(),
        name: AgentName::try_from("agent-1").unwrap(),
        fingerprint: CertFingerprint::from_digest([1; 32]),
        not_after: now + chrono::TimeDelta::days(60),
        peer: None,
    };
    let lease = setup.registry.claim(&admitted).unwrap();
    let (queue, _queued) = tokio::sync::mpsc::channel(1);
    let _ended = setup.registry.activate(&lease, queue, now).unwrap();
    let status = AgentMessage::Status(BotSnapshot {
        bot_id: BotId::new_v7(now, [2; 10]).unwrap(),
        state: BotState::Stopped,
        since: now,
        last_disconnect: None,
    });
    for _ in 0..3 {
        let _ = setup.registry.admit(&lease, &status);
    }

    let logs = setup.run_once().await;

    let lines = logs.lines_with(DROPPED).unwrap();
    assert_eq!(lines.len(), 1, "{}", logs.text());
    assert_eq!(lines[0]["level"], "INFO");
    assert_eq!(lines[0]["agent_id"], admitted.agent.to_string());
    assert_eq!(lines[0]["agent"], "agent-1");
    assert_eq!(lines[0]["dropped"], 3);
    let again = setup.run_once().await;
    assert_eq!(again.lines_with(DROPPED).unwrap().len(), 0);
}

/// A user for a challenge; no row is needed, since the store is in memory.
fn challenge_user(n: u8) -> ChallengeUser {
    ChallengeUser {
        id: UserId::new_v7(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap(), [n; 10]).unwrap(),
        role: Role::Member,
        password_changed_at: Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap(),
    }
}

#[sqlx::test(migrations = false)]
async fn a_run_drops_the_expired_challenges_and_keeps_the_live_ones(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup
        .challenges
        .issue_mfa(
            challenge_user(1),
            None,
            LoginStepId::new_v7(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap(), [1; 10])
                .unwrap(),
        )
        .unwrap();
    setup
        .challenges
        .issue_setup(challenge_user(2), SetupPurpose::FirstEnrollment)
        .unwrap();
    setup.clock.advance(Duration::from_mins(5));
    setup
        .challenges
        .issue_setup(challenge_user(3), SetupPurpose::FirstEnrollment)
        .unwrap();

    setup.run_once().await;

    assert_eq!(
        setup.challenges.held(),
        2,
        "the 5-minute mfa token is over, both setup tokens are live"
    );
    setup.clock.advance(Duration::from_mins(5));
    setup.run_once().await;
    assert_eq!(setup.challenges.held(), 1, "the first setup token is over");
}

#[sqlx::test(migrations = false)]
async fn a_run_without_refusals_logs_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup.limits.check_ip(ip("192.0.2.10")).unwrap();

    let logs = setup.run_once().await;

    assert_eq!(logs.text(), "");
}

#[sqlx::test(migrations = false)]
async fn a_run_forgets_full_buckets_and_purges_expired_streaks(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup.limits.check_ip(ip("192.0.2.10")).unwrap();
    setup.fail("alice").await;
    setup.clock.advance(Duration::from_hours(24));
    setup.fail("bob").await;

    setup.run_once().await;

    assert_eq!(setup.limits.tracked(), 0);
    assert_eq!(setup.row("alice").await, None, "expired");
    assert!(setup.row("bob").await.is_some(), "fresh");
}

#[sqlx::test(migrations = false)]
async fn a_failed_purge_is_a_warning_and_the_run_goes_on(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    for _ in 0..2 {
        let _ = setup.limits.check_ip(ip("192.0.2.10"));
    }
    setup.db.close().await;

    let logs = setup.run_once().await;

    let warnings = logs.lines_with(PURGE_FAILED).unwrap();
    assert_eq!(warnings.len(), 1, "{}", logs.text());
    assert_eq!(warnings[0]["level"], "WARN");
    assert!(
        warnings[0]["error"]
            .as_str()
            .unwrap()
            .starts_with("the database failed"),
        "{}",
        logs.text()
    );
    assert_eq!(
        logs.lines_with(SUMMARY).unwrap().len(),
        1,
        "the summary still comes"
    );
}
