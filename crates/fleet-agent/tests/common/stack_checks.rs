//! `deploy/dev/stack-checks.json`: what the compose e2e test
//! (`tests/slow_compose`) and the demo (`scripts/demo-agent.mjs`) both judge
//! the compose agent by. The test reads it with `include_str!`, the script with
//! `readFileSync`, so the two can't drift apart; `tests/deploy.rs` checks its
//! numbers against `agent.compose.toml` and `compose.dev.yaml` (ADR-0014).

use serde::Deserialize;
use serde_json::Value;

/// The file's text.
pub(crate) const STACK_CHECKS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../deploy/dev/stack-checks.json"
));

/// A line the agent may log while its server restarts: one whose target is
/// `target` and whose message starts with `message_prefix`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExpectedLine {
    pub(crate) target: String,
    pub(crate) message_prefix: String,
    /// Why it's expected, for the reader of the file and of a failure.
    pub(crate) why: String,
}

/// The whole file.
pub(crate) fn stack_checks() -> Value {
    serde_json::from_str(STACK_CHECKS).expect("deploy/dev/stack-checks.json should be JSON")
}

/// The file's `expected_warnings`.
pub(crate) fn expected_warnings() -> Vec<ExpectedLine> {
    serde_json::from_value(stack_checks()["expected_warnings"].clone())
        .expect("stack-checks.json's expected_warnings should be a list of warnings")
}

/// The file's `expected_restart_errors`: errors allowed only while the
/// server restarts, and no more of them than sessions closed there.
pub(crate) fn expected_restart_errors() -> Vec<ExpectedLine> {
    serde_json::from_value(stack_checks()["expected_restart_errors"].clone())
        .expect("stack-checks.json's expected_restart_errors should be a list of errors")
}

/// The file's number `key`, in milliseconds.
pub(crate) fn millis(key: &str) -> core::time::Duration {
    let value = stack_checks()[key]
        .as_u64()
        .unwrap_or_else(|| panic!("stack-checks.json should have the number {key}"));
    core::time::Duration::from_millis(value)
}
