//! One chaos test's world (ADR-0033): the production compose project, from
//! both images built here, installed as the smoke tests install it, plus
//! what the faults need and production doesn't have:
//!
//! - **A Minecraft server** in the project, offline-mode, on a network of
//!   its own (`play`) beside the fleet's: three bots on offline accounts
//!   join it for real, through the agent's own connector. The agent may
//!   dial that one private network and no public address.
//! - **A second agent** (`agent-two`, enrolled as `second`), which holds no
//!   bot: whatever happens to the first, the server never moves a bot to
//!   it, and the test would see it if it did.
//! - **The API on a fixed loopback port,** so the Owner's session outlives
//!   the server's restarts as a user's does.
//! - **For the full disk:** the database's volume and the backups' as
//!   `tmpfs` volumes with a size, a few megabytes in Docker's memory, never
//!   on its disk; and a `holder` container that keeps both mounted, since a
//!   `tmpfs` volume is gone when nothing mounts it.
//!
//! Its faults are Docker's own: `kill --signal SIGKILL`, `stop`, a network
//! disconnected from a container and connected again, and a volume written
//! until it's full.
//!
//! What it reads back: the API as the Owner; the Minecraft server's own log
//! (every join, and a second login of an account that's online, which
//! would be a bot on two agents); the server's `database check`, `vault
//! status` and `backup` commands; and both metrics ports through `probe`.

use core::time::Duration;
use std::time::Instant;

use fleet_api_types::accounts::CreateOfflineAccountRequest;
use fleet_api_types::agents::{AgentResponse, AgentStatus};
use fleet_api_types::audit::AuditPageQuery;
use fleet_api_types::bots::{BotPatchRequest, BotResponse, BotStateKind, BotStatusKind};
use fleet_api_types::{Open, PageQuery, SecretText};
use fleet_client::{ClientError, Trust};
use fleet_core::id::{AccountId, BotId};
use fleet_testkit::docker::{Compose, ContainerState};

use super::super::stack::{AGENT_CONFIG, Net, ProxyPath, QUICK, SERVER_CONFIG, SLOW, Spec, Stack};
use super::super::{Owner, create_owner, draw_password};
use crate::managed_check::minecraft::{IMAGE, TAG, VERSION};

/// The bots' accounts: offline ones, by their player names.
pub(crate) const BOTS: [&str; 3] = ["AfkBot1", "AfkBot2", "AfkBot3"];

/// Where every bot is sent: the project's Minecraft server, by its service
/// name.
const MINECRAFT: &str = "minecraft:25565";

/// How long the Minecraft server's first start may take: it downloads its
/// jar and makes its world.
const MINECRAFT_START: Duration = Duration::from_mins(8);

/// How long pulling the Minecraft server's image may take, on a machine
/// that doesn't have it.
const MINECRAFT_PULL: Duration = Duration::from_mins(15);

/// The least time between two questions to the API in a wait: under the
/// ten a second it allows one user.
const ASK_EVERY: Duration = Duration::from_millis(500);

/// The local agent's name, and the second one's.
pub(crate) const LOCAL: &str = "local";
pub(crate) const SECOND: &str = "second";

/// The second agent's compose profile and service.
const SECOND_PROFILE: &str = "second";
const SECOND_SERVICE: &str = "agent-two";

/// The capped volumes' holder: its compose profile and service.
const HOLDER: &str = "holder";

/// The image the holder runs: Caddy's pinned one, which the project uses
/// already for its `probe` and `files` services, read from
/// `deploy/compose.yaml`.
fn busybox_image() -> String {
    let compose =
        std::fs::read_to_string(super::super::stack::repo().join("deploy/compose.yaml")).unwrap();
    compose
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("image: caddy:"))
        .map(|rest| format!("caddy:{rest}"))
        .expect("compose.yaml pins Caddy's image")
}

/// The API's fixed loopback port for the stack on `net`.
fn api_port(net: Net) -> u16 {
    18_000 + u16::from(net.number())
}

