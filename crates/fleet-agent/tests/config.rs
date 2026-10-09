//! `fleet_agent::config::load` against real files and environment variables.
//! Every test runs inside `figment::Jail`: its own directory, and an
//! environment that's restored afterwards.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::Duration;

use figment::Jail;
use fleet_agent::config::{
    AgentConfig, AgentMode, ConfigError, ControlPlaneConfig, LogConfig, LogFilter, LogFilterError,
    LogFormat, ParseError, ParseProblem, ParseSource, ProblemKind, Problems, StandaloneBot,
    UnknownLogFormatError, load,
};
use fleet_core::bot::BotAccount;
use fleet_core::disconnect::{ConflictTexts, ConflictTextsError};
use fleet_core::mode::{ModePreset, UnknownPresetError};
use fleet_core::resilience::{CircuitPolicy, RetryPolicy, RetryPolicyError};
use fleet_core::value::{AgentNameError, McUsernameError};
use fleet_mc::McConfig;
use fleet_runtime::RuntimeConfig;
use jail::in_jail;
use rstest::rstest;

#[path = "common/jail.rs"]
mod jail;

/// The smallest valid config: a name and one bot.
const MINIMAL: &str = r#"
name = "agent-1"

[[standalone.bots]]
username = "AfkBot1"
server = "localhost:25565"
mode = "afk"
"#;

fn load_toml(jail: &mut Jail, toml: &str) -> Result<AgentConfig, ConfigError> {
    jail.create_file("agent.toml", toml).unwrap();
    load(&jail.directory().join("agent.toml"))
}

fn invalid(result: Result<AgentConfig, ConfigError>) -> Problems {
    match result {
        Err(ConfigError::Invalid(problems)) => problems,
        other => panic!("expected validation problems, got {other:?}"),
    }
}

