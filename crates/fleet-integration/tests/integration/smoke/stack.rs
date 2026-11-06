//! One smoke stack: a copy of `deploy/` in a temporary folder, with the
//! test's own `.env` and `compose.override.yaml` beside it, set up and
//! brought up under a project name of the test's own (ADR-0022, S19).
//!
//! - **The copy** holds the release's files as they are; the test adds only
//!   the host-only files an operator adds (`.env`, `compose.override.yaml`),
//!   so it runs exactly the compose files and configs of the repository.
//! - **The override** is the test's (U20): offline accounts allowed, for an
//!   account the share search asks on; ports published on ephemeral
//!   loopback ports instead of 80, 443 and 8080; and, off CI, the backups
//!   on a named volume, since a Windows folder shows as mode 777.
//! - **The setup:** on CI's throwaway runners (`AFKFLEET_SMOKE_SETUP=host`)
//!   `deploy/setup.sh init` itself, with `sudo` only for the backups'
//!   temporary folder; elsewhere the test runs the same steps through
//!   compose (no POSIX sh under PowerShell).
//! - **Cleanup:** `down --volumes --remove-orphans` for its own projects,
//!   before (an earlier run's leftovers) and after, the stand-in proxy's
//!   first. Images are kept.

use core::fmt::Write as _;
use core::time::Duration;
use std::fs;
use std::path::{Path, PathBuf};

use fleet_testkit::docker::{Cleanup, Compose, Finished, docker, finish};
use tempfile::TempDir;

use super::checks::Transcript;

/// The images `just test-stack` builds before the tests run.
pub(crate) const SERVER_IMAGE: &str = "afkfleet-server:dev";
pub(crate) const AGENT_IMAGE: &str = "afkfleet-agent:dev";
/// The config paths inside the containers.
pub(crate) const SERVER_CONFIG: &str = "/etc/afkfleet/server.toml";
pub(crate) const AGENT_CONFIG: &str = "/etc/afkfleet/agent.toml";
/// How long one quick call may take.
pub(crate) const QUICK: Duration = Duration::from_secs(60);
/// How long `up --wait` and the setup may take.
pub(crate) const SLOW: Duration = Duration::from_secs(240);

/// The release's files the copy holds, from `deploy/`.
const DEPLOY_FILES: [&str; 9] = [
    "compose.yaml",
    "compose.edge.yaml",
    "compose.caddy.yaml",
    "compose.loopback.yaml",
    "compose.grpc.yaml",
    "Caddyfile",
    "setup.sh",
    "prod/server.toml",
    "prod/agent.toml",
];

/// Which proxy path a stack takes (U5, S16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProxyPath {
    /// afkfleet's own Caddy.
    Caddy,
    /// An existing reverse proxy in a container on `afkfleet_edge`.
    Container,
    /// An existing reverse proxy on the host: the test itself.
    Host,
}

/// How the one-time steps run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Setup {
    /// `deploy/setup.sh init`, as root only for the backups' folder (CI).
    Script,
    /// The same steps through compose, the backups on a named volume.
    Steps,
}

impl Setup {
    /// `AFKFLEET_SMOKE_SETUP=host` picks the script (CI's runners).
    pub(crate) fn from_env() -> Self {
        match std::env::var("AFKFLEET_SMOKE_SETUP").as_deref() {
            Ok("host") => Self::Script,
            _ => Self::Steps,
        }
    }
}

/// The repository's root.
pub(crate) fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A running stack; see the [module docs](self). Dropping it takes its
/// projects down, the stand-in proxy's first.
#[derive(Debug)]
pub(crate) struct Stack {
    pub(crate) setup: Setup,
    pub(crate) compose: Compose,
    pub(crate) edge: Option<Compose>,
    pub(crate) net: Net,
    /// Every call's output, which the log check reads too.
    pub(crate) transcript: Transcript,
    _edge_cleanup: Option<Cleanup>,
    _cleanup: Cleanup,
    /// The copy, removed last.
    dir: TempDir,
}

/// A stack's addresses, apart from every other smoke stack's.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Net {
    /// The second octet of its subnets' 10.x prefix.
    n: u8,
}

