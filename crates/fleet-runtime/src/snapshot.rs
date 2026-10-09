//! A bot's snapshot `watch`, and its share of the `afkfleet_bots{state}`
//! gauge (Plan.md P4.9; ADR-0013).
//!
//! The supervisor owns each bot's [`SnapshotOwner`] for as long as it knows
//! the bot, and every actor of the bot publishes through a
//! [`SnapshotPublisher`] from it. Every write goes through one of the two,
//! and the raw `watch::Sender` stays private, so the gauge always counts
//! each bot exactly once, in the state its snapshot holds:
//! - the owner adds the bot when it's created and takes it away when it's
//!   dropped, which happens when the bot is removed or the supervisor ends
//! - each publish moves the bot from the old state's label to the new one
//! - a publisher that's dropped, as when an actor ends, changes nothing

use tokio::sync::watch;

use fleet_core::bot::BotSnapshot;

use crate::metrics;

/// Owns a bot's snapshot `watch`. It counts the bot in the gauge from its
/// creation until it's dropped. There's one per bot, so it isn't `Clone`;
/// actors get a [`SnapshotPublisher`].
#[derive(Debug)]
pub struct SnapshotOwner {
    sender: watch::Sender<BotSnapshot>,
}

impl SnapshotOwner {
    /// Starts a bot's snapshot at `initial`, and counts the bot in its state.
    #[must_use]
    pub fn new(initial: BotSnapshot) -> Self {
        metrics::bot_added(initial.state);
        Self {
            sender: watch::Sender::new(initial),
        }
    }

    /// Publishes `snapshot`, moving the bot to its state in the gauge.
    pub fn publish(&self, snapshot: BotSnapshot) {
        publish(&self.sender, snapshot);
    }

    /// A publisher for an actor of this bot.
    #[must_use]
    pub fn publisher(&self) -> SnapshotPublisher {
        SnapshotPublisher {
            sender: self.sender.clone(),
        }
    }

    /// A receiver that sees every published snapshot.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<BotSnapshot> {
        self.sender.subscribe()
    }

    /// The current snapshot. Hold it briefly: it locks the `watch`.
    #[must_use]
    pub fn borrow(&self) -> watch::Ref<'_, BotSnapshot> {
        self.sender.borrow()
    }
}

impl Drop for SnapshotOwner {
    fn drop(&mut self) {
        metrics::bot_gone(self.sender.borrow().state);
    }
}

/// Publishes a bot's snapshot for one of its actors. Dropping it doesn't
/// change the gauge: only the [`SnapshotOwner`] counts the bot in and out.
#[derive(Debug, Clone)]
pub struct SnapshotPublisher {
    sender: watch::Sender<BotSnapshot>,
}

impl SnapshotPublisher {
    /// Publishes `snapshot`, moving the bot to its state in the gauge.
    pub fn publish(&self, snapshot: BotSnapshot) {
        publish(&self.sender, snapshot);
    }

    /// The current snapshot. Hold it briefly: it locks the `watch`.
    #[must_use]
    pub fn borrow(&self) -> watch::Ref<'_, BotSnapshot> {
        self.sender.borrow()
    }
}

/// Replaces the snapshot and moves the bot in the gauge, from the state it
/// replaced to the new one.
fn publish(sender: &watch::Sender<BotSnapshot>, snapshot: BotSnapshot) {
    let state = snapshot.state;
    let old = sender.send_replace(snapshot);
    metrics::bot_moved(old.state, state);
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use fleet_core::bot::BotState;
    use fleet_core::id::BotId;

    use super::*;
    use crate::metrics::BOTS;
    use crate::metrics::testing::Recorder;

    fn snapshot(state: BotState) -> BotSnapshot {
        BotSnapshot {
            bot_id: "018bcfe5-6800-7bab-abab-abababababab"
                .parse::<BotId>()
                .unwrap(),
            state,
            since: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            last_disconnect: None,
        }
    }

    fn bots(recorder: &Recorder, state: &str) -> f64 {
        recorder.gauge(BOTS, &[("state", state)]).unwrap_or(0.0)
    }

    const STOPPING: BotState = BotState::Stopping { restart: false };

    #[test]
    fn a_new_owner_counts_the_bot_in_its_state() {
        let recorder = Recorder::default();
        let _guard = recorder.install();

        let _owner = SnapshotOwner::new(snapshot(BotState::Stopped));

        assert_eq!(bots(&recorder, "stopped"), 1.0);
    }

    #[test]
    fn a_publish_moves_the_bot_to_its_new_state() {
        let recorder = Recorder::default();
        let _guard = recorder.install();
        let owner = SnapshotOwner::new(snapshot(BotState::Stopped));
        let publisher = owner.publisher();

        publisher.publish(snapshot(STOPPING));

        assert_eq!(bots(&recorder, "stopped"), 0.0);
        assert_eq!(bots(&recorder, "stopping"), 1.0);
        assert_eq!(owner.borrow().state, STOPPING);
    }

    #[test]
    fn the_owner_publishes_like_a_publisher() {
        let recorder = Recorder::default();
        let _guard = recorder.install();
        let owner = SnapshotOwner::new(snapshot(BotState::Stopped));
        let mut receiver = owner.subscribe();

        owner.publish(snapshot(STOPPING));

        assert_eq!(bots(&recorder, "stopped"), 0.0);
        assert_eq!(bots(&recorder, "stopping"), 1.0);
        assert!(receiver.has_changed().unwrap());
        assert_eq!(receiver.borrow_and_update().state, STOPPING);
    }

    #[test]
    fn a_change_within_one_state_label_leaves_the_gauge_alone() {
        let recorder = Recorder::default();
        let _guard = recorder.install();
        let owner = SnapshotOwner::new(snapshot(STOPPING));

        owner.publish(snapshot(BotState::Stopping { restart: true }));

        assert_eq!(bots(&recorder, "stopping"), 1.0);
    }

    #[test]
    fn dropping_the_owner_takes_the_bot_out_of_its_current_state() {
        let recorder = Recorder::default();
        let _guard = recorder.install();
        let owner = SnapshotOwner::new(snapshot(BotState::Stopped));
        owner.publish(snapshot(STOPPING));

        drop(owner);

        assert_eq!(bots(&recorder, "stopped"), 0.0);
        assert_eq!(bots(&recorder, "stopping"), 0.0);
    }

    #[test]
    fn dropping_a_publisher_changes_nothing() {
        let recorder = Recorder::default();
        let _guard = recorder.install();
        let owner = SnapshotOwner::new(snapshot(BotState::Stopped));
        let publisher = owner.publisher();
        let clone = publisher.clone();

        drop(publisher);
        drop(clone);

        assert_eq!(bots(&recorder, "stopped"), 1.0);
        assert_eq!(owner.borrow().state, BotState::Stopped);
    }
}
