//! Throwaway (Phase 1): experiments that de-risk azalea 0.16.0 before
//! `fleet-mc` is built. Every experiment is a subcommand; README.md says how to
//! run each one, FINDINGS.md records what it showed, and ADR-0008 holds the
//! decisions. This crate is not a workspace member and is allowed to be messy.

mod exp;
mod host;
mod rcon;
mod session;

use std::error::Error;

use tracing_subscriber::EnvFilter;

/// The spike's error type: anything, as long as it can cross threads.
pub type Res<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

/// The local dev server from `deploy/compose.dev.yaml` (`just mc-up`).
/// `127.0.0.1`, not `localhost`: on Windows `localhost` may resolve to `::1`.
pub const DEV_SERVER: &str = "127.0.0.1:25565";

fn main() -> Res {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((command, rest)) = args.split_first() else {
        return Err("usage: azalea-spike <command> [args…] (see README.md)".into());
    };
    match command.as_str() {
        "join" => exp::join::run(rest),
        "host" => exp::host::run(rest),
        "host-builder-exit" => exp::host::builder_exit(rest),
        "exit-race" => exp::host::exit_race(rest),
        "fail" => exp::fail::run(rest),
        other => Err(format!("unknown command `{other}` (see README.md)").into()),
    }
}
