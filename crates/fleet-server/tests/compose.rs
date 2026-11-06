//! The production compose project, as its files are written (Plan.md
//! P12.2; ADR-0022, S14–S17, U18–U20); P12.7's smoke tests run it:
//! - every long-running service is hardened: the nonroot user, a read-only
//!   root, no capability, no new privilege, rotated logs, no swap, limits
//! - the server mounts every folder its config names, the setup service
//!   writes the keys and never the database, the agent reads the CA alone
//! - each service's grace period outlasts its worst-case shutdown
//! - the binaries get `.env` (U18) and nothing else does; `env.example`
//!   lists every variable compose reads and every setting the runbook
//!   names, with placeholders only, and the release holds no
//!   `compose.override.yaml`
//! - the server's metrics port is bound to its fixed fleet address (U19),
//!   and each proxy path trusts exactly the address its network gives the
//!   proxy (S16)
//! - nothing is published but each path's own port, never a metrics port
//! - Caddy is pinned the same everywhere, proxies only the app's routes and
//!   deletes the ticket and `q` from every logged URI
//! - nothing under `deploy/` but the development configs allows offline
//!   accounts (U20)
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fleet_server::cli::serve::{RUNTIME_SHUTDOWN, ShutdownTimeouts};
use fleet_server::config::HttpConfig;
use fleet_server::http::rate_limit::TrustedProxies;
use fleet_testkit::compose::{list, service, value};

const COMPOSE: &str = include_str!("../../../deploy/compose.yaml");
const EDGE: &str = include_str!("../../../deploy/compose.edge.yaml");
const CADDY: &str = include_str!("../../../deploy/compose.caddy.yaml");
const LOOPBACK: &str = include_str!("../../../deploy/compose.loopback.yaml");
const GRPC: &str = include_str!("../../../deploy/compose.grpc.yaml");
const REMOTE: &str = include_str!("../../../deploy/compose.agent.yaml");
const CADDYFILE: &str = include_str!("../../../deploy/Caddyfile");
const PROD: &str = include_str!("../../../deploy/prod/server.toml");
const ENV_EXAMPLE: &str = include_str!("../../../deploy/env.example");
/// The smoke test's stand-in proxy, pinned as Caddy is.
const STAND_IN: &str = include_str!("../../fleet-integration/tests/integration/smoke/edge.yaml");

/// The hardening every long-running service carries (S17).
const HARDENING: [&str; 7] = [
    "user: \"65532:65532\"",
    "read_only: true",
    "cap_drop: [ALL]",
    "- no-new-privileges:true",
    "driver: json-file",
    "max-size: 10m",
    "max-file: \"5\"",
];

/// The lines of compose.yaml's `x-hardened` anchor.
fn anchor() -> Vec<&'static str> {
    COMPOSE
        .lines()
        .skip_while(|line| *line != "x-hardened: &hardened")
        .skip(1)
        .take_while(|line| line.starts_with(' ') || line.is_empty())
        .collect()
}

/// The trimmed lines of `lines`.
fn trimmed<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    lines.iter().map(|line| line.trim()).collect()
}

/// A duration in seconds as compose writes it (`30s`).
fn secs(text: &str) -> Duration {
    Duration::from_secs(text.strip_suffix('s').unwrap().parse().unwrap())
}

#[test]
fn every_long_running_service_is_hardened() {
    let anchor = trimmed(&anchor());
    for item in HARDENING {
        assert!(
            anchor.contains(&item.trim_start()),
            "the anchor lacks {item}"
        );
    }
    assert!(anchor.contains(&"restart: unless-stopped"));
    for name in ["server", "agent", "setup", "probe"] {
        let lines = trimmed(&service(COMPOSE, name));
        assert!(lines.contains(&"<<: *hardened"), "{name} isn't hardened");
    }
    for (file, name) in [(CADDY, "caddy"), (REMOTE, "agent")] {
        let lines = trimmed(&service(file, name));
        for item in HARDENING {
            assert!(lines.contains(&item), "{name} lacks {item}");
        }
        assert!(lines.contains(&"restart: unless-stopped"), "{name}");
    }
}