/// The agents' retries in a chaos stack: a bot tries again after 20 s at
/// most, where production waits up to 5 minutes, and the circuit breaker is
/// out of reach, where production's would open after 8 failures for a
/// quarter of an hour. A test takes its Minecraft server down twice within
/// minutes and waits for every bot to be back; how the backoff grows and
/// when the breaker opens are fleet-runtime's own tests.
const RETRY: &str = "      AFKFLEET_AGENT__RETRY__MAX_DELAY_SECS: \"20\"
      AFKFLEET_AGENT__RETRY__CIRCUIT_FAILURES: \"1000\"
";

/// How large the capped volumes are: the database's and the backups'.
const DATA_CAP: &str = "32m";
const BACKUPS_CAP: &str = "16m";

/// The compose file a chaos stack adds to the production ones; see the
/// [module docs](self).
pub(crate) fn additions(net: Net) -> String {
    compose_file(net, false)
}

/// [`additions`], with the database's and the backups' volumes capped and
/// the holder that keeps them mounted.
pub(crate) fn capped_additions(net: Net) -> String {
    compose_file(net, true)
}

/// The capped stack's `holder`, which keeps both capped volumes mounted,
/// and the `files` service's view of the capped backups.
fn holder_services() -> String {
    let image = busybox_image();
    format!(
        r#"  {HOLDER}:
    image: {image}
    profiles: [{HOLDER}]
    user: "65532:65532"
    read_only: true
    cap_drop: [ALL]
    security_opt:
      - no-new-privileges:true
    restart: "no"
    network_mode: none
    entrypoint: ["sleep", "infinity"]
    stop_grace_period: 1s
    volumes:
      - data:/vol/data
      - chaos-backups:/vol/backups
    mem_limit: 64m
    cpus: 0.5
    pids_limit: 16
  files:
    volumes:
      - chaos-backups:/vol/backups:ro
"#
    )
}

/// The two capped volumes: `tmpfs` with a size, owned by the images' user.
fn capped_volumes() -> String {
    format!(
        r#"  # In Docker's memory, never on its disk, and no larger than this.
  data:
    driver: local
    driver_opts:
      type: tmpfs
      device: tmpfs
      o: "size={DATA_CAP},uid=65532,gid=65532,mode=0700"
  chaos-backups:
    driver: local
    driver_opts:
      type: tmpfs
      device: tmpfs
      o: "size={BACKUPS_CAP},uid=65532,gid=65532,mode=0700"
"#
    )
}

