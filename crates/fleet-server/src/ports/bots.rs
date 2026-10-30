//! The bots (Plan.md P11.1, P10.5, Appendix D; ADR-0019, S32–S40): one per
//! linked Minecraft account.
//!
//! A bot is created with its account, in the same transaction, on every path
//! that creates one, and deleted with it. Its row says what the bot should do
//! (the server it joins, its mode, whether it should run), which agent the
//! server assigned it to, and the last state an agent reported for it.
//!
//! **Assignments** change only through [`BotWrites::assign`] (from no agent),
//! [`BotWrites::detach`] (once that agent no longer runs the bot) and
//! [`BotWrites::release_all`] (a disabled agent's, by the host): never by
//! time (S32). A **release record** keeps an agent that may still run a bot
//! whose row no longer says so (a deleted account's bot, S39, or a hello's
//! stray, S43) fenced off from every other agent until it no longer does.

use core::fmt;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::account::{AccountKind, AccountStatus};
use fleet_core::authz::{Actor, GrantLevel, Role};
use fleet_core::bot::{BotAccount, DesiredRunState, LastReport};
use fleet_core::chat::ChatFormats;
use fleet_core::id::{AccountId, AgentId, BotId, ModeId, UserId};
use fleet_core::mode::ModeDefinition;
use fleet_core::placement::McIdentity;
use fleet_core::value::ServerAddress;

use super::accounts::ListedAccount;
use super::page::Page;
use super::server_settings::{ServerTexts, UnreadableFormats};
use super::store::StoreError;

/// A stored bot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bot {
    /// The ID.
    pub id: BotId,
    /// Its account; it never changes.
    pub account: AccountId,
    /// The server it joins; `None` until someone sets one, and a bot without
    /// one is never assigned.
    pub server: Option<ServerAddress>,
    /// What it does online.
    pub mode: ModeId,
    /// Whether it should run.
    pub desired: DesiredRunState,
    /// The agent the server assigned it to, if any.
    pub agent: Option<AgentId>,
    /// The last state an agent reported for it, and when the server
    /// received it; `None` until one is stored.
    pub last: Option<LastReport>,
}

/// A bot to create with its account: no server yet, stopped, unassigned,
/// with no state reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewBot {
    /// The ID, minted with its account's.
    pub id: BotId,
    /// Its account.
    pub account: AccountId,
    /// Its mode: the built-in `afk` for every new bot.
    pub mode: ModeId,
}

/// A bot with what placing it and building its spec need (S33, S37).
#[derive(Debug, Clone, PartialEq)]
pub struct BotRecord {
    /// The row.
    pub bot: Bot,
    /// The account it plays as, as its spec names it.
    pub account: BotAccount,
    /// That account as a player sees it.
    pub identity: McIdentity,
    /// The account's kind.
    pub kind: AccountKind,
    /// The account's status.
    pub status: AccountStatus,
    /// Whether the account's owner is enabled.
    pub owner_enabled: bool,
    /// Its mode's steps.
    pub mode: ModeDefinition,
}

/// A bot as one viewer sees it: its record, and its account as
/// [`super::accounts::AccountReads::listed`] shows it to them, with their
/// own grant on it.
#[derive(Debug, Clone, PartialEq)]
pub struct ListedBot {
    /// The bot, with what its spec and its placement need.
    pub record: BotRecord,
    /// Its account, as the viewer sees it.
    pub account: ListedAccount,
}

/// An open release record (S39, S43).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseRecord {
    /// The bot, which may be gone with its account.
    pub bot: BotId,
    /// The agent that may still run it.
    pub agent: AgentId,
    /// The account it played, as a player sees it.
    pub identity: McIdentity,
}

/// A bot whose row, or whose release record, doesn't read back: the
/// reconciler leaves it alone until it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableBot {
    /// The bot.
    pub bot: BotId,
    /// The agent its row, or its record, names.
    pub agent: Option<AgentId>,
    /// Its account as a player sees it, when that much reads back.
    pub identity: Option<McIdentity>,
    /// The table that doesn't read back.
    pub table: &'static str,
    /// Its first column that doesn't.
    pub column: &'static str,
    /// The row's rowid.
    pub rowid: i64,
    /// Whether what doesn't read back is its server's settings, not a row
    /// of its own (ADR-0020, R132): the reconciler logs that row once a
    /// pass, never each bot on its server.
    pub by_server: bool,
}

