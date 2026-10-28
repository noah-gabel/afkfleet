//! The deployment files around the agent (Plan.md P5.6, P5.7; ADR-0014):
//! - the dev configs load, the managed one with `just dev-enroll`'s paths
//! - the compose agent's bots join the compose server, and none of them
//!   clashes with `just dev-agent`'s, so both agents can run at once
//! - Docker's grace period outlasts the compose agent's worst-case shutdown,
//!   and the Minecraft server gets time to stop gracefully
//! - `run` and the healthcheck read the same config in the container
//! - `deploy/dev/stack-checks.json`'s numbers are the compose agent's, and
//!   `deploy/compose.isolated.yaml` unpublishes the server's port, as the
//!   compose e2e test and the demo assume
//! - the definition-of-done checks' agent config loads with exactly `just dod-agent`'s and
//!   `just dod-enroll`'s exports and dials only its container (ADR-0020, S72)
//!
//! The configs are loaded inside `figment::Jail` with an empty environment,
//! so a developer's own `AFKFLEET_AGENT__…` variables can't change them.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fleet_agent::config::{AgentConfig, AgentMode, LogFormat, StandaloneBot, load};
use fleet_agent::run::RUNTIME_SHUTDOWN;
use fleet_core::net::AddressClass;
use fleet_testkit::jail::in_jail;
use stack_checks::{expected_restart_errors, expected_warnings, millis, stack_checks};

#[path = "common/stack_checks.rs"]
mod stack_checks;

const COMPOSE: &str = include_str!("../../../deploy/compose.dev.yaml");
const ISOLATED: &str = include_str!("../../../deploy/compose.isolated.yaml");
const DOCKERFILE: &str = include_str!("../../../deploy/docker/agent.Dockerfile");

/// Where the image's `run` and HEALTHCHECK read the config.
const CONFIG_IN_IMAGE: &str = "/etc/afkfleet/agent.toml";

/// The repository's `deploy/dev/<name>`.
fn dev_config(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../deploy/dev")
        .join(name)
}

/// Loads `deploy/dev/<name>` without any environment variable.
fn load_dev_config(name: &str) -> AgentConfig {
    let path = dev_config(name);
    let mut loaded = None;
    in_jail(|jail| {
        jail.clear_env();
        loaded = Some(load(&path));
    });
    loaded
        .unwrap()
        .unwrap_or_else(|error| panic!("deploy/dev/{name} should load: {error}"))
}

fn standalone_bots(config: &AgentConfig) -> &[StandaloneBot] {
    match &config.mode {
        AgentMode::Standalone(bots) => bots,
        AgentMode::ControlPlane(_) => panic!("a dev config should be standalone"),
    }
}

/// The lines of the compose file `compose`'s service `name`.
fn service_lines(compose: &'static str, name: &str) -> Vec<&'static str> {
    let header = format!("  {name}:");
    let lines: Vec<&str> = compose
        .lines()
        .skip_while(|line| *line != header)
        .skip(1)
        .take_while(|line| {
            line.is_empty() || line.starts_with("    ") || line.trim_start().starts_with('#')
        })
        .collect();
    assert!(
        !lines.is_empty(),
        "the compose file should have a {name} service"
    );
    lines
}

/// compose.dev.yaml's value of `key` for the service `name`, if it has one on
/// its own line.
fn service_value(name: &str, key: &str) -> Option<&'static str> {
    service_lines(COMPOSE, name).into_iter().find_map(|line| {
        let value = line.trim().strip_prefix(key)?.strip_prefix(':')?;
        Some(value.trim())
    })
}

/// compose.dev.yaml's `stop_grace_period` for the service `name`.
fn stop_grace_period(name: &str) -> Duration {
    service_value(name, "stop_grace_period")
        .and_then(|value| value.strip_suffix('s'))
        .and_then(|secs| secs.parse().ok())
        .map_or_else(
            || panic!("the {name} service should set stop_grace_period in seconds, e.g. `20s`"),
            Duration::from_secs,
        )
}

/// The Dockerfile's instructions, each on one line, without comments.
fn dockerfile_instructions() -> Vec<String> {
    let mut instructions = Vec::new();
    let mut current = String::new();
    for line in DOCKERFILE.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(continued) = line.strip_suffix('\\') {
            current.push_str(continued.trim_end());
            current.push(' ');
        } else {
            current.push_str(line);
            instructions.push(core::mem::take(&mut current));
        }
    }
    instructions
}

