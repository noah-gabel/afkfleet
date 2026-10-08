//! [`BotSpec`]: what a bot should be doing.

use super::{BotState, FailReason, PauseReason};
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

impl BotAccount {
    /// Whether two bots with these accounts can't run side by side: offline
    /// names compare ignoring ASCII case, online accounts by [`AccountId`].
    ///
    /// Minecraft names are unique whatever their case, so `AfkBot1` and
    /// `afkbot1` clash. `PartialEq` stays exact: a bot whose account changes
    /// only in case still has a changed account (ADR-0013).
    #[must_use]
    pub fn clashes_with(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Offline(name), Self::Offline(other)) => {
                name.as_str().eq_ignore_ascii_case(other.as_str())
            }
            (Self::Online(account), Self::Online(other)) => account == other,
            (Self::Offline(_), Self::Online(_)) | (Self::Online(_), Self::Offline(_)) => false,
        }
    }
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

/// A state that outlives the actor that reached it: the bot stopped by itself,
/// and only a human's `Resume` or `Reset` connects it again (ADR-0013).
///
/// The server stores a bot's last state and passes it back when it assigns the
/// bot, so a bot that was paused for a human playing on its account stays
/// paused on another agent or after an agent restart (Plan.md §6 row 3; P10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StickyState {
    /// The bot paused because a human is playing on its account.
    Paused(PauseReason),
    /// The bot failed, and retrying won't help.
    Failed(FailReason),
}

impl From<StickyState> for BotState {
    fn from(sticky: StickyState) -> Self {
        match sticky {
            StickyState::Paused(reason) => Self::Paused { reason },
            StickyState::Failed(reason) => Self::Failed { reason },
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::disconnect::{ConflictKind, PermanentKind};

    const ACCOUNT: &str = "018bcfe5-6800-7bab-abab-abababababab";
    const OTHER_ACCOUNT: &str = "018bcfe5-6800-7cdc-8dcd-cdcdcdcdcdcd";

    fn offline(name: &str) -> BotAccount {
        BotAccount::Offline(name.parse().unwrap())
    }

    fn online(id: &str) -> BotAccount {
        BotAccount::Online(id.parse().unwrap())
    }

    #[rstest]
    #[case::same_offline_name(offline("AfkBot1"), offline("AfkBot1"), true)]
    #[case::offline_name_in_another_case(offline("AfkBot1"), offline("aFKbOT1"), true)]
    #[case::different_offline_names(offline("AfkBot1"), offline("AfkBot2"), false)]
    #[case::same_online_account(online(ACCOUNT), online(ACCOUNT), true)]
    #[case::different_online_accounts(online(ACCOUNT), online(OTHER_ACCOUNT), false)]
    #[case::offline_and_online(offline("AfkBot1"), online(ACCOUNT), false)]
    fn accounts_clash_when_they_name_the_same_player(
        #[case] first: BotAccount,
        #[case] second: BotAccount,
        #[case] clash: bool,
    ) {
        assert_eq!(first.clashes_with(&second), clash);
        assert_eq!(second.clashes_with(&first), clash, "symmetric");
    }

    #[test]
    fn equality_stays_exact_about_case() {
        assert_ne!(offline("AfkBot1"), offline("afkbot1"));
    }

    #[rstest]
    #[case::paused(
        StickyState::Paused(PauseReason::Conflict { kind: ConflictKind::DuplicateLogin }),
        BotState::Paused { reason: PauseReason::Conflict { kind: ConflictKind::DuplicateLogin } }
    )]
    #[case::failed(
        StickyState::Failed(FailReason::Permanent { kind: PermanentKind::Banned }),
        BotState::Failed { reason: FailReason::Permanent { kind: PermanentKind::Banned } }
    )]
    #[case::crash_loop(
        StickyState::Failed(FailReason::CrashLoop),
        BotState::Failed { reason: FailReason::CrashLoop }
    )]
    fn a_sticky_state_is_the_bot_state_it_names(
        #[case] sticky: StickyState,
        #[case] state: BotState,
    ) {
        assert_eq!(BotState::from(sticky), state);
    }
}
