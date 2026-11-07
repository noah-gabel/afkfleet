//! The production compose project, as its files are written (Plan.md
//! P12.2; ADR-0022, S14–S17, U18–U20); P12.7's smoke tests run it:
//! - every long-running service is hardened: the nonroot user, a read-only
//!   root, no capability, no new privilege, rotated logs, no swap, limits;
//!   no service sets the anchor's keys again, the one-shot services have
//!   small fixed limits, and every tmpfs is `noexec,nosuid,nodev`, takes
//!   none of them back and is never a volume's long-syntax tmpfs
//! - the server mounts every folder its config names, the setup service
//!   writes the keys and never the database, the agent reads the CA alone;
//!   the files service reaches the database, the server's pair and the
//!   backups alone, the maintenance service writes the database and reads
//!   the keys, and the keys and the CA are read-only everywhere else and
//!   never on Caddy's image (S25, S38)
//! - each service's grace period outlasts its worst-case shutdown
//! - the binaries get `.env` (U18) and nothing else does; `env.example`
//!   lists every variable compose reads and every setting the runbook
//!   names, with placeholders only, and the release holds no
//!   `compose.override.yaml`
//! - the server's metrics port is bound to its fixed fleet address (U19),
//!   and each proxy path trusts exactly the address its network gives the
//!   proxy (S16)
//! - nothing is published but each path's own port, never a metrics port:
//!   every service of every file has exactly its ports
//! - Caddy is pinned the same everywhere, proxies only the app's routes,
//!   drops its `Server` header and deletes the ticket and `q` from every
//!   logged URI
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
use fleet_testkit::compose::{block, list, service, value};

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

/// Every compose file of the release, by name.
const FILES: [(&str, &str); 6] = [
    ("compose.yaml", COMPOSE),
    ("compose.edge.yaml", EDGE),
    ("compose.caddy.yaml", CADDY),
    ("compose.loopback.yaml", LOOPBACK),
    ("compose.grpc.yaml", GRPC),
    ("compose.agent.yaml", REMOTE),
];

/// The names of the services `text` defines, in order.
fn services(text: &str) -> Vec<&str> {
    block(text, "services")
        .into_iter()
        .filter_map(|line| {
            let name = line.strip_prefix("  ")?.strip_suffix(':')?;
            (!name.starts_with([' ', '#'])).then_some(name)
        })
        .collect()
}

/// The keys a service's `lines` set at the service's own level.
fn own_keys<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    lines
        .iter()
        .filter_map(|line| {
            let rest = line.strip_prefix("    ")?;
            if rest.starts_with([' ', '#', '-']) {
                return None;
            }
            rest.split_once(':').map(|(key, _)| key)
        })
        .collect()
}

/// The anchor's keys that a service of compose.yaml, or an override of one,
/// would replace by setting them itself (PR #97's review).
const ANCHORED: [&str; 5] = ["user", "read_only", "cap_drop", "security_opt", "logging"];

/// Every service of compose.yaml takes the anchor and sets none of its keys
/// again, and no path's override sets one or `restart` on the server: a key
/// of its own silently replaces the anchor's, with nothing in the file to
/// show it.
#[test]
fn no_service_sets_the_anchors_keys_again() {
    for name in services(COMPOSE) {
        let lines = service(COMPOSE, name);
        assert!(trimmed(&lines).contains(&"<<: *hardened"), "{name}");
        let own = own_keys(&lines);
        for key in ANCHORED {
            assert!(!own.contains(&key), "{name} sets {key} itself");
        }
    }
    for (file, text) in [
        ("compose.edge.yaml", EDGE),
        ("compose.loopback.yaml", LOOPBACK),
        ("compose.grpc.yaml", GRPC),
    ] {
        for name in services(text) {
            let own = own_keys(&service(text, name));
            for key in ANCHORED.iter().chain(&["restart"]) {
                assert!(!own.contains(key), "{file}'s {name} sets {key}");
            }
        }
    }
}