/// The Dockerfile's one instruction starting with `keyword`.
fn instruction(keyword: &str) -> String {
    let prefix = format!("{keyword} ");
    let found: Vec<String> = dockerfile_instructions()
        .into_iter()
        .filter(|instruction| instruction.starts_with(&prefix))
        .collect();
    assert_eq!(found.len(), 1, "one {keyword} expected: {found:#?}");
    found.into_iter().next().unwrap()
}

/// stack-checks.json's `retry_windows`, as (attempt, (min, max)).
fn retry_windows() -> Vec<(u64, (Duration, Duration))> {
    let checks = stack_checks();
    let windows = checks["retry_windows"]
        .as_array()
        .expect("stack-checks.json should have the list retry_windows");
    windows
        .iter()
        .map(|window| {
            let number = |key: &str| {
                window[key]
                    .as_u64()
                    .unwrap_or_else(|| panic!("every retry window should have the number {key}"))
            };
            (
                number("attempt"),
                (
                    Duration::from_millis(number("min_ms")),
                    Duration::from_millis(number("max_ms")),
                ),
            )
        })
        .collect()
}

fn attempt(number: usize) -> NonZeroU32 {
    NonZeroU32::new(u32::try_from(number).unwrap()).unwrap()
}

#[test]
fn the_dev_agents_config_loads() {
    let config = load_dev_config("agent.toml");

    assert_ne!(standalone_bots(&config), []);
}

/// `just dev-enroll`'s config (ADR-0019): it loads with the three paths the
/// recipe passes, dials dev-server's control plane, and is useless without
/// them.
#[test]
fn the_managed_dev_config_loads_with_dev_enrolls_paths() {
    let path = dev_config("agent.managed.toml");
    let mut loaded = None;
    let mut bare = None;
    in_jail(|jail| {
        jail.clear_env();
        bare = Some(load(&path));
        let dir = jail.directory().to_owned();
        jail.set_env(
            "AFKFLEET_AGENT__CONTROL_PLANE__CA_CERT_FILE",
            dir.join("ca.crt").display(),
        );
        jail.set_env(
            "AFKFLEET_AGENT__CONTROL_PLANE__CERT_FILE",
            dir.join("agent.crt").display(),
        );
        jail.set_env(
            "AFKFLEET_AGENT__CONTROL_PLANE__KEY_FILE",
            dir.join("agent.key").display(),
        );
        loaded = Some(load(&path));
    });

    let config = loaded.unwrap().unwrap();
    let AgentMode::ControlPlane(plane) = config.mode else {
        panic!("a managed config");
    };
    assert_eq!(plane.url, "https://127.0.0.1:7443");
    assert!(plane.key_file.ends_with("agent.key"));
    // Exactly `just mc-up`'s published address (ADR-0019, S69, R58).
    assert_eq!(
        plane
            .private_targets
            .networks()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["127.0.0.1/32"]
    );
    assert!(
        bare.unwrap().is_err(),
        "the paths come only from the recipe"
    );
}

#[test]
fn every_compose_bot_joins_the_compose_server() {
    let config = load_dev_config("agent.compose.toml");

    let bots = standalone_bots(&config);
    assert_ne!(bots, []);
    for bot in bots {
        assert_eq!(
            bot.server.to_string(),
            "minecraft:25565",
            "{}",
            bot.username
        );
    }
}

#[test]
fn no_compose_bot_clashes_with_a_dev_bot_so_both_agents_can_run_at_once() {
    let compose = load_dev_config("agent.compose.toml");
    let dev = load_dev_config("agent.toml");

    for bot in standalone_bots(&compose) {
        for other in standalone_bots(&dev) {
            assert!(
                !bot.account().clashes_with(&other.account()),
                "{} clashes with {}",
                bot.username,
                other.username
            );
        }
    }
}

#[test]
fn dockers_grace_period_outlasts_the_compose_agents_worst_case_shutdown() {
    let config = load_dev_config("agent.compose.toml");
    let worst = config.runtime.shutdown_timeout + config.runtime.reply_timeout + RUNTIME_SHUTDOWN;

    let grace = stop_grace_period("agent");

    assert!(
        grace > worst,
        "stop_grace_period ({grace:?}) must exceed the worst-case shutdown ({worst:?})"
    );
}

