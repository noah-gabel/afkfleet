//! The `chat_messages` table's SQL: appending a message inside a write
//! transaction (a batch's, or a user's send), and one page of a bot's
//! messages (Plan.md P11.1, P10.8, P11.3; ADR-0019, S70; ADR-0020, S6, U3).
//!
//! A row's columns follow from its [`ChatEntry`]: a received message is
//! `in`, with fleet-core's [`ChatKind`] name and its sender, if any; a mode's
//! message is `out` with the kind `mode`, no sender and no cut; a user's line
//! is `out` with the kind `user`, no Minecraft sender, no cut, and the user
//! who sent it in `sent_by` (`NULL` once that user is deleted). Reading
//! rebuilds the entry through fleet-core's constructors, so a row they
//! refuse is [`StoreError::Corrupt`], naming the column.

use core::str::FromStr;

use fleet_core::chat::{ChatKind, ChatMessage, ChatSender, IncomingChat, IncomingChatError};
use fleet_core::id::BotId;
use fleet_core::value::Username;
use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;
use uuid::fmt::Hyphenated;

use super::convert::{id_text, parse_id, parse_time, time_text};
use super::error::store_error;
use crate::ports::chat::{ChatEntry, ChatMessageId, ChatRecord, ListedChat, NewChatMessage};
use crate::ports::page::Page;
use crate::ports::store::StoreError;

/// The table, as errors name it.
const TABLE: &str = "chat_messages";

/// What a bot received.
const IN: &str = "in";
/// What a bot sent.
const OUT: &str = "out";
/// The kind of a message the bot's mode sent.
const MODE: &str = "mode";
/// The kind of a line a user sent through the API.
const USER: &str = "user";

/// A row as stored, before its values are checked.
struct ChatRow {
    id: i64,
    bot_id: String,
    direction: String,
    kind: String,
    sender_name: Option<String>,
    sender_uuid: Option<String>,
    text: String,
    truncated: i64,
    at: String,
    sent_by: Option<String>,
    /// The sender's username, joined: `NULL` without a sender.
    sent_by_name: Option<String>,
}

/// The columns an entry is stored in.
struct Columns<'a> {
    direction: &'static str,
    kind: &'static str,
    sender_name: Option<&'a str>,
    sender_uuid: Option<String>,
    text: &'a str,
    truncated: bool,
    sent_by: Option<Hyphenated>,
}

impl<'a> Columns<'a> {
    fn of(entry: &'a ChatEntry) -> Self {
        match entry {
            ChatEntry::Received(chat) => Self {
                direction: IN,
                kind: chat.kind().as_str(),
                sender_name: chat.sender().map(ChatSender::name),
                sender_uuid: chat
                    .sender()
                    .and_then(ChatSender::uuid)
                    .map(|uuid| uuid.hyphenated().to_string()),
                text: chat.text(),
                truncated: chat.is_truncated(),
                sent_by: None,
            },
            ChatEntry::ModeSent(message) => Self {
                direction: OUT,
                kind: MODE,
                sender_name: None,
                sender_uuid: None,
                text: message.as_str(),
                truncated: false,
                sent_by: None,
            },
            ChatEntry::UserSent { message, by } => Self {
                direction: OUT,
                kind: USER,
                sender_name: None,
                sender_uuid: None,
                text: message.as_str(),
                truncated: false,
                sent_by: by.map(id_text),
            },
        }
    }
}

