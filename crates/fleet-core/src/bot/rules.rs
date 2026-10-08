//! [`BotRules`]: the per-bot rules that [`transition`](super::transition)
//! applies.

use crate::disconnect::ConflictTexts;
use crate::resilience::RetryPolicy;

/// The rules [`transition`](super::transition) applies to one bot (ADR-0013).
///
/// The retry policy comes from the runtime's config and is the same for every
/// bot; the conflict texts come from the bot's
/// [`BotSpec`](super::BotSpec).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotRules {
    /// How long to back off, and when an online session counts as stable.
    pub retry: RetryPolicy,
    /// Kick texts that count as a duplicate login.
    pub conflict_texts: ConflictTexts,
}
