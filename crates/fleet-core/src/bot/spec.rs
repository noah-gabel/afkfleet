//! [`BotSpec`]: what a bot should be doing.

use crate::disconnect::ConflictTexts;
use crate::id::{AccountId, BotId};
use crate::mode::ModeDefinition;
use crate::value::{McUsername, ServerAddress};

/// The account a bot plays as (ADR-0013).
///
/// A bot's account never changes: the runtime refuses a spec that names a
/// different account for a bot it already runs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BotAccount {
    /// An offline-mode account, for development and tests. The bot logs in
    /// with this name, and only servers in offline mode accept it.
    Offline(McUsername),
    /// An online-mode account linked on the server. The agent never holds its
    /// Microsoft token: it asks the server for a short-lived Minecraft session
    /// for each attempt (security rule 6).
    Online(AccountId),
}

/// Whether a bot should run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DesiredRunState {
    /// The bot should be online.
    Running,
    /// The bot should be offline.
    Stopped,
}

/// What a bot should be doing: its desired configuration (Plan.md P4.1;
/// ADR-0013).
///
/// The server builds it from a bot's row and sends it to the agent that runs
/// the bot; the standalone agent builds it from its config. Every field is a
/// validated type, so a spec is valid as soon as it exists. There's no serde:
/// the wire format comes with the proto conversions (P10).
#[derive(Debug, Clone, PartialEq)]
pub struct BotSpec {
    /// The bot.
    pub id: BotId,
    /// The account it plays as. It never changes for a bot.
    pub account: BotAccount,
    /// The server it joins.
    pub server: ServerAddress,
    /// What it does while it's online.
    pub mode: ModeDefinition,
    /// Whether it should run.
    pub desired: DesiredRunState,
    /// Kick texts that count as a duplicate login on its server.
    pub conflict_texts: ConflictTexts,
}