impl Net {
    pub(crate) fn fleet_subnet(self) -> String {
        format!("10.213.{}.0/24", self.n)
    }
    pub(crate) fn fleet_gateway(self) -> String {
        format!("10.213.{}.1", self.n)
    }
    pub(crate) fn server_address(self) -> String {
        format!("10.213.{}.10", self.n)
    }
    pub(crate) fn edge_subnet(self) -> String {
        format!("10.213.{}.0/24", self.n + 1)
    }
    pub(crate) fn edge_gateway(self) -> String {
        format!("10.213.{}.1", self.n + 1)
    }
    pub(crate) fn proxy_address(self) -> String {
        format!("10.213.{}.2", self.n + 1)
    }
}

impl Stack {
    /// The project of `path`.
    pub(crate) fn project(path: ProxyPath) -> &'static str {
        match path {
            ProxyPath::Caddy => "afkfleet-smoke-caddy",
            ProxyPath::Container => "afkfleet-smoke-container",
            ProxyPath::Host => "afkfleet-smoke-host",
        }
    }

    /// Copies `deploy/`, writes the test's `.env` and override, and takes
    /// down whatever an earlier run left of its projects.
    pub(crate) fn prepare(path: ProxyPath) -> Self {
        assert_eq!(
            std::env::var("AFKFLEET_SMOKE_IMAGES_BUILT").as_deref(),
            Ok("1"),
            "run the smoke tests with `just test-stack`, which builds both images first"
        );
        let setup = Setup::from_env();
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("prod")).unwrap();
        for file in DEPLOY_FILES {
            fs::copy(repo().join("deploy").join(file), dir.path().join(file)).unwrap();
        }
        let net = Net {
            n: match path {
                ProxyPath::Caddy => 10,
                ProxyPath::Container => 20,
                ProxyPath::Host => 30,
            },
        };
        let project = Self::project(path);
        let backups = (setup == Setup::Script).then(|| host_backups(dir.path()));
        fs::write(
            dir.path().join(".env"),
            env_file(path, project, net, backups.as_deref()),
        )
        .unwrap();
        fs::write(
            dir.path().join("compose.override.yaml"),
            override_file(path, setup, backups.as_deref()),
        )
        .unwrap();
        let compose = Compose::new(dir.path(), project, &[]);
        let edge = (path == ProxyPath::Container).then(|| {
            let folder = repo().join("crates/fleet-integration/tests/integration/smoke");
            fs::copy(folder.join("edge.yaml"), dir.path().join("edge.yaml")).unwrap();
            fs::copy(
                folder.join("edge.Caddyfile"),
                dir.path().join("edge.Caddyfile"),
            )
            .unwrap();
            fs::write(
                dir.path().join("edge.env"),
                format!(
                    "AFKFLEET_PROXY_ADDRESS={}\nAFKFLEET_EDGE_NETWORK={project}_edge\n",
                    net.proxy_address()
                ),
            )
            .unwrap();
            Compose::new(dir.path(), "afkfleet-smoke-edge", &["edge.yaml"])
                .with_env_file(&dir.path().join("edge.env"))
        });
        let mut transcript = Transcript::default();
        if let Some(edge) = &edge {
            transcript.keep(&edge.down(SLOW).unwrap());
        }
        transcript.keep(&compose.down(SLOW).unwrap());
        Self {
            setup,
            _edge_cleanup: edge.as_ref().map(|edge| edge.cleanup(SLOW)),
            _cleanup: compose.cleanup(SLOW),
            compose,
            edge,
            net,
            transcript,
            dir,
        }
    }

    /// The copy's folder.
    pub(crate) fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// Runs `args` and keeps their output; fails unless they succeed.
    pub(crate) fn ok(&mut self, args: &[&str], deadline: Duration) -> String {
        let finished = self.compose.run(args, deadline).unwrap();
        self.transcript.keep(&finished);
        assert!(
            finished.success,
            "docker compose {args:?} failed:\n{}\n{}",
            finished.stdout, finished.stderr
        );
        finished.stdout
    }

    /// The one-time steps (S14, S18), then every service up and healthy.
    pub(crate) fn set_up(&mut self) {
        match self.setup {
            Setup::Script => {
                let mut command = std::process::Command::new("sh");
                command.current_dir(self.dir()).args(["setup.sh", "init"]);
                let finished = finish(command, None, SLOW).unwrap();
                self.transcript.keep(&finished);
                assert!(
                    finished.success,
                    "setup.sh init failed:\n{}\n{}",
                    finished.stdout, finished.stderr
                );
            }
            Setup::Steps => {
                self.ok(
                    &[
                        "run",
                        "--rm",
                        "-T",
                        "setup",
                        "vault",
                        "create-key",
                        "--config",
                        SERVER_CONFIG,
                    ],
                    SLOW,
                );
                self.ok(
                    &[
                        "run",
                        "--rm",
                        "-T",
                        "setup",
                        "pki",
                        "init",
                        "--config",
                        SERVER_CONFIG,
                        "--dns",
                        "server",
                    ],
                    SLOW,
                );
                self.ok(
                    &[
                        "up",
                        "--detach",
                        "--wait",
                        "--wait-timeout",
                        "180",
                        "server",
                    ],
                    SLOW,
                );
                self.enroll_agent();
                self.ok(&["up", "--detach", "--wait", "--wait-timeout", "180"], SLOW);
            }
        }
        if let Some(edge) = &self.edge {
            let finished = edge
                .run(&["up", "--detach", "--wait", "--wait-timeout", "120"], SLOW)
                .unwrap();
            self.transcript.keep(&finished);
            assert!(
                finished.success,
                "the stand-in proxy didn't start:\n{}",
                finished.stderr
            );
        }
    }

    /// The local agent's enrollment as setup.sh pipes it: the bundle only
    /// in memory, never in the transcript, which keeps the token command's
    /// stderr alone.
    fn enroll_agent(&mut self) {
        let token = self
            .compose
            .run(
                &[
                    "exec",
                    "-T",
                    "server",
                    "/usr/local/bin/afkfleet-server",
                    "agent",
                    "create-enrollment-token",
                    "--config",
                    SERVER_CONFIG,
                ],
                QUICK,
            )
            .unwrap();
        self.transcript.keep_stderr(&token);
        assert!(token.success, "no enrollment token:\n{}", token.stderr);
        let enrolled = self
            .compose
            .run_with_stdin(
                &[
                    "run",
                    "--rm",
                    "-T",
                    "--no-deps",
                    "agent",
                    "enroll",
                    "--config",
                    AGENT_CONFIG,
                ],
                token.stdout.as_bytes(),
                QUICK,
            )
            .unwrap();
        self.transcript.keep(&enrolled);
        self.transcript.forbid(token.stdout.trim());
        assert!(
            enrolled.success,
            "the agent didn't enroll:\n{}",
            enrolled.stderr
        );
    }

    /// The host port `service`'s container `port` is published on.
    pub(crate) fn port(&mut self, compose: Option<&Compose>, service: &str, port: &str) -> u16 {
        let compose = compose.unwrap_or(&self.compose).clone();
        let finished = compose.run(&["port", service, port], QUICK).unwrap();
        self.transcript.keep(&finished);
        assert!(
            finished.success,
            "{service}:{port} isn't published: {}",
            finished.stderr
        );
        finished
            .stdout
            .trim()
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse().ok())
            .unwrap_or_else(|| panic!("no port in {:?}", finished.stdout))
    }

    /// `docker <args>`, kept.
    pub(crate) fn docker(&mut self, args: &[&str]) -> Finished {
        let finished = docker(self.dir.path(), args, QUICK).unwrap();
        self.transcript.keep(&finished);
        finished
    }

    /// Every service's whole log, of both projects, into the transcript the
    /// log check reads.
    pub(crate) fn collect_logs(&mut self) {
        let logs = self.compose.run(&["logs", "--no-color"], QUICK).unwrap();
        self.transcript.keep(&logs);
        if let Some(edge) = &self.edge {
            let logs = edge.run(&["logs", "--no-color"], QUICK).unwrap();
            self.transcript.keep(&logs);
        }
    }

    /// The container ID of `service`.
    pub(crate) fn container(&mut self, service: &str) -> String {
        let id = self.ok(&["ps", "--all", "--quiet", service], QUICK);
        let id = id.trim().to_owned();
        assert!(!id.is_empty(), "{service} has no container");
        id
    }
}

