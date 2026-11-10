//! Tests for the certificates' watch (Plan.md P13.6; ADR-0027): which
//! agents' certificates count as ending soon or expired, how often each is
//! named in the log, and the server certificate's own warning.
//!
//! Each test gets a fresh database from `#[sqlx::test]`. Time isn't paused:
//! sqlx's worker threads would make paused timeouts fire early (CLAUDE.md).
//! The watch's clock is a `ManualClock`. nextest runs each test in its own
//! process, so the log capture holds one test's lines.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use fleet_core::agent::Expiry;
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::id::AgentId;
use fleet_core::pki::CertFingerprint;
use fleet_core::value::AgentName;
use fleet_server::app::certificates::{
    CertificateWatch, REPORT_EVERY, SERVER_EXPIRY_WARNING, server_expiry,
};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::metrics::Metrics;
use fleet_server::ports::agents::{NewAgent, NewAgentCert};
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::store::Store as _;
use fleet_testkit::log_capture;
use fleet_testkit::system::ManualClock;
use rstest::rstest;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tracing::Level;

const NOW: &str = "2027-03-01T08:00:00Z";
/// The watch's own lines.
const TARGET: &str = "fleet_server::app::certificates";
const AGENT_SOON: &str = "an agent's certificate expires soon";
const AGENT_EXPIRED: &str = "an agent's certificate has expired";
const SERVER_SOON: &str = "the server certificate expires soon";
const SERVER_EXPIRED: &str = "the server certificate has expired";
/// The gauge of the server certificate's end.
const SERIES: &str = "afkfleet_server_certificate_not_after_timestamp_seconds";

fn now() -> DateTime<Utc> {
    NOW.parse().unwrap()
}

struct Setup {
    db: Database,
    clock: ManualClock,
    metrics: Metrics,
    watch: CertificateWatch,
}

async fn setup(options: SqliteConnectOptions) -> Setup {
    let db = Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap();
    let clock = ManualClock::new(now());
    let metrics = Metrics::prometheus();
    let watch = CertificateWatch::new(
        Arc::new(clock.clone()),
        Arc::new(db.clone()),
        metrics.recording().clone(),
    );
    Setup {
        db,
        clock,
        metrics,
        watch,
    }
}

impl Setup {
    /// Agent `n` named `name` with certificates of 60 days, each issued the
    /// given days before `NOW`; the listed ones are revoked.
    async fn agent(&self, n: u8, name: &str, issued_days_ago: &[i64]) -> AgentId {
        let id = AgentId::new_v7(now(), [n; 10]).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.agents()
            .insert(&NewAgent {
                id,
                name: AgentName::try_from(name).unwrap(),
                created_at: now() - TimeDelta::days(100),
            })
            .await
            .unwrap();
        for (index, days) in issued_days_ago.iter().enumerate() {
            let issued = now() - TimeDelta::days(*days);
            tx.agents()
                .insert_cert(&NewAgentCert {
                    fingerprint: CertFingerprint::from_digest(
                        [n * 16 + u8::try_from(index).unwrap(); 32],
                    ),
                    agent: id,
                    not_after: issued + TimeDelta::days(60),
                    created_at: issued,
                })
                .await
                .unwrap();
        }
        tx.commit(NewAuditEntry::new(
            now(),
            AuditAction::try_from("test.watch_setup").unwrap(),
            AuditOutcome::Success,
        ))
        .await
        .unwrap();
        id
    }

    async fn disable(&self, agent: AgentId) {
        let mut tx = self.db.write().await.unwrap();
        tx.agents().disable(agent).await.unwrap();
        tx.commit(NewAuditEntry::new(
            now(),
            AuditAction::try_from("test.watch_setup").unwrap(),
            AuditOutcome::Success,
        ))
        .await
        .unwrap();
    }