/// Everything the reconciler reads, from one snapshot of the database.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fleet {
    /// Every bot that reads back.
    pub bots: Vec<BotRecord>,
    /// Every open release record that reads back.
    pub releases: Vec<ReleaseRecord>,
    /// Every bot, or release record, that doesn't.
    pub unreadable: Vec<UnreadableBot>,
    /// The agents that are enabled.
    pub enabled_agents: BTreeSet<AgentId>,
    /// When each enabled agent's latest unrevoked certificate expires; an
    /// agent without one isn't here. What a bot's agent shows changes with
    /// it (ADR-0020, R37).
    pub agent_expiry: BTreeMap<AgentId, DateTime<Utc>>,
    /// Every server's conflict texts (ADR-0020, S76).
    pub server_texts: ServerTexts,
}

/// Who may see which bot, read in one read transaction (ADR-0020, S30):
/// every bot with its account and the account's owner, every grant, and
/// every user's role and whether they're enabled; with each bot's chat
/// formats, its current server's (S79, R146). Nothing else: no names, no
/// addresses except the keys of the settings rows that don't read back, for
/// their log line (S81), and nothing a viewer is sent but the parsed senders
/// the formats read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Access {
    /// Every bot.
    pub bots: Vec<BotAccess>,
    /// Every grant.
    pub grants: Vec<GrantAccess>,
    /// Every user.
    pub users: Vec<UserAccess>,
    /// Each bot whose current server's settings hold chat formats, with
    /// them: one shared copy a server.
    pub bot_formats: Vec<BotFormats>,
    /// The settings rows whose server or formats don't read back: they give
    /// no bot formats (S81).
    pub unreadable_formats: Vec<UnreadableFormats>,
}

/// A bot's chat formats: its current server's settings' (ADR-0020, R146).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotFormats {
    /// The bot.
    pub bot: BotId,
    /// Its server's formats, shared by every bot on it.
    pub formats: Arc<ChatFormats>,
}

/// A bot, its account and the account's owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BotAccess {
    /// The bot.
    pub bot: BotId,
    /// Its account.
    pub account: AccountId,
    /// The account's owner.
    pub owner: UserId,
}

/// A grant on an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantAccess {
    /// The account.
    pub account: AccountId,
    /// Its grantee.
    pub user: UserId,
    /// Its level.
    pub level: GrantLevel,
}

/// A user's role, and whether they're enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserAccess {
    /// The user.
    pub user: UserId,
    /// Their role.
    pub role: Role,
    /// Whether they're enabled.
    pub enabled: bool,
}

/// Reading bots through the read connections.
#[async_trait]
pub trait BotReads: Send + Sync + fmt::Debug {
    /// The bot `id`.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for a row that
    /// doesn't read back.
    async fn find(&self, id: BotId) -> Result<Option<Bot>, StoreError>;

    /// The bot of the account `account`.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn of_account(&self, account: AccountId) -> Result<Option<Bot>, StoreError>;

    /// The bot `id` with its account, owner and mode.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn record(&self, id: BotId) -> Result<Option<BotRecord>, StoreError>;

    /// The bots of `page` with their accounts, owners and modes, newest
    /// first: one more than its limit when another page follows.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn records(&self, page: Page<BotId>) -> Result<Vec<BotRecord>, StoreError>;

    /// The bot `id` as `viewer` sees it, with their grant on its account.
    /// The caller still asks `authorize()`.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn listed(&self, id: BotId, viewer: UserId) -> Result<Option<ListedBot>, StoreError>;

    /// The bots among `ids` that exist, each as [`BotReads::listed`] reads
    /// it for `viewer`, in one query (ADR-0020, S41), in ID order. The
    /// caller still asks `authorize()` for each.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn listed_in(&self, ids: &[BotId], viewer: UserId) -> Result<Vec<ListedBot>, StoreError>;

    /// One page of the bots `viewer` may see, newest first: those of the
    /// accounts [`super::accounts::AccountReads::list_visible`] lists for
    /// them, by the owners' current roles (ADR-0010). One more than its limit
    /// when another page follows; the caller still asks `authorize()` for
    /// each.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn visible(&self, viewer: Actor, page: Page<BotId>)
    -> Result<Vec<ListedBot>, StoreError>;

    /// Every bot, every open release record and the enabled agents, read in
    /// one read transaction, so they agree with each other.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for any row that
    /// doesn't read back.
    async fn fleet(&self) -> Result<Fleet, StoreError>;

    /// Who may see which bot: [`Access`], read in one read transaction, so
    /// its parts agree with each other (S30).
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for any row that
    /// doesn't read back: one unreadable row fails the whole read, so no
    /// viewer is judged on a part of it.
    async fn access(&self) -> Result<Access, StoreError>;
}

/// Writing bots inside a [`super::store::WriteTx`].
#[async_trait]
pub trait BotWrites: Send {
    /// Inserts `bot`.
    ///
    /// # Errors
    /// [`StoreError`]: an account that doesn't exist or has a bot already, a
    /// mode that doesn't exist, or an ID that does.
    async fn insert(&mut self, bot: &NewBot) -> Result<(), StoreError>;

