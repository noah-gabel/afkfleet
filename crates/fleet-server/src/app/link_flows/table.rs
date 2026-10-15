//! [`LinkFlows`]: every user's link flow, held in memory (Plan.md P9.4,
//! §7.4, ADR-0018).
//!
//! Each user has one slot, holding at most:
//! - one flow: waiting for the user at Microsoft's page, finishing (checking
//!   the profile and storing the account), or finished, with its result;
//! - one start in progress, between its [`Reservation`] and its
//!   registration, while it asks Microsoft for a device code.
//!
//! A user is *active* while they have a start in progress or a flow that's
//! waiting or finishing; at most `total` users are active. A start replaces
//! the user's waiting or finished flow: a waiting one is cancelled, so its
//! poller stops and its code is never polled again. A finishing flow isn't
//! replaced, since the user already approved it at Microsoft.
//!
//! A finished flow's result stays readable for [`FINISHED_RETENTION`], then
//! it's hidden and [`LinkFlows::purge_finished`] drops it. A restart drops
//! every flow; the user starts again.
//!
//! The lock is a std `Mutex`, never held across an `.await`.

use core::fmt;
use core::num::NonZeroU32;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use fleet_core::id::{LinkFlowId, UserId};
use fleet_core::system::Clock;
use fleet_core::time::elapsed;
use tokio_util::sync::CancellationToken;

use super::{FINISHED_RETENTION, LinkFlowStatus, LinkFlowView};

/// Every user's link flow; clones share it.
#[derive(Clone)]
pub struct LinkFlows {
    clock: Arc<dyn Clock>,
    table: Arc<Mutex<Table>>,
}

/// The slots, and the last ticket handed to a start.
#[derive(Default)]
struct Table {
    slots: HashMap<UserId, Slot>,
    tickets: u64,
}

/// One user's flow and start in progress.
#[derive(Default)]
struct Slot {
    flow: Option<Flow>,
    /// The ticket of the start in progress.
    starting: Option<u64>,
}

/// A flow.
struct Flow {
    id: LinkFlowId,
    phase: Phase,
}

/// Where a flow stands.
enum Phase {
    /// Waiting for the user at Microsoft's page until `expires_at`; `cancel`
    /// stops its poller.
    Waiting {
        expires_at: DateTime<Utc>,
        cancel: CancellationToken,
    },
    /// Microsoft authorized it: the profile is checked and the account
    /// stored.
    Finishing,
    /// It ended with `status` at `ended_at`.
    Finished {
        status: LinkFlowStatus,
        ended_at: DateTime<Utc>,
    },
}

impl Slot {
    /// Whether this user holds one of the `total` places.
    fn active(&self) -> bool {
        self.starting.is_some()
            || self
                .flow
                .as_ref()
                .is_some_and(|flow| !matches!(flow.phase, Phase::Finished { .. }))
    }

    /// The flow `id`, if it's this slot's.
    fn flow_mut(&mut self, id: LinkFlowId) -> Option<&mut Flow> {
        self.flow.as_mut().filter(|flow| flow.id == id)
    }
}

/// Why a start can't take its place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Busy {
    /// The user's flow is finishing, or another start of theirs is in
    /// progress.
    InProgress,
    /// `total` users are active already.
    AllBusy,
}

