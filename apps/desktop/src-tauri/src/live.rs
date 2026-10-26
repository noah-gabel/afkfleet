//! The live events watch (Plan.md P11.8; ADR-0020, U29, S59, S61, S64,
//! R82): the `watch_events` command forwards fleet-client's live events of
//! the current session to the UI over a Tauri `Channel`.
//!
//! - **It holds the connection's read guard for its whole life,** taken once,
//!   as every command does (ADR-0017), and keeps `recv` polled, sending each
//!   update at once: no queue (R74).
//! - **One watch at a time.** [`LiveSlot`] knows the current one: a newer
//!   watch stops the older, since a reloaded page's old watch would still
//!   send into the dead page.
//! - **A switch stops it.** `set_server_profile` stops the watch just before
//!   its exclusive guard, after the new server answered, and a watch that
//!   registers while the switch runs, its read taken before the writer asked
//!   for the guard, is stopped at birth (R82): no watch holds its read guard
//!   across a switch, and an identical profile's save restarts the socket on
//!   the same session. A watch whose read was queued behind the writer isn't
//!   one of them: [`Exclusive`] ends the switch before the guard is
//!   released, so it registers after the switch and runs on the new
//!   connection.
//! - **It ends** `Ok` when stopped, or when the channel refuses an update,
//!   which then never delivers again (S64); with `no_session` when the
//!   session ends; with `certificate` when the server's certificate is
//!   refused, which gets no timer (R73).
//! - **What crosses:** the server's frames as they came, why to fetch again,
//!   and why the socket went down with its wait, rounded up to whole
//!   seconds; a failed connect's error as any command's. Never the ticket or
//!   the token: fleet-client spends both inside itself.

use core::ops::{Deref, DerefMut};
use core::time::Duration;
use std::sync::{Mutex, MutexGuard, PoisonError};

use fleet_api_types::app::{
    CommandError, CommandErrorCode, LiveDisconnect, LiveRefetchReason, LiveUpdateMessage,
};
use fleet_client::{Disconnect, LiveEnd, LiveStop, LiveUpdate, RefetchReason};
use tauri::ipc::Channel;

use crate::error;
use crate::keychain::StoreFactory;
use crate::state::AppState;

/// The one watch that may run, and whether a switch runs; see the
/// [module docs](self).
#[derive(Debug, Default)]
pub(crate) struct LiveSlot {
    /// The current watch: its number, and its stop.
    current: Option<(u64, LiveStop)>,
    /// The last number handed out.
    issued: u64,
    /// How many switches run.
    switching: u32,
}

impl LiveSlot {
    /// Makes the watch `stop` belongs to the current one, stopping the
    /// older; while a switch runs it's stopped at once. Answers its number.
    pub(crate) fn register(&mut self, stop: LiveStop) -> u64 {
        self.issued = self.issued.wrapping_add(1);
        if let Some((_, older)) = self.current.take() {
            older.stop();
        }
        if self.switching > 0 {
            stop.stop();
        }
        self.current = Some((self.issued, stop));
        self.issued
    }

    /// Forgets watch `number`, if it's still the current one.
    pub(crate) fn unregister(&mut self, number: u64) {
        if self
            .current
            .as_ref()
            .is_some_and(|(current, _)| *current == number)
        {
            self.current = None;
        }
    }

    /// A switch starts: the current watch stops, and every watch that
    /// starts until [`LiveSlot::end_switch`] starts stopped.
    pub(crate) fn begin_switch(&mut self) {
        self.switching = self.switching.saturating_add(1);
        if let Some((_, current)) = self.current.take() {
            current.stop();
        }
    }

    /// A switch ended.
    pub(crate) fn end_switch(&mut self) {
        self.switching = self.switching.saturating_sub(1);
    }
}

/// A running switch, ended when this is dropped.
#[must_use = "the switch ends when this is dropped"]
pub(crate) struct Switching<'a> {
    slot: &'a Mutex<LiveSlot>,
}

impl Drop for Switching<'_> {
    fn drop(&mut self) {
        lock(self.slot).end_switch();
    }
}

/// A switch's exclusive guard: the switch, and the connection's write guard
/// `G`. The switch ends before the guard is released, since a struct's
/// fields drop in their declared order, so a watch whose read was queued
/// behind the writer registers after the switch and runs on the new
/// connection rather than starting stopped (R82).
#[must_use = "the switch and the guard end when this is dropped"]
pub(crate) struct Exclusive<'a, G> {
    /// Dropped first: the switch ends.
    _switching: Switching<'a>,
    /// Then the guard is released.
    guard: G,
}

impl<'a, G> Exclusive<'a, G> {
    /// The guard `guard`, taken during `switching`.
    pub(crate) const fn new(switching: Switching<'a>, guard: G) -> Self {
        Self {
            _switching: switching,
            guard,
        }
    }
}

impl<G: Deref> Deref for Exclusive<'_, G> {
    type Target = G::Target;

    fn deref(&self) -> &G::Target {
        &self.guard
    }
}

