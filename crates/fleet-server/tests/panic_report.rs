//! A panic report carries the request's span (Plan.md P6.6, ADR-0015).
//!
//! The panic handler logs only a fixed text; the payload reaches the log
//! through fleet-startup's panic hook, which logs through `tracing` at the
//! moment of the panic. This test installs a hook that does the same and
//! checks that its line carries the request ID of the trace layer's span. It
//! has its own test binary because the panic hook is process-wide.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::routing::get;
use chrono::DateTime;
use fleet_server::config::{HttpConfig, LogConfig};
use fleet_server::http::error::REQUEST_ID_HEADER;
use fleet_server::http::middleware::{self, RequestIds};
use fleet_startup::telemetry::{NoRules, PANIC_TARGET, layer_with};
use fleet_testkit::log_buffer::LogBuffer;
use fleet_testkit::system::{ManualClock, SeededRandom};
use tower::ServiceExt as _;
use tracing_subscriber::layer::SubscriberExt as _;

async fn panics() -> &'static str {
    panic!("the handler's payload")
}

#[tokio::test]
async fn the_panic_hooks_line_carries_the_requests_id() {
    let app = middleware::apply(
        Router::new().route("/panic", get(panics)),
        &HttpConfig {
            bind: HttpConfig::DEFAULT_BIND,
            request_timeout: core::time::Duration::from_secs(15),
            max_body_bytes: 65_536,
        },
        RequestIds::new(
            Arc::new(ManualClock::new(DateTime::UNIX_EPOCH)),
            Arc::new(SeededRandom::new(3)),
        ),
    );
    let buffer = LogBuffer::default();
    let subscriber = tracing_subscriber::registry().with(layer_with(
        &LogConfig::default(),
        &NoRules,
        false,
        buffer.clone(),
    ));
    let _guard = tracing::subscriber::set_default(subscriber);
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|info| {
        tracing::error!(
            target: PANIC_TARGET,
            payload = info.payload_as_str(),
            "a thread panicked"
        );
    }));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/panic")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    std::panic::set_hook(default_hook);

    let id = response.headers()[REQUEST_ID_HEADER].to_str().unwrap();
    let lines = buffer.json_lines().unwrap();
    let report = lines
        .iter()
        .find(|line| line["message"] == "a thread panicked")
        .unwrap_or_else(|| panic!("no panic report in {}", buffer.text()));
    assert_eq!(report["payload"], "the handler's payload");
    assert_eq!(report["span"]["request_id"], id);
    assert_eq!(report["span"]["route"], "/panic");
}
