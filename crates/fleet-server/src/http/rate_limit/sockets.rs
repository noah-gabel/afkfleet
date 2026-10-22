//! How many live events sockets each user holds open (Plan.md P11.5, §7.4;
//! ADR-0020, S35, U12): `[rate_limits] ws_connections_per_user`, 3 by
//! default.
//!
//! A connect takes its place before the upgrade, counted under one lock, so
//! two connects can't both take the last one. The place is a
//! [`SocketSlot`], which the socket's task holds for its whole life and
//! gives back when it's dropped: however the socket ends, a panic in its
//! task included, and when the upgrade itself fails. The lock is a std
//! `Mutex`, never held across an `.await`.

use core::fmt;
use core::num::NonZeroU32;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use fleet_core::id::UserId;

/// Every user's open sockets, by user.
type Open = Arc<Mutex<HashMap<UserId, u32>>>;

/// Locks `open`. A panic while holding it leaves it consistent, so a
/// poisoned lock is used as it is.
fn lock(open: &Open) -> MutexGuard<'_, HashMap<UserId, u32>> {
    open.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The places: at most `cap` a user.
#[derive(Debug)]
pub(super) struct SocketSlots {
    cap: NonZeroU32,
    open: Open,
}

impl SocketSlots {
    /// No socket open yet; at most `cap` a user.
    pub(super) fn new(cap: NonZeroU32) -> Self {
        Self {
            cap,
            open: Arc::default(),
        }
    }

    /// One of `user`'s places, if they hold fewer than the cap.
    pub(super) fn take(&self, user: UserId) -> Option<SocketSlot> {
        let mut open = lock(&self.open);
        let held = open.entry(user).or_insert(0);
        if *held >= self.cap.get() {
            return None;
        }
        *held = held.saturating_add(1);
        drop(open);
        Some(SocketSlot {
            user,
            open: Arc::clone(&self.open),
        })
    }

    /// How many sockets are open, everyone's together.
    pub(super) fn open(&self) -> u64 {
        lock(&self.open)
            .values()
            .map(|count| u64::from(*count))
            .sum()
    }
}

/// One user's place among their open sockets: given back when it's dropped.
#[must_use = "the place is given back as soon as it's dropped"]
pub struct SocketSlot {
    user: UserId,
    open: Open,
}

/// Its user only.
impl fmt::Debug for SocketSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SocketSlot")
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

impl Drop for SocketSlot {
    fn drop(&mut self) {
        let mut open = lock(&self.open);
        if let Some(held) = open.get_mut(&self.user) {
            *held = held.saturating_sub(1);
            if *held == 0 {
                open.remove(&self.user);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;

    use chrono::{DateTime, Utc};

    use super::*;

    fn user(n: u8) -> UserId {
        UserId::new_v7(
            DateTime::<Utc>::from_timestamp_millis(1_800_000_000_000).unwrap(),
            [n; 10],
        )
        .unwrap()
    }

    fn slots(cap: u32) -> SocketSlots {
        SocketSlots::new(NonZeroU32::new(cap).unwrap())
    }

    #[test]
    fn a_user_takes_up_to_the_cap_and_no_more() {
        let slots = slots(3);

        let held: Vec<_> = (0..3).map(|_| slots.take(user(1)).unwrap()).collect();
        let fourth = slots.take(user(1));

        assert!(fourth.is_none());
        assert_eq!(slots.open(), 3);
        drop(held);
    }

    #[test]
    fn the_cap_is_each_users_own() {
        let slots = slots(1);

        let first = slots.take(user(1));
        let second = slots.take(user(2));

        assert!(first.is_some());
        assert!(second.is_some());
        assert_eq!(slots.open(), 2);
    }

    #[test]
    fn a_dropped_slot_gives_its_place_back() {
        let slots = slots(1);
        let held = slots.take(user(1)).unwrap();

        drop(held);
        let again = slots.take(user(1));

        assert!(again.is_some());
        assert_eq!(slots.open(), 1);
    }

    #[test]
    fn a_slot_held_by_a_task_that_panics_gives_its_place_back() {
        let slots = slots(1);
        let held = slots.take(user(1)).unwrap();

        let ended = std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let _held = held;
                    panic!("the socket's task failed");
                })
                .join()
        });

        assert!(ended.is_err());
        assert_eq!(slots.open(), 0);
        assert!(slots.take(user(1)).is_some());
    }

    #[test]
    fn many_connects_at_once_take_exactly_the_cap() {
        const CONNECTS: usize = 16;
        for round in 0..50 {
            let slots = slots(3);
            let barrier = Barrier::new(CONNECTS);

            let taken = std::thread::scope(|scope| {
                let connects: Vec<_> = (0..CONNECTS)
                    .map(|_| {
                        scope.spawn(|| {
                            barrier.wait();
                            slots.take(user(1))
                        })
                    })
                    .collect();
                connects
                    .into_iter()
                    .map(|connect| connect.join().unwrap())
                    .collect::<Vec<_>>()
            });

            assert_eq!(taken.iter().flatten().count(), 3, "round {round}");
        }
    }

    #[test]
    fn debug_names_the_user_only() {
        let slots = slots(1);
        let held = slots.take(user(1)).unwrap();

        let shown = format!("{held:?}");

        assert!(shown.contains("user"), "{shown}");
        assert!(!shown.contains("open"), "{shown}");
    }
}