/// The backups' host folder as an operator's host has it (S15): a parent of
/// its own, root's and writable by root alone, which setup.sh checks and
/// never changes (PR #97's review), and the folder in it for the server's
/// user. Made in `dir` by the one step `sudo` runs on CI's throwaway
/// runners; `dir` itself is the runner's user's.
fn host_backups(dir: &Path) -> PathBuf {
    let parent = dir.join("srv");
    let backups = parent.join("backups");
    for (owner, mode, folder) in [("root", "755", &parent), ("65532", "700", &backups)] {
        let mut command = std::process::Command::new("sudo");
        command.args(["-n", "install", "-d", "-o", owner, "-g", owner, "-m", mode]);
        command.arg(folder);
        let finished = finish(command, None, QUICK).unwrap();
        assert!(finished.success, "sudo install failed: {}", finished.stderr);
    }
    backups
}

/// The test's `.env` for `path`'s `project` on `net` (U18): every line an
/// operator would write, the domain `localhost` on Caddy's internal CA, and
/// one setting of the server's own that the checks read back. Compose
/// splits `COMPOSE_FILE` at `;` on Windows; the host's `:` is set here, as a
/// Linux host has it by default.
fn env_file(path: ProxyPath, project: &str, net: Net, backups: Option<&Path>) -> String {
    let files = match path {
        ProxyPath::Caddy => "compose.yaml:compose.edge.yaml:compose.caddy.yaml",
        ProxyPath::Container => "compose.yaml:compose.edge.yaml",
        ProxyPath::Host => "compose.yaml:compose.loopback.yaml",
    };
    let mut env = format!(
        "COMPOSE_PROJECT_NAME={project}\n\
         COMPOSE_PATH_SEPARATOR=:\n\
         COMPOSE_FILE={files}:compose.override.yaml\n\
         AFKFLEET_SERVER_IMAGE={SERVER_IMAGE}\n\
         AFKFLEET_AGENT_IMAGE={AGENT_IMAGE}\n\
         AFKFLEET_DOMAIN=localhost\n\
         AFKFLEET_TLS=internal\n\
         AFKFLEET_ACME_EMAIL=smoke@example.com\n\
         AFKFLEET_FLEET_SUBNET={}\n\
         AFKFLEET_FLEET_GATEWAY={}\n\
         AFKFLEET_FLEET_RANGE={}\n\
         AFKFLEET_SERVER_FLEET_ADDRESS={}\n\
         AFKFLEET_EDGE_SUBNET={}\n\
         AFKFLEET_EDGE_GATEWAY={}\n\
         AFKFLEET_EDGE_RANGE={}\n\
         AFKFLEET_PROXY_ADDRESS={}\n\
         {SETTING}\n",
        net.fleet_subnet(),
        net.fleet_gateway(),
        net.fleet_subnet().replace(".0/24", ".128/25"),
        net.server_address(),
        net.edge_subnet(),
        net.edge_gateway(),
        net.edge_subnet().replace(".0/24", ".128/25"),
        net.proxy_address(),
    );
    if let Some(backups) = backups {
        let _ = writeln!(env, "AFKFLEET_BACKUPS_DIR={}", backups.display());
    }
    env
}

