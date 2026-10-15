//! `fleet_server::config::load` against real files and environment
//! variables. Every test runs inside `figment::Jail`: its own directory, and
//! an environment that's restored afterwards.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::num::NonZeroU32;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use figment::Jail;
use fleet_core::auth::{InviteTtl, LockoutPolicy, SessionPolicy};
use fleet_server::config::{
    AccountsConfig, Argon2Params, AuthConfig, ConfigError, DatabaseConfig, HttpConfig,
    LinkFlowLimits, LogConfig, LogFilterError, LogFormat, ParseError, ParseProblem, ParseSource,
    ProblemKind, Problems, Rate, RateLimitConfig, ServerConfig, UnknownLogFormatError, VaultConfig,
    load,
};
use fleet_server::http::rate_limit::{IpNet, IpNetError, TrustedProxies};
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::jail::in_jail;
use fleet_testkit::log_buffer::LogBuffer;
use rstest::rstest;
use tracing_subscriber::layer::SubscriberExt as _;

/// A day in seconds.
const DAY: u64 = 24 * 60 * 60;

/// The dev-mode warning, word for word.
const DEV_MODE_WARNING: &str = "dev mode is on: development-only features are enabled; never run a production server in dev mode";

/// An absolute path for the database, inside the jail.
fn db_path(jail: &Jail) -> PathBuf {
    jail.directory().join("afkfleet.db")
}

/// A TOML string for `path`; a literal string, so Windows backslashes stay.
fn toml_path(path: &Path) -> String {
    format!("'{}'", path.display())
}

/// The smallest valid config: only the database path.
fn minimal(jail: &Jail) -> String {
    format!(
        "[database]\npath = {}\n{}",
        toml_path(&db_path(jail)),
        vault(jail)
    )
}

/// An absolute path for vault key `id`, inside the jail. Loading the config
/// never reads it, so the file doesn't exist.
fn key_path(jail: &Jail, id: u32) -> PathBuf {
    jail.directory().join(format!("vault-key-{id}"))
}

/// The smallest valid `[vault]`: key 1, active.
fn vault(jail: &Jail) -> String {
    format!(
        "[vault]\nmaster_key_files = {{ 1 = {} }}\nactive_key_id = 1\n",
        toml_path(&key_path(jail, 1))
    )
}

/// The config of [`vault`].
fn vault_config(jail: &Jail) -> VaultConfig {
    VaultConfig {
        key_files: BTreeMap::from([(1, key_path(jail, 1))]),
        active_key_id: 1,
    }
}

fn load_toml(jail: &mut Jail, toml: &str) -> Result<ServerConfig, ConfigError> {
    jail.create_file("server.toml", toml).unwrap();
    load(&jail.directory().join("server.toml"))
}

fn invalid(result: Result<ServerConfig, ConfigError>) -> Problems {
    match result {
        Err(ConfigError::Invalid(problems)) => problems,
        other => panic!("expected validation problems, got {other:?}"),
    }
}

fn parse_error(result: Result<ServerConfig, ConfigError>) -> ParseError {
    match result {
        Err(ConfigError::Parse(error)) => *error,
        other => panic!("expected a parse error, got {other:?}"),
    }
}

fn keys(problems: &Problems) -> Vec<String> {
    problems
        .iter()
        .map(|problem| problem.key().to_string())
        .collect()
}

fn kinds(problems: &Problems) -> Vec<ProblemKind> {
    problems
        .iter()
        .map(|problem| problem.kind().clone())
        .collect()
}

fn addr(text: &str) -> SocketAddr {
    text.parse().unwrap()
}

fn net(text: &str) -> IpNet {
    text.parse().unwrap()
}

fn nonzero(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap()
}

fn is_the_file(source: &ParseSource) -> bool {
    matches!(source, ParseSource::File { path } if path.ends_with("server.toml"))
}

/// Runs `warn_if_dev_mode` under fleet-startup's layer, as the server logs.
fn dev_mode_lines(config: &ServerConfig) -> LogBuffer {
    let buffer = LogBuffer::default();
    let subscriber = tracing_subscriber::registry().with(layer_with(
        &LogConfig::default(),
        &NoRules,
        false,
        buffer.clone(),
    ));
    tracing::subscriber::with_default(subscriber, || config.warn_if_dev_mode());
    buffer
}

// --- Valid configs ---------------------------------------------------------

#[test]
fn only_the_database_path_and_the_vault_are_required() {
    in_jail(|jail| {
        let toml = minimal(jail);

        let config = load_toml(jail, &toml).unwrap();

        assert_eq!(
            config,
            ServerConfig {
                dev_mode: false,
                http: HttpConfig {
                    bind: addr("127.0.0.1:8080"),
                    request_timeout: Duration::from_secs(15),
                    max_body_bytes: 65_536,
                    trusted_proxies: TrustedProxies::default(),
                },
                database: DatabaseConfig {
                    path: db_path(jail),
                },
                log: LogConfig::default(),
                auth: AuthConfig {
                    argon2: Argon2Params::default(),
                    sessions: SessionPolicy::default(),
                    lockout: LockoutPolicy::default(),
                    invites: InviteTtl::default(),
                },
                vault: vault_config(jail),
                rate_limits: RateLimitConfig::default(),
                accounts: AccountsConfig::default(),
            }
        );
    });
}