/// No service adds a capability back but Caddy and the smoke test's
/// stand-in, and those only `NET_BIND_SERVICE`, under no-new-privileges, so
/// 2.11.7's binary starts and gains nothing (ADR-0022, S21).
#[test]
fn only_caddy_adds_a_capability_and_only_net_bind_service() {
    for (file, name) in [
        (COMPOSE, "server"),
        (COMPOSE, "agent"),
        (COMPOSE, "setup"),
        (COMPOSE, "probe"),
        (REMOTE, "agent"),
    ] {
        assert_eq!(value(&service(file, name), "cap_add"), None, "{name}");
    }
    for (file, name) in [(CADDY, "caddy"), (STAND_IN, "proxy")] {
        let lines = service(file, name);
        assert_eq!(
            value(&lines, "cap_add"),
            Some("[NET_BIND_SERVICE]"),
            "{name}"
        );
        assert_eq!(value(&lines, "cap_drop"), Some("[ALL]"), "{name}");
        assert!(
            trimmed(&lines).contains(&"- no-new-privileges:true"),
            "{name}"
        );
    }
    for file in [COMPOSE, EDGE, LOOPBACK, GRPC] {
        assert!(!file.contains("cap_add"));
    }
}

/// Memory without swap, so no key or token reaches the host's swap, and
/// every limit an `.env` knob with its default (S17).
#[rstest::rstest]
#[case::server(COMPOSE, "server", "SERVER", "512m", "1.0", "256")]
#[case::agent(COMPOSE, "agent", "AGENT", "512m", "1.0", "512")]
#[case::caddy(CADDY, "caddy", "CADDY", "256m", "0.5", "128")]
#[case::remote_agent(REMOTE, "agent", "AGENT", "512m", "1.0", "512")]
fn every_limit_is_a_knob_and_memory_has_no_swap(
    #[case] file: &str,
    #[case] name: &str,
    #[case] knob: &str,
    #[case] memory: &str,
    #[case] cpus: &str,
    #[case] pids: &str,
) {
    let lines = service(file, name);
    let memory = format!("${{AFKFLEET_{knob}_MEMORY:-{memory}}}");

    assert_eq!(value(&lines, "mem_limit"), Some(memory.as_str()));
    assert_eq!(value(&lines, "memswap_limit"), Some(memory.as_str()));
    assert_eq!(
        value(&lines, "cpus"),
        Some(format!("${{AFKFLEET_{knob}_CPUS:-{cpus}}}").as_str())
    );
    assert_eq!(
        value(&lines, "pids_limit"),
        Some(format!("${{AFKFLEET_{knob}_PIDS:-{pids}}}").as_str())
    );
}

/// Every path the production config names lies on a volume compose mounts
/// (S14); the keys, the CA and the server's pair read-only; the backups a
/// host folder Docker never creates (S15).
#[test]
fn the_server_mounts_every_folder_its_config_names() {
    let lines = service(COMPOSE, "server");
    let volumes = list(&lines, "volumes");

    assert_eq!(
        volumes,
        [
            "./prod/server.toml:/etc/afkfleet/server.toml:ro",
            "data:/data",
            "keys:/keys:ro",
            "ca:/ca:ro",
            "tls:/tls:ro",
            "type: bind",
        ]
    );
    let lines = trimmed(&lines);
    for expected in [
        "source: ${AFKFLEET_BACKUPS_DIR:-/srv/afkfleet-backups/backups}",
        "target: /backups",
        "create_host_path: false",
    ] {
        assert!(lines.contains(&expected), "{expected}");
    }
    for key in [
        "path",
        "backup_dir",
        "ca_cert_file",
        "ca_key_file",
        "server_cert_file",
        "server_key_file",
        "1",
    ] {
        let path = PROD
            .lines()
            .find_map(|line| {
                let (name, value) = line.split('#').next()?.split_once('=')?;
                (name.trim() == key).then(|| value.trim().trim_matches('"').to_owned())
            })
            .unwrap_or_else(|| panic!("server.toml should set {key}"));
        let folder = if key == "backup_dir" {
            path.clone()
        } else {
            Path::new(&path)
                .parent()
                .unwrap()
                .display()
                .to_string()
                .replace('\\', "/")
        };
        assert!(
            volumes
                .iter()
                .any(|mount| mount.contains(&format!(":{folder}")))
                || (folder == "/backups"),
            "{key} = {path} isn't on a mount"
        );
    }
}

