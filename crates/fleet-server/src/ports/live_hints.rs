//! What tells the live events sockets that the server changed something a
//! viewer may see (Plan.md P11.5; ADR-0020, S32, S33, S42, R36): a hint after
//! a commit, which never waits and never fails.
//!
//! A hint carries no facts a viewer could be sent: each socket reads what it
//! shows again, for its own viewer, when it sends (S32, S33). Services send
//! one only once their commit returned `Ok`, so the commit count has moved
//! before the hint arrives, and a socket judges it on the rights after the
//! change. Through this port a service can't publish what an agent
//! reported.

use core::fmt;

use fleet_core::authz::{ModeVisibility, ResourceContext, UserRef};
use fleet_core::id::{BotId, ModeId, UserId};

/// A mode was created, changed or deleted (S33).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeEvent {
    /// The mode.
    pub id: ModeId,
    /// Who could see it before, as the write's transaction read it: `None`
    /// for a new mode.
    pub was: Option<ModeSeen>,
}

/// What decided who could see a mode, as a write read it (R39).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeSeen {
    /// Its owner with their role then; `None` for a built-in mode.
    pub owner: Option<UserRef>,
    /// Its visibility then.
    pub visibility: ModeVisibility,
}

impl ModeSeen {
    /// The mode's context for `authorize()`, as it was.
    #[must_use]
    pub const fn context(&self) -> ResourceContext {
        ResourceContext::Mode {
            owner: self.owner,
            visibility: self.visibility,
        }
    }
}

/// Tells the live events sockets to look again; see the [module docs](self).
pub trait LiveHints: Send + Sync + fmt::Debug {
    /// What `bots` read as may have changed. Nothing for an empty list.
    fn bots(&self, bots: &[BotId]);

    /// A mode was created, changed or deleted.
    fn mode(&self, event: ModeEvent);

    /// `user`'s role changed (R11).
    fn role_changed(&self, user: UserId);
}

/// No sockets to tell: the hints go nowhere. For the host's commands and
/// for services built without the live events.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoLiveHints;

impl LiveHints for NoLiveHints {
    fn bots(&self, _bots: &[BotId]) {}

    fn mode(&self, _event: ModeEvent) {}

    fn role_changed(&self, _user: UserId) {}
}