/// The server's setting the `.env` sets, which must reach its container.
pub(crate) const SETTING: &str = "AFKFLEET_SERVER__DATABASE__BACKUP_KEEP=7";

/// The test's `compose.override.yaml`; see the [module docs](self).
fn override_file(path: ProxyPath, setup: Setup, backups: Option<&Path>) -> String {
    let mut server = String::from(
        "  server:\n    environment:\n      AFKFLEET_SERVER__ACCOUNTS__ALLOW_OFFLINE: \"true\"\n",
    );
    if setup == Setup::Steps {
        server.push_str("    volumes:\n      - smoke-backups:/backups\n");
    }
    if path == ProxyPath::Host {
        server.push_str("    ports: !override\n      - \"127.0.0.1::8080\"\n");
    }
    let probe_backups = match backups {
        Some(dir) => format!(
            "      - type: bind\n        source: {}\n        target: /backups\n        read_only: true\n",
            dir.display()
        ),
        None => "      - smoke-backups:/backups:ro\n".to_owned(),
    };
    let mut yaml = format!(
        "# The smoke test's own override (ADR-0022, S19, U20); never in deploy/.\nservices:\n{server}  probe:\n    volumes:\n{probe_backups}"
    );
    if path == ProxyPath::Caddy {
        yaml.push_str("  caddy:\n    ports: !override\n      - \"127.0.0.1::443\"\n");
    }
    if setup == Setup::Steps {
        yaml.push_str("volumes:\n  smoke-backups:\n");
    }
    yaml
}