#[test]
fn every_key_maps_to_its_setting() {
    in_jail(|jail| {
        let toml = format!(
            r#"
dev_mode = true

[http]
bind = "0.0.0.0:9090"
request_timeout_secs = 18
max_body_bytes = 131072
trusted_proxies = ["192.0.2.0/24", "2001:db8::/32"]

[database]
path = {}

[log]
format = "pretty"
filter = "debug,sqlx=warn"

[auth]
argon2_memory_kib = 65536
argon2_iterations = 3
argon2_parallelism = 4
argon2_max_concurrent = 8
access_token_ttl_secs = 600
refresh_token_idle_days = 14
refresh_token_absolute_days = 60
lockout_threshold = 7
invite_ttl_hours = 24

[rate_limits]
per_ip_per_sec = 50
per_ip_burst = 100
login_per_ip_per_min = 10
refresh_per_ip_per_min = 60
register_per_ip_per_hour = 2
per_user_per_sec = 20
per_user_burst = 60
link_flows_total = 5
link_flows_per_user_per_day = 20

[accounts]
max_per_member = 7
allow_offline = true

[vault]
master_key_files = {{ 3 = {} }}
active_key_id = 3
"#,
            toml_path(&db_path(jail)),
            toml_path(&key_path(jail, 3))
        );

        let config = load_toml(jail, &toml).unwrap();

        assert!(config.dev_mode);
        assert_eq!(config.http.bind, addr("0.0.0.0:9090"));
        assert_eq!(config.http.request_timeout, Duration::from_secs(18));
        assert_eq!(config.http.max_body_bytes, 131_072);
        assert_eq!(
            config.http.trusted_proxies,
            TrustedProxies::new(vec![net("192.0.2.0/24"), net("2001:db8::/32")])
        );
        assert_eq!(config.database.path, db_path(jail));
        assert_eq!(config.log.format, LogFormat::Pretty);
        assert_eq!(config.log.filter.as_str(), "debug,sqlx=warn");
        assert_eq!(
            config.auth.argon2,
            Argon2Params::try_new(65_536, 3, 4, 8).unwrap()
        );
        assert_eq!(
            config.auth.sessions,
            SessionPolicy::try_new(
                Duration::from_secs(600),
                Duration::from_secs(14 * DAY),
                Duration::from_secs(60 * DAY)
            )
            .unwrap()
        );
        assert_eq!(
            config.vault,
            VaultConfig {
                key_files: BTreeMap::from([(3, key_path(jail, 3))]),
                active_key_id: 3,
            }
        );
        assert_eq!(config.auth.lockout, LockoutPolicy::try_new(7).unwrap());
        assert_eq!(config.auth.invites, InviteTtl::try_new(24).unwrap());
        assert_eq!(
            config.rate_limits,
            RateLimitConfig {
                per_ip: Rate::per_second(nonzero(50), nonzero(100)),
                login: Rate::per_minute(nonzero(10)),
                refresh: Rate::per_minute(nonzero(60)),
                register: Rate::per_hour(nonzero(2)),
                per_user: Rate::per_second(nonzero(20), nonzero(60)),
                link_flows: LinkFlowLimits {
                    total: nonzero(5),
                    per_user_per_day: nonzero(20),
                },
            }
        );
        assert_eq!(
            config.accounts,
            AccountsConfig {
                max_per_member: nonzero(7),
                allow_offline: true,
            }
        );
    });
}

