//! The panic hook `fleet_agent::telemetry::init` installs, which is
//! process-wide state. One test, in its own binary.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

mod common;

use common::Capture;
use fleet_agent::config::LogConfig;
use fleet_agent::telemetry::{PANIC_TARGET, init, layer};
use tracing_subscriber::layer::SubscriberExt;

#[test]
fn a_panic_is_logged_once_with_its_thread_location_and_clean_payload() {
    init(&LogConfig::default()).unwrap();
    let capture = Capture::default();
    let subscriber =
        tracing_subscriber::registry().with(layer(&LogConfig::default(), false, capture.clone()));

    let outcome = std::thread::Builder::new()
        .name("worker-7".to_owned())
        .spawn(move || {
            tracing::subscriber::with_default(subscriber, || {
                panic!("first line\nsecond \u{1b}[31mline");
            });
        })
        .unwrap()
        .join();

    assert!(outcome.is_err());
    let lines = capture.json_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert_eq!(line["level"], "ERROR");
    assert_eq!(line["target"], PANIC_TARGET);
    assert_eq!(line["thread"], "worker-7");
    assert!(
        line["location"].as_str().unwrap().contains("panic_hook.rs"),
        "{line}"
    );
    assert_eq!(line["payload"], "first line | second [31mline");
}
