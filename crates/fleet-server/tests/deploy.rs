//! The server's image and its production config (Plan.md P12.1, P12.2;
//! ADR-0022, S13, S14):
//! - `serve` and the healthcheck read the same config in the container, and
//!   Docker's check outlasts the probe's own timeout
//! - the image exposes the API's and the gRPC port, never a metrics port,
//!   and the agent's image exposes nothing
//! - every base image is pinned by tag and index digest, the shared stages
//!   are the agent's word for word, and both images run on one base
//! - each build selects its own binary alone, and the labels name the
//!   repository's source and licence
//! - the folders the image makes are exactly those the production config
//!   names, owner-only and the nonroot user's; it runs as that user and has
//!   no VOLUME, so a forgotten mount can't go unnoticed
//! - `deploy/prod/server.toml` holds what compose relies on, and loads (on
//!   Unix, where its container paths are absolute; CI's Linux jobs), and
//!   with every server setting `env.example` names (U18)
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use fleet_server::cli::healthcheck;
use fleet_testkit::dockerfile::{every, final_stage, from_image, only, stage};

const SERVER: &str = include_str!("../../../deploy/docker/server.Dockerfile");
const AGENT: &str = include_str!("../../../deploy/docker/agent.Dockerfile");
const WORKSPACE: &str = include_str!("../../../Cargo.toml");
const PROD: &str = include_str!("../../../deploy/prod/server.toml");

/// Where the image's `serve` and HEALTHCHECK read the config.
const CONFIG_IN_IMAGE: &str = "/etc/afkfleet/server.toml";
/// The server's binary in the image.
const BINARY: &str = "/usr/local/bin/afkfleet-server";
/// The folders the image makes for compose's volumes (S14).
const FOLDERS: [&str; 5] = ["/data", "/backups", "/keys", "/ca", "/tls"];
/// The metrics ports, never exposed (S10).
const METRICS_PORTS: [&str; 2] = ["9480", "9481"];

/// `text`'s value of `key` in its `[section]` (`""` for the top level), as
/// written after the `=`: a comment cut off, the quotes taken off. The
/// production configs hold no `#` inside a value.
fn toml_value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut current = String::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            name.clone_into(&mut current);
            continue;
        }
        if current != section {
            continue;
        }
        if let Some((name, value)) = line.split_once('=')
            && name.trim() == key
        {
            return Some(value.trim().trim_matches('"').to_owned());
        }
    }
    None
}

/// `deploy/prod/server.toml`'s value of `key` in `[section]`.
fn prod(section: &str, key: &str) -> String {
    toml_value(PROD, section, key)
        .unwrap_or_else(|| panic!("deploy/prod/server.toml should set [{section}] {key}"))
}

/// The port of `address`, `host:port`.
fn port(address: &str) -> &str {
    address.rsplit_once(':').map(|(_, port)| port).unwrap()
}

/// `[workspace.package]`'s value of `key` in the root Cargo.toml.
fn workspace(key: &str) -> String {
    toml_value(WORKSPACE, "workspace.package", key)
        .unwrap_or_else(|| panic!("Cargo.toml should set [workspace.package] {key}"))
}

/// The one `keyword` instruction of the Dockerfile `text`, `name` for the
/// message.
fn one(text: &str, name: &str, keyword: &str) -> String {
    only(text, keyword).unwrap_or_else(|| panic!("{name}: one {keyword} expected"))
}

