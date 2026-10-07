//! The slow tests' local Minecraft servers, and helpers to drive sessions
//! against them.
//!
//! Every server is an `itzg/minecraft-server` container with the image and
//! `VERSION` pinned in `deploy/compose.dev.yaml` (the `pins` test checks
//! that). Only its game port is published, on `127.0.0.1` and a random port;
//! RCON stays inside the container and is reached with `rcon-cli`.

use core::future::Future;
use core::time::Duration;

use fleet_core::chat::{ChatKind, ChatSender, IncomingChat};
use fleet_core::id::BotId;
use fleet_core::mc::{
    ConnectParams, MinecraftConnector, SessionCredentials, SessionEvent, SessionEvents,
};
use fleet_core::value::ServerAddress;
use fleet_mc::{AzaleaConnector, McEvents, McSession};
use testcontainers::core::{ExecCommand, IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

/// The test server's image, as `deploy/compose.dev.yaml` pins it.
pub(crate) const IMAGE: &str = "itzg/minecraft-server";
/// The image's tag and digest, as `deploy/compose.dev.yaml` pins them.
pub(crate) const TAG: &str =
    "2026.9.2-java25@sha256:de5d1b1a83eba576f6c8a688fac2a3523ce457724cdebc8ea48d7818b74cdf6e";
/// The Minecraft version of the pinned azalea (ADR-0003).
pub(crate) const VERSION: &str = "26.1";

/// The game port inside the container.
const GAME_PORT: u16 = 25_565;
/// The first start downloads the server jar and generates the world.
const STARTUP: Duration = Duration::from_secs(300);
/// The upper bound for waits that only run out when a test fails.
pub(crate) const WITHIN: Duration = Duration::from_secs(60);
/// The connect timeout the sessions get (the agent's default, ADR-0011).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How the server checks the accounts that join.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Any name may join, unverified: what the dev stack runs.
    Offline,
    /// Every join is checked with Mojang's session server, and chat must be
    /// signed. Reaching Mojang needs internet.
    Online,
}

/// A running test server. Dropping it removes the container.
pub(crate) struct Server {
    container: ContainerAsync<GenericImage>,
    port: u16,
}

impl Server {
    /// Starts a server and waits until it's healthy.
    pub(crate) async fn start(mode: Mode) -> Self {
        let online = match mode {
            Mode::Offline => "FALSE",
            Mode::Online => "TRUE",
        };
        let container = GenericImage::new(IMAGE, TAG)
            .with_wait_for(WaitFor::healthcheck())
            .with_env_var("EULA", "TRUE")
            .with_env_var("TYPE", "VANILLA")
            .with_env_var("VERSION", VERSION)
            .with_env_var("ONLINE_MODE", online)
            .with_env_var("ENFORCE_SECURE_PROFILE", online)
            .with_env_var("LEVEL_TYPE", "minecraft:flat")
            .with_env_var("VIEW_DISTANCE", "4")
            .with_env_var("SIMULATION_DISTANCE", "4")
            .with_env_var("SPAWN_PROTECTION", "0")
            .with_env_var("MODE", "survival")
            .with_env_var("DIFFICULTY", "peaceful")
            .with_env_var("ENABLE_AUTOPAUSE", "FALSE")
            .with_env_var("ENABLE_RCON", "TRUE")
            .with_env_var("MEMORY", "1G")
            // Port 0: Docker picks a free host port.
            .with_mapped_port(0, GAME_PORT.tcp())
            .with_host_config_modifier(|config| {
                // Only the game port, and only on loopback: an offline-mode
                // server lets anyone join under any name.
                config.publish_all_ports = Some(false);
                for binding in config
                    .port_bindings
                    .iter_mut()
                    .flat_map(|bindings| bindings.values_mut())
                    .flatten()
                    .flatten()
                {
                    binding.host_ip = Some("127.0.0.1".to_owned());
                }
            })
            .with_startup_timeout(STARTUP)
            .start()
            .await
            .expect("the Minecraft container didn't start (is Docker running?)");
        let port = container
            .get_host_port_ipv4(GAME_PORT.tcp())
            .await
            .expect("the container's game port isn't published");
        Self { container, port }
    }

    /// The server's address, as a session connects to it.
    pub(crate) fn address(&self) -> ServerAddress {
        format!("127.0.0.1:{}", self.port).parse().unwrap()
    }

    /// Runs `command` through RCON and returns its output.
    pub(crate) async fn rcon(&self, command: &str) -> String {
        let mut result = within(
            "an RCON command",
            self.container.exec(ExecCommand::new(["rcon-cli", command])),
        )
        .await
        .expect("rcon-cli couldn't run");
        let stdout = result.stdout_to_vec().await.expect("no RCON output");
        String::from_utf8_lossy(&stdout).trim().to_owned()
    }
}

/// Awaits `future`, failing the test with `what` if it takes longer than
/// [`WITHIN`].
pub(crate) async fn within<F: Future>(what: &str, future: F) -> F::Output {
    tokio::time::timeout(WITHIN, future)
        .await
        .unwrap_or_else(|_| panic!("{what} didn't happen within {WITHIN:?}"))
}

/// A bot's ID; `n` tells bots apart.
pub(crate) fn bot(n: u8) -> BotId {
    format!("018bcfe5-6800-7bab-abab-abababababa{n:x}")
        .parse()
        .unwrap()
}

/// An offline account named `name`.
pub(crate) fn offline(name: &str) -> SessionCredentials {
    SessionCredentials::Offline {
        username: name.parse().unwrap(),
    }
}

/// Connects `bot_id` with `credentials` to `server`.
pub(crate) async fn connect(
    connector: &AzaleaConnector,
    server: &Server,
    bot_id: BotId,
    credentials: SessionCredentials,
) -> (McSession, McEvents) {
    let params = ConnectParams {
        bot_id,
        server: server.address(),
        credentials,
        connect_timeout: CONNECT_TIMEOUT,
    };
    within("connect", connector.connect(params))
        .await
        .expect("no host thread for the session")
}

/// Waits for an event that satisfies `wanted`, skipping others, and returns
/// it. Fails the test if the session ends first or [`WITHIN`] runs out, and
/// shows the events it skipped.
pub(crate) async fn wait_for(
    events: &mut McEvents,
    what: &str,
    wanted: impl Fn(&SessionEvent) -> bool,
) -> SessionEvent {
    let mut skipped = Vec::new();
    let found = tokio::time::timeout(WITHIN, async {
        loop {
            match events.next().await {
                Some(event) if wanted(&event) => return Some(event),
                Some(event) => skipped.push(event),
                None => return None,
            }
        }
    })
    .await;
    let Ok(found) = found else {
        panic!("{what} didn't happen within {WITHIN:?}; the session sent {skipped:#?}");
    };
    found.unwrap_or_else(|| panic!("the session ended before {what}; it sent {skipped:#?}"))
}

/// Whether `event` is chat of `kind` with `text`, sent by `sender` (`None`
/// for a system message).
pub(crate) fn is_chat(
    event: &SessionEvent,
    kind: ChatKind,
    sender: Option<&str>,
    text: &str,
) -> bool {
    let SessionEvent::Chat(chat) = event else {
        return false;
    };
    chat_matches(chat, kind, sender, text)
}

fn chat_matches(chat: &IncomingChat, kind: ChatKind, sender: Option<&str>, text: &str) -> bool {
    chat.kind() == kind && chat.sender().map(ChatSender::name) == sender && chat.text() == text
}