#[test]
fn environment_variables_override_the_file() {
    in_jail(|jail| {
        let other = jail.directory().join("other.db");
        jail.set_env("AFKFLEET_SERVER__DEV_MODE", true);
        jail.set_env("AFKFLEET_SERVER__HTTP__BIND", "[::1]:8443");
        jail.set_env("AFKFLEET_SERVER__HTTP__MAX_BODY_BYTES", 2048);
        jail.set_env("AFKFLEET_SERVER__DATABASE__PATH", other.display());
        jail.set_env("AFKFLEET_SERVER__LOG__FORMAT", "pretty");
        jail.set_env("AFKFLEET_SERVER__AUTH__ARGON2_ITERATIONS", 5);
        jail.set_env("AFKFLEET_SERVER__AUTH__ACCESS_TOKEN_TTL_SECS", 600);
        jail.set_env(
            "AFKFLEET_SERVER__HTTP__TRUSTED_PROXIES",
            r#"["192.0.2.0/24", "2001:db8::/32"]"#,
        );
        jail.set_env("AFKFLEET_SERVER__AUTH__LOCKOUT_THRESHOLD", 9);
        jail.set_env("AFKFLEET_SERVER__AUTH__INVITE_TTL_HOURS", 48);
        jail.set_env("AFKFLEET_SERVER__RATE_LIMITS__PER_IP_BURST", 80);
        jail.set_env("AFKFLEET_SERVER__RATE_LIMITS__LINK_FLOWS_TOTAL", 2);
        jail.set_env("AFKFLEET_SERVER__ACCOUNTS__MAX_PER_MEMBER", 1);
        jail.set_env("AFKFLEET_SERVER__ACCOUNTS__ALLOW_OFFLINE", true);
        let toml = minimal(jail);

        let config = load_toml(jail, &toml).unwrap();

        assert!(config.dev_mode);
        assert_eq!(config.http.bind, addr("[::1]:8443"));
        assert_eq!(config.http.max_body_bytes, 2048);
        assert_eq!(config.database.path, other);
        assert_eq!(config.log.format, LogFormat::Pretty);
        assert_eq!(config.auth.argon2.iterations(), 5);
        assert_eq!(config.auth.sessions.access_ttl(), Duration::from_secs(600));
        assert_eq!(
            config.http.trusted_proxies,
            TrustedProxies::new(vec![net("192.0.2.0/24"), net("2001:db8::/32")])
        );
        assert_eq!(config.auth.lockout.threshold(), 9);
        assert_eq!(config.auth.invites.hours(), 48);
        assert_eq!(config.rate_limits.per_ip.burst().get(), 80);
        assert_eq!(config.rate_limits.link_flows.total.get(), 2);
        assert_eq!(config.accounts.max_per_member.get(), 1);
        assert!(config.accounts.allow_offline);
    });
}

#[rstest]
#[case::any_ipv4("0.0.0.0:8080")]
#[case::any_ipv6("[::]:8080")]
#[case::loopback_ipv6("[::1]:8443")]
#[case::highest_port("127.0.0.1:65535")]
fn bind_takes_any_ip_address_with_a_fixed_port(#[case] bind: &str) {
    in_jail(|jail| {
        let toml = format!("{}\n[http]\nbind = \"{bind}\"\n", minimal(jail));

        let config = load_toml(jail, &toml).unwrap();

        assert_eq!(config.http.bind, addr(bind));
    });
}

// --- The dev-mode warning ----------------------------------------------------

#[test]
fn dev_mode_logs_one_warning_when_it_is_on() {
    in_jail(|jail| {
        let toml = format!("dev_mode = true\n{}", minimal(jail));
        let config = load_toml(jail, &toml).unwrap();

        let buffer = dev_mode_lines(&config);

        let lines = buffer.json_lines().unwrap();
        assert_eq!(lines.len(), 1, "{}", buffer.text());
        assert_eq!(lines[0]["level"], "WARN");
        assert_eq!(lines[0]["message"], DEV_MODE_WARNING);
    });
}

#[test]
fn dev_mode_is_off_by_default_and_logs_nothing() {
    in_jail(|jail| {
        let toml = minimal(jail);
        let config = load_toml(jail, &toml).unwrap();

        let buffer = dev_mode_lines(&config);

        assert!(!config.dev_mode);
        assert_eq!(buffer.text(), "");
    });
}

// --- Errors that stop loading -----------------------------------------------

#[test]
fn a_missing_file_is_an_error_naming_its_path() {
    in_jail(|jail| {
        let path = jail.directory().join("missing.toml");
        assert_eq!(load(&path), Err(ConfigError::NotFound { path }));
    });
}

#[test]
fn a_misspelled_key_in_the_file_names_the_key_and_the_file() {
    in_jail(|jail| {
        let toml = format!("{}\n[http]\nbnd = \"127.0.0.1:8080\"\n", minimal(jail));
        let error = parse_error(load_toml(jail, &toml));

        assert_eq!(error.key().to_string(), "http.bnd");
        assert!(matches!(error.problem(), ParseProblem::UnknownKey { .. }));
        assert!(is_the_file(error.source()), "{:?}", error.source());
    });
}

#[test]
fn a_misspelled_environment_variable_names_the_variable() {
    in_jail(|jail| {
        jail.set_env("AFKFLEET_SERVER__HTTP__BINDD", "\"127.0.0.1:8080\"");
        let toml = minimal(jail);
        let error = parse_error(load_toml(jail, &toml));

        assert_eq!(error.key().to_string(), "http.bindd");
        assert!(matches!(error.problem(), ParseProblem::UnknownKey { .. }));
        assert_eq!(
            error.source(),
            &ParseSource::Env {
                variable: "AFKFLEET_SERVER__HTTP__BINDD".to_owned()
            }
        );
    });
}

#[test]
fn a_section_of_a_later_phase_is_unknown_until_that_phase() {
    in_jail(|jail| {
        let toml = format!("{}\n[grpc]\nbind = \"0.0.0.0:7443\"\n", minimal(jail));
        let error = parse_error(load_toml(jail, &toml));

        assert_eq!(error.key().to_string(), "grpc");
        assert_eq!(
            error.problem(),
            &ParseProblem::UnknownKey {
                expected: &[
                    "dev_mode",
                    "http",
                    "database",
                    "log",
                    "auth",
                    "vault",
                    "rate_limits",
                    "accounts"
                ]
            }
        );
    });
}