impl<G: DerefMut> DerefMut for Exclusive<'_, G> {
    fn deref_mut(&mut self) -> &mut G::Target {
        &mut self.guard
    }
}

/// `slot`'s guard. A poisoned lock still holds a usable slot: a guard is
/// held only inside [`lock`]'s callers here, whose writes are plain
/// assignments and [`LiveStop::stop`], none of which can panic, so no panic
/// leaves a slot half-changed.
fn lock(slot: &Mutex<LiveSlot>) -> MutexGuard<'_, LiveSlot> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

impl<F: StoreFactory> AppState<F> {
    /// The `watch_events` command: forwards the current session's live
    /// events to `updates`, each at once, holding the connection's shared
    /// guard all along. A newer watch stops it, and so does a server switch
    /// once the new server answered; then, and when `updates` refuses an
    /// update (it never delivers again), it answers `Ok`. No update holds a
    /// ticket or a token.
    ///
    /// # Errors
    /// `no_server` without a server; `no_session` when the session ends, or
    /// there's none; `certificate` when the server's certificate is refused.
    pub async fn watch_events(
        &self,
        updates: Channel<LiveUpdateMessage>,
    ) -> Result<(), CommandError> {
        let conn = self.conn.read().await;
        let mut live = conn.client()?.live_events();
        let number = lock(&self.live).register(live.stopper());
        let ended = loop {
            match live.recv().await {
                Ok(update) => {
                    if updates.send(message(update)).is_err() {
                        // The channel numbers each update before it's sent,
                        // so after a refused one it never delivers again.
                        let _closed = live.close().await;
                        break Ok(());
                    }
                }
                Err(end) => break answer(end),
            }
        };
        lock(&self.live).unregister(number);
        ended
    }

    /// Starts a switch: stops the watch, and every watch that registers
    /// before the answer is dropped starts stopped.
    #[must_use = "the switch ends when the answer is dropped"]
    pub(crate) fn begin_switch(&self) -> Switching<'_> {
        lock(&self.live).begin_switch();
        Switching { slot: &self.live }
    }
}

/// `update` as the UI gets it.
#[must_use]
pub(crate) fn message(update: LiveUpdate) -> LiveUpdateMessage {
    match update {
        LiveUpdate::Refetch(reason) => LiveUpdateMessage::Refetch {
            reason: match reason {
                RefetchReason::Ready => LiveRefetchReason::Ready,
                RefetchReason::Resync => LiveRefetchReason::Resync,
                RefetchReason::Unreadable => LiveRefetchReason::Unreadable,
            },
        },
        LiveUpdate::Event(event) => LiveUpdateMessage::Event { event },
        LiveUpdate::Disconnected { why, retry_in } => LiveUpdateMessage::Disconnected {
            disconnect: match why {
                Disconnect::Closed { code } => LiveDisconnect::Closed { code },
                Disconnect::Lost => LiveDisconnect::Lost,
                Disconnect::Silent => LiveDisconnect::Silent,
                Disconnect::NotOpened(failure) => LiveDisconnect::NotOpened {
                    error: error::client(failure),
                },
            },
            retry_in_secs: whole_secs(retry_in),
        },
    }
}

/// `wait` in whole seconds, rounded up, so only no wait at all is 0.
#[must_use]
fn whole_secs(wait: Duration) -> u32 {
    let secs = wait
        .as_secs()
        .saturating_add(u64::from(wait.subsec_nanos() > 0));
    u32::try_from(secs).unwrap_or(u32::MAX)
}

