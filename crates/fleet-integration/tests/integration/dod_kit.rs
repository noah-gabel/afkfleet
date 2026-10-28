//! The definition-of-done checks' kit (ADR-0020, U1, U50, S72, R124): the server's and the
//! agent's configs fit each other and their Minecraft container. Each is
//! loaded through its own loader with exactly the recipe's exports from the
//! justfile, their paths in a temporary folder, never target/dod-run (U33).
//! These run in the default profile: no account, no Docker, no port bound.

use core::net::IpAddr;
use std::path::{Path, PathBuf};

use chrono::{TimeZone as _, Utc};
use fleet_agent::config::{AgentConfig, AgentMode};
use fleet_server::cli::pki;
use fleet_server::config::ServerConfig;
use fleet_testkit::jail::in_jail;
use fleet_testkit::system::{ManualClock, SeededRandom};

/// The repository's `path`.
fn repo(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

/// The repository's file `path`, read now: the kit's files are only read at
/// run time, so a missing one fails its test, never the build.
fn read(path: &str) -> String {
    std::fs::read_to_string(repo(path)).unwrap()
}

/// `recipe`'s exports, its paths under target/dod-run moved into `root`.
fn exports(recipe: &str, root: &Path) -> Vec<(String, String)> {
    fleet_testkit::justfile::recipe(&read("justfile"), recipe)
        .unwrap()
        .exports_under(&["target", "dod-run"], root)
        .unwrap()
}

/// The kit's server config and its agent config, each with exactly its
/// recipe's exports (`dod-server`'s and `dod-agent`'s), and the folder that
/// stands for target/dod-run, which `test` may use until it returns.
fn with_configs(test: impl FnOnce(&ServerConfig, &AgentConfig, &Path)) {
    in_jail(|jail| {
        let dir = jail.directory().to_owned();
        let server = exports("dod-server", &dir);
        let agent = exports("dod-agent", &dir);
        jail.clear_env();
        for (name, value) in &server {
            jail.set_env(name, value);
        }
        let server = fleet_server::config::load(&repo("deploy/dod/server.toml")).unwrap();
        jail.clear_env();
        for (name, value) in &agent {
            jail.set_env(name, value);
        }
        let agent = fleet_agent::config::load(&repo("deploy/dod/agent.toml")).unwrap();
        test(&server, &agent, &dir);
    });
}

/// `compose`'s lines under its `minecraft` service's `key:`, each
/// trimmed, `- ` and quotes removed: the `ports:` list, say.
fn list(compose: &str, key: &str) -> Vec<String> {
    compose
        .lines()
        .skip_while(|line| line.trim() != format!("{key}:"))
        .skip(1)
        .map_while(|line| line.trim().strip_prefix("- "))
        .map(|item| item.trim_matches('"').to_owned())
        .collect()
}

/// The `minecraft` service's environment lines of `compose` whose key is
/// RCON's, as `KEY: value`.
fn rcon_settings(compose: &str) -> Vec<String> {
    compose
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("ENABLE_RCON:") || line.starts_with("RCON_"))
        .map(ToOwned::to_owned)
        .collect()
}

#[test]
fn the_agent_dials_the_servers_grpc_bind() {
    with_configs(|server, agent, _| {
        let AgentMode::ControlPlane(plane) = &agent.mode else {
            panic!("a managed agent");
        };
        let grpc = server.grpc.as_ref().unwrap();

        assert_eq!(plane.url, format!("https://{}", grpc.bind));
    });
}

#[test]
fn pki_init_on_the_dod_config_writes_its_four_files() {
    with_configs(|server, _, dir| {
        std::fs::create_dir_all(dir.join("pki")).unwrap();
        let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 10, 27, 12, 0, 0).unwrap());
        let random = SeededRandom::new(53);
        // Its output names the files: never shown.
        let (mut out, mut err) = (Vec::new(), Vec::new());

        // The names scripts/dev-pki.mjs passes; its test pins them.
        let created = pki::init(
            server,
            &["localhost".to_owned()],
            &[IpAddr::from([127, 0, 0, 1]), "::1".parse().unwrap()],
            &clock,
            &random,
            &mut out,
            &mut err,
        );

        assert_eq!(created, pki::Exit::Created);
        for file in ["ca.crt", "ca.key", "server.crt", "server.key"] {
            assert!(dir.join("pki").join(file).is_file(), "{file}");
        }
    });
}

#[test]
fn the_agent_dials_the_containers_published_address_and_no_public_one() {
    let compose = read("deploy/compose.dod.yaml");
    let ports = list(&compose, "ports");
    let published: IpAddr = ports
        .first()
        .and_then(|port| port.split(':').next())
        .unwrap()
        .parse()
        .unwrap();

    with_configs(|_, agent, _| {
        let AgentMode::ControlPlane(plane) = &agent.mode else {
            panic!("a managed agent");
        };

        assert!(plane.private_targets.contains(published), "{published}");
        assert!(!plane.public_targets);
    });
}

#[test]
fn the_dod_container_keeps_compose_devs_rcon_and_publishes_only_its_game_port() {
    let dod = read("deploy/compose.dod.yaml");
    let dev = read("deploy/compose.dev.yaml");

    // RCON on, with the password itzg draws at each start: no key sets one,
    // or a port, and the check's `rcon-cli list` runs inside the container.
    assert_eq!(rcon_settings(&dod), rcon_settings(&dev));
    assert_eq!(rcon_settings(&dod), ["ENABLE_RCON: \"TRUE\""]);
    assert_eq!(list(&dod, "ports"), ["127.0.0.1:25566:25565"]);
}

#[test]
fn no_setting_or_top_level_export_puts_a_config_key_in_a_dod_recipes_environment() {
    let justfile = fleet_testkit::justfile::environment(&read("justfile")).unwrap();

    // `set export` would export every parameter, `set dotenv-*` a .env file's
    // variables, and a top-level export reaches every recipe (PR #78's review).
    for setting in justfile.settings() {
        assert!(
            setting != "export" && !setting.starts_with("dotenv"),
            "{setting}"
        );
    }
    for export in justfile.exports() {
        assert!(!export.starts_with("AFKFLEET_"), "{export}");
    }
}