impl LinkFlows {
    /// No flows yet; finished results are hidden by `clock`.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            table: Arc::default(),
        }
    }

    /// The table, poison-tolerantly.
    fn table(&self) -> MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes `user`'s place for a start, while at most `total` users are
    /// active. The user's own waiting or finished flow stays until
    /// [`Reservation::replace`].
    pub(super) fn reserve(&self, user: UserId, total: NonZeroU32) -> Result<Reservation, Busy> {
        let mut table = self.table();
        let active = table.slots.values().filter(|slot| slot.active()).count();
        let slot = table.slots.get(&user);
        let finishing = slot
            .and_then(|slot| slot.flow.as_ref())
            .is_some_and(|flow| matches!(flow.phase, Phase::Finishing));
        if finishing || slot.is_some_and(|slot| slot.starting.is_some()) {
            return Err(Busy::InProgress);
        }
        let reuses = slot.is_some_and(Slot::active);
        let cap = usize::try_from(total.get()).unwrap_or(usize::MAX);
        if !reuses && active >= cap {
            return Err(Busy::AllBusy);
        }
        table.tickets = table.tickets.wrapping_add(1);
        let ticket = table.tickets;
        table.slots.entry(user).or_default().starting = Some(ticket);
        Ok(Reservation {
            flows: self.clone(),
            user,
            ticket,
            registered: false,
        })
    }

    /// Moves `user`'s waiting flow `id` to finishing; `false` when it was
    /// replaced or cancelled meanwhile, and must store nothing.
    pub(super) fn begin_finishing(&self, user: UserId, id: LinkFlowId) -> bool {
        let mut table = self.table();
        let Some(flow) = table
            .slots
            .get_mut(&user)
            .and_then(|slot| slot.flow_mut(id))
        else {
            return false;
        };
        match &flow.phase {
            Phase::Waiting { cancel, .. } if !cancel.is_cancelled() => {
                flow.phase = Phase::Finishing;
                true
            }
            Phase::Waiting { .. } | Phase::Finishing | Phase::Finished { .. } => false,
        }
    }

    /// Ends `user`'s flow `id` with `status`, unless it ended already or was
    /// replaced.
    pub(super) fn end(&self, user: UserId, id: LinkFlowId, status: LinkFlowStatus) {
        let ended_at = self.clock.now();
        let mut table = self.table();
        if let Some(flow) = table
            .slots
            .get_mut(&user)
            .and_then(|slot| slot.flow_mut(id))
            && !matches!(flow.phase, Phase::Finished { .. })
        {
            flow.phase = Phase::Finished { status, ended_at };
        }
    }

    /// The flow `id` and who started it; `None` for an unknown or replaced
    /// flow, and for a finished one past [`FINISHED_RETENTION`].
    #[must_use]
    pub fn view(&self, id: LinkFlowId) -> Option<LinkFlowView> {
        let now = self.clock.now();
        let table = self.table();
        table.slots.iter().find_map(|(user, slot)| {
            let flow = slot.flow.as_ref().filter(|flow| flow.id == id)?;
            let status = match &flow.phase {
                Phase::Waiting { expires_at, .. } => LinkFlowStatus::Waiting {
                    expires_at: *expires_at,
                },
                Phase::Finishing => LinkFlowStatus::Finishing,
                Phase::Finished { status, ended_at } => {
                    if elapsed(*ended_at, now) >= FINISHED_RETENTION {
                        return None;
                    }
                    status.clone()
                }
            };
            Some(LinkFlowView {
                creator: *user,
                status,
            })
        })
    }

    /// Drops every finished flow past [`FINISHED_RETENTION`], and every slot
    /// left empty; returns how many flows it dropped. The server's
    /// housekeeping calls it each minute.
    #[must_use = "the housekeeping logs how many it dropped"]
    pub fn purge_finished(&self) -> usize {
        let now = self.clock.now();
        let mut table = self.table();
        let mut dropped = 0;
        for slot in table.slots.values_mut() {
            let past = slot.flow.as_ref().is_some_and(|flow| match &flow.phase {
                Phase::Finished { ended_at, .. } => elapsed(*ended_at, now) >= FINISHED_RETENTION,
                Phase::Waiting { .. } | Phase::Finishing => false,
            });
            if past {
                slot.flow = None;
                dropped += 1;
            }
        }
        table
            .slots
            .retain(|_, slot| slot.flow.is_some() || slot.starting.is_some());
        dropped
    }

    /// How many users hold a place: a start in progress, or a flow that's
    /// waiting or finishing.
    #[must_use]
    pub fn active(&self) -> usize {
        self.table()
            .slots
            .values()
            .filter(|slot| slot.active())
            .count()
    }

    /// How many flows are held, finished ones included.
    #[must_use]
    pub fn held(&self) -> usize {
        self.table()
            .slots
            .values()
            .filter(|slot| slot.flow.is_some())
            .count()
    }
}

