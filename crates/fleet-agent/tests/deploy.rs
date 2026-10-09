//! The deployment files around the agent (Plan.md P5.6; ADR-0014):
//! - both dev configs load
//! - the compose agent's bots join the compose server, and none of them
//!   clashes with `just dev-agent`'s, so both agents can run at once
//! - Docker's grace period outlasts the compose agent's worst-case shutdown
//! - `run` and the healthcheck read the same config in the container
//!
//! The configs are loaded inside `figment::Jail` with an empty environment,
//! so a developer's own `AFKFLEET_AGENT__…` variables can't change them.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use fleet_agent::config::{AgentConfig, AgentMode, StandaloneBot, load};
use fleet_agent::run::RUNTIME_SHUTDOWN;
use jail::in_jail;

#[path = "common/jail.rs"]
mod jail;

const COMPOSE: &str = include_str!("../../../deploy/compose.dev.yaml");
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

/// The lines of compose.dev.yaml's `agent` service.
fn agent_service() -> Vec<&'static str> {
    let lines: Vec<&str> = COMPOSE
        .lines()
        .skip_while(|line| *line != "  agent:")
        .skip(1)
        .take_while(|line| {
            line.is_empty() || line.starts_with("    ") || line.trim_start().starts_with('#')
        })
        .collect();
    assert!(
        !lines.is_empty(),
        "compose.dev.yaml should have an agent service"
    );
    lines
}

/// The agent service's value of `key`, if it has one on its own line.
fn agent_value(key: &str) -> Option<&'static str> {
    agent_service().into_iter().find_map(|line| {
        let value = line.trim().strip_prefix(key)?.strip_prefix(':')?;
        Some(value.trim())
    })
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

#[test]
fn the_dev_agents_config_loads() {
    let config = load_dev_config("agent.toml");

    assert_ne!(standalone_bots(&config), []);
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

    let grace = agent_value("stop_grace_period")
        .and_then(|value| value.strip_suffix('s'))
        .and_then(|secs| secs.parse().ok())
        .map(Duration::from_secs)
        .expect("the agent service should set stop_grace_period in seconds, e.g. `20s`");

    assert!(
        grace > worst,
        "stop_grace_period ({grace:?}) must exceed the worst-case shutdown ({worst:?})"
    );
}

#[test]
fn compose_mounts_the_compose_agents_config_where_the_image_reads_it() {
    let mount = agent_service()
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
