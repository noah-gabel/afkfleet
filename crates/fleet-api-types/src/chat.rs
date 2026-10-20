//! A bot's chat (Plan.md P11.3; ADR-0020, S4–S8): a line a user sends a bot,
//! and what the server answers.
//!
//! - **`POST /bots/{id}/chat`** takes a [`ChatSendRequest`]: plain chat, or a
//!   command on the server's allowlist (Control). A command off the
//!   allowlist is never sent here: a Manage holder gets 403
//!   `command_needs_step_up` and sends it again through `/command`.
//! - **`POST /bots/{id}/command`** takes a [`ChatCommandRequest`]: any
//!   command, after a step-up; Control for an allowlisted one, Manage for any
//!   other (S8). A line that isn't a command is 422.
//! - **Both answer** [`ChatSendResponse`]: 200 when the bot's agent said the
//!   bot sent it, 202 when no answer came in time (S5). Either way the line
//!   is in the bot's chat history as what the user asked it to send, never as
//!   proof that it reached the Minecraft server (S7).
//! - **`GET /bots/{id}/chat`** takes a [`ChatPageQuery`] and answers a
//!   [`ChatPage`] of [`ChatLine`]s, newest first, each built for its viewer
//!   (S6, S9, U3): a command's arguments only for Manage holders, the
//!   sender's name only for viewers who see the account's owner, and `mine`
//!   on the viewer's own lines. A line's text is cleaned, a line break
//!   becoming ` | `, so no server can make one message look like two (S11).

use fleet_core::chat::{ChatKind, ChatMessage, ChatMessageError, ChatSender, IncomingChat};
use garde::Validate;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::IntoParams;
use utoipa::openapi::path::{Parameter, ParameterBuilder, ParameterIn};
use utoipa::openapi::schema::{ObjectBuilder, Type};
use utoipa::openapi::{RefOr, Required, Schema};
use utoipa::{PartialSchema as _, ToSchema};

use crate::ids::{ChatRequestId, ID_CHARS, id_schema};
use crate::int::{SafeInt, integer_schema};
use crate::open::{Open, OpenEnum};
use crate::page::{PAGE_DEFAULT, PAGE_MAX};
use crate::text::BoundedText;
use crate::time::Timestamp;
use crate::users::UserLabel;

/// The most UTF-16 code units of a chat line, as Minecraft measures it: an
/// emoji counts 2 (`ChatMessage::MAX_LEN`).
pub const CHAT_MESSAGE_MAX_UNITS: usize = ChatMessage::MAX_LEN;

/// `POST /bots/{id}/chat`: a line for the bot to send, plain chat or a
/// command on the server's allowlist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Validate, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatSendRequest {
    /// The line, one line of at most [`CHAT_MESSAGE_MAX_UNITS`] UTF-16 code
    /// units once trimmed, without control characters, `§` or text-direction
    /// marks. A line starting with `/` is a command.
    #[garde(custom(valid_message))]
    #[schema(schema_with = BoundedText::<CHAT_MESSAGE_MAX_UNITS>::schema)]
    pub message: String,
}

/// `POST /bots/{id}/command`: a command for the bot to send, after a
/// step-up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Validate, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatCommandRequest {
    /// The command: a line as [`ChatSendRequest::message`] takes it, which
    /// starts with `/` once trimmed.
    #[garde(custom(valid_command))]
    #[schema(schema_with = BoundedText::<CHAT_MESSAGE_MAX_UNITS>::schema)]
    pub message: String,
}

/// The garde rule of a chat line: a valid `ChatMessage`. The message names
/// the rule, never the text.
fn valid_message<C>(text: &str, _context: &C) -> garde::Result {
    line(text).map(drop)
}

/// The garde rule of a command: a valid line that starts with `/` once
/// trimmed; the line's own rule is named first.
fn valid_command<C>(text: &str, _context: &C) -> garde::Result {
    if line(text)?.is_command() {
        Ok(())
    } else {
        Err(garde::Error::new(NOT_A_COMMAND))
    }
}

/// `text` as a chat line, or the rule it breaks.
fn line(text: &str) -> Result<ChatMessage, garde::Error> {
    ChatMessage::try_from(text).map_err(|error| garde::Error::new(message_rule(error)))
}

/// The field message of a line that isn't a command, on `/command`.
const NOT_A_COMMAND: &str = "must be a command, starting with /";