/// Opaque: counts only.
impl fmt::Debug for LinkFlows {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LinkFlows")
            .field("active", &self.active())
            .field("held", &self.held())
            .finish()
    }
}

/// A start's place, from [`LinkFlows::reserve`] until it registers its flow.
/// Dropping it unregistered frees the place, so a start that fails, or whose
/// request is dropped, never keeps it.
pub(super) struct Reservation {
    flows: LinkFlows,
    user: UserId,
    ticket: u64,
    registered: bool,
}

impl Reservation {
    /// Makes room for the new flow: cancels and drops the user's waiting
    /// flow, whose ID it returns, or drops their finished one. A flow that
    /// became finishing meanwhile stays, and the start is
    /// [`Busy::InProgress`].
    pub(super) fn replace(&self) -> Result<Option<LinkFlowId>, Busy> {
        let mut table = self.flows.table();
        let Some(slot) = table.slots.get_mut(&self.user) else {
            return Ok(None);
        };
        let replaced = match slot.flow.as_ref().map(|flow| (flow.id, &flow.phase)) {
            Some((_, Phase::Finishing)) => return Err(Busy::InProgress),
            Some((id, Phase::Waiting { cancel, .. })) => {
                cancel.cancel();
                Some(id)
            }
            None | Some((_, Phase::Finished { .. })) => None,
        };
        slot.flow = None;
        Ok(replaced)
    }

