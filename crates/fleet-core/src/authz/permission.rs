//! [`Permission`]: what an actor wants to do, and [`ModeKind`], what a mode
//! is made of.

use super::{CommandCheck, GrantLevel, ModeVisibility, Role};
use crate::id::UserId;
use crate::mode::{ModeBody, ModeDefinition};

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
    /// Change the bot's server address (Manage).
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
    /// Approve or refuse the account's pending link: someone above its
    /// owner, so the Owner for Admins' and Members' links and an Admin for
    /// Members'. The actor must see the account; a grant never counts, and
    /// nobody reviews their own (ADR-0018).
    ReviewLink,
    /// Give up one's own grant on the account: any grantee without implicit
    /// Manage, at any level (ADR-0020, S51). The account's owner, the Owner
    /// and an Admin on a Member's account hold none to give up.
    LeaveAccount,

    // Fleet-wide (`ResourceContext::Global`).
    /// Link a new Microsoft account (every role; P9.4 checks the quota).
    LinkAccount,
    /// Create a mode (steps, private: every role; steps, shared: Admin+; a
    /// script: Admin+, ADR-0036, D15).
    CreateMode {
        /// Who may see the new mode.
        visibility: ModeVisibility,
        /// What it's made of.
        kind: ModeKind,
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
    /// Disable an agent (Admin+).
    DisableAgent,
    /// Release a disabled agent's bots, on the operator's word that its
    /// host is stopped (Admin+; ADR-0020, S49): its own permission, since
    /// it lifts a guard that disabling keeps.
    ReleaseAgent,
    /// Read the audit log (Admin+).
    ViewAuditLog,
    /// See every server's settings: the conflict texts and the chat
    /// formats every bot on a server shares (Admin+; ADR-0020, S75).
    ViewServerSettings,
    /// Create, change or delete a server's settings (Admin+; ADR-0020, U54). A
    /// change reaches every user's bots on that
    /// server, the Owner's too.
    ConfigureServerSettings,

    // Invites (`ResourceContext::Invite`).
    /// Revoke an invite (the same rule as creating it).
    RevokeInvite,

    // Modes (`ResourceContext::Mode`).
    /// See a mode and use it on a bot.
    ViewMode,
    /// Change a mode. Built-in modes are read-only. The actor needs the
    /// rights to edit the mode both as it is and as it will be, each asked
    /// with that mode's visibility and kind; a script's needs Admin+
    /// (ADR-0036, D15), so a Member never edits a scripted mode, not even
    /// into steps.
    UpdateMode {
        /// Who may see the mode, as it is or after the change.
        visibility: ModeVisibility,
        /// What it's made of, as it is or after the change.
        kind: ModeKind,
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
    /// See a user's grants at a role change: the grants they hold, dormant
    /// ones included, and their shared accounts (Owner, as for [`SetRole`];
    /// ADR-0020, S50).
    ///
    /// [`SetRole`]: Permission::SetRole
    ReviewUserGrants,

    // Personal resources (`ResourceContext::Personal`).
    /// Use one's own profile, sessions, 2FA, link flows and event tickets
    /// (only the user they belong to).
    UsePersonal,
}

/// What a mode is made of, as [`authorize`](super::authorize) tells modes
/// apart: only an Admin or the Owner writes a script, which is new code that
/// hostile input reaches and which moves a bot (ADR-0036, D15). A Member
/// runs a shared scripted mode on the bots they control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModeKind {
    /// Steps, each an action with its schedule.
    Steps,
    /// A script.
    Script,
}

impl ModeKind {
    /// What `mode` is made of.
    #[must_use]
    pub const fn of(mode: &ModeDefinition) -> Self {
        match mode.body() {
            ModeBody::Steps(_) => Self::Steps,
            ModeBody::Script(_) => Self::Script,
        }
    }

    /// Its stable name, for the audit log: `steps` or `script`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Steps => "steps",
            Self::Script => "script",
        }
    }
}
