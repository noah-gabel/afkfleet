//! The server's log layer caps `azalea_auth` at `info`, whatever the
//! configured filter names (Plan.md P9.3, ADR-0011, ADR-0015, ADR-0018).
//!
//! fleet-server has no azalea-auth code (ADR-0018), so the cap is a guard,
//! and the test logs under azalea-auth's targets itself. The config is
//! loaded from a file in a figment jail through `fleet_server::config::load`,
//! as `serve` loads it. The layer is
//! `layer_with(&config.log, &NoRules, …)`, the same function and rules
//! `init_with` installs for every command. Only `info`, `warn` and `error`
//! may come through from azalea-auth. Its `info` line must come through
//! every time, and a `fleet_server` trace line wherever the filter enables
//! everything at `trace`, so the test can't pass on an empty buffer.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::path::Path;

use figment::Jail;
use fleet_server::config::{ServerConfig, load};
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::jail::in_jail;
use fleet_testkit::log_buffer::LogBuffer;
use rstest::rstest;
use tracing_subscriber::layer::SubscriberExt as _;

/// The levels below the cap.
const BELOW: [&str; 2] = ["trace", "debug"];
/// The levels the cap lets through.
const THROUGH: [&str; 3] = ["info", "warn", "error"];
/// azalea-auth's crate target and a module of it.
const TARGETS: [&str; 2] = ["azalea_auth", "azalea_auth::auth"];

/// What `RUST_LOG` says in every case. `EnvFilter::builder().parse_lossy`
/// ignores it today; it's set anyway, so a later switch to
/// `from_default_env` can't open a second, uncapped way in unnoticed.
const RUST_LOG: &str = "trace,azalea_auth=trace";

fn toml_path(path: &Path) -> String {
    format!("'{}'", path.display())
}

/// A valid config whose `[log] filter` is `filter`.
fn config_with(jail: &mut Jail, filter: &str) -> ServerConfig {
    let toml = format!(
        "[database]\npath = {}\n[vault]\nmaster_key_files = {{ 1 = {} }}\nactive_key_id = 1\n[log]\nfilter = \"{filter}\"\n",
        toml_path(&jail.directory().join("afkfleet.db")),
        toml_path(&jail.directory().join("vault-key-1")),
    );
    jail.create_file("server.toml", &toml).unwrap();
    load(&jail.directory().join("server.toml")).unwrap()
}

/// The message of a line under `target` at `level`.
fn message(target: &str, level: &str) -> String {
    format!("{target} at {level}")
}

/// Logs one line per azalea-auth target and level, and a `fleet_server`
/// trace line, through the server's layer for `config`.
fn emit(config: &ServerConfig) -> LogBuffer {
    let logs = LogBuffer::default();
    let subscriber =
        tracing_subscriber::registry().with(layer_with(&config.log, &NoRules, false, logs.clone()));
    tracing::subscriber::with_default(subscriber, || {
        tracing::trace!(target: "azalea_auth", "{}", message("azalea_auth", "trace"));
        tracing::debug!(target: "azalea_auth", "{}", message("azalea_auth", "debug"));
        tracing::info!(target: "azalea_auth", "{}", message("azalea_auth", "info"));
        tracing::warn!(target: "azalea_auth", "{}", message("azalea_auth", "warn"));
        tracing::error!(target: "azalea_auth", "{}", message("azalea_auth", "error"));
        tracing::trace!(target: "azalea_auth::auth", "{}", message("azalea_auth::auth", "trace"));
        tracing::debug!(target: "azalea_auth::auth", "{}", message("azalea_auth::auth", "debug"));
        tracing::info!(target: "azalea_auth::auth", "{}", message("azalea_auth::auth", "info"));
        tracing::warn!(target: "azalea_auth::auth", "{}", message("azalea_auth::auth", "warn"));
        tracing::error!(target: "azalea_auth::auth", "{}", message("azalea_auth::auth", "error"));
        tracing::trace!(target: "fleet_server", "{}", message("fleet_server", "trace"));
    });
    logs
}

fn count(logs: &LogBuffer, target: &str, level: &str) -> usize {
    logs.lines_with(&message(target, level)).unwrap().len()
}

/// Nothing below `info` from azalea-auth; its `info`, `warn` and `error`
/// lines, and the `fleet_server` trace line when everything is at `trace`.
fn assert_capped(logs: &LogBuffer, everything_at_trace: bool) {
    for target in TARGETS {
        for level in BELOW {
            assert_eq!(
                count(logs, target, level),
                0,
                "{target} at {level} got through"
            );
        }
    }
    for level in THROUGH {
        assert_eq!(
            count(logs, "azalea_auth", level),
            1,
            "azalea_auth at {level}"
        );
    }
    let fleet_server = usize::from(everything_at_trace);
    assert_eq!(count(logs, "fleet_server", "trace"), fleet_server);
}

#[rstest]
#[case::trace("trace", true)]
#[case::trace_naming_azalea_auth("trace,azalea_auth=trace", true)]
#[case::azalea_auth_alone("azalea_auth=trace", false)]
#[case::trace_naming_a_module("trace,azalea_auth::auth=trace", true)]
fn azalea_auth_stays_at_info_whatever_the_file_names(
    #[case] filter: &str,
    #[case] everything_at_trace: bool,
) {
    in_jail(|jail| {
        jail.set_env("RUST_LOG", RUST_LOG);
        let config = config_with(jail, filter);

        let logs = emit(&config);

        assert_capped(&logs, everything_at_trace);
        if !everything_at_trace {
            // A module of azalea-auth that the filter doesn't name stays
            // where the crate's directive puts it, under the cap.
            for level in THROUGH {
                assert_eq!(count(&logs, "azalea_auth::auth", level), 1);
            }
        }
    });
}

#[test]
fn azalea_auth_stays_at_info_when_the_environment_names_it() {
    in_jail(|jail| {
        jail.set_env("RUST_LOG", RUST_LOG);
        jail.set_env("AFKFLEET_SERVER__LOG__FILTER", "trace,azalea_auth=trace");
        let config = config_with(jail, "info");

        let logs = emit(&config);

        // The environment won: `fleet_server` is at trace.
        assert_capped(&logs, true);
    });
}