#[test]
fn a_value_of_the_wrong_type_names_the_key_and_the_file() {
    in_jail(|jail| {
        let toml = format!("{}\n[http]\nrequest_timeout_secs = \"15\"\n", minimal(jail));
        let error = parse_error(load_toml(jail, &toml));

        assert_eq!(error.key().to_string(), "http.request_timeout_secs");
        assert!(matches!(error.problem(), ParseProblem::WrongType { .. }));
        assert!(is_the_file(error.source()), "{:?}", error.source());
    });
}

// --- Validation ---------------------------------------------------------------

#[rstest]
#[case::no_section("")]
#[case::no_key("[database]\n")]
fn the_database_path_is_required(#[case] toml: &str) {
    in_jail(|jail| {
        let toml = format!("{toml}{}", vault(jail));
        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(keys(&problems), ["database.path"]);
        assert_eq!(kinds(&problems), [ProblemKind::Missing]);
    });
}

#[rstest]
#[case::empty("", ProblemKind::Empty)]
#[case::file_name("afkfleet.db", ProblemKind::NotAbsolute)]
#[case::dot_relative("./data/afkfleet.db", ProblemKind::NotAbsolute)]
#[case::parent_relative("../afkfleet.db", ProblemKind::NotAbsolute)]
fn the_database_path_must_be_absolute(#[case] path: &str, #[case] kind: ProblemKind) {
    in_jail(|jail| {
        let toml = format!("[database]\npath = '{path}'\n{}", vault(jail));
        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(keys(&problems), ["database.path"]);
        assert_eq!(kinds(&problems), [kind]);
    });
}

#[rstest]
#[case::host_name("localhost:8080", ProblemKind::SocketAddress)]
#[case::no_port("127.0.0.1", ProblemKind::SocketAddress)]
#[case::port_too_large("127.0.0.1:65536", ProblemKind::SocketAddress)]
#[case::ipv6_without_brackets("::1:8080", ProblemKind::SocketAddress)]
#[case::empty("", ProblemKind::SocketAddress)]
#[case::port_zero("127.0.0.1:0", ProblemKind::PortZero)]
#[case::port_zero_ipv6("[::]:0", ProblemKind::PortZero)]
fn bind_must_be_an_ip_address_with_a_fixed_port(#[case] bind: &str, #[case] kind: ProblemKind) {
    in_jail(|jail| {
        let toml = format!("{}\n[http]\nbind = \"{bind}\"\n", minimal(jail));
        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(keys(&problems), ["http.bind"]);
        assert_eq!(kinds(&problems), [kind]);
    });
}

/// A key, and its lowest and highest valid values.
#[rstest]
#[case::request_timeout("request_timeout_secs", 1, 19)]
#[case::max_body("max_body_bytes", 1024, 1_048_576)]
fn numbers_are_checked_against_their_range(#[case] key: &str, #[case] min: u64, #[case] max: u64) {
    in_jail(|jail| {
        let base = minimal(jail);
        let with = |value: u64| format!("{base}\n[http]\n{key} = {value}\n");

        for value in [min, max] {
            assert!(load_toml(jail, &with(value)).is_ok(), "{key} = {value}");
        }
        for value in [min - 1, max + 1] {
            let problems = invalid(load_toml(jail, &with(value)));
            assert_eq!(keys(&problems), [format!("http.{key}")], "{key} = {value}");
            assert!(
                matches!(
                    problems.iter().next().unwrap().kind(),
                    ProblemKind::OutOfRange { .. }
                ),
                "{key} = {value}"
            );
        }
    });
}

/// An `[auth]` key, and its lowest and highest valid values: nothing below
/// §7.2's baseline (ADR-0016).
#[rstest]
#[case::memory("argon2_memory_kib", 19_456, 1_048_576)]
#[case::iterations("argon2_iterations", 2, 10)]
#[case::parallelism("argon2_parallelism", 1, 16)]
#[case::max_concurrent("argon2_max_concurrent", 1, 64)]
fn argon2_keys_are_checked_against_their_range(
    #[case] key: &str,
    #[case] min: u64,
    #[case] max: u64,
) {
    in_jail(|jail| {
        let base = minimal(jail);
        let with = |value: u64| format!("{base}\n[auth]\n{key} = {value}\n");

        for value in [min, max] {
            assert!(load_toml(jail, &with(value)).is_ok(), "{key} = {value}");
        }
        for value in [min - 1, max + 1] {
            let problems = invalid(load_toml(jail, &with(value)));
            assert_eq!(keys(&problems), [format!("auth.{key}")], "{key} = {value}");
            assert!(
                matches!(
                    problems.iter().next().unwrap().kind(),
                    ProblemKind::OutOfRange { .. }
                ),
                "{key} = {value}"
            );
        }
    });
}

