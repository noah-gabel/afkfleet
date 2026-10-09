//! `fleet_agent::telemetry::init`, which sets process-wide state: the global
//! subscriber and the `log` bridge. One test, in its own binary.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

mod common;

use common::Capture;
use fleet_agent::config::{LogConfig, LogFilter, LogFormat};
use fleet_agent::telemetry::{TelemetryError, init, layer};
use tracing_subscriber::layer::SubscriberExt;

fn config(filter: &str) -> LogConfig {
    LogConfig {
        format: LogFormat::Json,
        filter: LogFilter::try_from(filter).unwrap(),
    }
}

#[test]
fn init_installs_once_and_log_records_are_filtered_by_their_targets() {
    // `trace`, so the `log` crate's own maximum level lets every record reach
    // the bridge, and only the agent's filter decides.
    assert_eq!(init(&config("trace")), Ok(()));
    assert_eq!(
        init(&config("trace")),
        Err(TelemetryError::AlreadyInstalled)
    );

    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(layer(
        &config("trace,azalea_auth::certs=trace"),
        false,
        capture.clone(),
    ));
    tracing::subscriber::with_default(subscriber, || {
        log::debug!(target: "azalea_auth::certs", "dropped by the azalea_auth cap");
        log::info!(target: "azalea_auth::certs", "kept at info");
        log::debug!(target: "reqwest::connect", "kept at debug");
    });

    let messages: Vec<(String, String)> = capture
        .json_lines()
        .into_iter()
        .map(|line| {
            (
                line["target"].as_str().unwrap().to_owned(),
                line["message"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        messages,
        [
            ("azalea_auth::certs".to_owned(), "kept at info".to_owned()),
            ("reqwest::connect".to_owned(), "kept at debug".to_owned()),
        ]
    );
}
