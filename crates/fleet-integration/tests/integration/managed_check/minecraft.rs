//! The real Minecraft server of the managed check (ADR-0019, S73) and of the
//! slow on-respawn case (ADR-0020, S26): an `itzg/minecraft-server`
//! container, online-mode as P1.8's check has it, or offline-mode with RCON
//! for the slow case, with the image and `VERSION` pinned in
//! `deploy/compose.dev.yaml` (the `pins` test checks that). Only its game
//! port is published, on `127.0.0.1` and a port Docker picks; the bot dials
//! `127.0.0.1:<port>`, never `localhost`, which may resolve to `::1` (R58).
//! RCON is the image's default (on, with a password itzg draws at each
//! start), set by name only for the slow case, the one that uses it. Its
//! port isn't published: [`Minecraft::rcon`] runs `rcon-cli` inside the
//! container, which reads that password there, so none is ever on a command
//! line.
//!
//! It's removed by [`Minecraft::remove`] at the end, its volumes with it:
//! they hold the account's name in the server's usercache and logs. Past
//! the removal's bound, its handle is released undropped and the user
//! removes it by hand, since testcontainers' `Drop` would wait for Docker
//! with no bound.

use core::mem::ManuallyDrop;
use core::time::Duration;

use testcontainers::core::{ExecCommand, IntoContainerPort as _, WaitFor};
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
    /// Starts the online-mode server and waits until it's healthy, or says
    /// why not. Never with a value from the account: it has none.
    pub(crate) async fn start() -> Result<Self, String> {
        Self::launch(true).await
    }

    /// Starts an offline-mode server with RCON, for a case with an offline
    /// account, and waits until it's healthy, or says why not.
    pub(crate) async fn start_offline() -> Result<Self, String> {
        Self::launch(false).await
    }

    async fn launch(online: bool) -> Result<Self, String> {
        // Online, every join is checked with Mojang's session server and
        // chat must be signed, as on a public server.
        let checked = if online { "TRUE" } else { "FALSE" };
        let request = GenericImage::new(IMAGE, TAG)
            .with_wait_for(WaitFor::healthcheck())
            .with_env_var("EULA", "TRUE")
            .with_env_var("TYPE", "VANILLA")
            .with_env_var("VERSION", VERSION)
            .with_env_var("ONLINE_MODE", checked)
            .with_env_var("ENFORCE_SECURE_PROFILE", checked)
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
            .with_startup_timeout(STARTUP);
        // RCON is the image's default either way; the slow case, which
        // uses it, asks for it by name.
        let request = if online {
            request
        } else {
            request.with_env_var("ENABLE_RCON", "TRUE")
        };
        let container = request
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

    /// Runs `command` through `rcon-cli` inside the container, within
    /// `within`, and returns its output.
    ///
    /// # Errors
    /// A fixed text when it didn't run or end in time.
    pub(crate) async fn rcon(
        &self,
        command: &str,
        within: Duration,
    ) -> Result<String, &'static str> {
        let run = async {
            let mut result = self
                .container
                .exec(ExecCommand::new(["rcon-cli", command]))
                .await
                .map_err(|_| "rcon-cli couldn't run")?;
            let stdout = result
                .stdout_to_vec()
                .await
                .map_err(|_| "rcon-cli's output couldn't be read")?;
            Ok(String::from_utf8_lossy(&stdout).trim().to_owned())
        };
        tokio::time::timeout(within, run)
            .await
            .map_err(|_| "rcon-cli didn't end in time")?
    }

    /// Removes the container and its volumes within `within`.
    ///
    /// # Errors
    /// A fixed text when Docker refused, or didn't answer within `within`.
    /// Then the handle is released, never dropped: testcontainers 0.28's
    /// `Drop` removes the container again and waits for Docker with no
    /// bound (`block_in_place` on this runtime), which would break the
    /// check's budget. The container is left for the user to remove.
    pub(crate) async fn remove(self, within: Duration) -> Result<(), &'static str> {
        match ended_or_released(tokio::time::sleep(within), self.container.rm()).await {
            Some(Ok(())) => Ok(()),
            // Its handle went inside `rm`, whose `Drop` asked Docker once
            // more: bounded only by Docker's answer, which just came.
            Some(Err(_)) => Err("Docker refused to remove it"),
            None => Err("Docker didn't remove it in time"),
        }
    }
}

/// `future`'s output if it ends before `bound`. Past the bound, the future
/// is released without being dropped, so a `Drop` that would wait without a
/// bound never runs.
async fn ended_or_released<F: Future>(
    bound: impl Future<Output = ()>,
    future: F,
) -> Option<F::Output> {
    // Boxed, so it stays where it was polled when it's released.
    let mut future = ManuallyDrop::new(Box::pin(future));
    let ended = tokio::select! {
        biased;
        // Cancel-safe: polled through a reference, so losing the race drops
        // nothing.
        output = &mut *future => Some(output),
        () = bound => None,
    };
    if ended.is_some() {
        // It ended, so dropping it now runs only what it still holds.
        drop(ManuallyDrop::into_inner(future));
    }
    ended
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// Says when it's dropped.
    struct Flag(Arc<AtomicBool>);

    impl Drop for Flag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn a_removal_past_its_bound_is_released_and_never_dropped() {
        let dropped = Arc::new(AtomicBool::new(false));
        let held = Flag(Arc::clone(&dropped));
        let hangs = async move {
            let _held = held;
            core::future::pending::<()>().await;
        };

        let ended = ended_or_released(core::future::ready(()), hangs).await;

        assert_eq!(ended, None);
        assert!(!dropped.load(Ordering::SeqCst), "its Drop never ran");
    }

    #[tokio::test]
    async fn a_removal_before_its_bound_ends_and_is_dropped() {
        let dropped = Arc::new(AtomicBool::new(false));
        let held = Flag(Arc::clone(&dropped));
        let ends = async move {
            let _held = held;
            tokio::task::yield_now().await;
        };

        let ended = ended_or_released(core::future::pending(), ends).await;

        assert_eq!(ended, Some(()));
        assert!(dropped.load(Ordering::SeqCst));
    }
}