/// A token lifetime key, its lowest and highest valid values, and a setting
/// that keeps the idle lifetime within the absolute one at both ends
/// (ADR-0016).
#[rstest]
#[case::access("access_token_ttl_secs", 60, 3600, "")]
#[case::idle("refresh_token_idle_days", 1, 90, "refresh_token_absolute_days = 365")]
#[case::absolute("refresh_token_absolute_days", 1, 365, "refresh_token_idle_days = 1")]
fn token_lifetimes_are_checked_against_their_range(
    #[case] key: &str,
    #[case] min: u64,
    #[case] max: u64,
    #[case] other: &str,
) {
    in_jail(|jail| {
        let base = minimal(jail);
        let with = |value: u64| format!("{base}\n[auth]\n{other}\n{key} = {value}\n");

        for value in [min, max] {
            assert!(load_toml(jail, &with(value)).is_ok(), "{key} = {value}");
        }
        for value in [min - 1, max + 1] {
            let problems = invalid(load_toml(jail, &with(value)));
            assert_eq!(keys(&problems), [format!("auth.{key}")], "{key} = {value}");
            assert!(
                matches!(
                    problems.iter().next().unwrap().kind(),
                    ProblemKind::OutOfRange { .. }
                ),
                "{key} = {value}"
            );
        }
    });
}

#[test]
fn an_idle_lifetime_longer_than_the_absolute_one_is_refused() {
    in_jail(|jail| {
        let toml = format!(
            "{}\n[auth]\nrefresh_token_idle_days = 31\nrefresh_token_absolute_days = 30\n",
            minimal(jail)
        );

        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(keys(&problems), ["auth.refresh_token_idle_days"]);
        assert_eq!(
            kinds(&problems),
            [ProblemKind::GreaterThanKey {
                key: "refresh_token_absolute_days"
            }]
        );
        assert_eq!(
            problems.iter().next().unwrap().kind().to_string(),
            "greater than `refresh_token_absolute_days`"
        );
    });
}

#[test]
fn an_idle_lifetime_equal_to_the_absolute_one_is_accepted() {
    in_jail(|jail| {
        let toml = format!(
            "{}\n[auth]\nrefresh_token_idle_days = 30\nrefresh_token_absolute_days = 30\n",
            minimal(jail)
        );

        let config = load_toml(jail, &toml).unwrap();

        assert_eq!(config.auth.sessions.idle(), config.auth.sessions.absolute());
    });
}

#[test]
fn a_lifetime_out_of_range_isnt_also_compared_with_the_other() {
    in_jail(|jail| {
        let toml = format!(
            "{}\n[auth]\nrefresh_token_idle_days = 91\nrefresh_token_absolute_days = 30\n",
            minimal(jail)
        );

        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(keys(&problems), ["auth.refresh_token_idle_days"]);
        assert!(matches!(
            problems.iter().next().unwrap().kind(),
            ProblemKind::OutOfRange { .. }
        ));
    });
}

#[test]
fn every_argon2_key_out_of_range_is_reported_at_once() {
    in_jail(|jail| {
        let toml = format!(
            "{}\n[auth]\nargon2_memory_kib = 1024\nargon2_iterations = 1\nargon2_parallelism = 0\nargon2_max_concurrent = 0\n",
            minimal(jail)
        );

        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(
            keys(&problems),
            [
                "auth.argon2_iterations",
                "auth.argon2_max_concurrent",
                "auth.argon2_memory_kib",
                "auth.argon2_parallelism",
            ]
        );
    });
}

#[test]
fn bad_log_settings_are_refused() {
    in_jail(|jail| {
        let toml = format!(
            "{}\n[log]\nformat = \"compact\"\nfilter = \"info,sqlx=loud\"\n",
            minimal(jail)
        );
        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(keys(&problems), ["log.filter", "log.format"]);
        assert_eq!(
            kinds(&problems),
            [
                ProblemKind::LogFilter(LogFilterError::InvalidDirective { index: 1 }),
                ProblemKind::LogFormat(UnknownLogFormatError),
            ]
        );
        assert!(!problems.to_string().contains("loud"));
    });
}

#[test]
fn every_problem_is_reported_at_once_without_values() {
    in_jail(|jail| {
        let problems = invalid(load_toml(
            jail,
            r#"
[http]
bind = "marker1"
request_timeout_secs = 0
max_body_bytes = 5

[database]
path = "marker2.db"

[log]
format = "marker3"
filter = "marker4=loud"

[vault]
master_key_files = { 1 = "marker5" }
active_key_id = 7
"#,
        ));

        assert_eq!(
            keys(&problems),
            [
                "database.path",
                "http.bind",
                "http.max_body_bytes",
                "http.request_timeout_secs",
                "log.filter",
                "log.format",
                "vault.master_key_files.1",
            ]
        );
        let shown = ConfigError::Invalid(problems).to_string();
        assert!(!shown.contains("marker"), "{shown}");
    });
}

// --- [vault] -------------------------------------------------------------------------

