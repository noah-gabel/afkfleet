//! The signals that stop the agent (Plan.md P5.4, ADR-0014): SIGTERM and
//! SIGINT on Unix, Ctrl+C and Ctrl+Break on Windows.
//!
//! The run waits on a [`ShutdownSignals`] source, so tests send it signals
//! through a channel; the binary passes [`OsSignals`]. The first signal shuts
//! the fleet down; a later one is only logged, because the shutdown is
//! already running and has its own deadline.
//!
//! A source whose streams end never makes a signal up: it waits forever.

use core::fmt;
use std::io;

#[cfg(not(any(unix, windows)))]
compile_error!("afkfleet-agent supports only Unix and Windows signals");

/// A signal that asks the agent to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal {
    /// SIGTERM, which `docker stop` sends (Unix).
    Terminate,
    /// SIGINT, from Ctrl+C in a terminal (Unix).
    Interrupt,
    /// Ctrl+C (Windows).
    CtrlC,
    /// Ctrl+Break (Windows).
    CtrlBreak,
}

impl Signal {
    /// The signal's usual name, as the log shows it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Terminate => "SIGTERM",
            Self::Interrupt => "SIGINT",
            Self::CtrlC => "Ctrl+C",
            Self::CtrlBreak => "Ctrl+Break",
        }
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Where the agent's stop signals come from.
pub trait ShutdownSignals: Send {
    /// Waits for the next signal.
    ///
    /// Cancel-safe: dropping the wait loses no signal. Once no signal can
    /// come any more, it waits forever.
    fn recv(&mut self) -> impl Future<Output = Signal> + Send;
}

/// The operating system's stop signals: SIGTERM and SIGINT on Unix, Ctrl+C
/// and Ctrl+Break on Windows.
#[derive(Debug)]
pub struct OsSignals {
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(windows)]
    ctrl_c: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    ctrl_break: tokio::signal::windows::CtrlBreak,
}

impl OsSignals {
    /// Installs the handlers. From now on these signals no longer end the
    /// process; they wait for [`recv`](ShutdownSignals::recv).
    ///
    /// It must run inside a tokio runtime with IO enabled.
    ///
    /// # Errors
    /// The OS's error if a handler can't be installed.
    #[cfg(unix)]
    pub fn install() -> io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            terminate: signal(SignalKind::terminate())?,
            interrupt: signal(SignalKind::interrupt())?,
        })
    }

    /// Installs the handlers. From now on these signals no longer end the
    /// process; they wait for [`recv`](ShutdownSignals::recv).
    ///
    /// It must run inside a tokio runtime with IO enabled.
    ///
    /// # Errors
    /// The OS's error if a handler can't be installed.
    #[cfg(windows)]
    pub fn install() -> io::Result<Self> {
        use tokio::signal::windows::{ctrl_break, ctrl_c};
        Ok(Self {
            ctrl_c: ctrl_c()?,
            ctrl_break: ctrl_break()?,
        })
    }
}

impl ShutdownSignals for OsSignals {
    fn recv(&mut self) -> impl Future<Output = Signal> + Send {
        #[cfg(unix)]
        let next = first_of(
            &mut self.terminate,
            Signal::Terminate,
            &mut self.interrupt,
            Signal::Interrupt,
        );
        #[cfg(windows)]
        let next = first_of(
            &mut self.ctrl_c,
            Signal::CtrlC,
            &mut self.ctrl_break,
            Signal::CtrlBreak,
        );
        next
    }
}

/// One of tokio's signal streams.
trait SignalStream: Send {
    /// The next delivery, or `None` once the stream has ended. Cancel-safe.
    fn next(&mut self) -> impl Future<Output = Option<()>> + Send;
}

#[cfg(unix)]
impl SignalStream for tokio::signal::unix::Signal {
    fn next(&mut self) -> impl Future<Output = Option<()>> + Send {
        self.recv()
    }
}

#[cfg(windows)]
impl SignalStream for tokio::signal::windows::CtrlC {
    fn next(&mut self) -> impl Future<Output = Option<()>> + Send {
        self.recv()
    }
}

#[cfg(windows)]
impl SignalStream for tokio::signal::windows::CtrlBreak {
    fn next(&mut self) -> impl Future<Output = Option<()>> + Send {
        self.recv()
    }
}

/// The next signal from either stream, named as `first_name` or
/// `second_name`. Once both streams have ended, it waits forever.
async fn first_of(
    first: &mut impl SignalStream,
    first_name: Signal,
    second: &mut impl SignalStream,
    second_name: Signal,
) -> Signal {
    tokio::select! {
        // Both branches are cancel-safe, as tokio's signal streams are. A
        // stream that has ended disables its branch.
        Some(()) = first.next() => first_name,
        Some(()) = second.next() => second_name,
        // Both ended: no signal can come any more.
        else => core::future::pending().await,
    }
}

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

    use rstest::rstest;
    use tokio::sync::mpsc;

    use super::*;

    /// A stream the test feeds; it ends when the sender is dropped.
    struct ChannelStream(mpsc::Receiver<()>);

    impl SignalStream for ChannelStream {
        fn next(&mut self) -> impl Future<Output = Option<()>> + Send {
            self.0.recv()
        }
    }

    fn stream() -> (mpsc::Sender<()>, ChannelStream) {
        let (sender, receiver) = mpsc::channel(4);
        (sender, ChannelStream(receiver))
    }

    /// The next signal of the pair, or `None` if none comes within a minute.
    async fn next_of(first: &mut ChannelStream, second: &mut ChannelStream) -> Option<Signal> {
        tokio::time::timeout(
            Duration::from_secs(60),
            first_of(first, Signal::Terminate, second, Signal::Interrupt),
        )
        .await
        .ok()
    }

    #[rstest]
    #[case::terminate(Signal::Terminate, "SIGTERM")]
    #[case::interrupt(Signal::Interrupt, "SIGINT")]
    #[case::ctrl_c(Signal::CtrlC, "Ctrl+C")]
    #[case::ctrl_break(Signal::CtrlBreak, "Ctrl+Break")]
    fn signals_name_themselves(#[case] signal: Signal, #[case] name: &str) {
        assert_eq!(signal.name(), name);
        assert_eq!(signal.to_string(), name);
    }

    #[tokio::test(start_paused = true)]
    async fn each_streams_signal_has_its_own_name() {
        let (first_sender, mut first) = stream();
        let (second_sender, mut second) = stream();

        second_sender.send(()).await.unwrap();
        let from_second = next_of(&mut first, &mut second).await;
        first_sender.send(()).await.unwrap();
        let from_first = next_of(&mut first, &mut second).await;

        assert_eq!(from_second, Some(Signal::Interrupt));
        assert_eq!(from_first, Some(Signal::Terminate));
    }

    #[tokio::test(start_paused = true)]
    async fn a_stream_that_ended_leaves_the_other_working() {
        let (first_sender, mut first) = stream();
        let (second_sender, mut second) = stream();
        drop(first_sender);

        second_sender.send(()).await.unwrap();
        let received = next_of(&mut first, &mut second).await;

        assert_eq!(received, Some(Signal::Interrupt));
    }

    #[tokio::test(start_paused = true)]
    async fn once_both_streams_ended_no_signal_is_made_up() {
        let (first_sender, mut first) = stream();
        let (second_sender, mut second) = stream();
        drop((first_sender, second_sender));

        let received = next_of(&mut first, &mut second).await;

        assert_eq!(received, None);
    }

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
