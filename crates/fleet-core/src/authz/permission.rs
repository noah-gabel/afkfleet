//! [`Permission`]: what an actor wants to do.

use super::{CommandCheck, GrantLevel, ModeVisibility, Role};
use crate::id::UserId;

/// Something an actor wants to do.
///
/// Each permission applies to one kind of [`ResourceContext`], named in the
/// groups below; [`authorize`] denies it on any other. The level or role in
/// parentheses is what it needs (Plan.md §7.3, ADR-0010).
///
/// [`ResourceContext`]: super::ResourceContext
/// [`authorize`]: super::authorize
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    // Accounts and bots (`ResourceContext::Account`).
    /// See a bot and its account: status, settings, chat history and live
    /// events (View).
    ViewBot,
    /// Start, stop, restart, reset or resume a bot (Control).
    ControlBot,
    /// Send a chat message or command through the bot (Control; Manage for a
    /// command off the allowlist).
    SendChat(CommandCheck),
    /// Switch the bot to a mode (Control; Manage if the mode sends a command
    /// off the allowlist). The caller also needs [`Permission::ViewMode`] on
    /// the mode itself.
    SetBotMode(CommandCheck),
    /// Change the bot's server address or auto-start (Manage).
    ConfigureBot,
    /// List the account's grants, or revoke one (Manage).
    ManageGrants,
    /// Give a user a grant on the account, or change theirs (Manage). Nobody
    /// grants above their own level, or to themselves.
    Grant {
        /// The level granted.
        level: GrantLevel,
        /// The user who receives it.
        grantee: UserId,
    },
    /// Sign the Microsoft account in again (Manage).
    RelinkAccount,
    /// Delete the account and its bot (Manage).
    DeleteAccount,

    // Fleet-wide (`ResourceContext::Global`).
    /// Link a new Microsoft account (every role; P9.4 checks the quota).
    LinkAccount,
    /// Create a mode (private: every role; shared: Admin+).
    CreateMode {
        /// Who may see the new mode.
        visibility: ModeVisibility,
    },
    /// List all users (Admin+).
    ListUsers,
    /// List the pending invites (Admin+).
    ListInvites,
    /// Create an invite (for a Member: Admin+; for an Admin: Owner; for an
    /// Owner: nobody).
    CreateInvite {
        /// The role the invite gives.
        role: Role,
    },
    /// List the agents (Admin+).
    ViewAgents,
    /// Create an agent enrollment token (Owner). Every agent can receive
    /// session tokens for the bots assigned to it, so only the Owner decides
    /// which agents exist (ADR-0010).
    EnrollAgent,
    /// Disable an agent (Admin+).
    DisableAgent,
    /// Read the audit log (Admin+).
    ViewAuditLog,

    // Invites (`ResourceContext::Invite`).
    /// Revoke an invite (the same rule as creating it).
    RevokeInvite,

    // Modes (`ResourceContext::Mode`).
    /// See a mode and use it on a bot.
    ViewMode,
    /// Change a mode. Built-in modes are read-only. The actor needs the
    /// rights to edit the mode both as it is and with its new visibility.
    UpdateMode {
        /// Who may see the mode after the change.
        visibility: ModeVisibility,
    },
    /// Delete a mode. Built-in modes are read-only.
    DeleteMode,

    // Users (`ResourceContext::User`).
    /// Change a user's role between Member and Admin (Owner). Nobody changes
    /// their own role or the Owner's, and nobody becomes the Owner this way.
    SetRole {
        /// The new role.
        role: Role,
    },
    /// Disable or enable a user (Owner; Admins for Members). Nobody disables
    /// themselves or the Owner.
    SetDisabled,

    // Personal resources (`ResourceContext::Personal`).
    /// Use one's own profile, sessions, 2FA, link flows and event tickets
    /// (only the user they belong to).
    UsePersonal,
}