#[test]
fn the_vault_s_key_files_map_their_ids_to_absolute_paths() {
    in_jail(|jail| {
        let toml = format!(
            "[database]\npath = {}\n[vault]\nmaster_key_files = {{ 1 = {}, 4294967295 = {} }}\nactive_key_id = 4294967295\n",
            toml_path(&db_path(jail)),
            toml_path(&key_path(jail, 1)),
            toml_path(&key_path(jail, 2)),
        );

        let config = load_toml(jail, &toml).unwrap();

        assert_eq!(
            config.vault,
            VaultConfig {
                key_files: BTreeMap::from([
                    (1, key_path(jail, 1)),
                    (4_294_967_295, key_path(jail, 2)),
                ]),
                active_key_id: 4_294_967_295,
            }
        );
    });
}

#[test]
fn a_key_file_can_come_from_the_environment() {
    in_jail(|jail| {
        jail.set_env(
            "AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__2",
            key_path(jail, 2).display(),
        );
        let toml = format!(
            "[database]\npath = {}\n[vault]\nactive_key_id = 2\n",
            toml_path(&db_path(jail))
        );

        let config = load_toml(jail, &toml).unwrap();

        assert_eq!(
            config.vault.key_files,
            BTreeMap::from([(2, key_path(jail, 2))])
        );
        assert_eq!(config.vault.active_key_id, 2);
    });
}

#[test]
fn the_vault_section_is_required() {
    in_jail(|jail| {
        let toml = format!("[database]\npath = {}\n", toml_path(&db_path(jail)));

        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(
            keys(&problems),
            ["vault.active_key_id", "vault.master_key_files"]
        );
        assert_eq!(
            kinds(&problems),
            [ProblemKind::Missing, ProblemKind::Missing]
        );
    });
}

#[rstest]
#[case::zero("0")]
#[case::too_large("4294967296")]
#[case::leading_zero("01")]
#[case::plus_sign("+1")]
#[case::negative("-1")]
#[case::not_a_number("one")]
fn a_key_file_s_id_must_be_a_canonical_u32_from_1(#[case] id: &str) {
    in_jail(|jail| {
        let toml = format!(
            "[database]\npath = {}\n[vault]\nmaster_key_files = {{ \"{id}\" = {} }}\nactive_key_id = 1\n",
            toml_path(&db_path(jail)),
            toml_path(&key_path(jail, 1)),
        );

        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(keys(&problems), [format!("vault.master_key_files.{id}")]);
        assert_eq!(kinds(&problems), [ProblemKind::KeyId]);
    });
}

#[rstest]
#[case::none(0)]
#[case::seventeen(17)]
fn the_vault_lists_1_to_16_key_files(#[case] count: u32) {
    in_jail(|jail| {
        let entries: Vec<String> = (1..=count)
            .map(|id| format!("{id} = {}", toml_path(&key_path(jail, id))))
            .collect();
        let toml = format!(
            "[database]\npath = {}\n[vault]\nmaster_key_files = {{ {} }}\nactive_key_id = 1\n",
            toml_path(&db_path(jail)),
            entries.join(", ")
        );

        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(keys(&problems), ["vault.master_key_files"]);
        assert_eq!(kinds(&problems), [ProblemKind::KeyCount { max: 16 }]);
    });
}

#[test]
fn sixteen_key_files_are_allowed() {
    in_jail(|jail| {
        let entries: Vec<String> = (1..=16)
            .map(|id| format!("{id} = {}", toml_path(&key_path(jail, id))))
            .collect();
        let toml = format!(
            "[database]\npath = {}\n[vault]\nmaster_key_files = {{ {} }}\nactive_key_id = 16\n",
            toml_path(&db_path(jail)),
            entries.join(", ")
        );

        let config = load_toml(jail, &toml).unwrap();

        assert_eq!(config.vault.key_files.len(), 16);
    });
}

#[rstest]
#[case::empty("", ProblemKind::Empty)]
#[case::file_name("vault-key-1", ProblemKind::NotAbsolute)]
#[case::relative("./secrets/vault-key-1", ProblemKind::NotAbsolute)]
fn a_key_file_s_path_must_be_absolute(#[case] path: &str, #[case] kind: ProblemKind) {
    in_jail(|jail| {
        let toml = format!(
            "[database]\npath = {}\n[vault]\nmaster_key_files = {{ 1 = '{path}' }}\nactive_key_id = 1\n",
            toml_path(&db_path(jail)),
        );

        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(keys(&problems), ["vault.master_key_files.1"]);
        assert_eq!(kinds(&problems), [kind]);
    });
}

#[rstest]
#[case::zero(0, ProblemKind::KeyId)]
#[case::too_large(4_294_967_296, ProblemKind::KeyId)]
#[case::not_listed(2, ProblemKind::UnknownKeyId)]
fn the_active_key_must_be_one_of_the_key_files(#[case] active: u64, #[case] kind: ProblemKind) {
    in_jail(|jail| {
        let toml = format!(
            "[database]\npath = {}\n[vault]\nmaster_key_files = {{ 1 = {} }}\nactive_key_id = {active}\n",
            toml_path(&db_path(jail)),
            toml_path(&key_path(jail, 1)),
        );

        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(keys(&problems), ["vault.active_key_id"]);
        assert_eq!(kinds(&problems), [kind]);
    });
}