/// The one-time steps' service: the server's image with no network, the
/// keys', CA's and certificate's volumes writable, never the database's,
/// started only by `docker compose run` (S14).
#[test]
fn the_setup_service_writes_the_keys_and_never_the_database() {
    let lines = service(COMPOSE, "setup");

    assert_eq!(
        list(&lines, "volumes"),
        [
            "./prod/server.toml:/etc/afkfleet/server.toml:ro",
            "keys:/keys",
            "ca:/ca",
            "tls:/tls",
        ]
    );
    assert_eq!(value(&lines, "network_mode"), Some("none"));
    assert_eq!(value(&lines, "profiles"), Some("[setup]"));
    assert_eq!(value(&lines, "restart"), Some("\"no\""));
}

/// The server's grace outlasts its worst-case shutdown with the largest
/// request timeout the config allows (19 s): the drain, the close, the
/// runtime's own shutdown (ADR-0015). Caddy's outlasts its own grace.
#[test]
fn each_grace_period_outlasts_its_worst_case_shutdown() {
    let http = HttpConfig {
        bind: HttpConfig::DEFAULT_BIND,
        request_timeout: Duration::from_secs(19),
        max_body_bytes: 65_536,
        trusted_proxies: TrustedProxies::default(),
    };
    let worst = ShutdownTimeouts::of(&http).deadline() + RUNTIME_SHUTDOWN;
    let caddy_grace = CADDYFILE
        .lines()
        .find_map(|line| line.trim().strip_prefix("grace_period "))
        .map(secs)
        .expect("the Caddyfile should set grace_period");

    let server = secs(value(&service(COMPOSE, "server"), "stop_grace_period").unwrap());
    let caddy = secs(value(&service(CADDY, "caddy"), "stop_grace_period").unwrap());

    assert!(server > worst, "{server:?} <= {worst:?}");
    assert!(caddy > caddy_grace, "{caddy:?} <= {caddy_grace:?}");
}

/// The binaries read `.env`'s settings (U18), each from the host's own
/// file, which updates never replace; the proxy and the probe get none.
#[test]
fn the_binaries_and_only_they_read_env() {
    for (file, name, reads) in [
        (COMPOSE, "server", true),
        (COMPOSE, "agent", true),
        (COMPOSE, "setup", true),
        (COMPOSE, "probe", false),
        (CADDY, "caddy", false),
        (REMOTE, "agent", true),
    ] {
        let lines = trimmed(&service(file, name));
        let env_file = lines.contains(&"env_file:")
            && lines.contains(&"- path: .env")
            && lines.contains(&"required: true");
        assert_eq!(env_file, reads, "{name}");
    }
}