    /// The value of `series`, as the render writes it.
    fn metric(&self, series: &str) -> Option<String> {
        self.metrics.render().text().lines().find_map(|line| {
            line.strip_prefix(series)?
                .strip_prefix(' ')
                .map(str::to_owned)
        })
    }

    fn counted(&self) -> (Option<String>, Option<String>) {
        (
            self.metric("afkfleet_agent_certificates{state=\"expiring\"}"),
            self.metric("afkfleet_agent_certificates{state=\"expired\"}"),
        )
    }
}

fn lines(level: Level, needle: &str) -> usize {
    log_capture::count_at(TARGET, level, needle)
}

#[rstest]
#[case::a_year_left(365, Expiry::Fine)]
#[case::thirty_days_left(30, Expiry::Fine)]
#[case::just_under_thirty_days(29, Expiry::Soon)]
#[case::a_day_left(1, Expiry::Soon)]
#[case::at_its_end(0, Expiry::Expired)]
#[case::past_its_end(-1, Expiry::Expired)]
fn the_server_certificate_ends_soon_in_its_last_thirty_days(
    #[case] days_left: i64,
    #[case] expected: Expiry,
) {
    assert_eq!(
        server_expiry(now() + TimeDelta::days(days_left), now()),
        expected
    );
    assert_eq!(SERVER_EXPIRY_WARNING, Duration::from_hours(30 * 24));
    assert_eq!(REPORT_EVERY, Duration::from_hours(24));
}

#[sqlx::test(migrations = false)]
async fn the_gauges_count_the_enabled_agents_whose_latest_certificate_ends_soon_or_ended(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    assert_eq!(
        setup.counted(),
        (Some("0".to_owned()), Some("0".to_owned())),
        "both series exist from the start"
    );
    // Issued 10 days ago: fine. 46 days ago: exactly 14 days left, fine.
    setup.agent(1, "fresh", &[10]).await;
    setup.agent(2, "at-the-line", &[46]).await;
    // 47 and 59 days ago: ending soon. 60 and 90 days ago: expired.
    setup.agent(3, "soon", &[47]).await;
    setup.agent(4, "last-day", &[59]).await;
    setup.agent(5, "ended-now", &[60]).await;
    setup.agent(6, "long-gone", &[90]).await;
    // Renewed 5 days ago: its older certificate doesn't count.
    setup.agent(7, "renewed", &[50, 5]).await;
    // Disabled, and one with no certificate at all: left out.
    let off = setup.agent(8, "off", &[59]).await;
    setup.disable(off).await;
    setup.agent(9, "bare", &[]).await;

    setup.watch.run_once().await;

    assert_eq!(
        setup.counted(),
        (Some("2".to_owned()), Some("2".to_owned()))
    );
    // The counts follow the agents at the next run: one is disabled, and
    // the one at the line crossed it a minute later.
    setup.clock.advance(Duration::from_mins(1));
    setup
        .disable(AgentId::new_v7(now(), [6; 10]).unwrap())
        .await;
    setup.watch.run_once().await;
    assert_eq!(
        setup.counted(),
        (Some("3".to_owned()), Some("1".to_owned()))
    );
}