/// `deploy/dev/server.toml`, the config `just dev-server` runs.
fn dev_config() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/dev/server.toml")
}

#[test]
fn the_dev_config_runs_in_dev_mode_with_the_database_and_vault_key_from_the_environment() {
    in_jail(|jail| {
        let path = db_path(jail);
        jail.set_env("AFKFLEET_SERVER__DATABASE__PATH", path.display());
        jail.set_env(
            "AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1",
            key_path(jail, 1).display(),
        );

        let config = load(&dev_config()).unwrap();

        assert!(config.dev_mode);
        assert_eq!(config.http.bind, HttpConfig::DEFAULT_BIND);
        assert_eq!(config.database.path, path);
        assert_eq!(config.log.format, LogFormat::Pretty);
        assert_eq!(config.vault, vault_config(jail));
        assert!(config.accounts.allow_offline);
    });
}

/// `just dev-rotate-key` (keys 1 and 2, active 2) and `just dev-server-rotated`
/// (key 2 only) set the active key through the environment over the dev
/// config's `active_key_id = 1` (ADR-0018).
#[rstest]
#[case::rotation(&[1, 2])]
#[case::rotated_server(&[2])]
fn the_active_key_can_come_from_the_environment(#[case] ids: &[u32]) {
    in_jail(|jail| {
        jail.set_env("AFKFLEET_SERVER__DATABASE__PATH", db_path(jail).display());
        for &id in ids {
            jail.set_env(
                format!("AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__{id}"),
                key_path(jail, id).display(),
            );
        }
        jail.set_env("AFKFLEET_SERVER__VAULT__ACTIVE_KEY_ID", 2);

        let config = load(&dev_config()).unwrap();

        assert_eq!(config.vault.active_key_id, 2);
        assert_eq!(
            config.vault.key_files,
            ids.iter()
                .map(|&id| (id, key_path(jail, id)))
                .collect::<BTreeMap<_, _>>()
        );
    });
}

#[test]
fn the_dev_config_holds_neither_the_database_path_nor_the_vault_key_file() {
    in_jail(|_jail| {
        let problems = invalid(load(&dev_config()));

        assert_eq!(keys(&problems), ["database.path", "vault.master_key_files"]);
        assert_eq!(
            kinds(&problems),
            [ProblemKind::Missing, ProblemKind::Missing]
        );
    });
}

// --- Rate limits, the lockout threshold and trusted proxies (P7.10) --------------------

/// A `[rate_limits]` key, and its lowest and highest valid values (ADR-0016).
#[rstest]
#[case::per_ip("per_ip_per_sec", 1, 1000)]
#[case::per_ip_burst("per_ip_burst", 1, 10_000)]
#[case::login("login_per_ip_per_min", 1, 60)]
#[case::refresh("refresh_per_ip_per_min", 1, 600)]
#[case::register("register_per_ip_per_hour", 1, 100)]
#[case::per_user("per_user_per_sec", 1, 1000)]
#[case::per_user_burst("per_user_burst", 1, 10_000)]
#[case::link_flows_total("link_flows_total", 1, 16)]
#[case::link_flows_per_user_per_day("link_flows_per_user_per_day", 1, 100)]
fn rate_limit_keys_are_checked_against_their_range(
    #[case] key: &str,
    #[case] min: u64,
    #[case] max: u64,
) {
    in_jail(|jail| {
        let base = minimal(jail);
        let with = |value: u64| format!("{base}\n[rate_limits]\n{key} = {value}\n");

        for value in [min, max] {
            assert!(load_toml(jail, &with(value)).is_ok(), "{key} = {value}");
        }
        for value in [min - 1, max + 1] {
            let problems = invalid(load_toml(jail, &with(value)));
            assert_eq!(
                keys(&problems),
                [format!("rate_limits.{key}")],
                "{key} = {value}"
            );
            assert!(
                matches!(
                    problems.iter().next().unwrap().kind(),
                    ProblemKind::OutOfRange { .. }
                ),
                "{key} = {value}"
            );
        }
    });
}

#[test]
fn a_members_quota_is_1_to_100_accounts() {
    in_jail(|jail| {
        let base = minimal(jail);
        let with = |value: u32| format!("{base}\n[accounts]\nmax_per_member = {value}\n");

        for value in [1, 100] {
            let config = load_toml(jail, &with(value)).unwrap();
            assert_eq!(config.accounts.max_per_member.get(), value);
        }
        for (value, message) in [(0, "lower than 1"), (101, "greater than 100")] {
            let problems = invalid(load_toml(jail, &with(value)));
            assert_eq!(keys(&problems), ["accounts.max_per_member"]);
            assert_eq!(
                kinds(&problems),
                [ProblemKind::OutOfRange {
                    message: message.to_owned()
                }]
            );
        }
    });
}

