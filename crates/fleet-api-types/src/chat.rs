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

use fleet_core::chat::{ChatMessage, ChatMessageError};
use garde::Validate;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::{PartialSchema as _, ToSchema};

use crate::ids::{ChatRequestId, id_schema};
use crate::int::SafeInt;
use crate::open::{Open, OpenEnum};
use crate::text::BoundedText;

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