/// The field message of each way a chat line can be invalid.
const fn message_rule(error: ChatMessageError) -> &'static str {
    match error {
        ChatMessageError::Empty => "must not be empty",
        ChatMessageError::ForbiddenChar { .. } => {
            "must be one line, without control characters, formatting codes or text-direction marks"
        }
        ChatMessageError::TooLong { .. } => {
            "must be at most 256 characters, an emoji counting as 2"
        }
    }
}

/// What came of a line the server sent to its bot's agent.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on one always keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChatSendOutcome {
    /// The bot's agent said the bot sent it (200).
    Sent,
    /// No answer came in time: the line went to the bot's agent, and may yet
    /// be sent or refused (202).
    Pending,
}

impl OpenEnum for ChatSendOutcome {
    const ALL: &'static [Self] = &[Self::Sent, Self::Pending];

    fn as_str(self) -> &'static str {
        match self {
            Self::Sent => "sent",
            Self::Pending => "pending",
        }
    }
}

/// The answer to a line that went to its bot's agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct ChatSendResponse {
    /// The chat request's ID, which the bot's agent repeats in its answer,
    /// and live events will carry.
    #[ts(type = "string")]
    #[schema(schema_with = id_schema)]
    pub request_id: ChatRequestId,
    /// The line's ID in the bot's chat history.
    pub message_id: SafeInt,
    /// What came of it.
    #[ts(as = "ChatSendOutcome")]
    #[schema(value_type = ChatSendOutcome)]
    pub outcome: Open<ChatSendOutcome>,
}

/// The most characters of a chat line's text as the API sends it (S11): a
/// received message keeps its line breaks, up to
/// `IncomingChat::MAX_TEXT_LEN` characters, and each becomes the three
/// characters ` | ` on the way, so three times that holds every stored text
/// uncut.
pub const CHAT_TEXT_MAX_CHARS: usize = 3 * IncomingChat::MAX_TEXT_LEN;

/// The most characters of a Minecraft sender's name (`ChatSender`'s cap).
pub const CHAT_SENDER_MAX_CHARS: usize = ChatSender::MAX_NAME_LEN;

/// `GET /bots/{id}/chat?before=<id>&limit=<n>`: which page of a bot's chat
/// to answer, newest first. Like [`crate::PageQuery`], with the chat's own
/// cursor, a row ID, which crosses as a [`SafeInt`] (ADR-0015). A parameter
/// that isn't one of these two, or one given twice, is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct ChatPageQuery {
    /// Only lines older than this one: the `next` of the page before. At
    /// least 1, as every row ID is.
    #[garde(inner(custom(|cursor: &SafeInt, &(): &()| positive_cursor(*cursor))))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<SafeInt>,
    /// How many lines at most: 1 to [`PAGE_MAX`].
    #[garde(range(min = 1, max = PAGE_MAX))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

impl ChatPageQuery {
    /// The page size: `limit`, or [`PAGE_DEFAULT`].
    #[must_use]
    pub fn limit_or_default(&self) -> u32 {
        self.limit.unwrap_or(PAGE_DEFAULT)
    }
}

/// The garde rule of a cursor: at least 1, in garde's own words for a range.
fn positive_cursor(cursor: SafeInt) -> garde::Result {
    if cursor.get() < 1 {
        Err(garde::Error::new("lower than 1"))
    } else {
        Ok(())
    }
}

impl IntoParams for ChatPageQuery {
    fn into_params(_parameter_in_provider: impl Fn() -> Option<ParameterIn>) -> Vec<Parameter> {
        vec![
            ParameterBuilder::new()
                .name("before")
                .parameter_in(ParameterIn::Query)
                .required(Required::False)
                .description(Some(
                    "Only lines older than this ID: the `next` of the page before.",
                ))
                .schema(Some(cursor_schema()))
                .build(),
            ParameterBuilder::new()
                .name("limit")
                .parameter_in(ParameterIn::Query)
                .required(Required::False)
                .description(Some("How many lines at most; 50 by default."))
                .schema(Some(integer_schema::<1, PAGE_MAX>()))
                .build(),
        ]
    }
}

/// A cursor's schema: an integer from 1 to [`SafeInt::MAX`].
fn cursor_schema() -> RefOr<Schema> {
    ObjectBuilder::new()
        .schema_type(Type::Integer)
        .minimum(Some(1))
        .maximum(Some(SafeInt::MAX))
        .into()
}

