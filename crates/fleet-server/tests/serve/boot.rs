//! `serve::run` in-process (Plan.md P6.11 and Phase 6's Definition of Done,
//! ADR-0015): the wiring `serve` runs, on a listener the test binds itself.
//! Real time, since the database runs on sqlx's worker threads (CLAUDE.md).

use core::num::NonZeroU32;
use core::time::Duration;

use fleet_server::cli::healthcheck::{self, HttpResponse};
use fleet_server::cli::serve::{self, Exit};
use fleet_server::config::Rate;
use fleet_server::infra::sqlite::{self, DatabaseOptions, MIGRATOR};
use fleet_startup::signals::Signal;

use chrono::DateTime;
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::authz::Role;
use fleet_core::id::{AccountId, UserId};
use fleet_core::value::Username;
use fleet_server::config::ServerConfig;
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::store::Store as _;
use fleet_server::ports::users::{NewUser, PasswordHash};
use secrecy::SecretString;

use crate::support::{ChannelSignals, capture, listener, server_config, test_dir, vault_keys};

#[tokio::test]
async fn a_missing_database_folder_fails_the_start_before_listening() {
    let dir = test_dir("boot_missing_folder");
    let (listener, address) = listener().await;
    let mut config = server_config(&dir, address, false);
    config.database.path = dir.join("no-such-folder").join("afkfleet.db");
    let (logs, _guard) = capture();

    let keys = vault_keys(&config);
    let exit = serve::run(&config, listener, ChannelSignals::silent(), keys).await;

    assert_eq!(exit, Exit::StartupFailed);
    assert_eq!(exit.code(), 1);
    let lines = logs.json_lines().unwrap();
    let failed = logs.lines_with("the server can't start").unwrap();
    assert_eq!(failed.len(), 1, "{}", logs.text());
    assert_eq!(failed[0]["level"], "ERROR");
    let error = failed[0]["error"].as_str().unwrap();
    assert!(
        error.starts_with("the database can't be opened: the database's folder "),
        "{error}"
    );
    assert!(
        lines
            .iter()
            .all(|line| line["message"] != "the server is listening"),
        "{}",
        logs.text()
    );
}

/// The longest the boot test waits for anything.
const PATIENCE: Duration = Duration::from_secs(30);

/// Phase 6's Definition of Done: the app boots in-process on a fresh
/// temporary database, with the wiring `serve` uses, passes both health
/// checks over a real connection, and stops cleanly on a signal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_app_boots_on_a_fresh_database_passes_the_health_checks_and_stops_cleanly() {
    let dir = test_dir("boot_dod");
    let (listener, address) = listener().await;
    let config = server_config(&dir, address, false);
    let (sender, signals) = ChannelSignals::new();
    let keys = vault_keys(&config);
    let server = tokio::spawn({
        let config = config.clone();
        async move { serve::run(&config, listener, signals, keys).await }
    });

    for path in ["/health/live", "/health/ready"] {
        let answer = healthcheck::get(address, path, PATIENCE).await.unwrap();
        assert_eq!(
            answer,
            HttpResponse {
                status: 200,
                body: Vec::new()
            },
            "{path}"
        );
    }
    let health = healthcheck::check(&config.http).await;
    assert!(health.is_healthy(), "{health}");

    sender.send(Signal::Terminate).await.unwrap();
    let exit = tokio::time::timeout(PATIENCE, server)
        .await
        .expect("the server should stop within its deadline")
        .unwrap();

    assert_eq!(exit, Exit::Stopped);
    assert!(!dir.join("afkfleet.db-wal").exists(), "closed cleanly");
    let database = sqlite::open(&config.database, DatabaseOptions::default())
        .await
        .unwrap();
    let applied: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!: i64" FROM _sqlx_migrations WHERE success"#
    )
    .fetch_one(database.read_pool())
    .await
    .unwrap();
    assert_eq!(usize::try_from(applied).unwrap(), MIGRATOR.iter().count());
    database.close().await;
}