fn compose_file(net: Net, capped: bool) -> String {
    let play = net.play_subnet();
    let port = api_port(net);
    // The capped stack's server writes its backups into the capped volume.
    let server_backups = if capped {
        "    volumes:\n      - chaos-backups:/backups\n"
    } else {
        ""
    };
    let (holder, capped_volumes) = if capped {
        (holder_services(), capped_volumes())
    } else {
        (String::new(), String::new())
    };
    format!(
        r#"# The chaos tests' own additions (ADR-0033); never in deploy/.
services:
  server:
    ports: !override
      - "127.0.0.1:{port}:8080"
    environment:
      # The least a config allows, so a few hundred lines give retention work.
      AFKFLEET_SERVER__CHAT__MAX_LINES_PER_BOT: "100"
{server_backups}  minecraft:
    image: {IMAGE}:{TAG}
    environment:
      EULA: "TRUE"
      TYPE: VANILLA
      VERSION: "{VERSION}"
      ONLINE_MODE: "FALSE"
      ENFORCE_SECURE_PROFILE: "FALSE"
      LEVEL_TYPE: minecraft:flat
      VIEW_DISTANCE: "4"
      SIMULATION_DISTANCE: "4"
      SPAWN_PROTECTION: "0"
      MODE: survival
      DIFFICULTY: peaceful
      ENABLE_AUTOPAUSE: "FALSE"
      ENABLE_RCON: "TRUE"
      MEMORY: 1G
    networks: [play]
    stop_grace_period: 60s
    mem_limit: 2g
    healthcheck:
      test: mc-health
      start_period: 3m
      interval: 5s
      retries: 20
  agent:
    networks: [fleet, play]
    environment:
      AFKFLEET_AGENT__CONTROL_PLANE__PUBLIC_TARGETS: "false"
      AFKFLEET_AGENT__CONTROL_PLANE__PRIVATE_TARGETS: '["{play}"]'
{RETRY}  {SECOND_SERVICE}:
    image: ${{AFKFLEET_AGENT_IMAGE}}
    profiles: [{SECOND_PROFILE}]
    pull_policy: never
    user: "65532:65532"
    read_only: true
    cap_drop: [ALL]
    security_opt:
      - no-new-privileges:true
    restart: unless-stopped
    env_file:
      - path: .env
        required: true
    environment:
      AFKFLEET_AGENT__NAME: {SECOND}
      AFKFLEET_AGENT__CONTROL_PLANE__PUBLIC_TARGETS: "false"
      AFKFLEET_AGENT__CONTROL_PLANE__PRIVATE_TARGETS: '["{play}"]'
{RETRY}    volumes:
      - ./prod/agent.toml:/etc/afkfleet/agent.toml:ro
      - agent-two-data:/data
      - ca:/ca:ro
    tmpfs:
      - /tmp:size=1m,noexec,nosuid,nodev
    networks: [fleet, play]
    stop_grace_period: 20s
    mem_limit: 512m
    cpus: 1.0
    pids_limit: 512
{holder}networks:
  play:
    ipam:
      config:
        - subnet: {play}
volumes:
  agent-two-data:
{capped_volumes}"#
    )
}

/// Asks `ask` until it answers: woken by `service`'s next log line, or
/// every 2 s, never more often than [`ASK_EVERY`], within [`SLOW`].
pub(crate) async fn until<T>(
    compose: &Compose,
    service: &str,
    what: &str,
    mut ask: impl AsyncFnMut() -> Option<T>,
) -> T {
    let follower = compose.follow(service).unwrap();
    let started = Instant::now();
    loop {
        let asked = Instant::now();
        if let Some(answer) = ask().await {
            return answer;
        }
        assert!(started.elapsed() < SLOW, "{what}: not within {SLOW:?}");
        follower.wait(Duration::from_secs(2));
        while let Some(left) = ASK_EVERY.checked_sub(asked.elapsed()) {
            follower.wait(left);
        }
    }
}

/// A running chaos stack; see the [module docs](self).
pub(crate) struct Chaos {
    pub(crate) stack: Stack,
    pub(crate) owner: Owner,
    /// The three bots, in [`BOTS`]' order.
    pub(crate) bots: Vec<BotId>,
    /// Where the API answers, the same through every restart.
    pub(crate) url: String,
    /// The accounts the API said it created, which every later check must
    /// find, each with its audit entry.
    acknowledged: Vec<(String, AccountId)>,
    /// The newest backup taken after a fault.
    backup: Option<String>,
    /// Whether the volumes are capped, and their holder runs.
    capped: bool,
}

impl Chaos {
    /// Installs the stack as `project` on `net`, with the Minecraft server
    /// and the second agent, the Owner logged in and the three bots online
    /// on the local agent.
    pub(crate) async fn start(project: &'static str, net: u8, capped: bool) -> Self {
        let mut stack = Stack::prepare_as(Spec {
            path: ProxyPath::Host,
            project,
            net,
            make_backups: true,
            ops: true,
            grpc: None,
            extra: Some(if capped { capped_additions } else { additions }),
        });
        if capped {
            // First, and for the test's whole life: a tmpfs volume lives
            // only while something mounts it.
            stack.ok(&["--profile", HOLDER, "up", "--detach", HOLDER], SLOW);
        }
        // Beside the install: its first start downloads its jar.
        stack.ok(&["up", "--detach", "minecraft"], MINECRAFT_PULL);
        stack.set_up();
        stack.ok(
            &[
                "up",
                "--detach",
                "--wait",
                "--wait-timeout",
                "420",
                "minecraft",
            ],
            MINECRAFT_START,
        );
        let password = draw_password();
        stack.transcript.forbid(&password);
        create_owner(&mut stack, &password);
        let url = format!("http://127.0.0.1:{}", api_port(stack.net));
        let owner = Owner::first(&url, Trust::System, &password, &mut stack).await;
        let mut chaos = Self {
            stack,
            owner,
            bots: Vec::new(),
            url,
            acknowledged: Vec::new(),
            backup: None,
            capped,
        };
        for name in BOTS {
            let bot = chaos.bot(name).await;
            chaos.bots.push(bot);
        }
        chaos.settled().await;
        // Only now, with every bot on the local agent: the second one holds
        // none, and must never get one.
        chaos.enroll_second();
        chaos.agent(SECOND, AgentStatus::Online).await;
        chaos
    }

