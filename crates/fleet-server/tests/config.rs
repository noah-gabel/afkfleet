//! `fleet_server::config::load` against real files and environment
//! variables. Every test runs inside `figment::Jail`: its own directory, and
//! an environment that's restored afterwards.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use figment::Jail;
use fleet_server::config::{
    ConfigError, DatabaseConfig, HttpConfig, LogConfig, LogFilterError, LogFormat, ParseError,
    ParseProblem, ParseSource, ProblemKind, Problems, ServerConfig, UnknownLogFormatError, load,
};
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::jail::in_jail;
use fleet_testkit::log_buffer::LogBuffer;
use rstest::rstest;
use tracing_subscriber::layer::SubscriberExt as _;

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
    format!("[database]\npath = {}\n", toml_path(&db_path(jail)))
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
fn only_the_database_path_is_required() {
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
                },
                database: DatabaseConfig {
                    path: db_path(jail),
                },
                log: LogConfig::default(),
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
request_timeout_secs = 30
max_body_bytes = 131072

[database]
path = {}

[log]
format = "pretty"
filter = "debug,sqlx=warn"
"#,
            toml_path(&db_path(jail))
        );

        let config = load_toml(jail, &toml).unwrap();

        assert!(config.dev_mode);
        assert_eq!(config.http.bind, addr("0.0.0.0:9090"));
        assert_eq!(config.http.request_timeout, Duration::from_secs(30));
        assert_eq!(config.http.max_body_bytes, 131_072);
        assert_eq!(config.database.path, db_path(jail));
        assert_eq!(config.log.format, LogFormat::Pretty);
        assert_eq!(config.log.filter.as_str(), "debug,sqlx=warn");
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
        let toml = minimal(jail);

        let config = load_toml(jail, &toml).unwrap();

        assert!(config.dev_mode);
        assert_eq!(config.http.bind, addr("[::1]:8443"));
        assert_eq!(config.http.max_body_bytes, 2048);
        assert_eq!(config.database.path, other);
        assert_eq!(config.log.format, LogFormat::Pretty);
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
                expected: &["dev_mode", "http", "database", "log"]
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
        let problems = invalid(load_toml(jail, toml));

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
        let problems = invalid(load_toml(jail, &format!("[database]\npath = '{path}'\n")));

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
#[case::request_timeout("request_timeout_secs", 1, 300)]
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
            ]
        );
        let shown = ConfigError::Invalid(problems).to_string();
        assert!(!shown.contains("marker"), "{shown}");
    });
}