#[sqlx::test(migrations = false)]
async fn each_ending_certificate_is_named_once_a_day_the_first_run_included(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    log_capture::install().unwrap();
    let setup = setup(options).await;
    setup.agent(1, "agent-fresh", &[10]).await;
    setup.agent(3, "agent-thirteen", &[47]).await;
    setup.agent(4, "agent-last-day", &[59]).await;
    setup.agent(6, "agent-gone", &[90]).await;

    setup.watch.run_once().await;

    assert_eq!(lines(Level::WARN, AGENT_SOON), 2);
    assert_eq!(lines(Level::ERROR, AGENT_EXPIRED), 1);
    // Named, with what to do; a healthy agent isn't.
    assert_eq!(log_capture::count(TARGET, "agent-thirteen"), 1);
    assert_eq!(log_capture::count(TARGET, "agent-last-day"), 1);
    assert_eq!(log_capture::count(TARGET, "agent-gone"), 1);
    assert_eq!(log_capture::count(TARGET, "agent-fresh"), 0);
    assert_eq!(log_capture::count(TARGET, "enroll its host again"), 3);
    // The minute's runs in between say nothing more.
    for _ in 0..3 {
        setup.clock.advance(Duration::from_mins(1));
        setup.watch.run_once().await;
    }
    assert_eq!(lines(Level::WARN, AGENT_SOON), 2);
    assert_eq!(lines(Level::ERROR, AGENT_EXPIRED), 1);
    // A day after the first report: again, as things are then (one has 12
    // days left, the other expired meanwhile).
    setup.clock.advance(Duration::from_mins(24 * 60 - 3));
    setup.watch.run_once().await;
    assert_eq!(lines(Level::WARN, AGENT_SOON), 3);
    assert_eq!(lines(Level::ERROR, AGENT_EXPIRED), 3);
}

#[sqlx::test(migrations = false)]
async fn a_fleet_whose_certificates_are_fine_logs_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    log_capture::install().unwrap();
    let setup = setup(options).await;
    setup.agent(1, "fresh", &[10]).await;
    setup.agent(7, "renewed", &[50, 5]).await;
    let watch = setup
        .watch
        .clone()
        .with_server_certificate(now() + TimeDelta::days(200));

    watch.run_once().await;

    assert_eq!(log_capture::count(TARGET, "certificate"), 0);
    assert_eq!(
        setup.counted(),
        (Some("0".to_owned()), Some("0".to_owned()))
    );
}

#[sqlx::test(migrations = false)]
async fn the_server_certificates_end_is_a_gauge_and_its_last_thirty_days_a_daily_warning(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    log_capture::install().unwrap();
    let setup = setup(options).await;
    assert_eq!(setup.metric(SERIES).as_deref(), Some("0"), "0 without one");
    let not_after = now() + TimeDelta::days(31);
    let watch = setup.watch.clone().with_server_certificate(not_after);

    watch.run_once().await;

    assert_eq!(
        setup.metric(SERIES),
        Some(not_after.timestamp().to_string())
    );
    assert_eq!(lines(Level::WARN, SERVER_SOON), 0, "31 days left");
    // Two days on, 29 are left: warned of, once a day.
    setup.clock.advance(Duration::from_hours(2 * 24));
    watch.run_once().await;
    setup.clock.advance(Duration::from_mins(1));
    watch.run_once().await;
    assert_eq!(lines(Level::WARN, SERVER_SOON), 1);
    assert_eq!(log_capture::count(TARGET, "pki server-cert"), 1);
    setup.clock.advance(REPORT_EVERY);
    watch.run_once().await;
    assert_eq!(lines(Level::WARN, SERVER_SOON), 2);
    // Past its end: an error, as at the start.
    setup.clock.advance(Duration::from_hours(30 * 24));
    watch.run_once().await;
    assert_eq!(lines(Level::WARN, SERVER_SOON), 2);
    assert_eq!(lines(Level::ERROR, SERVER_EXPIRED), 1);
}

#[sqlx::test(migrations = false)]
async fn a_database_that_cant_be_read_keeps_the_counts_and_names_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    log_capture::install().unwrap();
    let setup = setup(options).await;
    setup.agent(3, "soon", &[47]).await;
    setup.watch.run_once().await;
    assert_eq!(lines(Level::WARN, AGENT_SOON), 1);
    setup.clock.advance(REPORT_EVERY);
    setup.db.close().await;

    setup.watch.run_once().await;

    assert_eq!(lines(Level::WARN, AGENT_SOON), 1, "nothing was read");
    assert_eq!(
        setup.counted(),
        (Some("1".to_owned()), Some("0".to_owned())),
        "the last counts stay"
    );
}