/// The one-shot services run with small fixed limits, never `.env` knobs:
/// `setup` makes a key and a PKI, `probe` reads one page (PR #97's review),
/// `files` moves and copies a few files (S38).
#[rstest::rstest]
#[case::setup("setup", "128m", "0.5", "64")]
#[case::probe("probe", "32m", "0.25", "16")]
#[case::files("files", "64m", "0.5", "16")]
fn the_one_shot_services_have_small_fixed_limits(
    #[case] name: &str,
    #[case] memory: &str,
    #[case] cpus: &str,
    #[case] pids: &str,
) {
    let lines = service(COMPOSE, name);

    assert_eq!(value(&lines, "mem_limit"), Some(memory));
    assert_eq!(value(&lines, "memswap_limit"), Some(memory));
    assert_eq!(value(&lines, "cpus"), Some(cpus));
    assert_eq!(value(&lines, "pids_limit"), Some(pids));
}

/// Every tmpfs of every file mounts `noexec,nosuid,nodev`, written as a
/// list, so nothing written there runs (PR #97's review), and none takes
/// one back with `exec`, `suid` or `dev`, which would win as the later
/// option (PR #98's review).
#[test]
fn every_tmpfs_is_noexec_nosuid_and_nodev() {
    let mut mounts = 0;
    for (file, text) in FILES {
        for name in services(text) {
            let lines = service(text, name);
            assert_eq!(value(&lines, "tmpfs"), None, "{file}'s {name}: a list");
            for mount in list(&lines, "tmpfs") {
                let options: Vec<&str> = mount
                    .split_once(':')
                    .map_or_else(Vec::new, |(_, options)| options.split(',').collect());
                for flag in ["noexec", "nosuid", "nodev"] {
                    assert!(options.contains(&flag), "{file}'s {name}: {mount}");
                }
                for flag in ["exec", "suid", "dev"] {
                    assert!(!options.contains(&flag), "{file}'s {name}: {mount}");
                }
                mounts += 1;
            }
        }
    }
    assert_eq!(
        mounts, 7,
        "server, agent, setup, maintenance, caddy (2), the remote agent"
    );
}

