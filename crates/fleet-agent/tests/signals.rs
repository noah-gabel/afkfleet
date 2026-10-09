//! `OsSignals` receiving real signals (Plan.md P5.4). The test signals its own
//! process, so it's one test in its own binary: under plain `cargo test`, a
//! second test in the same process would receive this one's signals too.
//! Unix only; Windows' Ctrl+C and Ctrl+Break can't be sent without unsafe
//! code, so `just dev-agent` exercises them.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`.
#![cfg(test)]
#![cfg(unix)]

use std::process::Command;
use std::time::Duration;

use fleet_agent::signals::{OsSignals, ShutdownSignals, Signal};

#[tokio::test]
async fn os_signals_receive_sigterm_then_sigint_sent_to_this_process() {
    let mut signals = OsSignals::install().unwrap();
    let pid = std::process::id().to_string();

    for (flag, expected) in [("-TERM", Signal::Terminate), ("-INT", Signal::Interrupt)] {
        let status = Command::new("kill").args([flag, &pid]).status().unwrap();
        assert!(status.success());

        let received = tokio::time::timeout(Duration::from_secs(10), signals.recv()).await;

        assert_eq!(received.ok(), Some(expected));
    }
}