    /// `args` for the second agent's service, with its profile.
    fn second<'a>(args: &[&'a str]) -> Vec<&'a str> {
        let mut full = vec!["--profile", SECOND_PROFILE];
        full.extend_from_slice(args);
        full
    }

    /// The second agent, enrolled as the local one is and started.
    fn enroll_second(&mut self) {
        let token = self
            .stack
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
        assert!(token.success, "no enrollment token:\n{}", token.stderr);
        self.stack.transcript.forbid(token.stdout.trim());
        let enrolled = self
            .stack
            .compose
            .run_with_stdin(
                &Self::second(&[
                    "run",
                    "--rm",
                    "-T",
                    "--no-deps",
                    SECOND_SERVICE,
                    "enroll",
                    "--config",
                    AGENT_CONFIG,
                ]),
                token.stdout.as_bytes(),
                QUICK,
            )
            .unwrap();
        self.stack.transcript.keep(&enrolled);
        assert!(
            enrolled.success,
            "the second agent didn't enroll:\n{}",
            enrolled.stderr
        );
        self.stack.ok(
            &Self::second(&[
                "up",
                "--detach",
                "--wait",
                "--wait-timeout",
                "180",
                SECOND_SERVICE,
            ]),
            SLOW,
        );
    }

    /// A new offline account `name` and its bot, sent to the project's
    /// Minecraft server and started.
    async fn bot(&mut self, name: &str) -> BotId {
        let client = &self.owner.client;
        let account = client
            .create_offline_account(&CreateOfflineAccountRequest {
                name: SecretText::new(name.to_owned()),
            })
            .await
            .unwrap();
        let bots = client
            .bots(&PageQuery {
                before: None,
                limit: Some(100),
            })
            .await
            .unwrap();
        let bot = bots
            .items
            .iter()
            .find(|bot| bot.account.id == account.id)
            .expect("the account's bot is listed")
            .id;
        client
            .patch_bot(
                bot,
                &BotPatchRequest {
                    server_address: MINECRAFT.to_owned(),
                },
            )
            .await
            .unwrap();
        client.start_bot(bot).await.unwrap();
        bot
    }

    /// One more bot, on a new offline account `name`, started like the
    /// first three.
    pub(crate) async fn new_bot(&mut self, name: &str) {
        let bot = self.bot(name).await;
        self.bots.push(bot);
    }

    /// `bot` as the API shows it now, or nothing while the server doesn't
    /// answer.
    pub(crate) async fn shown(&self, bot: BotId) -> Option<BotResponse> {
        self.owner.client.bot(bot).await.ok()
    }

    /// Whether `shown` is online as its agent reports it now.
    pub(crate) fn reported_online(shown: &BotResponse) -> bool {
        shown.status.kind.known() == Some(BotStatusKind::Reported)
            && shown.status.state.as_ref().and_then(Open::known) == Some(BotStateKind::Online)
    }

    /// Waits until every bot is back at its desired state: online, as its
    /// agent reports it now.
    pub(crate) async fn settled(&self) {
        let bots = self.bots.clone();
        until(
            &self.stack.compose,
            "server",
            "every bot online",
            async || {
                for bot in &bots {
                    if !self
                        .shown(*bot)
                        .await
                        .is_some_and(|shown| Self::reported_online(&shown))
                    {
                        return None;
                    }
                }
                Some(())
            },
        )
        .await;
    }

    /// Every agent as the API lists it now, or nothing while the server
    /// doesn't answer.
    pub(crate) async fn agents(&self) -> Option<Vec<AgentResponse>> {
        self.owner
            .client
            .agents(&PageQuery {
                before: None,
                limit: Some(100),
            })
            .await
            .ok()
            .map(|page| page.items)
    }

    /// Waits until the agent `name` reads `status`; the agent then.
    pub(crate) async fn agent(&self, name: &str, status: AgentStatus) -> AgentResponse {
        until(
            &self.stack.compose,
            "server",
            &format!("the agent {name} {status:?}"),
            async || {
                self.agents().await?.into_iter().find(|agent| {
                    agent.name.as_str() == name && agent.status.known() == Some(status)
                })
            },
        )
        .await
    }

    /// The Minecraft server's whole log.
    pub(crate) fn minecraft_log(&mut self) -> String {
        self.stack.ok(
            &["logs", "--no-color", "--no-log-prefix", "minecraft"],
            QUICK,
        )
    }

    /// How often `name` joined the Minecraft server so far.
    pub(crate) fn joins(&mut self, name: &str) -> usize {
        let joined = format!("]: {name} joined the game");
        self.minecraft_log()
            .lines()
            .filter(|line| line.ends_with(&joined))
            .count()
    }

    /// How often the Minecraft server ended a session because its account
    /// logged in a second time: a bot on two agents at once.
    pub(crate) fn second_logins(&mut self) -> usize {
        self.minecraft_log()
            .lines()
            .filter(|line| line.contains("logged in from another location"))
            .count()
    }

    /// Waits until `name` joined the Minecraft server `times` times.
    pub(crate) async fn joined(&mut self, name: &str, times: usize) {
        let compose = self.stack.compose.clone();
        let joined = format!("]: {name} joined the game");
        until(
            &compose,
            "minecraft",
            &format!("{name} joined {times} times"),
            async || {
                let log = compose
                    .run(
                        &["logs", "--no-color", "--no-log-prefix", "minecraft"],
                        QUICK,
                    )
                    .ok()?;
                (log.stdout
                    .lines()
                    .filter(|line| line.ends_with(&joined))
                    .count()
                    >= times)
                    .then_some(())
            },
        )
        .await;
    }

    /// `command` through the Minecraft server's own console; its answer.
    pub(crate) fn rcon(&mut self, command: &[&str]) -> String {
        let mut args = vec!["exec", "-T", "minecraft", "rcon-cli"];
        args.extend_from_slice(command);
        self.stack.ok(&args, QUICK)
    }

    /// A write the API acknowledges: a new offline account `name`. Every
    /// later [`Chaos::assert_kept`] finds it.
    pub(crate) async fn write(&mut self, name: &str) {
        if let Err(error) = self.try_write(name).await {
            // The server's own last lines say why: its errors and warnings,
            // which hold no secret (the transcript checks that at the end of
            // a test that gets there).
            let log = self
                .stack
                .compose
                .run(
                    &[
                        "logs",
                        "--no-color",
                        "--no-log-prefix",
                        "--tail",
                        "400",
                        "server",
                    ],
                    QUICK,
                )
                .map(|finished| finished.stdout)
                .unwrap_or_default();
            let said: Vec<&str> = log
                .lines()
                .filter(|line| line.contains("\"ERROR\"") || line.contains("\"WARN\""))
                .collect();
            let from = said.len().saturating_sub(25);
            panic!(
                "the API refused {name}: {error:?}\nthe server's last errors and warnings:\n{}",
                said.get(from..).unwrap_or_default().join("\n")
            );
        }
    }

    /// A write the API may refuse: a new offline account `name`. One it
    /// takes counts as acknowledged.
    pub(crate) async fn try_write(&mut self, name: &str) -> Result<(), ClientError> {
        let account = self
            .owner
            .client
            .create_offline_account(&CreateOfflineAccountRequest {
                name: SecretText::new(name.to_owned()),
            })
            .await?;
        self.acknowledged.push((name.to_owned(), account.id));
        Ok(())
    }

    /// Every write the API acknowledged is still there, with its audit
    /// entry.
    pub(crate) async fn assert_kept(&self) {
        let client = &self.owner.client;
        let listed = client
            .accounts(&PageQuery {
                before: None,
                limit: Some(100),
            })
            .await
            .unwrap()
            .items;
        for (name, id) in &self.acknowledged {
            assert!(
                listed.iter().any(|account| account.id == *id),
                "the acknowledged account {name} is gone"
            );
            let entries = client
                .audit(&AuditPageQuery {
                    before: None,
                    limit: Some(10),
                    target_kind: Some("account".to_owned()),
                    target_id: Some(id.to_string()),
                })
                .await
                .unwrap()
                .items;
            assert_ne!(entries.len(), 0, "{name} has no audit entry");
        }
    }

    /// The server's `database check` says the database is sound: through
    /// the running server's container, or through `maintenance` while it's
    /// down.
    pub(crate) fn assert_sound(&mut self, running: bool) {
        let said = if running {
            self.stack.ok(
                &[
                    "exec",
                    "-T",
                    "server",
                    "/usr/local/bin/afkfleet-server",
                    "database",
                    "check",
                    "--config",
                    SERVER_CONFIG,
                ],
                QUICK,
            )
        } else {
            self.stack.maintenance(&["database", "check"])
        };
        assert_eq!(said.trim(), fleet_server::cli::database::SOUND);
    }

    /// What must hold after every fault, once the services are back: every
    /// bot at its desired state and both agents online; no account ever
    /// logged in twice; every acknowledged write there with its audit
    /// entry; the database sound; every secret sealed with a key the server
    /// has; and a backup taken now, which the server checks before it
    /// counts and [`Chaos::end`] restores.
    pub(crate) async fn after_the_fault(&mut self) {
        self.settled().await;
        self.agent(LOCAL, AgentStatus::Online).await;
        self.agent(SECOND, AgentStatus::Online).await;
        assert_eq!(self.second_logins(), 0, "a bot ran on two agents");
        self.assert_kept().await;
        self.assert_sound(true);
        self.stack.ok(
            &[
                "exec",
                "-T",
                "server",
                "/usr/local/bin/afkfleet-server",
                "vault",
                "status",
                "--config",
                SERVER_CONFIG,
            ],
            QUICK,
        );
        self.backup = Some(self.stack.backup());
    }

    /// The Owner logs in anew with the same password and authenticator:
    /// the vault opens the secret it sealed.
    pub(crate) async fn log_in_again(&mut self) {
        let url = self.url.clone();
        self.owner.again(&url, Trust::System, &self.stack).await;
    }

    /// Kills `service` with `SIGKILL`; its container then, which Docker
    /// may have started again already.
    pub(crate) fn kill(&mut self, service: &str) -> ContainerState {
        let id = self.stack.container(service);
        self.stack
            .ok(&["kill", "--signal", "SIGKILL", service], QUICK);
        let inspect = self.stack.docker(&["inspect", &id]);
        ContainerState::from_inspect(&inspect.stdout).unwrap()
    }

    /// Stops `service` as an operator does; its exit code.
    pub(crate) fn stop(&mut self, service: &str) -> i64 {
        let id = self.stack.container(service);
        self.stack.ok(&["stop", service], SLOW);
        let inspect = self.stack.docker(&["inspect", &id]);
        ContainerState::from_inspect(&inspect.stdout)
            .unwrap()
            .exit_code
    }

    /// Starts `service` and waits until it's healthy.
    pub(crate) fn bring_up(&mut self, service: &str) {
        self.stack.ok(
            &["up", "--detach", "--wait", "--wait-timeout", "420", service],
            MINECRAFT_START,
        );
    }

    /// The project's fleet network, which the server and the agents share.
    fn fleet_network(&self) -> String {
        format!("{}_fleet", self.stack.compose.project())
    }

    /// Cuts `service` off the fleet network: nothing reaches the server
    /// from it any more, and nothing the server sends reaches it. Its other
    /// networks stay.
    pub(crate) fn partition(&mut self, service: &str) {
        let id = self.stack.container(service);
        let network = self.fleet_network();
        let cut = self.stack.docker(&["network", "disconnect", &network, &id]);
        assert!(cut.success, "{}", cut.stderr);
    }

    /// Puts `service` back on the fleet network, under its service name.
    pub(crate) fn heal(&mut self, service: &str) {
        let id = self.stack.container(service);
        let network = self.fleet_network();
        let back = self
            .stack
            .docker(&["network", "connect", "--alias", service, &network, &id]);
        assert!(back.success, "{}", back.stderr);
    }

    /// `script` in the capped volumes' holder; whatever it printed.
    fn in_holder(&mut self, script: &str) -> String {
        assert!(self.capped, "only a capped stack has a holder");
        let finished = self
            .stack
            .compose
            .run(
                &[
                    "--profile",
                    HOLDER,
                    "exec",
                    "-T",
                    HOLDER,
                    "sh",
                    "-c",
                    script,
                ],
                SLOW,
            )
            .unwrap();
        self.stack.transcript.keep(&finished);
        format!("{}{}", finished.stdout, finished.stderr)
    }

    /// Fills the capped volume `volume` (`data` or `backups`) to its last
    /// byte; how many kilobytes are free then, as `df` reads it.
    pub(crate) fn fill(&mut self, volume: &str) -> u64 {
        // The first dd ends when the volume is full, the second takes what
        // a smaller block still fits.
        let left = self.in_holder(&format!(
            "dd if=/dev/zero of=/vol/{volume}/chaos-filler bs=64k 2>/dev/null; \
             dd if=/dev/zero of=/vol/{volume}/chaos-filler-tail bs=512 2>/dev/null; \
             df -k /vol/{volume} | tail -n 1"
        ));
        left.split_whitespace()
            .nth(3)
            .and_then(|free| free.parse().ok())
            .unwrap_or_else(|| panic!("df's line can't be read: {left}"))
    }

    /// Gives the capped volume `volume` its room back.
    pub(crate) fn free(&mut self, volume: &str) {
        self.in_holder(&format!(
            "rm -f /vol/{volume}/chaos-filler /vol/{volume}/chaos-filler-tail"
        ));
    }

    /// Removes every finished backup from the capped backups' volume, so
    /// the server's schedule finds one due.
    pub(crate) fn drop_backups(&mut self) {
        self.in_holder("rm -f /vol/backups/afkfleet-backup-*.db");
    }

    /// The server's metrics, as its port renders them now.
    pub(crate) fn metrics(&mut self) -> String {
        self.stack.ok(&["run", "--rm", "probe"], SLOW)
    }

    /// What every chaos test ends with: the newest backup, taken after the
    /// last fault, restored with `restore`'s own steps (the old database
    /// aside, the migration, every secret's key, the agents), afkfleet
    /// started on it, the Owner logged in anew, every acknowledged write
    /// there and every bot online again; then nothing forbidden in any
    /// output.
    pub(crate) async fn end(mut self) {
        let backup = self.backup.clone().expect("a backup after the last fault");
        self.stack.restore(&backup);
        self.stack.start();
        self.stack.ok(
            &Self::second(&[
                "up",
                "--detach",
                "--wait",
                "--wait-timeout",
                "180",
                SECOND_SERVICE,
            ]),
            SLOW,
        );
        self.log_in_again().await;
        self.assert_kept().await;
        self.settled().await;
        assert_eq!(self.second_logins(), 0, "a bot ran on two agents");
        self.assert_sound(true);
        self.stack.collect_logs();
        self.stack.transcript.assert_clean();
    }
}
