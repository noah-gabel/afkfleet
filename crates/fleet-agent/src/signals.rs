//! The signals that stop the agent (Plan.md P5.4, ADR-0014): SIGTERM and
//! SIGINT on Unix, Ctrl+C and Ctrl+Break on Windows.
//!
//! They live in fleet-startup, which both binaries share (ADR-0015); this
//! module re-exports them under their old paths. The run waits on a
//! [`ShutdownSignals`] source, so tests send it signals through a channel;
//! the binary passes [`OsSignals`]. The first signal shuts the fleet down; a
//! later one is only logged, because the shutdown is already running and has
//! its own deadline.

pub use fleet_startup::signals::{OsSignals, ShutdownSignals, Signal};

#[cfg(test)]
pub(crate) mod testing {
    //! Signal sources for the crate's unit tests.

    use tokio::sync::mpsc;

    use super::{ShutdownSignals, Signal};

    /// Delivers what the test sends; waits forever once the sender is gone.
    #[derive(Debug)]
    pub(crate) struct ChannelSignals(mpsc::Receiver<Signal>);

    impl ChannelSignals {
        /// A source and the sender that feeds it.
        pub(crate) fn new() -> (mpsc::Sender<Signal>, Self) {
            let (sender, receiver) = mpsc::channel(4);
            (sender, Self(receiver))
        }
    }

    impl ShutdownSignals for ChannelSignals {
        async fn recv(&mut self) -> Signal {
            match self.0.recv().await {
                Some(signal) => signal,
                None => core::future::pending().await,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn the_channel_source_waits_forever_once_its_sender_is_gone() {
        let (sender, mut signals) = testing::ChannelSignals::new();
        sender.send(Signal::CtrlBreak).await.unwrap();
        drop(sender);

        let first = tokio::time::timeout(Duration::from_secs(60), signals.recv()).await;
        let second = tokio::time::timeout(Duration::from_secs(60), signals.recv()).await;

        assert_eq!(first.ok(), Some(Signal::CtrlBreak));
        assert!(second.is_err());
    }
}