#[test]
fn the_image_serves_and_healthchecks_with_the_same_config() {
    assert_eq!(
        one(SERVER, "server", "ENTRYPOINT"),
        format!(r#"ENTRYPOINT ["{BINARY}"]"#)
    );
    assert_eq!(
        one(SERVER, "server", "CMD"),
        format!(r#"CMD ["serve", "--config", "{CONFIG_IN_IMAGE}"]"#)
    );
    let healthcheck = one(SERVER, "server", "HEALTHCHECK");
    assert!(
        healthcheck.ends_with(&format!(
            r#" CMD ["{BINARY}", "healthcheck", "--config", "{CONFIG_IN_IMAGE}"]"#
        )),
        "{healthcheck}"
    );
}

/// The probe gives up after its own 3 s (ADR-0015), within Docker's
/// `--timeout`, so a slow server reads as unhealthy, never as a killed check.
#[test]
fn dockers_check_outlasts_the_probes_own_timeout() {
    let healthcheck = one(SERVER, "server", "HEALTHCHECK");
    let timeout = healthcheck
        .split_whitespace()
        .find_map(|part| part.strip_prefix("--timeout="))
        .and_then(|value| value.strip_suffix('s'))
        .and_then(|secs| secs.parse().ok())
        .map(Duration::from_secs)
        .expect("HEALTHCHECK should set --timeout in seconds");

    assert!(timeout > healthcheck::TIMEOUT, "{timeout:?}");
}

#[test]
fn the_image_exposes_the_api_and_grpc_ports_and_never_a_metrics_port() {
    let expose = one(SERVER, "server", "EXPOSE");

    assert_eq!(
        expose,
        format!(
            "EXPOSE {} {}",
            port(&prod("http", "bind")),
            port(&prod("grpc", "bind"))
        )
    );
    for metrics in METRICS_PORTS {
        assert!(
            !expose.split_whitespace().any(|part| part == metrics),
            "{expose}"
        );
    }
    assert_eq!(every(AGENT, "EXPOSE"), Vec::<String>::new());
}

/// Every base image is pinned by its tag and its multi-arch index digest,
/// so a tag that moves can't change what is built (ADR-0014).
#[test]
fn every_base_image_is_pinned_by_tag_and_index_digest() {
    for (name, text) in [("server", SERVER), ("agent", AGENT)] {
        let stages: BTreeSet<String> = every(text, "FROM")
            .iter()
            .filter_map(|from| from.split_once(" AS ").map(|(_, stage)| stage.to_owned()))
            .collect();
        for from in every(text, "FROM") {
            let image = from_image(&from).unwrap();
            if stages.contains(image) {
                continue;
            }
            let (tagged, digest) = image
                .split_once("@sha256:")
                .unwrap_or_else(|| panic!("{name}: {image} should be pinned by digest"));
            assert!(
                digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
                "{name}: {image}"
            );
            assert!(tagged.contains(':'), "{name}: {image} should name its tag");
        }
    }
}

/// The server's image shares the agent's tool and recipe stages word for
/// word, so one build of both reuses their layers, and runs on the same base.
#[test]
fn the_shared_stages_are_the_agents_word_for_word() {
    for shared in ["chef", "planner"] {
        let server = stage(SERVER, shared);
        assert!(server.is_some(), "the server has no {shared} stage");
        assert_eq!(server, stage(AGENT, shared), "{shared}");
    }
    let base = |text| final_stage(text).and_then(|stage| stage.into_iter().next());
    assert!(base(SERVER).is_some());
    assert_eq!(base(SERVER), base(AGENT));
}

/// Each image cooks and builds its own binary alone: both packages in one
/// build would merge their features.
#[test]
fn each_build_selects_its_own_binary_alone() {
    for (text, package, binary, other) in [
        (SERVER, "fleet-server", "afkfleet-server", "fleet-agent"),
        (AGENT, "fleet-agent", "afkfleet-agent", "fleet-server"),
    ] {
        let builder = stage(text, "builder").expect("a builder stage");
        let cargo: Vec<&String> = builder
            .iter()
            .filter(|instruction| {
                instruction.starts_with("RUN cargo chef cook")
                    || instruction.starts_with("RUN cargo build")
            })
            .collect();
        assert_eq!(cargo.len(), 2, "{package}: {builder:#?}");
        for line in cargo {
            assert!(
                line.contains(&format!("--package {package} --bin {binary}")),
                "{line}"
            );
            assert!(!line.contains(other), "{line}");
        }
    }
}

#[test]
fn the_labels_name_the_repositorys_source_and_licence() {
    for (text, title) in [(SERVER, "afkfleet-server"), (AGENT, "afkfleet-agent")] {
        let label = one(text, title, "LABEL");
        for expected in [
            format!(r#"org.opencontainers.image.title="{title}""#),
            format!(
                r#"org.opencontainers.image.source="{}""#,
                workspace("repository")
            ),
            format!(
                r#"org.opencontainers.image.licenses="{}""#,
                workspace("license")
            ),
        ] {
            assert!(label.contains(&expected), "{title}: {label}");
        }
    }
}

/// The folders the image makes are exactly those the production config
/// puts its files in, each the nonroot user's and owner-only, so a new
/// named volume takes that owner (S14); the image runs as that user, and
/// has no VOLUME, whose anonymous volume would hide a forgotten mount.
#[test]
fn the_image_makes_exactly_the_production_configs_folders() {
    let mkdir = format!(
        "RUN mkdir -m 0700 /folders {}",
        FOLDERS.map(|folder| format!("/folders{folder}")).join(" ")
    );
    let builder = stage(SERVER, "builder").expect("a builder stage");
    let runtime = final_stage(SERVER).expect("a runtime stage");
    let files = [
        prod("database", "path"),
        prod("vault.master_key_files", "1"),
        prod("grpc", "ca_cert_file"),
        prod("grpc", "ca_key_file"),
        prod("grpc", "server_cert_file"),
        prod("grpc", "server_key_file"),
    ];
    let mut used: BTreeSet<String> = files
        .iter()
        .map(|file| {
            let folder = Path::new(file).parent().unwrap();
            folder.to_str().unwrap().replace('\\', "/")
        })
        .collect();
    used.insert(prod("database", "backup_dir"));

    assert!(builder.contains(&mkdir), "{builder:#?}");
    assert!(
        runtime.contains(&"COPY --from=builder --chown=65532:65532 /folders/ /".to_owned()),
        "{runtime:#?}"
    );
    assert_eq!(one(SERVER, "server", "USER"), "USER 65532:65532");
    assert_eq!(every(SERVER, "VOLUME"), Vec::<String>::new());
    assert_eq!(
        used,
        FOLDERS.iter().map(|folder| (*folder).to_owned()).collect()
    );
}

/// What compose relies on, read from the file as written, on any platform.
#[test]
fn the_production_config_holds_what_compose_relies_on() {
    assert_eq!(prod("", "dev_mode"), "false");
    assert_eq!(prod("http", "bind"), "0.0.0.0:8080");
    assert_eq!(prod("grpc", "bind"), "0.0.0.0:7443");
    assert_eq!(prod("accounts", "allow_offline"), "false");
    assert_eq!(prod("log", "format"), "json");
    assert_eq!(prod("vault", "active_key_id"), "1");
    // Each proxy path's compose file sets these (S16, U19): never the file.
    assert_eq!(toml_value(PROD, "http", "trusted_proxies"), None);
    assert!(
        !sections(PROD).contains(&"[metrics]".to_owned()),
        "[metrics] is compose's alone"
    );
}

/// The section headers of `text`.
fn sections(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim().to_owned())
        .filter(|line| line.starts_with('['))
        .collect()
}

/// The whole file through the server's own loader, with no environment:
/// it validates, backups on, the control plane on, metrics left to compose.
#[cfg(unix)]
#[test]
fn the_production_config_loads() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/prod/server.toml");
    let mut loaded = None;
    fleet_testkit::jail::in_jail(|jail| {
        jail.clear_env();
        loaded = Some(fleet_server::config::load(&path));
    });

    let config = loaded.unwrap().unwrap();

    assert!(!config.dev_mode);
    assert_eq!(config.database.path, Path::new("/data/afkfleet.db"));
    assert_eq!(
        config.database.backups.map(|backups| backups.dir),
        Some(Path::new("/backups").to_path_buf())
    );
    assert_eq!(config.metrics, None);
    assert!(!config.accounts.allow_offline);
    assert!(config.grpc.is_some());
}

/// `env.example`'s server lines (`AFKFLEET_SERVER__…`), set or commented
/// out, as (name, value).
fn env_example_settings(prefix: &str) -> Vec<(String, String)> {
    include_str!("../../../deploy/env.example")
        .lines()
        .map(|line| line.strip_prefix('#').unwrap_or(line))
        .filter(|line| line.starts_with(prefix))
        .map(|line| {
            let (name, value) = line.split_once('=').unwrap();
            (name.to_owned(), value.to_owned())
        })
        .collect()
}

/// Every server setting `env.example` names loads with the production
/// config, each with its example's value: an unknown key would stop the
/// start (U18). The container paths point into the jail, so it runs here
/// on Windows too.
#[test]
fn every_server_setting_env_example_names_loads() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/prod/server.toml");
    let settings = env_example_settings("AFKFLEET_SERVER__");
    let mut loaded = None;
    fleet_testkit::jail::in_jail(|jail| {
        let dir = jail.directory().to_owned();
        jail.clear_env();
        for key in [
            "DATABASE__PATH",
            "DATABASE__BACKUP_DIR",
            "VAULT__MASTER_KEY_FILES__1",
            "GRPC__CA_CERT_FILE",
            "GRPC__CA_KEY_FILE",
            "GRPC__SERVER_CERT_FILE",
            "GRPC__SERVER_KEY_FILE",
        ] {
            jail.set_env(format!("AFKFLEET_SERVER__{key}"), dir.join(key).display());
        }
        for (name, value) in &settings {
            let value = if value.starts_with('/') {
                dir.join(name).display().to_string()
            } else {
                value.clone()
            };
            jail.set_env(name, value);
        }
        loaded = Some(fleet_server::config::load(&path));
    });

    let config = loaded
        .unwrap()
        .unwrap_or_else(|error| panic!("env.example's settings should load: {error}"));

    assert!(settings.len() >= 6, "{settings:?}");
    assert_eq!(config.vault.active_key_id, 2);
    assert_eq!(config.vault.key_files.len(), 2);
}