/// Appends `message` while its bot's row names its agent: one statement,
/// whose `WHERE EXISTS` reads the row inside the same write transaction
/// (`BEGIN IMMEDIATE` holds every other writer off), so the bot can't be
/// deleted or moved between the check and the insert.
pub(super) async fn append(
    conn: &mut SqliteConnection,
    message: &NewChatMessage,
) -> Result<Option<ChatMessageId>, StoreError> {
    let at = time_text(message.at).ok_or(StoreError::Unstorable {
        table: TABLE,
        column: "at",
    })?;
    let bot = id_text(message.bot);
    let agent = id_text(message.agent);
    let Columns {
        direction,
        kind,
        sender_name,
        sender_uuid,
        text,
        truncated,
        sent_by,
    } = Columns::of(&message.entry);
    let truncated = i64::from(truncated);
    let done = sqlx::query!(
        "INSERT INTO chat_messages
             (bot_id, direction, kind, sender_name, sender_uuid, text, truncated, at, sent_by)
         SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?10
         WHERE EXISTS (SELECT 1 FROM bots WHERE id = ?1 AND agent_id = ?9)",
        bot,
        direction,
        kind,
        sender_name,
        sender_uuid,
        text,
        truncated,
        at,
        agent,
        sent_by
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok((done.rows_affected() == 1).then(|| ChatMessageId::new(done.last_insert_rowid())))
}

/// One page of `bot`'s messages, newest first by ID (commit order, which a
/// user's line can lead by one ingest batch; ADR-0020, U5), one row more
/// than the page holds, each user's line with its sender's username.
pub(super) async fn page(
    pool: &SqlitePool,
    bot: BotId,
    page: Page<ChatMessageId>,
) -> Result<Vec<ListedChat>, StoreError> {
    let bot = id_text(bot);
    let before = page.before.map_or(i64::MAX, ChatMessageId::get);
    let fetch = i64::from(page.limit.fetch());
    sqlx::query_as!(
        ChatRow,
        r#"SELECT c.id AS "id!: i64", c.bot_id, c.direction, c.kind, c.sender_name,
                  c.sender_uuid, c.text, c.truncated, c.at, c.sent_by,
                  u.username AS sent_by_name
           FROM chat_messages c LEFT JOIN users u ON u.id = c.sent_by
           WHERE c.bot_id = ? AND c.id < ? ORDER BY c.id DESC LIMIT ?"#,
        bot,
        before,
        fetch
    )
    .fetch_all(pool)
    .await
    .map_err(store_error)?
    .iter()
    .map(listed_from_row)
    .collect()
}

/// A row with its sender's username.
fn listed_from_row(row: &ChatRow) -> Result<ListedChat, StoreError> {
    let record = record_from_row(row)?;
    let corrupt = || StoreError::Corrupt {
        table: TABLE,
        column: "sent_by",
        rowid: row.id,
    };
    // A sender's name that doesn't read back is a corrupt line, and so is a
    // sender the join doesn't find: deleting a user sets the reference to
    // `NULL`, so a dangling one means the database lost a user.
    let sent_by = match (row.sent_by.as_deref(), row.sent_by_name.as_deref()) {
        (Some(_), Some(name)) => Some(Username::try_from(name).map_err(|_| corrupt())?),
        (Some(_), None) => return Err(corrupt()),
        (None, _) => None,
    };
    Ok(ListedChat { record, sent_by })
}

/// Checks every value of a stored row and rebuilds its entry.
fn record_from_row(row: &ChatRow) -> Result<ChatRecord, StoreError> {
    let corrupt = |column| StoreError::Corrupt {
        table: TABLE,
        column,
        rowid: row.id,
    };
    let truncated = match row.truncated {
        0 => false,
        1 => true,
        _ => return Err(corrupt("truncated")),
    };
    let entry = match row.direction.as_str() {
        IN => {
            let kind = ChatKind::from_str(&row.kind).map_err(|_| corrupt("kind"))?;
            let uuid = row
                .sender_uuid
                .as_deref()
                .map(|text| parse_id::<Uuid>(text).ok_or_else(|| corrupt("sender_uuid")))
                .transpose()?;
            let sender = match (row.sender_name.as_deref(), uuid) {
                (Some(name), uuid) => Some((name, uuid)),
                (None, None) => None,
                (None, Some(_)) => return Err(corrupt("sender_uuid")),
            };
            let chat =
                IncomingChat::from_parts(kind, sender, &row.text, truncated).map_err(|error| {
                    match error {
                        IncomingChatError::UnknownKind => corrupt("kind"),
                        IncomingChatError::SenderOnSystemMessage
                        | IncomingChatError::MissingSender => corrupt("sender_name"),
                    }
                })?;
            ChatEntry::Received(chat)
        }
        OUT => {
            if row.sender_name.is_some() || row.sender_uuid.is_some() {
                return Err(corrupt("sender_name"));
            }
            if truncated {
                return Err(corrupt("truncated"));
            }
            // Stored exactly as the mode or the user sent it: a text the
            // constructor would change (untrimmed, say) didn't come from here.
            let message = ChatMessage::try_from(row.text.as_str())
                .ok()
                .filter(|message| message.as_str() == row.text)
                .ok_or_else(|| corrupt("text"))?;
            match row.kind.as_str() {
                MODE => ChatEntry::ModeSent(message),
                USER => ChatEntry::UserSent {
                    message,
                    // The table's CHECK keeps a sender on user lines only.
                    by: row
                        .sent_by
                        .as_deref()
                        .map(|text| parse_id(text).ok_or_else(|| corrupt("sent_by")))
                        .transpose()?,
                },
                _ => return Err(corrupt("kind")),
            }
        }
        _ => return Err(corrupt("direction")),
    };
    Ok(ChatRecord {
        id: ChatMessageId::new(row.id),
        bot: parse_id(&row.bot_id).ok_or_else(|| corrupt("bot_id"))?,
        at: parse_time(&row.at).ok_or_else(|| corrupt("at"))?,
        entry,
    })
}