/// The server's metrics port is bound to its fixed address on the fleet
/// network (U19), the address compose gives it from the same variable,
/// outside the range Docker hands out by itself; Caddy is never on fleet.
#[test]
fn the_metrics_port_is_bound_to_the_servers_fleet_address() {
    let address = "${AFKFLEET_SERVER_FLEET_ADDRESS:-172.30.0.10}";
    let server = service(COMPOSE, "server");

    assert_eq!(
        value(&server, "AFKFLEET_SERVER__METRICS__BIND"),
        Some(format!("{address}:9480").as_str())
    );
    assert_eq!(value(&server, "ipv4_address"), Some(address));
    assert!(
        trimmed(&COMPOSE.lines().collect::<Vec<_>>())
            .contains(&"ip_range: ${AFKFLEET_FLEET_RANGE:-172.30.0.128/25}"),
        "10 lies below the automatic range"
    );
    let caddy_networks = trimmed(&service(CADDY, "caddy"));
    assert!(!caddy_networks.contains(&"fleet:"));
}

/// Each proxy path trusts the one address its proxy has (S16): the
/// container proxy's fixed edge address, which Caddy takes too, or the
/// fleet network's gateway for a proxy on the host.
#[test]
fn each_path_trusts_exactly_its_proxys_address() {
    let edge = service(EDGE, "server");
    let loopback = service(LOOPBACK, "server");

    assert_eq!(
        value(&edge, "AFKFLEET_SERVER__HTTP__TRUSTED_PROXIES"),
        Some("'[\"${AFKFLEET_PROXY_ADDRESS:-172.30.1.2}/32\"]'")
    );
    assert_eq!(
        value(&service(CADDY, "caddy"), "ipv4_address"),
        Some("${AFKFLEET_PROXY_ADDRESS:-172.30.1.2}")
    );
    assert!(EDGE.contains("ip_range: ${AFKFLEET_EDGE_RANGE:-172.30.1.128/25}"));
    assert_eq!(
        value(&loopback, "AFKFLEET_SERVER__HTTP__TRUSTED_PROXIES"),
        Some("'[\"${AFKFLEET_FLEET_GATEWAY:-172.30.0.1}/32\"]'")
    );
    assert!(COMPOSE.contains("gateway: ${AFKFLEET_FLEET_GATEWAY:-172.30.0.1}"));
    assert!(!PROD.contains("trusted_proxies ="));
}

/// compose.yaml publishes nothing; each path's file its own port alone, the
/// host's API on loopback only; no metrics port anywhere.
#[test]
fn nothing_is_published_but_each_paths_own_port() {
    for name in ["server", "agent", "setup", "probe"] {
        assert_eq!(
            list(&service(COMPOSE, name), "ports"),
            Vec::<&str>::new(),
            "{name}"
        );
    }
    assert_eq!(
        list(&service(LOOPBACK, "server"), "ports"),
        ["\"127.0.0.1:${AFKFLEET_HTTP_PORT:-8080}:8080\""]
    );
    assert_eq!(
        list(&service(GRPC, "server"), "ports"),
        ["\"${AFKFLEET_GRPC_PUBLISH:-7443}:7443\""]
    );
    assert_eq!(
        list(&service(CADDY, "caddy"), "ports"),
        ["\"80:80\"", "\"443:443\""]
    );
    assert_eq!(list(&service(REMOTE, "agent"), "ports"), Vec::<&str>::new());
    for file in [COMPOSE, EDGE, CADDY, LOOPBACK, GRPC, REMOTE] {
        for port in ["9480:", "9481:"] {
            assert!(!file.contains(&format!("\"{port}")), "{port} is published");
        }
    }
}

/// Caddy is one pinned image everywhere it runs: the probe, afkfleet's own
/// Caddy and the smoke test's stand-in (U13).
#[test]
fn caddy_is_pinned_the_same_everywhere() {
    let probe = value(&service(COMPOSE, "probe"), "image").unwrap();
    let caddy = value(&service(CADDY, "caddy"), "image").unwrap();
    let stand_in = value(&service(STAND_IN, "proxy"), "image").unwrap();

    assert_eq!(caddy, probe);
    assert_eq!(stand_in, probe);
    let (tag, digest) = probe.split_once("@sha256:").unwrap();
    assert_eq!(tag, "caddy:2.11.7-alpine");
    assert!(digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()));
}