/// No service mounts a tmpfs in a volume's long syntax (`type: tmpfs`),
/// whose options can't say `noexec`, `nosuid` or `nodev`, so the check
/// above would miss it (PR #98's review).
#[test]
fn no_volume_is_a_long_syntax_tmpfs() {
    for (file, text) in FILES {
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or_default();
            let words: String = line.split_whitespace().collect();
            assert!(
                !words.contains("type:tmpfs"),
                "{file}: a long-syntax tmpfs: {line}"
            );
        }
    }
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
    for name in ["server", "agent", "setup", "probe", "files", "maintenance"] {
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
        (COMPOSE, "files"),
        (COMPOSE, "maintenance"),
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
#[case::maintenance(COMPOSE, "maintenance", "SERVER", "512m", "1.0", "256")]
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

/// The volumes' file operations (`deploy/files.sh`): Caddy's pinned image for
/// its busybox, no network, the database's and the server certificate's
/// volumes at `/vol` with `nocopy` (it never creates or fills one), the
/// backups read-only, never the keys or the CA; started only by
/// `docker compose run` (S25, S38).
#[test]
fn the_files_service_reaches_the_database_the_server_pair_and_the_backups_alone() {
    let lines = service(COMPOSE, "files");
    let probe = value(&service(COMPOSE, "probe"), "image").unwrap();
    let written = trimmed(&lines);

    assert_eq!(value(&lines, "image"), Some(probe));
    assert_eq!(
        list(&lines, "volumes"),
        [
            "./files.sh:/afkfleet/files.sh:ro",
            "type: volume",
            "type: volume",
            "type: bind",
        ]
    );
    for expected in [
        "source: data",
        "target: /vol/data",
        "source: tls",
        "target: /vol/tls",
        "source: ${AFKFLEET_BACKUPS_DIR:-/srv/afkfleet-backups/backups}",
        "target: /vol/backups",
        "read_only: true",
        "create_host_path: false",
    ] {
        assert!(written.contains(&expected), "{expected}");
    }
    assert_eq!(
        written.iter().filter(|line| **line == "nocopy: true").count(),
        2
    );
    assert_eq!(
        value(&lines, "entrypoint"),
        Some(r#"["/bin/sh", "/afkfleet/files.sh"]"#)
    );
    assert_eq!(value(&lines, "network_mode"), Some("none"));
    assert_eq!(value(&lines, "profiles"), Some("[files]"));
    assert_eq!(value(&lines, "restart"), Some("\"no\""));
    assert_eq!(list(&lines, "tmpfs"), Vec::<&str>::new());
}

/// The server's commands while `serve` is stopped (`migrate`, `vault
/// status`, `vault rotate-key`, `agent list` and `disable`): the server's
/// image with no network, the database writable, the keys read-only, never
/// the CA, the server's pair or the backups (S25, S38).
#[test]
fn the_maintenance_service_writes_the_database_and_reads_the_keys_alone() {
    let lines = service(COMPOSE, "maintenance");

    assert_eq!(
        list(&lines, "volumes"),
        [
            "./prod/server.toml:/etc/afkfleet/server.toml:ro",
            "data:/data",
            "keys:/keys:ro",
        ]
    );
    assert_eq!(value(&lines, "network_mode"), Some("none"));
    assert_eq!(value(&lines, "profiles"), Some("[maintenance]"));
    assert_eq!(value(&lines, "restart"), Some("\"no\""));
    assert_eq!(value(&lines, "disable"), Some("true"), "no healthcheck");
}

/// The keys and the CA are read-only everywhere but `setup`, which makes
/// them, and never reach a service on Caddy's image (S25, S38).
#[test]
fn the_keys_and_the_ca_are_read_only_but_for_setup_and_never_on_caddys_image() {
    let mut checked = 0;
    for (file, text) in FILES {
        for name in services(text) {
            let lines = service(text, name);
            let caddy = value(&lines, "image").is_some_and(|image| image.starts_with("caddy:"));
            for mount in list(&lines, "volumes") {
                let source = mount.split(':').next().unwrap_or_default();
                if ["keys", "ca"].contains(&source) {
                    checked += 1;
                    assert!(!caddy, "{file}'s {name} mounts {mount}");
                    assert!(
                        name == "setup" || mount.ends_with(":ro"),
                        "{file}'s {name}: {mount}"
                    );
                }
            }
            for line in trimmed(&lines) {
                let long = line == "source: keys" || line == "source: ca";
                assert!(!long, "{file}'s {name}: a long-syntax {line}");
            }
        }
    }
    assert_eq!(
        checked, 6,
        "server (2), agent, setup (2), maintenance"
    );
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
        (COMPOSE, "files", false),
        (COMPOSE, "maintenance", true),
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

/// A service's name and the ports it publishes, as written.
type Published = (&'static str, &'static [&'static str]);

/// compose.yaml publishes nothing; each path's file its own port alone, the
/// host's API on loopback only; no metrics port anywhere. Every service of
/// every file, with exactly the ports it publishes: each path's own, never a
/// metrics port, nothing else (PR #97's review: a list per service, not a
/// text search for `"9480:`).
#[test]
fn nothing_is_published_but_each_paths_own_port() {
    let none: &[&str] = &[];
    let expected: [(&str, &str, &[Published]); 6] = [
        (
            "compose.yaml",
            COMPOSE,
            &[
                ("server", none),
                ("agent", none),
                ("setup", none),
                ("probe", none),
                ("files", none),
                ("maintenance", none),
            ],
        ),
        ("compose.edge.yaml", EDGE, &[("server", none)]),
        (
            "compose.caddy.yaml",
            CADDY,
            &[("caddy", &["\"80:80\"", "\"443:443\""])],
        ),
        (
            "compose.loopback.yaml",
            LOOPBACK,
            &[(
                "server",
                &["\"127.0.0.1:${AFKFLEET_HTTP_PORT:-8080}:8080\""],
            )],
        ),
        (
            "compose.grpc.yaml",
            GRPC,
            &[("server", &["\"${AFKFLEET_GRPC_PUBLISH:-7443}:7443\""])],
        ),
        ("compose.agent.yaml", REMOTE, &[("agent", none)]),
    ];
    for (file, text, published) in expected {
        let names: Vec<&str> = published.iter().map(|(name, _)| *name).collect();
        assert_eq!(services(text), names, "{file}'s services");
        for (name, ports) in published {
            let lines = service(text, name);
            assert_eq!(value(&lines, "ports"), None, "{file}'s {name}: a list");
            assert_eq!(list(&lines, "ports"), *ports, "{file}'s {name}");
            assert_eq!(value(&lines, "expose"), None, "{file}'s {name}");
            assert_eq!(list(&lines, "expose"), none, "{file}'s {name}");
        }
    }
}

/// Caddy is one pinned image everywhere it runs: the probe, afkfleet's own
/// Caddy and the smoke test's stand-in, and the weekly audit scans that one
/// (U12, U13).
#[test]
fn caddy_is_pinned_the_same_everywhere() {
    let probe = value(&service(COMPOSE, "probe"), "image").unwrap();
    let caddy = value(&service(CADDY, "caddy"), "image").unwrap();
    let stand_in = value(&service(STAND_IN, "proxy"), "image").unwrap();
    let files = value(&service(COMPOSE, "files"), "image").unwrap();
    let scanned: Vec<&str> = include_str!("../../../justfile")
        .split_whitespace()
        .filter(|word| word.starts_with("caddy:"))
        .collect();

    assert_eq!(caddy, probe);
    assert_eq!(stand_in, probe);
    assert_eq!(files, probe);
    assert_eq!(scanned, [probe, probe], "image-scan pulls and saves it");
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
        "-Server",
    ] {
        assert!(lines.contains(&expected), "the Caddyfile lacks {expected}");
    }
}

/// Caddy's settings must be set in `.env`, or compose refuses to start: an
/// empty ACME email, for one, would make Caddy restart in a loop (PR #97's
/// review).
#[test]
fn caddys_settings_must_be_set_in_env() {
    let caddy = service(CADDY, "caddy");

    for name in ["AFKFLEET_DOMAIN", "AFKFLEET_TLS", "AFKFLEET_ACME_EMAIL"] {
        let set = value(&caddy, name).unwrap();
        assert!(set.starts_with(&format!("${{{name}:?")), "{name}: {set}");
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
        (COMPOSE, "maintenance", "AFKFLEET_SERVER_IMAGE"),
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
        // Spaces, tabs and either quote go, so `= true`, `:\t"true"` and
        // `: 'true'` all read alike (PR #97's and PR #98's reviews).
        let text = text
            .to_ascii_lowercase()
            .replace([' ', '\t', '"', '\''], "");
        assert!(!text.contains("allow_offline=true"), "{}", file.display());
        assert!(!text.contains("allow_offline:true"), "{}", file.display());
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

/// `env.example` names the two images in comments alone: a release's files
/// pin their own images by digest as the compose files' defaults, so a host
/// `.env` sets one only to run other images (ADR-0022, S23, S34).
#[test]
fn env_example_names_the_images_in_comments_alone() {
    for name in ["AFKFLEET_SERVER_IMAGE", "AFKFLEET_AGENT_IMAGE"] {
        let lines: Vec<&str> = ENV_EXAMPLE
            .lines()
            .filter(|line| line.trim_start_matches('#').starts_with(&format!("{name}=")))
            .collect();
        assert_eq!(lines.len(), 1, "{name}: {lines:?}");
        assert!(
            lines.iter().all(|line| line.starts_with('#')),
            "{name} is set: {lines:?}"
        );
    }
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