fn parse_error(result: Result<AgentConfig, ConfigError>) -> ParseError {
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

fn nonzero(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap()
}

fn secs(value: u64) -> Duration {
    Duration::from_secs(value)
}

fn bots(config: &AgentConfig) -> &[StandaloneBot] {
    match &config.mode {
        AgentMode::Standalone(bots) => bots,
        AgentMode::ControlPlane(_) => panic!("expected a standalone agent"),
    }
}

fn is_the_file(source: &ParseSource) -> bool {
    matches!(source, ParseSource::File { path } if path.ends_with("agent.toml"))
}

// --- Valid configs ---------------------------------------------------------

#[test]
fn every_key_maps_to_its_setting() {
    in_jail(|jail| {
        let heartbeat = jail.directory().join("agent.alive");
        let toml = format!(
            r#"
name = "agent-1"

[runtime]
max_bots = 40
watchdog_timeout_secs = 31
packet_liveness_timeout_secs = 32
connect_timeout_secs = 33
max_abandoned_threads = 4
shutdown_timeout_secs = 11
heartbeat_file = '{}'

[retry]
base_delay_secs = 6
max_delay_secs = 301
stable_after_secs = 302
circuit_failures = 9
circuit_window_secs = 601
circuit_cooldown_secs = 901

[log]
format = "pretty"
filter = "debug,azalea=info"

[[standalone.bots]]
username = "AfkBot1"
server = "localhost:25565"
mode = "afk"

[[standalone.bots]]
username = "AfkBot2"
server = "mc.example.com"
mode = "farm"
conflict_texts = ["You logged in from another location"]
"#,
            heartbeat.display()
        );

        let config = load_toml(jail, &toml).unwrap();

        assert_eq!(config.name.as_str(), "agent-1");
        assert_eq!(
            config.runtime,
            RuntimeConfig {
                max_bots: nonzero(40),
                watchdog_timeout: secs(31),
                packet_liveness_timeout: secs(32),
                connect_timeout: secs(33),
                shutdown_timeout: secs(11),
                ..RuntimeConfig::default()
            }
        );
        assert_eq!(
            config.mc,
            McConfig {
                max_abandoned_threads: nonzero(4),
                ..McConfig::default()
            }
        );
        assert_eq!(
            config.retry,
            RetryPolicy::try_new(secs(6), secs(301), secs(302)).unwrap()
        );
        assert_eq!(
            config.circuit,
            CircuitPolicy::try_new(nonzero(9), secs(601), secs(901)).unwrap()
        );
        assert_eq!(config.heartbeat_file, heartbeat);
        assert_eq!(
            config.log,
            LogConfig {
                format: LogFormat::Pretty,
                filter: LogFilter::try_from("debug,azalea=info").unwrap(),
            }
        );
        assert_eq!(
            bots(&config),
            [
                StandaloneBot {
                    username: "AfkBot1".parse().unwrap(),
                    server: "localhost:25565".parse().unwrap(),
                    mode: ModePreset::Afk,
                    conflict_texts: ConflictTexts::default(),
                },
                StandaloneBot {
                    username: "AfkBot2".parse().unwrap(),
                    server: "mc.example.com".parse().unwrap(),
                    mode: ModePreset::Farm,
                    conflict_texts: ConflictTexts::try_new(&[
                        "You logged in from another location"
                    ])
                    .unwrap(),
                },
            ]
        );
        assert_eq!(
            bots(&config)[0].account(),
            BotAccount::Offline("AfkBot1".parse().unwrap())
        );
    });
}

#[test]
fn missing_keys_take_their_defaults() {
    in_jail(|jail| {
        let config = load_toml(jail, MINIMAL).unwrap();

        assert_eq!(config.runtime, RuntimeConfig::default());
        assert_eq!(config.mc, McConfig::default());
        assert_eq!(
            config.retry,
            RetryPolicy::try_new(secs(5), secs(300), secs(300)).unwrap()
        );
        assert_eq!(
            config.circuit,
            CircuitPolicy::try_new(nonzero(8), secs(600), secs(900)).unwrap()
        );
        assert_eq!(
            config.heartbeat_file,
            std::env::temp_dir().join("afkfleet-agent.alive")
        );
        assert_eq!(bots(&config)[0].conflict_texts, ConflictTexts::default());
        assert_eq!(config.log, LogConfig::default());
    });
}

#[test]
fn a_control_plane_section_makes_a_managed_agent() {
    in_jail(|jail| {
        let config = load_toml(
            jail,
            r#"
name = "agent-1"

[control_plane]
url = "https://fleet.example.com:7443"
ca_cert_file = "/run/secrets/fleet_ca_cert"
cert_file = "/data/agent.crt"
key_file = "/data/agent.key"
"#,
        )
        .unwrap();

        assert_eq!(
            config.mode,
            AgentMode::ControlPlane(ControlPlaneConfig {
                url: "https://fleet.example.com:7443".to_owned(),
                ca_cert_file: PathBuf::from("/run/secrets/fleet_ca_cert"),
                cert_file: PathBuf::from("/data/agent.crt"),
                key_file: PathBuf::from("/data/agent.key"),
            })
        );
    });
}

#[test]
fn environment_variables_override_the_file() {
    in_jail(|jail| {
        jail.set_env("AFKFLEET_AGENT__RUNTIME__MAX_BOTS", 20);
        jail.set_env("AFKFLEET_AGENT__NAME", "agent-2");

        let config = load_toml(jail, MINIMAL).unwrap();

        assert_eq!(config.runtime.max_bots, nonzero(20));
        assert_eq!(config.name.as_str(), "agent-2");
    });
}

#[test]
fn a_numeric_text_from_the_environment_must_be_quoted() {
    in_jail(|jail| {
        jail.set_env("AFKFLEET_AGENT__NAME", "123");
        let error = parse_error(load_toml(jail, MINIMAL));
        assert_eq!(error.key().to_string(), "name");
        assert!(matches!(error.problem(), ParseProblem::WrongType { .. }));
        assert_eq!(
            error.source(),
            &ParseSource::Env {
                variable: "AFKFLEET_AGENT__NAME".to_owned()
            }
        );

        jail.set_env("AFKFLEET_AGENT__NAME", "\"123\"");
        assert_eq!(load_toml(jail, MINIMAL).unwrap().name.as_str(), "123");
    });
}

#[test]
fn a_log_filter_from_the_environment_stays_text() {
    in_jail(|jail| {
        jail.set_env("AFKFLEET_AGENT__LOG__FILTER", "debug,azalea=info");

        let config = load_toml(jail, MINIMAL).unwrap();

        assert_eq!(config.log.filter.as_str(), "debug,azalea=info");
    });
}

#[test]
fn spaces_between_filter_directives_are_allowed() {
    in_jail(|jail| {
        let toml = format!("{MINIMAL}\n[log]\nfilter = \"debug, azalea_client=info\"\n");

        let config = load_toml(jail, &toml).unwrap();

        assert_eq!(config.log.filter.as_str(), "debug,azalea_client=info");
    });
}

#[test]
fn bad_log_settings_are_refused() {
    in_jail(|jail| {
        let toml =
            format!("{MINIMAL}\n[log]\nformat = \"compact\"\nfilter = \"info,azalea=loud\"\n");
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

// --- Errors that stop loading -----------------------------------------------

#[test]
fn a_missing_file_is_an_error_naming_its_path() {
    in_jail(|jail| {
        let path = jail.directory().join("missing.toml");
        assert_eq!(load(&path), Err(ConfigError::NotFound { path }));
    });
}

#[test]
fn a_file_in_a_parent_directory_is_never_used() {
    in_jail(|jail| {
        jail.create_file("agent.toml", MINIMAL).unwrap();
        jail.create_dir("sub").unwrap();
        jail.change_dir("sub").unwrap();

        assert_eq!(
            load(Path::new("agent.toml")),
            Err(ConfigError::NotFound {
                path: PathBuf::from("agent.toml")
            })
        );
    });
}

#[test]
fn a_misspelled_key_in_the_file_names_the_key_and_the_file() {
    in_jail(|jail| {
        let toml = format!("{MINIMAL}\n[runtime]\nmax_botz = 20\n");
        let error = parse_error(load_toml(jail, &toml));

        assert_eq!(error.key().to_string(), "runtime.max_botz");
        assert!(matches!(error.problem(), ParseProblem::UnknownKey { .. }));
        assert!(is_the_file(error.source()), "{:?}", error.source());
    });
}

#[test]
fn a_misspelled_environment_variable_names_the_variable() {
    in_jail(|jail| {
        jail.set_env("AFKFLEET_AGENT__RUNTIME__MAXBOTS", 20);
        let error = parse_error(load_toml(jail, MINIMAL));

        assert_eq!(error.key().to_string(), "runtime.maxbots");
        assert!(matches!(error.problem(), ParseProblem::UnknownKey { .. }));
        assert_eq!(
            error.source(),
            &ParseSource::Env {
                variable: "AFKFLEET_AGENT__RUNTIME__MAXBOTS".to_owned()
            }
        );
    });
}

#[test]
fn a_misspelled_key_in_a_bot_names_the_bot() {
    in_jail(|jail| {
        let toml = format!(
            "{MINIMAL}\n[[standalone.bots]]\nusernme = \"AfkBot2\"\nserver = \"localhost\"\nmode = \"afk\"\n"
        );
        let error = parse_error(load_toml(jail, &toml));

        assert_eq!(error.key().to_string(), "standalone.bots[1].usernme");
        assert!(matches!(error.problem(), ParseProblem::UnknownKey { .. }));
    });
}

#[test]
fn a_value_of_the_wrong_type_names_the_key_and_the_file() {
    in_jail(|jail| {
        let toml = format!("{MINIMAL}\n[runtime]\nmax_bots = \"many\"\n");
        let error = parse_error(load_toml(jail, &toml));

        assert_eq!(error.key().to_string(), "runtime.max_bots");
        assert!(matches!(error.problem(), ParseProblem::WrongType { .. }));
        assert!(is_the_file(error.source()), "{:?}", error.source());
    });
}

#[test]
fn bad_toml_names_the_position_but_not_the_content() {
    in_jail(|jail| {
        let error = parse_error(load_toml(
            jail,
            "name = \"agent-1\"\nconflict = \"marker-text\n",
        ));

        let ParseProblem::Other { first_line } = error.problem() else {
            panic!("expected a syntax error, got {:?}", error.problem());
        };
        assert!(first_line.contains("line 2"), "{first_line}");
        assert!(!error.to_string().contains("marker-text"), "{error}");
    });
}

// --- Validation ---------------------------------------------------------------

#[test]
fn every_problem_is_reported_at_once_without_values() {
    in_jail(|jail| {
        let problems = invalid(load_toml(
            jail,
            r#"
name = "_marker1"

[runtime]
max_bots = 0
connect_timeout_secs = 4
heartbeat_file = "marker2.alive"

[retry]
stable_after_secs = 59

[[standalone.bots]]
username = "marker 3"
server = "marker4://x"
mode = "marker5"
conflict_texts = [""]

[[standalone.bots]]
server = "localhost"
"#,
        ));

        assert_eq!(
            keys(&problems),
            [
                "name",
                "retry.stable_after_secs",
                "runtime.connect_timeout_secs",
                "runtime.heartbeat_file",
                "runtime.max_bots",
                "standalone.bots[0].conflict_texts",
                "standalone.bots[0].mode",
                "standalone.bots[0].server",
                "standalone.bots[0].username",
                "standalone.bots[1].mode",
                "standalone.bots[1].username",
            ]
        );
        let shown = ConfigError::Invalid(problems).to_string();
        assert!(!shown.contains("marker"), "{shown}");
    });
}

/// A key, its lowest and highest valid values, and settings that the
/// highest one needs to pass fleet-core's own rules.
#[rstest]
#[case::max_bots("runtime", "max_bots", 1, 1000, "")]
#[case::watchdog("runtime", "watchdog_timeout_secs", 5, 600, "")]
#[case::liveness("runtime", "packet_liveness_timeout_secs", 5, 600, "")]
#[case::connect("runtime", "connect_timeout_secs", 5, 300, "")]
#[case::abandoned_threads("runtime", "max_abandoned_threads", 1, 100, "")]
#[case::shutdown("runtime", "shutdown_timeout_secs", 1, 300, "")]
#[case::base_delay("retry", "base_delay_secs", 1, 3600, "max_delay_secs = 86400")]
#[case::max_delay("retry", "max_delay_secs", 2, 86_400, "base_delay_secs = 1")]
#[case::stable_after("retry", "stable_after_secs", 60, 86_400, "")]
#[case::circuit_failures("retry", "circuit_failures", 1, 1000, "")]
#[case::circuit_window("retry", "circuit_window_secs", 1, 86_400, "")]
#[case::circuit_cooldown("retry", "circuit_cooldown_secs", 1, 86_400, "")]
fn numbers_are_checked_against_their_range(
    #[case] section: &str,
    #[case] key: &str,
    #[case] min: u64,
    #[case] max: u64,
    #[case] extra: &str,
) {
    in_jail(|jail| {
        let with = |value: u64| format!("{MINIMAL}\n[{section}]\n{key} = {value}\n{extra}\n");
        // `max_delay_secs` can't be 1 with a base of 1 (fleet-core wants twice
        // the base), so its lowest refused value is the range's 0.
        let below = if key == "max_delay_secs" { 0 } else { min - 1 };

        for value in [min, max] {
            assert!(load_toml(jail, &with(value)).is_ok(), "{key} = {value}");
        }
        for value in [below, max + 1] {
            let problems = invalid(load_toml(jail, &with(value)));
            assert_eq!(
                keys(&problems),
                [format!("{section}.{key}")],
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
fn a_max_delay_below_twice_the_base_is_reported_once() {
    in_jail(|jail| {
        let toml = format!("{MINIMAL}\n[retry]\nbase_delay_secs = 100\nmax_delay_secs = 150\n");
        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(keys(&problems), ["retry.max_delay_secs"]);
        assert_eq!(
            kinds(&problems),
            [ProblemKind::Retry(RetryPolicyError::MaxTooSmall)]
        );
    });
}

#[test]
fn both_modes_together_are_refused() {
    in_jail(|jail| {
        let toml = format!(
            "{MINIMAL}\n[control_plane]\nurl = \"https://fleet.example.com:7443\"\n\
             ca_cert_file = \"ca.crt\"\ncert_file = \"agent.crt\"\nkey_file = \"agent.key\"\n"
        );
        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(kinds(&problems), [ProblemKind::BothModes]);
    });
}

#[test]
fn a_config_without_a_mode_is_refused() {
    in_jail(|jail| {
        let problems = invalid(load_toml(jail, "name = \"agent-1\"\n"));

        assert_eq!(kinds(&problems), [ProblemKind::NoMode]);
        assert_eq!(keys(&problems), [""]);
    });
}

#[test]
fn a_standalone_agent_needs_a_bot() {
    in_jail(|jail| {
        let problems = invalid(load_toml(jail, "name = \"agent-1\"\n[standalone]\n"));

        assert_eq!(keys(&problems), ["standalone.bots"]);
        assert_eq!(kinds(&problems), [ProblemKind::NoBots]);
    });
}

#[test]
fn more_bots_than_max_bots_are_refused() {
    in_jail(|jail| {
        let toml = format!(
            "{MINIMAL}\n[[standalone.bots]]\nusername = \"AfkBot2\"\nserver = \"localhost\"\n\
             mode = \"afk\"\n\n[runtime]\nmax_bots = 1\n"
        );
        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(keys(&problems), ["standalone.bots"]);
        assert_eq!(
            kinds(&problems),
            [ProblemKind::TooManyBots { count: 2, max: 1 }]
        );
    });
}

#[test]
fn the_bot_count_is_not_checked_against_an_invalid_max_bots() {
    in_jail(|jail| {
        let toml = format!("{MINIMAL}\n[runtime]\nmax_bots = 0\n");
        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(keys(&problems), ["runtime.max_bots"]);
    });
}

#[test]
fn bots_whose_accounts_clash_are_refused_ignoring_case() {
    in_jail(|jail| {
        let bot = |name: &str| {
            format!(
                "[[standalone.bots]]\nusername = \"{name}\"\nserver = \"localhost\"\nmode = \"afk\"\n"
            )
        };
        let toml = format!(
            "name = \"agent-1\"\n{}{}{}",
            bot("AfkBot1"),
            bot("AfkBot2"),
            bot("afkbot1")
        );
        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(keys(&problems), ["standalone.bots[2].username"]);
        assert_eq!(kinds(&problems), [ProblemKind::Clash { with: 0 }]);
        assert_eq!(
            problems.to_string(),
            "  - standalone.bots[2].username: clashes with standalone.bots[0] \
             (names compare ignoring case)"
        );
    });
}

#[test]
fn invalid_names_are_not_checked_for_clashes() {
    in_jail(|jail| {
        let bot =
            "[[standalone.bots]]\nusername = \"a b\"\nserver = \"localhost\"\nmode = \"afk\"\n";
        let problems = invalid(load_toml(jail, &format!("name = \"agent-1\"\n{bot}{bot}")));

        assert_eq!(
            kinds(&problems),
            [
                ProblemKind::Username(McUsernameError::InvalidChar { index: 1 }),
                ProblemKind::Username(McUsernameError::InvalidChar { index: 1 }),
            ]
        );
    });
}

#[test]
fn every_missing_key_is_reported_together() {
    in_jail(|jail| {
        let problems = invalid(load_toml(jail, "[[standalone.bots]]\n"));
        assert_eq!(
            keys(&problems),
            [
                "name",
                "standalone.bots[0].mode",
                "standalone.bots[0].server",
                "standalone.bots[0].username",
            ]
        );
        assert!(
            kinds(&problems)
                .iter()
                .all(|kind| *kind == ProblemKind::Missing)
        );

        let problems = invalid(load_toml(jail, "name = \"agent-1\"\n[control_plane]\n"));
        assert_eq!(
            keys(&problems),
            [
                "control_plane.ca_cert_file",
                "control_plane.cert_file",
                "control_plane.key_file",
                "control_plane.url",
            ]
        );
        assert!(
            kinds(&problems)
                .iter()
                .all(|kind| *kind == ProblemKind::Missing)
        );
    });
}

#[test]
fn empty_control_plane_keys_are_refused() {
    in_jail(|jail| {
        let problems = invalid(load_toml(
            jail,
            "name = \"agent-1\"\n[control_plane]\nurl = \"\"\nca_cert_file = \"\"\ncert_file = \"\"\nkey_file = \"\"\n",
        ));

        assert_eq!(problems.len(), 4);
        assert!(
            kinds(&problems)
                .iter()
                .all(|kind| *kind == ProblemKind::Empty)
        );
    });
}

#[test]
fn a_relative_heartbeat_file_is_refused() {
    in_jail(|jail| {
        let toml = format!("{MINIMAL}\n[runtime]\nheartbeat_file = \"agent.alive\"\n");
        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(keys(&problems), ["runtime.heartbeat_file"]);
        assert_eq!(kinds(&problems), [ProblemKind::NotAbsolute]);
    });
}

#[test]
fn an_invalid_name_is_refused() {
    in_jail(|jail| {
        let problems = invalid(load_toml(jail, &MINIMAL.replace("agent-1", ".agent")));

        assert_eq!(
            kinds(&problems),
            [ProblemKind::AgentName(AgentNameError::InvalidStart)]
        );
    });
}

#[test]
fn an_unknown_mode_lists_the_presets() {
    in_jail(|jail| {
        let problems = invalid(load_toml(jail, &MINIMAL.replace("\"afk\"", "\"fishing\"")));

        assert_eq!(keys(&problems), ["standalone.bots[0].mode"]);
        assert_eq!(kinds(&problems), [ProblemKind::Mode(UnknownPresetError)]);
        assert!(problems.to_string().ends_with("expected one of: afk, farm"));
    });
}

#[test]
fn a_bad_conflict_text_names_its_bot_and_position() {
    in_jail(|jail| {
        let toml = format!(
            "{MINIMAL}\n[[standalone.bots]]\nusername = \"AfkBot2\"\nserver = \"localhost\"\n\
             mode = \"afk\"\nconflict_texts = [\"fine\", \"a\u{a7}b\"]\n"
        );
        let problems = invalid(load_toml(jail, &toml));

        assert_eq!(keys(&problems), ["standalone.bots[1].conflict_texts"]);
        assert_eq!(
            kinds(&problems),
            [ProblemKind::ConflictTexts(
                ConflictTextsError::ForbiddenChar {
                    index: 1,
                    position: 1
                }
            )]
        );
        assert!(!problems.to_string().contains("a\u{a7}b"));
    });
}