/// P7.10's one real-socket test (ADR-0016): requests over real connections
/// reach the per-IP limit with their peer's address. `oneshot` tests set the
/// address themselves, so only a real connection proves `serve` provides it
/// (`into_make_service_with_connect_info`): without it, every request would
/// be a 500, and the limit would never refuse one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_over_real_connections_reach_the_per_ip_limit_with_their_address() {
    let dir = test_dir("boot_per_ip");
    let (listener, address) = listener().await;
    let mut config = server_config(&dir, address, false);
    let one = NonZeroU32::MIN;
    config.rate_limits.per_ip = Rate::per_second(one, one.saturating_add(1));
    let (sender, signals) = ChannelSignals::new();
    let keys = vault_keys(&config);
    let server = tokio::spawn({
        let config = config.clone();
        async move { serve::run(&config, listener, signals, keys).await }
    });

    let mut statuses = Vec::new();
    // A burst of 2 refilled once a second: the third of these quick
    // requests is refused, unless each took a second.
    for _ in 0..20 {
        let answer = healthcheck::get(address, "/health/live", PATIENCE)
            .await
            .unwrap();
        statuses.push(answer.status);
        if answer.status != 200 {
            break;
        }
    }

    assert_eq!(statuses.first(), Some(&200), "{statuses:?}");
    assert_eq!(statuses.last(), Some(&429), "{statuses:?}");
    sender.send(Signal::Terminate).await.unwrap();
    let exit = tokio::time::timeout(PATIENCE, server)
        .await
        .expect("the server should stop within its deadline")
        .unwrap();
    assert_eq!(exit, Exit::Stopped);
}

// --- Offline accounts kept with the switch off (P9.7) ---------------------------

/// What the start logs about them.
const KEPT_OFFLINE: &str = "offline accounts are kept while [accounts] allow_offline is off; no bot may use them until it's on again";

/// Adds `offline` offline accounts to the database `config` names,
/// migrating it first, then closes it.
async fn keep_offline_accounts(config: &ServerConfig, offline: u8) {
    let database = sqlite::open(&config.database, DatabaseOptions::default())
        .await
        .unwrap();
    let at = DateTime::from_timestamp_millis(1_800_000_000_000).unwrap();
    let owner = UserId::new_v7(at, [1; 10]).unwrap();
    let mut tx = database.write().await.unwrap();
    tx.users()
        .insert(&NewUser {
            id: owner,
            username: Username::try_from("user1").unwrap(),
            password_hash: PasswordHash::new(SecretString::from(
                "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g",
            ))
            .unwrap(),
            role: Role::Member,
            created_at: at,
        })
        .await
        .unwrap();
    for n in 1..=offline {
        tx.accounts()
            .insert(&McAccount {
                id: AccountId::new_v7(at, [n; 10]).unwrap(),
                owner,
                name: format!("OffBot{n}").parse().unwrap(),
                created_at: at,
                credential: AccountCredential::Offline,
            })
            .await
            .unwrap();
    }
    tx.commit(NewAuditEntry::new(
        at,
        AuditAction::try_from("test.boot_setup").unwrap(),
        AuditOutcome::Success,
    ))
    .await
    .unwrap();
    database.close().await;
}

/// Starts `config`'s server, stops it at once, and returns what it logged
/// about kept offline accounts.
async fn kept_offline_lines(config: &ServerConfig) -> Vec<serde_json::Value> {
    let (listener, _) = listener().await;
    let (logs, _guard) = capture();
    let (sender, signals) = ChannelSignals::new();
    sender.send(Signal::Terminate).await.unwrap();

    let exit = tokio::time::timeout(
        PATIENCE,
        serve::run(config, listener, signals, vault_keys(config)),
    )
    .await
    .unwrap();

    assert_eq!(exit, Exit::Stopped, "{}", logs.text());
    logs.lines_with(KEPT_OFFLINE).unwrap()
}

#[tokio::test]
async fn a_start_warns_once_about_offline_accounts_kept_while_they_are_off() {
    let dir = test_dir("boot_offline_kept");
    let (_, address) = listener().await;
    let config = server_config(&dir, address, false);
    keep_offline_accounts(&config, 2).await;

    let lines = kept_offline_lines(&config).await;

    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["level"], "WARN");
    assert_eq!(lines[0]["offline_accounts"], 2);
}

#[tokio::test]
async fn no_warning_while_offline_accounts_are_allowed_or_none_is_kept() {
    let on = test_dir("boot_offline_on");
    let (_, address) = listener().await;
    let mut allowed = server_config(&on, address, false);
    allowed.accounts.allow_offline = true;
    keep_offline_accounts(&allowed, 1).await;
    let none = test_dir("boot_offline_none");
    let off = server_config(&none, address, false);
    keep_offline_accounts(&off, 0).await;

    assert_eq!(kept_offline_lines(&allowed).await.len(), 0);
    assert_eq!(kept_offline_lines(&off).await.len(), 0);
}
