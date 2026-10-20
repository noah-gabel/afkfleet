//! A bot's chat history (Plan.md P11.3; ADR-0020, S6, S9, U3, U5).
//!
//! [`BotService::chat`] reads the bot as its viewer sees it, asks
//! `authorize()` for `ViewBot` (a bot the viewer may not see is an
//! unknown one), and pages its chat newest first by ID, each user's line
//! with its sender's username. What each line shows its viewer (a command's
//! arguments, the sender's name, `mine`) is the HTTP layer's field rule,
//! which takes the viewer's level on the account this answers with.
//!
//! The order is the server's commit order: a user's line commits at once,
//! received chat with the ingest's next batch, so a line can come before a
//! message received a moment earlier (U5).

use fleet_core::authz::{Actor, AuthzError, Permission, authorize};
use fleet_core::id::BotId;

use super::BotService;
use crate::ports::accounts::ListedAccount;
use crate::ports::chat::{ChatMessageId, ListedChat};
use crate::ports::page::{Page, Paged};
use crate::ports::store::StoreError;

/// One page of a bot's chat, and its account as the viewer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatHistory {
    /// The bot's account, with the viewer's grant: the field rules' level.
    pub account: ListedAccount,
    /// The lines, newest first, and the next page's cursor.
    pub lines: Paged<ListedChat, ChatMessageId>,
}

/// Why a bot's chat can't be read.
#[derive(Debug, thiserror::Error)]
pub enum ChatHistoryError {
    /// No such bot.
    #[error("no such bot")]
    NotFound,
    /// `authorize()` refused: a bot the viewer may not see.
    #[error(transparent)]
    Denied(AuthzError),
    /// The database.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl BotService {
    /// One page of the bot `id`'s chat for `viewer`, newest first; see the
    /// [module docs](self).
    ///
    /// # Errors
    /// [`ChatHistoryError::NotFound`] for an unknown bot,
    /// [`ChatHistoryError::Denied`] for one the viewer may not see, and
    /// [`ChatHistoryError::Store`].
    pub async fn chat(
        &self,
        viewer: &Actor,
        id: BotId,
        page: Page<ChatMessageId>,
    ) -> Result<ChatHistory, ChatHistoryError> {
        let Some(listed) = self.store.bots().listed(id, viewer.id).await? else {
            return Err(ChatHistoryError::NotFound);
        };
        authorize(viewer, Permission::ViewBot, &listed.account.context())
            .map_err(ChatHistoryError::Denied)?;
        let rows = self.store.chat().page(id, page).await?;
        Ok(ChatHistory {
            account: listed.account,
            lines: Paged::split(rows, page.limit, |line: &ListedChat| line.record.id),
        })
    }
}