/// The Caddyfile routes only the app's routes to the server's API port;
/// takes certificates from Let's Encrypt alone, or Caddy's own CA for
/// tests; sends HSTS; has no admin endpoint; and deletes the ticket and `q`
/// from every URI it logs (ADR-0020, S28, S127; ADR-0022, S16).
#[test]
fn caddy_proxies_only_the_apps_routes_and_logs_neither_ticket_nor_q() {
    let lines: Vec<&str> = CADDYFILE
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .collect();

    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("reverse_proxy"))
            .collect::<Vec<_>>(),
        [&"reverse_proxy server:8080"]
    );
    for expected in [
        "@app path /api/v1/* /health/live /health/ready",
        "respond 404",
        "admin off",
        "delete ticket",
        "delete q",
        "request>uri query {",
        "dir https://acme-v02.api.letsencrypt.org/directory",
        "tls internal",
        "import tls_{$AFKFLEET_TLS}",
        "Strict-Transport-Security \"max-age=31536000\"",
    ] {
        assert!(lines.contains(&expected), "the Caddyfile lacks {expected}");
    }
}

/// The images come from `.env`, as an operator pins them; compose names no
/// image of afkfleet's itself.
#[test]
fn the_images_come_from_env() {
    for (file, name, variable) in [
        (COMPOSE, "server", "AFKFLEET_SERVER_IMAGE"),
        (COMPOSE, "setup", "AFKFLEET_SERVER_IMAGE"),
        (COMPOSE, "agent", "AFKFLEET_AGENT_IMAGE"),
        (REMOTE, "agent", "AFKFLEET_AGENT_IMAGE"),
    ] {
        let image = value(&service(file, name), "image").unwrap();
        assert!(
            image.starts_with(&format!("${{{variable}:?")),
            "{name}: {image}"
        );
    }
}

/// Every file under `dir`, recursively.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(files(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// Nothing under `deploy/` allows offline accounts but the development
/// configs, `deploy/dev/` (U20): the smoke test's overlay lives with it.
#[test]
fn nothing_under_deploy_allows_offline_accounts() {
    let deploy = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy");
    let dev = deploy.join("dev");
    let mut checked = 0;
    for file in files(&deploy) {
        if file.starts_with(&dev) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        checked += 1;
        let text = text.to_ascii_lowercase().replace(' ', "");
        assert!(!text.contains("allow_offline=true"), "{}", file.display());
        assert!(
            !text.contains("allow_offline:\"true\"") && !text.contains("allow_offline:true"),
            "{}",
            file.display()
        );
    }
    assert!(checked > 10, "{checked}");
}

/// Every variable `text` reads (`${NAME…}`), in order.
fn variables(text: &str) -> Vec<&str> {
    text.split("${")
        .skip(1)
        .map(|rest| {
            let end = rest
                .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
                .unwrap_or(rest.len());
            rest.get(..end).unwrap()
        })
        .collect()
}

/// Whether `env.example` has a line for `name`, set or commented out.
fn env_example_names(name: &str) -> bool {
    ENV_EXAMPLE.lines().any(|line| {
        line.strip_prefix('#')
            .unwrap_or(line)
            .strip_prefix(name)
            .is_some_and(|rest| rest.starts_with('='))
    })
}

/// `env.example` has a line for every variable the server host's compose
/// files read (U18), so an operator sees every setting there is.
#[test]
fn env_example_lists_every_variable_the_compose_files_read() {
    let mut checked = 0;
    for (name, file) in [
        ("compose.yaml", COMPOSE),
        ("compose.edge.yaml", EDGE),
        ("compose.caddy.yaml", CADDY),
        ("compose.loopback.yaml", LOOPBACK),
        ("compose.grpc.yaml", GRPC),
    ] {
        for variable in variables(file) {
            checked += 1;
            assert!(env_example_names(variable), "{name}: {variable}");
        }
    }
    assert!(checked > 20, "{checked}");
}