    /// The bot `id`, read in this transaction.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn find(&mut self, id: BotId) -> Result<Option<Bot>, StoreError>;

    /// The agent the bot `id`'s row assigns it to, read in this
    /// transaction: `None` without a row, `Some(None)` when it's
    /// unassigned. It reads no other column, so a row whose other columns
    /// don't read back still answers.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for an agent ID
    /// that doesn't read back.
    async fn agent_of(&mut self, id: BotId) -> Result<Option<Option<AgentId>>, StoreError>;

    /// The ID of the account `account`'s bot, read in this transaction, for
    /// the live events' hint after a write to the account (ADR-0020, S42).
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for an ID that
    /// doesn't read back.
    async fn id_of_account(&mut self, account: AccountId) -> Result<Option<BotId>, StoreError>;

    /// The IDs of `owner`'s accounts' bots, in ID order, read in this
    /// transaction (S42).
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn owned_by(&mut self, owner: UserId) -> Result<Vec<BotId>, StoreError>;

    /// The IDs of the bots whose rows assign them to `agent`, in ID order,
    /// read in this transaction (S42).
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn assigned_to(&mut self, agent: AgentId) -> Result<Vec<BotId>, StoreError>;

    /// Assigns `bot` to `agent`, but only while it has no agent and
    /// `agent` is enabled. Whether it changed.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn assign(&mut self, bot: BotId, agent: AgentId) -> Result<bool, StoreError>;

    /// Stores `report` as `bot`'s last state, but only while its row names
    /// `agent` and no later report is stored. Whether it changed.
    ///
    /// # Errors
    /// [`StoreError`], or [`StoreError::Unstorable`] for a time outside
    /// the years 0 to 9999.
    async fn record_state(
        &mut self,
        bot: BotId,
        agent: AgentId,
        report: &LastReport,
    ) -> Result<bool, StoreError>;

    /// Clears `bot`'s assignment to `agent`, and `agent`'s release record
    /// of it: the agent no longer runs it. Whether either changed.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn detach(&mut self, bot: BotId, agent: AgentId) -> Result<bool, StoreError>;

    /// Writes a release record of `bot` for `agent`, at `at`, from its row
    /// and account, or from another agent's record of it when the row is
    /// gone; nothing when `agent` has one already, or nothing says which
    /// account it plays. Whether one was written.
    ///
    /// # Errors
    /// [`StoreError`], or [`StoreError::Unstorable`] for `at`.
    async fn stray(
        &mut self,
        bot: BotId,
        agent: AgentId,
        at: DateTime<Utc>,
    ) -> Result<bool, StoreError>;

    /// Clears every assignment to `agent` and deletes every release record
    /// naming it: the host released a disabled agent's bots. The bots it
    /// cleared, each once, in ID order.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for an ID that
    /// doesn't read back.
    async fn release_all(&mut self, agent: AgentId) -> Result<Vec<BotId>, StoreError>;

    /// Sets `bot`'s server address to `server`, in its written form. Whether
    /// it changed: `false` for the address it has, or no such bot. It never
    /// touches the bot's agent.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn set_server(&mut self, bot: BotId, server: &ServerAddress) -> Result<bool, StoreError>;

    /// Switches `bot` to the mode `mode`. Whether it changed: `false` for
    /// the mode it has, or no such bot.
    ///
    /// # Errors
    /// [`StoreError`]: a mode that doesn't exist, among the rest.
    async fn set_mode(&mut self, bot: BotId, mode: ModeId) -> Result<bool, StoreError>;

    /// The accounts whose bots run `mode`, in ID order, each once (an
    /// account has one bot), read in this transaction: what an edit of the
    /// mode is checked against and what keeps it from being deleted
    /// (ADR-0020, S17).
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for an ID that
    /// doesn't read back.
    async fn accounts_with_mode(&mut self, mode: ModeId) -> Result<Vec<AccountId>, StoreError>;

    /// Sets whether `bot` should run. Whether it changed: `false` when it
    /// says so already, or no such bot.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn set_desired(
        &mut self,
        bot: BotId,
        desired: DesiredRunState,
    ) -> Result<bool, StoreError>;

    /// Clears `bot`'s stored last state, but only while its row names no
    /// agent and the stored state is still `last`: a Resume or a Reset of a
    /// bot that runs nowhere, so its next placement starts it instead of
    /// restoring it (ADR-0019, S48). Whether it changed.
    ///
    /// # Errors
    /// [`StoreError`], or [`StoreError::Unstorable`] for a time outside the
    /// years 0 to 9999.
    async fn clear_sticky(&mut self, bot: BotId, last: &LastReport) -> Result<bool, StoreError>;
}