/// Whether a bot received a line or sent it.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on one always keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChatDirection {
    /// The bot received it from its Minecraft server.
    In,
    /// The bot sent it: its mode, or a user through the API.
    Out,
}

impl OpenEnum for ChatDirection {
    const ALL: &'static [Self] = &[Self::In, Self::Out];

    fn as_str(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
        }
    }
}

/// What kind of line it is: what a bot received, as its Minecraft server
/// sent it, or who had it sent.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on one always keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChatLineKind {
    /// A player's chat message (received).
    Chat,
    /// A player's `/me` (received).
    Emote,
    /// A whisper to the bot (received).
    Whisper,
    /// A player's `/say` (received).
    Announcement,
    /// A system message, which has no sender (received).
    System,
    /// What the bot's mode sent.
    Mode,
    /// What a user had the bot send through the API: what was asked, not
    /// proof that it reached the Minecraft server (S7).
    User,
}

impl OpenEnum for ChatLineKind {
    const ALL: &'static [Self] = &[
        Self::Chat,
        Self::Emote,
        Self::Whisper,
        Self::Announcement,
        Self::System,
        Self::Mode,
        Self::User,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Emote => "emote",
            Self::Whisper => "whisper",
            Self::Announcement => "announcement",
            Self::System => "system",
            Self::Mode => "mode",
            Self::User => "user",
        }
    }
}

impl From<ChatKind> for ChatLineKind {
    fn from(kind: ChatKind) -> Self {
        match kind {
            ChatKind::Chat => Self::Chat,
            ChatKind::Emote => Self::Emote,
            ChatKind::Whisper => Self::Whisper,
            ChatKind::Announcement => Self::Announcement,
            ChatKind::System => Self::System,
        }
    }
}

/// Who sent a received line, as its Minecraft server claims it: never
/// verified (ADR-0011).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct ChatLineSender {
    /// The display name, cleaned.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<CHAT_SENDER_MAX_CHARS>::schema)]
    pub name: BoundedText<CHAT_SENDER_MAX_CHARS>,
    /// The player's UUID, hyphenated, when the server gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = BoundedText::<ID_CHARS>::schema, nullable = false)]
    pub uuid: Option<BoundedText<ID_CHARS>>,
}

/// One line of a bot's chat, as its viewer may see it (S6, S9, U3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct ChatLine {
    /// The line's ID, the history's cursor: a newer line has a greater one.
    /// The order is the server's commit order, which a user's line can lead
    /// by a moment (ADR-0020, U5).
    pub id: SafeInt,
    /// When the server received it, or a user's line was sent.
    #[ts(type = "string")]
    #[schema(schema_with = Timestamp::schema)]
    pub at: Timestamp,
    /// Received or sent.
    #[ts(as = "ChatDirection")]
    #[schema(value_type = ChatDirection)]
    pub direction: Open<ChatDirection>,
    /// What kind of line it is.
    #[ts(as = "ChatLineKind")]
    #[schema(value_type = ChatLineKind)]
    pub kind: Open<ChatLineKind>,
    /// The text, cleaned, a line break as ` | ` (S11). Below Manage, a
    /// command a mode or a user sent is only its `/` and name.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<CHAT_TEXT_MAX_CHARS>::schema)]
    pub text: BoundedText<CHAT_TEXT_MAX_CHARS>,
    /// Whether a received text was cut where it was stored.
    pub truncated: bool,
    /// Whether a command's arguments were left out for this viewer (S9).
    pub arguments_hidden: bool,
    /// Who sent a received player message, as the Minecraft server claims.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub sender: Option<ChatLineSender>,
    /// Who had a user's line sent: for viewers who see the account's owner,
    /// and only while that user exists (U3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub sent_by: Option<UserLabel>,
    /// Whether the viewer had this line sent (U3).
    pub mine: bool,
}

