//! A bot's chat history (Plan.md P11.3; ADR-0020, S6, S9, U3, U5).
//!
//! [`BotService::chat`] reads the bot as its viewer sees it, asks
//! `authorize()` for `ViewBot` (a bot the viewer may not see is an
//! unknown one), and pages its chat newest first by ID, each user's line
//! with its sender's username. What each line shows its viewer (a command's
//! arguments, the sender's name, `mine`) is the HTTP layer's field rule,
//! which takes the viewer's level on the account this answers with.
//!
//! A received system line is read with the chat formats of the bot's
//! current server, which the HTTP layer's field rule turns into its parsed
//! sender (ADR-0020, S79, R146). Formats that don't read back count as none,
//! and are logged at each read that meets them (S81).
//!
//! The order is the server's commit order: a user's line commits at once,
//! received chat with the ingest's next batch, so a line can come before a
//! message received a moment earlier (U5).

use fleet_core::authz::{Actor, AuthzError, Permission, authorize};
use fleet_core::chat::ChatFormats;
use fleet_core::id::BotId;
use fleet_core::value::ServerKey;

use super::BotService;
use crate::app::server_settings;
use crate::ports::accounts::ListedAccount;
use crate::ports::chat::{ChatMessageId, ListedChat};
use crate::ports::page::{Page, Paged};
use crate::ports::server_settings::ServerFormats;
use crate::ports::store::StoreError;

/// One page of a bot's chat, and its account as the viewer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatHistory {
    /// The bot's account, with the viewer's grant: the field rules' level.
    pub account: ListedAccount,
    /// The lines, newest first, and the next page's cursor.
    pub lines: Paged<ListedChat, ChatMessageId>,
    /// The chat formats of the bot's current server, which its system lines
    /// are read with (ADR-0020, S79, S81, R146): none without a server,
    /// settings or formats, or when its settings' formats don't read back.
    pub formats: ChatFormats,
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
        let formats = match &listed.record.bot.server {
            None => ChatFormats::default(),
            Some(server) => self.formats_of(&ServerKey::from(server)).await?,
        };
        Ok(ChatHistory {
            account: listed.account,
            lines: Paged::split(rows, page.limit, |line: &ListedChat| line.record.id),
            formats,
        })
    }

    /// The formats a bot on `server` reads its system lines with: none
    /// without settings, and none, logged, when they don't read back (S81).
    async fn formats_of(&self, server: &ServerKey) -> Result<ChatFormats, StoreError> {
        let formats = match self.store.server_settings().formats_of(server).await? {
            ServerFormats::NoSettings => ChatFormats::default(),
            ServerFormats::Readable(formats) => formats,
            ServerFormats::Unreadable(row) => {
                server_settings::unreadable_formats(&row);
                ChatFormats::default()
            }
        };
        Ok(formats)
    }
}