/// A remote agent's compose file names, in its header, every variable it
/// reads: its host has no env.example of its own.
#[test]
fn the_remote_agents_header_names_every_variable_it_reads() {
    let header: String = REMOTE
        .lines()
        .take_while(|line| line.starts_with('#'))
        .collect();
    for variable in variables(REMOTE) {
        assert!(header.contains(variable), "{variable}");
    }
}

/// Every setting the runbook tells an operator to change is a line of
/// `env.example` (U18), and public dialling is on with no line at all
/// (U10).
#[test]
fn env_example_lists_every_setting_the_runbook_names() {
    for key in [
        "AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__2",
        "AFKFLEET_SERVER__VAULT__ACTIVE_KEY_ID",
        "AFKFLEET_SERVER__DATABASE__BACKUP_KEEP",
        "AFKFLEET_SERVER__DATABASE__BACKUP_INTERVAL_HOURS",
        "AFKFLEET_SERVER__CHAT__RETENTION_DAYS",
        "AFKFLEET_SERVER__CHAT__MAX_LINES_PER_BOT",
        "AFKFLEET_AGENT__CONTROL_PLANE__PUBLIC_TARGETS",
        "AFKFLEET_AGENT__CONTROL_PLANE__PRIVATE_TARGETS",
        "AFKFLEET_SERVER_MEMORY",
        "AFKFLEET_AGENT_MEMORY",
        "AFKFLEET_FLEET_SUBNET",
        "AFKFLEET_EDGE_SUBNET",
        "AFKFLEET_BACKUPS_DIR",
    ] {
        assert!(env_example_names(key), "{key}");
    }
    assert!(
        ENV_EXAMPLE
            .lines()
            .any(|line| line == "AFKFLEET_AGENT__CONTROL_PLANE__PUBLIC_TARGETS=true"),
        "U10"
    );
}

/// The file names `env.example` mentions, whose last part isn't a domain.
const FILE_ENDINGS: [&str; 7] = ["yaml", "toml", "sh", "crt", "key", "md", "example"];

/// `env.example` holds placeholders only (CLAUDE.md's rule 11): `example.com`
/// names and emails, documentation addresses, loopback, and the networks'
/// own defaults.
#[test]
fn env_example_holds_placeholders_only() {
    let mut checked = 0;
    for token in ENV_EXAMPLE
        .split(|c: char| !(c.is_ascii_alphanumeric() || ".-@".contains(c)))
        .map(|token| token.trim_matches('.'))
    {
        let labels: Vec<&str> = token.split('.').collect();
        if token.contains('@') {
            checked += 1;
            assert!(token.ends_with("@example.com"), "{token}");
        } else if labels.len() == 4 && labels.iter().all(|l| l.parse::<u8>().is_ok()) {
            checked += 1;
            assert!(
                [
                    "192.0.2.",
                    "198.51.100.",
                    "203.0.113.",
                    "172.30.0.",
                    "172.30.1."
                ]
                .iter()
                .any(|prefix| token.starts_with(prefix))
                    || token == "127.0.0.1",
                "{token}"
            );
        } else if let [.., last] = labels.as_slice()
            && labels.len() > 1
            && last.len() > 1
            && last.bytes().all(|b| b.is_ascii_alphabetic())
            && !FILE_ENDINGS.contains(last)
        {
            checked += 1;
            assert!(
                token == "example.com" || token.ends_with(".example.com"),
                "{token}"
            );
        }
    }
    assert!(checked > 10, "{checked}");
}

/// No `compose.override.yaml` is in the release: it's the operator's own,
/// which `.env`'s `COMPOSE_FILE` names (U18).
#[test]
fn the_release_holds_no_compose_override() {
    let deploy = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy");

    assert!(deploy.join("compose.yaml").is_file());
    assert!(!deploy.join("compose.override.yaml").exists());
}
