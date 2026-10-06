//! Authorization: who may do what (Plan.md §7.3, P2.9; ADR-0010).
//!
//! [`authorize`] is the one function that decides every access. It's pure
//! and denies by default: only the rules below allow anything. It takes the
//! [`Actor`] who asks, the [`Permission`] they want, and a
//! [`ResourceContext`]: the facts about the resource that the caller loaded.
//!
//! - **Roles.** There's one Owner; Admins manage Members; Members manage
//!   what's theirs or shared with them ([`Role`]).
//! - **Accounts and bots.** A bot is authorized through its Minecraft
//!   account. The actor's level on an account ([`GrantLevel`]:
//!   `View < Control < Manage`) is the higher of an explicit grant and an
//!   implicit Manage. The account's owner, the Owner, and an Admin on a
//!   Member's account have implicit Manage. Admins have none on the Owner's
//!   or other Admins' accounts. Only Manage edits grants, nobody grants
//!   above their own level, and nobody grants to themselves.
//! - **Commands.** A `/command` that isn't on the [`CommandAllowlist`] needs
//!   Manage, whether it's sent as chat or in a mode the bot switches to.
//! - **Modes.** Built-in modes are read-only, even for the Owner. Shared
//!   modes are visible to everyone and edited by their creator, while
//!   they're an Admin, and by the Owner. Private modes follow the account
//!   rule.
//! - **Users and invites.** Admins disable Members and handle Member
//!   invites. Only the Owner changes roles, handles Admin invites and
//!   enrolls agents. Nobody acts on themselves or on the Owner, and nobody
//!   becomes the Owner this way.
//! - **Personal resources** (profile, sessions, 2FA, link flows, event
//!   tickets) belong to one user only.
//!
//! An [`AuthzError`] tells the API what to answer:
//! - [`AuthzError::NotFound`] (404) when the actor may not even see the
//!   resource, so its existence stays hidden.
//! - [`AuthzError::Forbidden`] (403) when they may see it, but not do this.
//! - [`AuthzError::WrongResource`] (500) when the caller checked a permission
//!   against the wrong kind of resource: a bug, denied like everything else.

mod allowlist;
mod authorize;
mod context;
mod level;
mod permission;

pub use allowlist::{CommandAllowlist, CommandAllowlistError, CommandCheck};
pub use authorize::{AuthzError, authorize};
pub use context::{Actor, ResourceContext, UserRef};
pub use level::{GrantLevel, LevelError, ModeVisibility, Role};
pub use permission::Permission;
