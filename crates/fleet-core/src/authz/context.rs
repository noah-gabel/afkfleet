//! [`Actor`] and [`ResourceContext`]: who asks, and the facts about what they
//! ask for.

use super::{GrantLevel, ModeVisibility, Role};
use crate::id::UserId;

/// The user who asks: their ID and current role.
///
/// The server builds it from the session on every request, so a role change
/// takes effect immediately (Plan.md §7.2). Disabled users never get this far
/// (P7.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Actor {
    /// The user's ID.
    pub id: UserId,
    /// The user's current role.
    pub role: Role,
}

/// Another user a decision depends on, with their current role: the owner of
/// an account or a mode, or the target of user management.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UserRef {
    /// The user's ID.
    pub id: UserId,
    /// The user's current role.
    pub role: Role,
}

/// What a [`Permission`] is checked against, with the facts [`authorize`]
/// needs.
///
/// The caller loads these facts. A resource that doesn't exist at all is
/// answered with 404 before [`authorize`] is called.
///
/// [`Permission`]: super::Permission
/// [`authorize`]: super::authorize
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceContext {
    /// Actions without a single resource: linking an account, creating modes
    /// and invites, listing users and invites, agents and the audit log.
    Global,
    /// A Minecraft account and its bot. A bot is authorized through its
    /// account, because they're 1:1.
    Account {
        /// The user who linked the account.
        owner: UserRef,
        /// The grant the *actor* holds on the account, if any.
        grant: Option<GrantLevel>,
    },
    /// A mode.
    Mode {
        /// The user who created it. `None` is a built-in mode (ADR-0010).
        owner: Option<UserRef>,
        /// Who may see it.
        visibility: ModeVisibility,
    },
    /// A user, as the target of user management.
    User(UserRef),
    /// A pending invite.
    Invite {
        /// The role the invite gives.
        role: Role,
    },
    /// Something that belongs to one user only: their profile, sessions and
    /// 2FA, a Microsoft link flow they started, or an event ticket.
    Personal {
        /// The user it belongs to.
        owner: UserId,
    },
}