#[test]
fn the_lockout_threshold_is_3_to_20() {
    in_jail(|jail| {
        let base = minimal(jail);
        let with = |value: u32| format!("{base}\n[auth]\nlockout_threshold = {value}\n");

        for value in [3, 20] {
            let config = load_toml(jail, &with(value)).unwrap();
            assert_eq!(config.auth.lockout.threshold(), value);
        }
        for (value, message) in [(2, "lower than 3"), (21, "greater than 20")] {
            let problems = invalid(load_toml(jail, &with(value)));
            assert_eq!(keys(&problems), ["auth.lockout_threshold"]);
            assert_eq!(
                kinds(&problems),
                [ProblemKind::OutOfRange {
                    message: message.to_owned()
                }]
            );
        }
    });
}

#[test]
fn the_invite_lifetime_is_1_to_720_hours() {
    in_jail(|jail| {
        let base = minimal(jail);
        let with = |value: u32| format!("{base}\n[auth]\ninvite_ttl_hours = {value}\n");

        for value in [1, 720] {
            let config = load_toml(jail, &with(value)).unwrap();
            assert_eq!(config.auth.invites.hours(), value);
        }
        for (value, message) in [(0, "lower than 1"), (721, "greater than 720")] {
            let problems = invalid(load_toml(jail, &with(value)));
            assert_eq!(keys(&problems), ["auth.invite_ttl_hours"]);
            assert_eq!(
                kinds(&problems),
                [ProblemKind::OutOfRange {
                    message: message.to_owned()
                }]
            );
        }
    });
}

#[test]
fn an_empty_list_of_trusted_proxies_trusts_nobody() {
    in_jail(|jail| {
        let toml = format!("{}\n[http]\ntrusted_proxies = []\n", minimal(jail));

        let config = load_toml(jail, &toml).unwrap();

        assert_eq!(config.http.trusted_proxies, TrustedProxies::default());
    });
}

#[rstest]
#[case::bare_address("192.0.2.10", IpNetError::Shape)]
#[case::host_name("proxy.example.com/24", IpNetError::Shape)]
#[case::too_long("192.0.2.0/33", IpNetError::PrefixLength { max: 32 })]
#[case::host_bits("192.0.2.5/24", IpNetError::HostBits)]
#[case::ipv6_host_bits("2001:db8::1/32", IpNetError::HostBits)]
#[case::mapped("::ffff:192.0.2.0/120", IpNetError::MappedIpv4)]
#[case::ipv4_everything("0.0.0.0/0", IpNetError::Everything)]
#[case::ipv6_everything("::/0", IpNetError::Everything)]
fn a_trusted_proxy_that_isnt_strict_cidr_is_refused_at_its_index(
    #[case] entry: &str,
    #[case] error: IpNetError,
) {
    in_jail(|jail| {
        let toml = format!(
            "{}\n[http]\ntrusted_proxies = [\"198.51.100.0/24\", \"{entry}\"]\n",
            minimal(jail)
        );

        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(keys(&problems), ["http.trusted_proxies[1]"]);
        assert_eq!(kinds(&problems), [ProblemKind::IpNet(error)]);
        let shown = problems.iter().next().unwrap().kind().to_string();
        assert!(!shown.contains(entry), "{shown}");
    });
}

#[test]
fn a_trusted_proxy_listed_twice_is_refused_at_its_second_index() {
    in_jail(|jail| {
        let toml = format!(
            "{}\n[http]\ntrusted_proxies = [\"192.0.2.0/24\", \"198.51.100.0/24\", \"192.0.2.0/24\"]\n",
            minimal(jail)
        );

        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(keys(&problems), ["http.trusted_proxies[2]"]);
        assert_eq!(kinds(&problems), [ProblemKind::Duplicate]);
    });
}

/// `n` different /32 networks in 198.51.100.0/24, as a TOML list.
fn proxies(n: u8) -> String {
    let entries: Vec<String> = (0..n).map(|i| format!("\"198.51.100.{i}/32\"")).collect();
    format!("[{}]", entries.join(", "))
}

#[test]
fn at_most_32_trusted_proxies_are_allowed() {
    in_jail(|jail| {
        let base = minimal(jail);
        let with = |n: u8| format!("{base}\n[http]\ntrusted_proxies = {}\n", proxies(n));

        let most = load_toml(jail, &with(32)).unwrap();
        let problems = invalid(load_toml(jail, &with(33)));

        assert_eq!(most.http.trusted_proxies.networks().len(), 32);
        assert_eq!(keys(&problems), ["http.trusted_proxies"]);
        assert_eq!(kinds(&problems), [ProblemKind::TooManyEntries { max: 32 }]);
    });
}

#[test]
fn every_bad_trusted_proxy_is_reported_at_once() {
    in_jail(|jail| {
        let toml = format!(
            "{}\n[http]\ntrusted_proxies = [\"192.0.2.10\", \"192.0.2.0/24\", \"2001:db8::1/32\"]\n",
            minimal(jail)
        );

        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(
            keys(&problems),
            ["http.trusted_proxies[0]", "http.trusted_proxies[2]"]
        );
    });
}