/// `GET /bots/{id}/chat`: one page of a bot's chat, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct ChatPage {
    /// The lines.
    pub items: Vec<ChatLine>,
    /// The `before` of the next page, when one follows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub next: Option<SafeInt>,
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use serde_json::json;

    use super::*;

    const REQUEST: &str = "018bcfe5-6800-7101-8101-010101010103";

    fn problems<T: Validate<Context = ()>>(request: &T) -> Vec<(String, String)> {
        request.validate().map_or_else(
            |report| {
                report
                    .iter()
                    .map(|(path, error)| (path.to_string(), error.message().to_owned()))
                    .collect()
            },
            |()| Vec::new(),
        )
    }

    fn chat(message: &str) -> Vec<(String, String)> {
        problems(&ChatSendRequest {
            message: message.to_owned(),
        })
    }

    fn command(message: &str) -> Vec<(String, String)> {
        problems(&ChatCommandRequest {
            message: message.to_owned(),
        })
    }

    fn on_message(message: &str) -> Vec<(String, String)> {
        vec![("message".to_owned(), message.to_owned())]
    }

    #[test]
    fn the_limit_is_the_core_types() {
        assert_eq!(CHAT_MESSAGE_MAX_UNITS, 256);
    }

    #[rstest]
    #[case::plain("hello there")]
    #[case::trimmed("  hello  ")]
    #[case::command("/spawn")]
    #[case::unlisted_command("/op AfkBot1")]
    #[case::at_the_limit(&"x".repeat(256))]
    #[case::emoji_at_the_limit(&"😀".repeat(128))]
    fn a_valid_line_is_valid(#[case] message: &str) {
        assert_eq!(chat(message), Vec::<(String, String)>::new());
    }

    #[rstest]
    #[case::empty("", "must not be empty")]
    #[case::only_spaces("   ", "must not be empty")]
    #[case::a_line_break(
        "one\ntwo",
        "must be one line, without control characters, formatting codes or text-direction marks"
    )]
    #[case::a_section_sign(
        "§cred",
        "must be one line, without control characters, formatting codes or text-direction marks"
    )]
    #[case::a_direction_mark(
        "abc\u{202E}def",
        "must be one line, without control characters, formatting codes or text-direction marks"
    )]
    #[case::too_long(&"x".repeat(257), "must be at most 256 characters, an emoji counting as 2")]
    #[case::emoji_too_long(
        &"😀".repeat(129),
        "must be at most 256 characters, an emoji counting as 2"
    )]
    fn an_invalid_line_names_the_rule_on_its_field(#[case] message: &str, #[case] rule: &str) {
        assert_eq!(chat(message), on_message(rule));
    }

    #[rstest]
    #[case::plain("hello")]
    #[case::slash_later("hello /spawn")]
    fn a_command_request_refuses_a_line_that_isnt_a_command(#[case] message: &str) {
        assert_eq!(
            command(message),
            on_message("must be a command, starting with /")
        );
    }

    #[rstest]
    #[case::empty("", "must not be empty")]
    #[case::a_line_break(
        "/one\ntwo",
        "must be one line, without control characters, formatting codes or text-direction marks"
    )]
    fn a_command_request_names_the_lines_own_rule_first(#[case] message: &str, #[case] rule: &str) {
        assert_eq!(command(message), on_message(rule));
    }

    /// Every line `/chat` would point to `/command` passes its rule, so the
    /// app's resend never ends on a 422 (S8).
    #[rstest]
    #[case::unlisted("/op AfkBot1")]
    #[case::trimmed("  /op AfkBot1")]
    #[case::a_lone_slash("/")]
    #[case::a_space_after_the_slash("/ spawn")]
    #[case::double_slash("//wand")]
    #[case::namespaced("/minecraft:op AfkBot1")]
    #[case::at_the_limit(&format!("/{}", "x".repeat(255)))]
    fn every_command_chat_takes_passes_the_command_rule(#[case] message: &str) {
        assert_eq!(chat(message), Vec::<(String, String)>::new());
        assert_eq!(command(message), Vec::<(String, String)>::new());
    }

    /// Every text a client reads is cleaned on the way, which drops `§`
    /// with the character after it: each rule's message must come through
    /// unchanged (U4).
    #[test]
    fn every_rules_message_survives_the_cleaning_of_what_clients_read() {
        let messages = [
            message_rule(ChatMessageError::Empty),
            message_rule(ChatMessageError::ForbiddenChar { index: 0 }),
            message_rule(ChatMessageError::TooLong { units: 257 }),
            NOT_A_COMMAND,
        ];

        for message in messages {
            let read = BoundedText::<{ crate::error::FIELD_MESSAGE_MAX_CHARS }>::new(message);
            assert_eq!(read.as_str(), message);
        }
    }

    #[test]
    fn a_rule_never_echoes_the_line() {
        let secret = "/login secret_label\nmore";

        let found = [chat(secret), command(secret), command("secret_label")];

        assert!(
            found.iter().all(|problems| problems.len() == 1),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .flatten()
                .all(|(_, message)| !message.contains("secret_label")),
            "{found:?}"
        );
    }

    #[test]
    fn the_requests_refuse_unknown_fields() {
        let chat = serde_json::from_value::<ChatSendRequest>(
            json!({ "message": "hi", "bot_id": REQUEST }),
        );
        let command = serde_json::from_value::<ChatCommandRequest>(
            json!({ "message": "/spawn", "route": "chat" }),
        );

        assert!(chat.is_err());
        assert!(command.is_err());
    }

    #[test]
    fn the_requests_round_trip() {
        let chat = ChatSendRequest {
            message: "hello".to_owned(),
        };
        let command = ChatCommandRequest {
            message: "/spawn".to_owned(),
        };

        let chat_json = serde_json::to_value(&chat).unwrap();
        let command_json = serde_json::to_value(&command).unwrap();

        assert_eq!(chat_json, json!({ "message": "hello" }));
        assert_eq!(command_json, json!({ "message": "/spawn" }));
        assert_eq!(
            serde_json::from_value::<ChatSendRequest>(chat_json).unwrap(),
            chat
        );
        assert_eq!(
            serde_json::from_value::<ChatCommandRequest>(command_json).unwrap(),
            command
        );
    }

    #[rstest]
    #[case(ChatSendOutcome::Sent, "sent")]
    #[case(ChatSendOutcome::Pending, "pending")]
    fn every_outcome_has_its_name(#[case] outcome: ChatSendOutcome, #[case] name: &str) {
        assert_eq!(outcome.as_str(), name);
        assert_eq!(serde_json::to_value(outcome).unwrap(), json!(name));
    }

    // --- The history (P11.3; ADR-0020, S6, S9, S11, U3) ---

    fn query(before: Option<i64>, limit: Option<u32>) -> ChatPageQuery {
        ChatPageQuery {
            before: before.map(|id| SafeInt::try_new(id).unwrap()),
            limit,
        }
    }

    #[rstest]
    #[case::the_first_page(None, None)]
    #[case::the_smallest_cursor(Some(1), Some(1))]
    #[case::the_largest(Some(SafeInt::MAX), Some(100))]
    fn a_page_query_takes_a_cursor_from_1_and_a_limit_from_1_to_100(
        #[case] before: Option<i64>,
        #[case] limit: Option<u32>,
    ) {
        assert_eq!(problems(&query(before, limit)), Vec::new());
    }

    #[rstest]
    #[case::a_zero_cursor(Some(0), None, "before", "lower than 1")]
    #[case::a_negative_cursor(Some(-1), None, "before", "lower than 1")]
    #[case::a_zero_limit(None, Some(0), "limit", "lower than 1")]
    #[case::a_limit_over_100(None, Some(101), "limit", "greater than 100")]
    fn a_value_out_of_range_names_its_rule_on_its_field(
        #[case] before: Option<i64>,
        #[case] limit: Option<u32>,
        #[case] field: &str,
        #[case] rule: &str,
    ) {
        assert_eq!(
            problems(&query(before, limit)),
            vec![(field.to_owned(), rule.to_owned())]
        );
    }

    #[test]
    fn a_page_query_leaves_out_what_it_lacks_and_refuses_anything_else() {
        let empty: ChatPageQuery = serde_json::from_value(json!({})).unwrap();
        let unknown = serde_json::from_value::<ChatPageQuery>(json!({ "before": 4, "after": 2 }));

        assert_eq!(empty, query(None, None));
        assert_eq!(empty.limit_or_default(), 50);
        assert_eq!(query(None, Some(7)).limit_or_default(), 7);
        assert!(unknown.is_err());
    }

    #[rstest]
    #[case(ChatKind::Chat, ChatLineKind::Chat, "chat")]
    #[case(ChatKind::Emote, ChatLineKind::Emote, "emote")]
    #[case(ChatKind::Whisper, ChatLineKind::Whisper, "whisper")]
    #[case(ChatKind::Announcement, ChatLineKind::Announcement, "announcement")]
    #[case(ChatKind::System, ChatLineKind::System, "system")]
    fn every_received_kind_has_its_line_kind(
        #[case] received: ChatKind,
        #[case] kind: ChatLineKind,
        #[case] name: &str,
    ) {
        assert_eq!(ChatLineKind::from(received), kind);
        assert_eq!(kind.as_str(), name);
        assert_eq!(received.as_str(), name, "the stored name is the API's");
    }

    #[test]
    fn every_kind_and_direction_has_its_name() {
        assert_eq!(ChatLineKind::Mode.as_str(), "mode");
        assert_eq!(ChatLineKind::User.as_str(), "user");
        assert_eq!(ChatLineKind::ALL.len(), 7);
        assert_eq!(ChatDirection::In.as_str(), "in");
        assert_eq!(ChatDirection::Out.as_str(), "out");
        for kind in ChatLineKind::ALL {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(kind.as_str()));
        }
    }

    fn a_line() -> ChatLine {
        ChatLine {
            id: SafeInt::try_new(41).unwrap(),
            at: serde_json::from_value(json!("2027-01-15T08:00:00.000Z")).unwrap(),
            direction: ChatDirection::Out.into(),
            kind: ChatLineKind::User.into(),
            text: BoundedText::new("/login"),
            truncated: false,
            arguments_hidden: true,
            sender: None,
            sent_by: None,
            mine: true,
        }
    }

    #[test]
    fn a_line_leaves_out_what_it_lacks_and_round_trips() {
        let line = a_line();

        let value = serde_json::to_value(&line).unwrap();

        assert_eq!(
            value,
            json!({
                "id": 41,
                "at": "2027-01-15T08:00:00.000Z",
                "direction": "out",
                "kind": "user",
                "text": "/login",
                "truncated": false,
                "arguments_hidden": true,
                "mine": true,
            })
        );
        assert_eq!(serde_json::from_value::<ChatLine>(value).unwrap(), line);
    }

    #[test]
    fn a_line_reads_a_kind_and_direction_it_doesnt_know() {
        let mut value = serde_json::to_value(a_line()).unwrap();
        value["kind"] = json!("shout");
        value["direction"] = json!("sideways");

        let read: ChatLine = serde_json::from_value(value).unwrap();

        assert_eq!(read.kind.known(), None);
        assert_eq!(read.kind.as_str(), "shout");
        assert_eq!(read.direction.as_str(), "sideways");
    }

    /// A stored received text keeps up to 1024 characters with its line
    /// breaks; each becomes ` | ` on the way out, and none is ever cut
    /// (S11). The worst case is all line breaks.
    #[rstest]
    #[case::only_line_breaks(&"\n".repeat(1024))]
    #[case::every_other(&"a\n".repeat(512))]
    #[case::a_few(&format!("{}\n{}", "x".repeat(600), "y".repeat(423)))]
    #[case::emoji_and_line_breaks(&"😀\n".repeat(512))]
    #[case::longer_than_stored(&"\nz".repeat(700))]
    fn a_stored_text_crosses_uncut_a_line_break_as_a_separator(#[case] raw: &str) {
        let stored = IncomingChat::system(raw);

        let sent = BoundedText::<CHAT_TEXT_MAX_CHARS>::new(stored.text());

        assert_eq!(sent.as_str(), stored.text().replace('\n', " | "));
        assert!(!sent.as_str().contains('\n'));
    }

    #[test]
    fn the_cap_holds_a_text_of_only_line_breaks_exactly() {
        let stored = IncomingChat::system(&"\n".repeat(IncomingChat::MAX_TEXT_LEN));

        let sent = BoundedText::<CHAT_TEXT_MAX_CHARS>::new(stored.text());

        assert_eq!(sent.as_str().chars().count(), CHAT_TEXT_MAX_CHARS);
        assert_eq!(CHAT_TEXT_MAX_CHARS, 3072);
        assert_eq!(CHAT_SENDER_MAX_CHARS, 64);
    }

    #[test]
    fn the_response_round_trips_and_reads_an_outcome_it_doesnt_know() {
        let response = ChatSendResponse {
            request_id: REQUEST.parse().unwrap(),
            message_id: SafeInt::try_new(42).unwrap(),
            outcome: ChatSendOutcome::Pending.into(),
        };

        let value = serde_json::to_value(&response).unwrap();
        let newer: ChatSendResponse = serde_json::from_value(
            json!({ "request_id": REQUEST, "message_id": 42, "outcome": "queued", "extra": 1 }),
        )
        .unwrap();

        assert_eq!(
            value,
            json!({ "request_id": REQUEST, "message_id": 42, "outcome": "pending" })
        );
        assert_eq!(
            serde_json::from_value::<ChatSendResponse>(value).unwrap(),
            response
        );
        assert_eq!(newer.outcome.known(), None);
        assert_eq!(newer.outcome.as_str(), "queued");
    }
}
