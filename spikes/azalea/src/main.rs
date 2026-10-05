//! Throwaway (Phase 1): experiments that de-risk azalea 0.16.0 before
//! `fleet-mc` is built. Every experiment is a subcommand; README.md says how to
//! run each one, FINDINGS.md records what it showed, and ADR-0008 holds the
//! decisions. This crate is not a workspace member and is allowed to be messy.

mod exp;
mod host;
mod rcon;
mod sample;
mod session;

use std::error::Error;

use tracing_subscriber::EnvFilter;

/// The spike's error type: anything, as long as it can cross threads.
pub type Res<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

/// The local dev server from `deploy/compose.dev.yaml` (`just mc-up`).
/// `127.0.0.1`, not `localhost`: on Windows `localhost` may resolve to `::1`.
/// `linux.sh` overrides it with `SPIKE_SERVER=minecraft:25565`.
pub static DEV_SERVER: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    std::env::var("SPIKE_SERVER").unwrap_or_else(|_| "127.0.0.1:25565".to_owned())
});

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
        "chat" => exp::chat::run(rest),
        "actions" => exp::actions::run(rest),
        "idle-actions" => exp::actions::idle(rest),
        "fault" => exp::panic::run(rest),
        "scale" => exp::scale::run(rest),
        "leak" => exp::leak::run(rest),
        "fetch-token" => exp::account::fetch_token(rest),
        "account-join" => exp::account::account_join(rest),
        "account-check" => exp::account::account_check(rest),
        other => Err(format!("unknown command `{other}` (see README.md)").into()),
    }
}