/// Docker's default 10 s could kill the server before `stop` ends, so the
/// bots would see a reset instead of the "Server closed" kick.
#[test]
fn the_minecraft_server_gets_at_least_30_s_to_stop_gracefully() {
    let grace = stop_grace_period("minecraft");

    assert!(grace >= Duration::from_secs(30), "{grace:?}");
}

#[test]
fn compose_mounts_the_compose_agents_config_where_the_image_reads_it() {
    let mount = service_lines(COMPOSE, "agent")
        .into_iter()
        .map(str::trim)
        .find(|line| line.contains("agent.compose.toml"))
        .expect("the agent service should mount agent.compose.toml");

    assert_eq!(
        mount,
        format!("- ./dev/agent.compose.toml:{CONFIG_IN_IMAGE}:ro")
    );
}

#[test]
fn the_images_run_and_healthcheck_read_the_same_config() {
    let binary = "/usr/local/bin/afkfleet-agent";

    assert_eq!(
        instruction("ENTRYPOINT"),
        format!(r#"ENTRYPOINT ["{binary}"]"#)
    );
    assert_eq!(
        instruction("CMD"),
        format!(r#"CMD ["run", "--config", "{CONFIG_IN_IMAGE}"]"#)
    );
    let healthcheck = instruction("HEALTHCHECK");
    assert!(
        healthcheck.ends_with(&format!(
            r#" CMD ["{binary}", "healthcheck", "--config", "{CONFIG_IN_IMAGE}"]"#
        )),
        "{healthcheck}"
    );
}

#[test]
fn the_isolated_override_unpublishes_the_minecraft_servers_port() {
    let minecraft = service_lines(ISOLATED, "minecraft");

    assert!(
        minecraft
            .iter()
            .any(|line| line.trim() == "ports: !reset []"),
        "{minecraft:#?}"
    );
}

/// The windows run from attempt 1 to the first one the maximum delay caps,
/// which then applies to every later attempt.
#[test]
fn the_shared_retry_windows_are_the_compose_agents_retry_policy() {
    let policy = load_dev_config("agent.compose.toml").retry;
    let windows = retry_windows();

    assert_ne!(windows, []);
    for (index, (number, window)) in windows.iter().enumerate() {
        let current = attempt(index + 1);
        assert_eq!(
            *number,
            u64::from(current.get()),
            "the windows should be in order"
        );
        assert_eq!(*window, policy.bounds(current), "attempt {current}");
        if index > 0 {
            assert_ne!(
                policy.bounds(current),
                policy.bounds(attempt(index)),
                "attempt {current} repeats the capped window before it; drop it"
            );
        }
    }
    let after = attempt(windows.len() + 1);
    assert_eq!(
        policy.bounds(after),
        windows.last().unwrap().1,
        "attempt {after} isn't capped yet; add its window"
    );
}

#[test]
fn the_shared_connect_timeout_is_the_compose_agents() {
    let config = load_dev_config("agent.compose.toml");

    assert_eq!(millis("connect_timeout_ms"), config.runtime.connect_timeout);
}

#[test]
fn the_shared_stop_grace_period_is_the_compose_agents() {
    assert_eq!(millis("stop_grace_period_ms"), stop_grace_period("agent"));
}

#[test]
fn every_expected_line_names_its_target_a_message_prefix_and_why() {
    for lines in [expected_warnings(), expected_restart_errors()] {
        assert_ne!(lines, []);
        for line in lines {
            assert!(
                !line.target.is_empty() && !line.message_prefix.is_empty() && !line.why.is_empty(),
                "{line:?}"
            );
        }
    }
}

// --- deploy/dod/agent.toml (ADR-0020, U1, U50, S72, R124) ---

/// The repository's `path`.
fn repo(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

/// `recipe`'s exports, its paths under target/dod-run moved into `root`.
fn dod_exports(recipe: &str, root: &Path) -> Vec<(String, String)> {
    let justfile = std::fs::read_to_string(repo("justfile")).unwrap();
    fleet_testkit::justfile::recipe(&justfile, recipe)
        .unwrap()
        .exports_under(&["target", "dod-run"], root)
        .unwrap()
}

/// Loads `deploy/dod/agent.toml` with exactly `recipe`'s exports, in a jail
/// whose folder stands for target/dod-run (U33: never the real one).
fn load_dod_config(recipe: &str) -> (AgentConfig, PathBuf) {
    let path = repo("deploy/dod/agent.toml");
    let mut loaded = None;
    in_jail(|jail| {
        let dir = jail.directory().to_owned();
        let exports = dod_exports(recipe, &dir);
        jail.clear_env();
        for (name, value) in &exports {
            jail.set_env(name, value);
        }
        loaded = Some((load(&path), dir));
    });
    let (config, dir) = loaded.unwrap();
    (
        config.unwrap_or_else(|error| panic!("deploy/dod/agent.toml should load: {error}")),
        dir,
    )
}

#[test]
fn the_dod_agent_loads_with_dod_agents_exports_and_dials_only_its_container() {
    let (config, dir) = load_dod_config("dod-agent");

    assert_eq!(config.name.as_str(), "dod-agent");
    assert_eq!(config.log.format, LogFormat::Pretty);
    assert_eq!(config.log.filter.as_str(), "info");
    assert_eq!(config.heartbeat_file, dir.join("agent").join("agent.alive"));
    let AgentMode::ControlPlane(plane) = &config.mode else {
        panic!("a managed config");
    };
    assert_eq!(plane.url, "https://127.0.0.1:7453");
    assert_eq!(plane.ca_cert_file, dir.join("pki").join("ca.crt"));
    assert_eq!(plane.cert_file, dir.join("agent").join("agent.crt"));
    assert_eq!(plane.key_file, dir.join("agent").join("agent.key"));
    assert_eq!(
        plane
            .private_targets
            .networks()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["127.0.0.1/32"]
    );
    // U50: the user's rule, no account of theirs on a public server yet.
    assert!(!plane.public_targets);
    let policy = config.mode.target_policy();
    assert_eq!(
        policy
            .check("8.8.8.8".parse().unwrap())
            .map_err(|refused| refused.class()),
        Err(AddressClass::Public)
    );
    assert_eq!(policy.check("127.0.0.1".parse().unwrap()), Ok(()));
}

#[test]
fn dod_enroll_writes_the_identity_dod_agent_runs() {
    let root = Path::new("/dod-run");
    let agent = dod_exports("dod-agent", root);

    let enroll = dod_exports("dod-enroll", root);

    for export in &enroll {
        assert!(agent.contains(export), "{export:?}");
    }
    let names: Vec<&str> = enroll.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "AFKFLEET_AGENT__CONTROL_PLANE__CA_CERT_FILE",
            "AFKFLEET_AGENT__CONTROL_PLANE__CERT_FILE",
            "AFKFLEET_AGENT__CONTROL_PLANE__KEY_FILE",
            "AFKFLEET_AGENT__LOG__FILTER",
        ]
    );
    let (config, _) = load_dod_config("dod-enroll");
    assert!(matches!(config.mode, AgentMode::ControlPlane(_)));
}

#[test]
fn the_dod_minecraft_server_gets_at_least_30_s_to_stop_gracefully() {
    let compose = std::fs::read_to_string(repo("deploy/compose.dod.yaml")).unwrap();

    let grace = compose
        .lines()
        .find_map(|line| line.trim().strip_prefix("stop_grace_period:"))
        .and_then(|value| value.trim().strip_suffix('s'))
        .and_then(|seconds| seconds.parse().ok())
        .map(Duration::from_secs)
        .unwrap();

    assert!(grace >= Duration::from_secs(30), "{grace:?}");
}

/// PR #78's review: `dod-agent` exports `public_targets` itself, as it does
/// the log filter, so a variable left in the user's shell can't flip it (an
/// exported parameter replaces what the shell had).
#[test]
fn the_dod_agent_refuses_public_addresses_whatever_the_shell_says() {
    let path = repo("deploy/dod/agent.toml");
    let mut loaded = None;
    in_jail(|jail| {
        let exports = dod_exports("dod-agent", jail.directory());
        jail.clear_env();
        jail.set_env("AFKFLEET_AGENT__CONTROL_PLANE__PUBLIC_TARGETS", "true");
        for (name, value) in &exports {
            jail.set_env(name, value);
        }
        loaded = Some(load(&path));
    });

    let config = loaded.unwrap().unwrap();

    let AgentMode::ControlPlane(plane) = &config.mode else {
        panic!("a managed config");
    };
    assert!(!plane.public_targets);
}