    /// Registers the new flow `id`, waiting until `expires_at` and stopped by
    /// `cancel`, in the place this start held.
    pub(super) fn register(
        mut self,
        id: LinkFlowId,
        expires_at: DateTime<Utc>,
        cancel: CancellationToken,
    ) {
        let mut table = self.flows.table();
        let slot = table.slots.entry(self.user).or_default();
        slot.starting = None;
        slot.flow = Some(Flow {
            id,
            phase: Phase::Waiting { expires_at, cancel },
        });
        drop(table);
        self.registered = true;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.registered {
            return;
        }
        let mut table = self.flows.table();
        if let Some(slot) = table.slots.get_mut(&self.user)
            && slot.starting == Some(self.ticket)
        {
            slot.starting = None;
            if slot.flow.is_none() {
                table.slots.remove(&self.user);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use fleet_core::account::{AccountStatus, RefusalReason};
    use fleet_core::id::AccountId;
    use fleet_testkit::system::ManualClock;

    use super::*;

    /// The start of every test, in milliseconds.
    const T0: i64 = 1_800_000_000_000;

    fn at(millis: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(millis).unwrap()
    }

    fn user(n: u8) -> UserId {
        UserId::new_v7(at(T0), [n; 10]).unwrap()
    }

    fn flow_id(n: u8) -> LinkFlowId {
        LinkFlowId::new_v7(at(T0), [n; 10]).unwrap()
    }

    fn total(n: u32) -> NonZeroU32 {
        NonZeroU32::new(n).unwrap()
    }

    fn flows() -> (LinkFlows, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::new(at(T0)));
        (LinkFlows::new(clock.clone()), clock)
    }

    /// Starts and registers flow `n` of user `n`; returns its cancel token.
    fn waiting(flows: &LinkFlows, n: u8) -> CancellationToken {
        let reservation = flows.reserve(user(n), total(16)).unwrap();
        reservation.replace().unwrap();
        let cancel = CancellationToken::new();
        reservation.register(flow_id(n), at(T0 + 900_000), cancel.clone());
        cancel
    }

    fn status(flows: &LinkFlows, n: u8) -> Option<LinkFlowStatus> {
        flows.view(flow_id(n)).map(|view| view.status)
    }

    #[test]
    fn a_registered_flow_waits_with_its_expiry_for_its_creator() {
        let (flows, _) = flows();

        waiting(&flows, 1);

        let view = flows.view(flow_id(1)).unwrap();
        assert_eq!(view.creator, user(1));
        assert_eq!(
            view.status,
            LinkFlowStatus::Waiting {
                expires_at: at(T0 + 900_000)
            }
        );
        assert_eq!(flows.active(), 1);
        assert_eq!(flows.held(), 1);
    }

    #[test]
    fn an_unknown_flow_has_no_view() {
        let (flows, _) = flows();
        waiting(&flows, 1);

        assert!(flows.view(flow_id(2)).is_none());
    }

    #[test]
    fn a_reservation_counts_toward_the_total() {
        let (flows, _) = flows();
        let _first = flows.reserve(user(1), total(2)).unwrap();
        let _second = flows.reserve(user(2), total(2)).unwrap();

        assert_eq!(flows.active(), 2);
        assert!(matches!(
            flows.reserve(user(3), total(2)),
            Err(Busy::AllBusy)
        ));
    }

    #[test]
    fn a_dropped_reservation_frees_its_place() {
        let (flows, _) = flows();
        let first = flows.reserve(user(1), total(1)).unwrap();

        drop(first);

        assert_eq!(flows.active(), 0);
        assert!(flows.reserve(user(2), total(1)).is_ok());
    }

    #[test]
    fn a_dropped_reservation_keeps_the_users_waiting_flow() {
        let (flows, _) = flows();
        waiting(&flows, 1);

        drop(flows.reserve(user(1), total(1)).unwrap());

        assert!(matches!(
            status(&flows, 1),
            Some(LinkFlowStatus::Waiting { .. })
        ));
    }

    #[test]
    fn a_users_waiting_flow_reuses_its_place_at_the_cap() {
        let (flows, _) = flows();
        waiting(&flows, 1);
        waiting(&flows, 2);

        assert!(flows.reserve(user(1), total(2)).is_ok());
        assert!(matches!(
            flows.reserve(user(3), total(2)),
            Err(Busy::AllBusy)
        ));
    }

    #[test]
    fn a_finished_flow_holds_no_place() {
        let (flows, _) = flows();
        waiting(&flows, 1);
        flows.end(user(1), flow_id(1), LinkFlowStatus::Declined);

        assert_eq!(flows.active(), 0);
        assert!(flows.reserve(user(2), total(1)).is_ok());
    }

    #[test]
    fn a_second_start_in_progress_is_refused() {
        let (flows, _) = flows();
        let _first = flows.reserve(user(1), total(16)).unwrap();

        assert!(matches!(
            flows.reserve(user(1), total(16)),
            Err(Busy::InProgress)
        ));
    }

    #[test]
    fn a_finishing_flow_refuses_a_new_start() {
        let (flows, _) = flows();
        waiting(&flows, 1);
        assert!(flows.begin_finishing(user(1), flow_id(1)));

        assert!(matches!(
            flows.reserve(user(1), total(16)),
            Err(Busy::InProgress)
        ));
        assert_eq!(status(&flows, 1), Some(LinkFlowStatus::Finishing));
    }

    #[test]
    fn replacing_cancels_and_drops_the_waiting_flow() {
        let (flows, _) = flows();
        let old = waiting(&flows, 1);
        let reservation = flows.reserve(user(1), total(16)).unwrap();

        let replaced = reservation.replace().unwrap();
        reservation.register(flow_id(2), at(T0 + 900_000), CancellationToken::new());

        assert_eq!(replaced, Some(flow_id(1)));
        assert!(old.is_cancelled());
        assert!(flows.view(flow_id(1)).is_none());
        assert!(matches!(
            flows.view(flow_id(2)).map(|view| view.status),
            Some(LinkFlowStatus::Waiting { .. })
        ));
        assert_eq!(flows.active(), 1);
    }

    #[test]
    fn replacing_drops_a_finished_flow_without_naming_it() {
        let (flows, _) = flows();
        waiting(&flows, 1);
        flows.end(user(1), flow_id(1), LinkFlowStatus::Expired);
        let reservation = flows.reserve(user(1), total(16)).unwrap();

        assert_eq!(reservation.replace(), Ok(None));
        assert!(flows.view(flow_id(1)).is_none());
    }

    #[test]
    fn a_flow_that_became_finishing_isnt_replaced() {
        let (flows, _) = flows();
        waiting(&flows, 1);
        let reservation = flows.reserve(user(1), total(16)).unwrap();
        assert!(flows.begin_finishing(user(1), flow_id(1)));

        assert_eq!(reservation.replace(), Err(Busy::InProgress));
        drop(reservation);

        assert_eq!(status(&flows, 1), Some(LinkFlowStatus::Finishing));
        assert_eq!(flows.active(), 1);
    }

    #[test]
    fn a_replaced_flow_cant_start_finishing() {
        let (flows, _) = flows();
        waiting(&flows, 1);
        let reservation = flows.reserve(user(1), total(16)).unwrap();
        reservation.replace().unwrap();

        assert!(!flows.begin_finishing(user(1), flow_id(1)));
    }

    #[test]
    fn a_cancelled_flow_cant_start_finishing() {
        let (flows, _) = flows();
        let cancel = waiting(&flows, 1);

        cancel.cancel();

        assert!(!flows.begin_finishing(user(1), flow_id(1)));
        assert!(!flows.begin_finishing(user(1), flow_id(1)));
    }

    #[test]
    fn a_flow_ends_once_and_keeps_its_result() {
        let (flows, _) = flows();
        waiting(&flows, 1);
        let linked = LinkFlowStatus::Linked {
            account: AccountId::new_v7(at(T0), [9; 10]).unwrap(),
            status: AccountStatus::Pending,
            name: "AfkBot1".parse().unwrap(),
        };

        flows.end(user(1), flow_id(1), linked.clone());
        flows.end(user(1), flow_id(1), LinkFlowStatus::Failed);

        assert_eq!(status(&flows, 1), Some(linked));
        assert!(!flows.begin_finishing(user(1), flow_id(1)));
    }

    #[test]
    fn ending_another_flow_of_the_user_changes_nothing() {
        let (flows, _) = flows();
        waiting(&flows, 1);

        flows.end(user(1), flow_id(9), LinkFlowStatus::Failed);

        assert!(matches!(
            status(&flows, 1),
            Some(LinkFlowStatus::Waiting { .. })
        ));
    }

    #[test]
    fn a_finished_flow_is_hidden_after_15_minutes_and_purged() {
        let (flows, clock) = flows();
        waiting(&flows, 1);
        waiting(&flows, 2);
        flows.end(
            user(1),
            flow_id(1),
            LinkFlowStatus::Refused(RefusalReason::NotOwned),
        );

        clock.advance(Duration::from_mins(15) - Duration::from_millis(1));
        assert_eq!(
            status(&flows, 1),
            Some(LinkFlowStatus::Refused(RefusalReason::NotOwned))
        );
        assert_eq!(flows.purge_finished(), 0);

        clock.advance(Duration::from_millis(1));
        assert!(flows.view(flow_id(1)).is_none());
        assert_eq!(flows.purge_finished(), 1);
        assert_eq!(flows.held(), 1);
        assert!(matches!(
            status(&flows, 2),
            Some(LinkFlowStatus::Waiting { .. })
        ));
    }

    #[test]
    fn the_purge_keeps_a_start_in_progress() {
        let (flows, clock) = flows();
        waiting(&flows, 1);
        flows.end(user(1), flow_id(1), LinkFlowStatus::Declined);
        let _reservation = flows.reserve(user(1), total(16)).unwrap();
        clock.advance(Duration::from_mins(15));

        assert_eq!(flows.purge_finished(), 1);

        assert_eq!(flows.active(), 1);
    }

    #[test]
    fn the_retention_is_15_minutes() {
        assert_eq!(FINISHED_RETENTION, Duration::from_mins(15));
    }

    #[test]
    fn debug_shows_counts_only() {
        let (flows, _) = flows();
        waiting(&flows, 1);

        assert_eq!(format!("{flows:?}"), "LinkFlows { active: 1, held: 1 }");
    }
}
