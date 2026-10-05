//! Thin helpers around `docker compose` for driving the test servers: RCON
//! commands, restarts with different settings. Blocking (std::process); fine
//! for a spike.

use std::process::Command;

use crate::Res;

/// The dev server's compose file, independent of the current directory.
pub const DEV_COMPOSE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../deploy/compose.dev.yaml");
/// The version-mismatch server (P1.3).
pub const MISMATCH_COMPOSE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/compose.mismatch.yaml");

/// Runs one RCON command on the dev server and returns its output.
pub fn rcon(command: &str) -> Res<String> {
    compose(
        DEV_COMPOSE,
        &["exec", "-T", "minecraft", "rcon-cli", command],
        &[],
    )
}

/// Runs `docker compose --file <file> <args…>` with extra environment
/// variables and returns stdout.
pub fn compose(file: &str, args: &[&str], env: &[(&str, &str)]) -> Res<String> {
    let mut cmd = Command::new("docker");
    cmd.args(["compose", "--file", file]).args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output()?;
    if !out.status.success() {
        return Err(format!(
            "docker compose {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// `docker <args…>` (e.g. `kill`, `pause`), returning stdout.
pub fn docker(args: &[&str]) -> Res<String> {
    let out = Command::new("docker").args(args).output()?;
    if !out.status.success() {
        return Err(format!(
            "docker {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// The dev server's container name (compose project `afkfleet-dev`).
pub const DEV_CONTAINER: &str = "afkfleet-dev-minecraft-1";

/// (Re)starts the dev server and waits until it's healthy.
pub fn dev_up(env: &[(&str, &str)]) -> Res<String> {
    compose(
        DEV_COMPOSE,
        &[
            "up",
            "--detach",
            "--wait",
            "--wait-timeout",
            "300",
            "minecraft",
        ],
        env,
    )
}
