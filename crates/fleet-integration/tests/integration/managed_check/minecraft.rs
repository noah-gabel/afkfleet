//! The managed check's Minecraft server (ADR-0019, S73): an online-mode
//! `itzg/minecraft-server` container, as P1.8's check has it, with the image
//! and `VERSION` pinned in `deploy/compose.dev.yaml` (the `pins` test checks
//! that). Only its game port is published, on `127.0.0.1` and a port
//! Docker picks; the bot dials `127.0.0.1:<port>`, never `localhost`, which
//! may resolve to `::1` (R58).
//!
//! It's removed by [`Minecraft::remove`] at the check's end, its volumes
//! with it: they hold the account's name in the server's usercache and
//! logs. Dropping it is the fallback.

use core::time::Duration;

use testcontainers::core::{IntoContainerPort as _, WaitFor};
use testcontainers::runners::AsyncRunner as _;
use testcontainers::{ContainerAsync, GenericImage, ImageExt as _};

/// The server's image, as `deploy/compose.dev.yaml` pins it.
pub(crate) const IMAGE: &str = "itzg/minecraft-server";
/// The image's tag and digest, as `deploy/compose.dev.yaml` pins them.
pub(crate) const TAG: &str =
    "2026.9.2-java25@sha256:de5d1b1a83eba576f6c8a688fac2a3523ce457724cdebc8ea48d7818b74cdf6e";
/// The Minecraft version of the pinned azalea (ADR-0003).
pub(crate) const VERSION: &str = "26.1";

/// The game port inside the container.
const GAME_PORT: u16 = 25_565;
/// The first start downloads the server jar and generates the world.
pub(crate) const STARTUP: Duration = Duration::from_secs(300);

/// The running server; see the [module docs](self).
pub(crate) struct Minecraft {
    container: ContainerAsync<GenericImage>,
    port: u16,
}

impl Minecraft {
    /// Starts the server and waits until it's healthy, or says why not.
    /// Never with a value from the account: it has none.
    pub(crate) async fn start() -> Result<Self, String> {
        let container = GenericImage::new(IMAGE, TAG)
            .with_wait_for(WaitFor::healthcheck())
            .with_env_var("EULA", "TRUE")
            .with_env_var("TYPE", "VANILLA")
            .with_env_var("VERSION", VERSION)
            // Every join is checked with Mojang's session server, and chat
            // must be signed, as on a public server.
            .with_env_var("ONLINE_MODE", "TRUE")
            .with_env_var("ENFORCE_SECURE_PROFILE", "TRUE")
            .with_env_var("LEVEL_TYPE", "minecraft:flat")
            .with_env_var("VIEW_DISTANCE", "4")
            .with_env_var("SIMULATION_DISTANCE", "4")
            .with_env_var("SPAWN_PROTECTION", "0")
            .with_env_var("MODE", "survival")
            .with_env_var("DIFFICULTY", "peaceful")
            .with_env_var("ENABLE_AUTOPAUSE", "FALSE")
            .with_env_var("MEMORY", "1G")
            // Port 0: Docker picks a free host port.
            .with_mapped_port(0, GAME_PORT.tcp())
            .with_host_config_modifier(|config| {
                // Only the game port, and only on loopback.
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
            .map_err(|_| "the container didn't start: is Docker Desktop running?".to_owned())?;
        let port = container
            .get_host_port_ipv4(GAME_PORT.tcp())
            .await
            .map_err(|_| "the container's game port isn't published".to_owned())?;
        Ok(Self { container, port })
    }

    /// The address the bot is sent to.
    pub(crate) fn address(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// Removes the container and its volumes; whether it went.
    pub(crate) async fn remove(self) -> bool {
        tokio::time::timeout(Duration::from_secs(60), self.container.rm())
            .await
            .is_ok_and(|removed| removed.is_ok())
    }
}