/// How a watch whose socket ended with `end` answers.
fn answer(end: LiveEnd) -> Result<(), CommandError> {
    match end {
        LiveEnd::Stopped => Ok(()),
        LiveEnd::NoSession => Err(CommandError::new(CommandErrorCode::NoSession)),
        LiveEnd::Certificate => Err(CommandError::new(CommandErrorCode::Certificate)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use fleet_api_types::events::{EventMessage, EventMessageKind};
    use fleet_client::{Client, ClientConfig, ClientError, MemoryStore, ServerUrl, Trust};
    use rstest::rstest;

    use super::*;

    /// A stop of a socket nobody drives.
    fn stop() -> LiveStop {
        let client = Client::new(
            ClientConfig {
                server: ServerUrl::parse("https://example.com").unwrap(),
                trust: Trust::System,
            },
            MemoryStore::new(),
        )
        .unwrap();
        client.live_events().stopper()
    }

    #[test]
    fn a_newer_watch_stops_the_older_one() {
        let mut slot = LiveSlot::default();
        let (older, newer) = (stop(), stop());

        let first = slot.register(older.clone());
        let second = slot.register(newer.clone());

        assert!(older.is_stopped());
        assert!(!newer.is_stopped());
        assert_ne!(first, second);
    }

    #[test]
    fn a_switch_stops_the_watch_and_one_that_starts_during_it_starts_stopped() {
        let mut slot = LiveSlot::default();
        let (running, during, after) = (stop(), stop(), stop());
        slot.register(running.clone());

        slot.begin_switch();
        slot.register(during.clone());
        slot.end_switch();
        slot.register(after.clone());

        assert!(running.is_stopped());
        assert!(during.is_stopped());
        assert!(!after.is_stopped());
    }

    /// A guard that notes, when it's released, whether the switch had
    /// ended.
    struct Probe<'a> {
        slot: &'a Mutex<LiveSlot>,
        ended_first: &'a AtomicBool,
    }

    impl Drop for Probe<'_> {
        fn drop(&mut self) {
            let ended = lock(self.slot).switching == 0;
            self.ended_first.store(ended, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_switchs_exclusive_guard_ends_the_switch_before_it_is_released() {
        let slot = Mutex::new(LiveSlot::default());
        let ended_first = AtomicBool::new(false);
        lock(&slot).begin_switch();
        let exclusive = Exclusive::new(
            Switching { slot: &slot },
            Probe {
                slot: &slot,
                ended_first: &ended_first,
            },
        );

        drop(exclusive);

        assert!(ended_first.load(Ordering::SeqCst));
    }

    #[test]
    fn two_switches_at_once_both_end_before_a_watch_runs() {
        let mut slot = LiveSlot::default();
        let (between, after) = (stop(), stop());

        slot.begin_switch();
        slot.begin_switch();
        slot.end_switch();
        slot.register(between.clone());
        slot.end_switch();
        slot.register(after.clone());

        assert!(between.is_stopped());
        assert!(!after.is_stopped());
    }

    #[test]
    fn a_watch_forgets_only_itself() {
        let mut slot = LiveSlot::default();
        let (older, newer) = (stop(), stop());
        let first = slot.register(older);
        slot.register(newer.clone());

        slot.unregister(first);
        slot.begin_switch();

        assert!(
            newer.is_stopped(),
            "the newer watch was still the current one"
        );
    }

    #[rstest]
    #[case::ready(RefetchReason::Ready, LiveRefetchReason::Ready)]
    #[case::resync(RefetchReason::Resync, LiveRefetchReason::Resync)]
    #[case::unreadable(RefetchReason::Unreadable, LiveRefetchReason::Unreadable)]
    fn a_refetch_keeps_its_reason(#[case] reason: RefetchReason, #[case] sent: LiveRefetchReason) {
        assert_eq!(
            message(LiveUpdate::Refetch(reason)),
            LiveUpdateMessage::Refetch { reason: sent }
        );
    }

    #[test]
    fn an_event_crosses_as_it_came() {
        let event: EventMessage = serde_json::from_value(serde_json::json!({
            "type": "bot_removed",
            "bot_id": "01900000-0000-7000-8000-000000000001"
        }))
        .unwrap();

        let sent = message(LiveUpdate::Event(Box::new(event.clone())));

        assert_eq!(
            sent,
            LiveUpdateMessage::Event {
                event: Box::new(event)
            }
        );
        let LiveUpdateMessage::Event { event } = sent else {
            unreachable!()
        };
        assert_eq!(event.kind.known(), Some(EventMessageKind::BotRemoved));
    }

    #[rstest]
    #[case::closed(Disconnect::Closed { code: Some(1001) }, LiveDisconnect::Closed { code: Some(1001) })]
    #[case::closed_without_a_code(Disconnect::Closed { code: None }, LiveDisconnect::Closed { code: None })]
    #[case::lost(Disconnect::Lost, LiveDisconnect::Lost)]
    #[case::silent(Disconnect::Silent, LiveDisconnect::Silent)]
    #[case::not_opened(
        Disconnect::NotOpened(ClientError::Unreachable(None)),
        LiveDisconnect::NotOpened { error: CommandError::new(CommandErrorCode::Unreachable) }
    )]
    fn a_disconnect_says_why(#[case] why: Disconnect, #[case] sent: LiveDisconnect) {
        let update = LiveUpdate::Disconnected {
            why,
            retry_in: Duration::from_secs(2),
        };

        assert_eq!(
            message(update),
            LiveUpdateMessage::Disconnected {
                disconnect: sent,
                retry_in_secs: 2
            }
        );
    }

    #[rstest]
    #[case::none(Duration::ZERO, 0)]
    #[case::a_moment(Duration::from_millis(1), 1)]
    #[case::whole(Duration::from_secs(45), 45)]
    #[case::rounded_up(Duration::from_millis(1_240), 2)]
    #[case::past_u32(Duration::from_secs(u64::MAX), u32::MAX)]
    fn a_wait_is_whole_seconds_rounded_up(#[case] wait: Duration, #[case] secs: u32) {
        assert_eq!(whole_secs(wait), secs);
    }

    #[rstest]
    #[case::stopped(LiveEnd::Stopped, None)]
    #[case::no_session(LiveEnd::NoSession, Some(CommandErrorCode::NoSession))]
    #[case::certificate(LiveEnd::Certificate, Some(CommandErrorCode::Certificate))]
    fn each_end_answers_as_the_command_says(
        #[case] end: LiveEnd,
        #[case] code: Option<CommandErrorCode>,
    ) {
        assert_eq!(answer(end).err().map(|error| error.code), code);
    }
}
