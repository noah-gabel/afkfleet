//! The error envelope (Plan.md P6.5, ADR-0015): every failed request
//! answers with an [`ErrorResponse`],
//! `{ "error": { "code", "message", "request_id", "fields"? } }`.
//!
//! - `code` says what went wrong, as an [`ErrorCode`] read through [`Open`],
//!   so a code a newer server adds still reads back.
//! - `message` is the code's fixed sentence ([`ErrorCode::message`]), never
//!   runtime data. The app shows its own text per code and this one for a
//!   code it doesn't know.
//! - `request_id` names the request in the server's log.
//! - `fields` is there only for `validation_failed`: one [`FieldError`] per
//!   broken rule.
//!
//! Every text is a [`BoundedText`], cleaned and capped when read.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::open::{Open, OpenEnum};
use crate::text::BoundedText;

/// The most characters of [`ErrorBody::message`].
pub const MESSAGE_MAX_CHARS: usize = 256;
/// The most characters of [`ErrorBody::request_id`].
pub const REQUEST_ID_MAX_CHARS: usize = 64;
/// The most characters of [`FieldError::path`].
pub const FIELD_PATH_MAX_CHARS: usize = 128;
/// The most characters of [`FieldError::message`].
pub const FIELD_MESSAGE_MAX_CHARS: usize = 256;

/// What went wrong with a request.
/// This union lists the codes this version knows; a newer server may send
/// others, so code that branches on a code always keeps a default branch that
/// shows the message and the request ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The request couldn't be read (400).
    BadRequest,
    /// No valid credentials came with the request (401).
    Unauthorized,
    /// The caller may see the resource, but not do this (403).
    Forbidden,
    /// The resource doesn't exist, or the caller may not see it (404).
    NotFound,
    /// The path doesn't take this method (405).
    MethodNotAllowed,
    /// The request took too long (408).
    Timeout,
    /// The request conflicts with the current state, such as a name that's
    /// taken (409).
    Conflict,
    /// The request body is larger than the server accepts (413).
    PayloadTooLarge,
    /// The request body isn't JSON (415).
    UnsupportedMediaType,
    /// Fields broke their rules; the body lists them (422).
    ValidationFailed,
    /// Too many requests; `Retry-After` says when to try again (429).
    RateLimited,
    /// The server failed; its log holds the details under the request ID
    /// (500).
    Internal,
    /// The server is busy; `Retry-After` says when to try again (503).
    Busy,
}

impl ErrorCode {
    /// The code's fixed sentence: the same for every error with this code,
    /// and never runtime data.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::BadRequest => "The request couldn't be read.",
            Self::Unauthorized => "Authentication is required.",
            Self::Forbidden => "You don't have permission to do this.",
            Self::NotFound => "The resource doesn't exist.",
            Self::MethodNotAllowed => "This method isn't allowed here.",
            Self::Timeout => "The request took too long.",
            Self::Conflict => "The request conflicts with the current state.",
            Self::PayloadTooLarge => "The request body is too large.",
            Self::UnsupportedMediaType => "The request body must be JSON.",
            Self::ValidationFailed => "Some fields are invalid.",
            Self::RateLimited => "Too many requests. Try again later.",
            Self::Internal => {
                "Something went wrong on the server. Quote the request ID when reporting it."
            }
            Self::Busy => "The server is busy. Try again shortly.",
        }
    }
}

impl OpenEnum for ErrorCode {
    const ALL: &'static [Self] = &[
        Self::BadRequest,
        Self::Unauthorized,
        Self::Forbidden,
        Self::NotFound,
        Self::MethodNotAllowed,
        Self::Timeout,
        Self::Conflict,
        Self::PayloadTooLarge,
        Self::UnsupportedMediaType,
        Self::ValidationFailed,
        Self::RateLimited,
        Self::Internal,
        Self::Busy,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::MethodNotAllowed => "method_not_allowed",
            Self::Timeout => "timeout",
            Self::Conflict => "conflict",
            Self::PayloadTooLarge => "payload_too_large",
            Self::UnsupportedMediaType => "unsupported_media_type",
            Self::ValidationFailed => "validation_failed",
            Self::RateLimited => "rate_limited",
            Self::Internal => "internal",
            Self::Busy => "busy",
        }
    }
}

/// The body of every error response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ErrorResponse {
    /// The error.
    pub error: ErrorBody,
}

/// What went wrong, for the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ErrorBody {
    /// What went wrong.
    #[ts(as = "ErrorCode")]
    pub code: Open<ErrorCode>,
    /// The code's fixed sentence.
    #[ts(type = "string")]
    pub message: BoundedText<MESSAGE_MAX_CHARS>,
    /// The request's ID, which names it in the server's log.
    #[ts(type = "string")]
    pub request_id: BoundedText<REQUEST_ID_MAX_CHARS>,
    /// Every broken rule; only for `validation_failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fields: Option<Vec<FieldError>>,
}

impl ErrorBody {
    /// An error with `code`'s fixed message.
    #[must_use]
    pub fn new(
        code: ErrorCode,
        request_id: BoundedText<REQUEST_ID_MAX_CHARS>,
        fields: Option<Vec<FieldError>>,
    ) -> Self {
        Self {
            code: Open::from(code),
            message: BoundedText::new(code.message()),
            request_id,
            fields,
        }
    }
}

/// A field that broke a rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FieldError {
    /// Where the field is, in garde's notation: `name`, `steps[0].angle`.
    #[ts(type = "string")]
    pub path: BoundedText<FIELD_PATH_MAX_CHARS>,
    /// Which rule it broke. It names the rule, never the value.
    #[ts(type = "string")]
    pub message: BoundedText<FIELD_MESSAGE_MAX_CHARS>,
}

impl FieldError {
    /// A field error at `path`.
    #[must_use]
    pub fn new(path: &str, message: &str) -> Self {
        Self {
            path: BoundedText::new(path),
            message: BoundedText::new(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use serde_json::json;

    use super::*;

    /// Each code's position, through an exhaustive match: a new variant
    /// doesn't compile until it's added here, and then the test below fails
    /// until it's in `ALL` too.
    fn index(code: ErrorCode) -> usize {
        match code {
            ErrorCode::BadRequest => 0,
            ErrorCode::Unauthorized => 1,
            ErrorCode::Forbidden => 2,
            ErrorCode::NotFound => 3,
            ErrorCode::MethodNotAllowed => 4,
            ErrorCode::Timeout => 5,
            ErrorCode::Conflict => 6,
            ErrorCode::PayloadTooLarge => 7,
            ErrorCode::UnsupportedMediaType => 8,
            ErrorCode::ValidationFailed => 9,
            ErrorCode::RateLimited => 10,
            ErrorCode::Internal => 11,
            ErrorCode::Busy => 12,
        }
    }

    const CODES: usize = 13;

    #[test]
    fn all_lists_every_code_exactly_once() {
        let mut seen = [0_u8; CODES];
        for code in ErrorCode::ALL {
            seen[index(*code)] += 1;
        }

        assert_eq!(ErrorCode::ALL.len(), CODES);
        assert_eq!(seen, [1; CODES]);
    }

    #[test]
    fn every_wire_name_is_serdes_name() {
        for code in ErrorCode::ALL {
            let serde_name = serde_json::to_value(code).unwrap();

            assert_eq!(serde_name, json!(code.as_str()), "{code:?}");
        }
    }

    #[rstest]
    #[case(ErrorCode::BadRequest, "bad_request", "The request couldn't be read.")]
    #[case(ErrorCode::Unauthorized, "unauthorized", "Authentication is required.")]
    #[case(
        ErrorCode::Forbidden,
        "forbidden",
        "You don't have permission to do this."
    )]
    #[case(ErrorCode::NotFound, "not_found", "The resource doesn't exist.")]
    #[case(
        ErrorCode::MethodNotAllowed,
        "method_not_allowed",
        "This method isn't allowed here."
    )]
    #[case(ErrorCode::Timeout, "timeout", "The request took too long.")]
    #[case(
        ErrorCode::Conflict,
        "conflict",
        "The request conflicts with the current state."
    )]
    #[case(
        ErrorCode::PayloadTooLarge,
        "payload_too_large",
        "The request body is too large."
    )]
    #[case(
        ErrorCode::UnsupportedMediaType,
        "unsupported_media_type",
        "The request body must be JSON."
    )]
    #[case(
        ErrorCode::ValidationFailed,
        "validation_failed",
        "Some fields are invalid."
    )]
    #[case(
        ErrorCode::RateLimited,
        "rate_limited",
        "Too many requests. Try again later."
    )]
    #[case(
        ErrorCode::Internal,
        "internal",
        "Something went wrong on the server. Quote the request ID when reporting it."
    )]
    #[case(ErrorCode::Busy, "busy", "The server is busy. Try again shortly.")]
    fn every_code_has_its_name_and_fixed_message(
        #[case] code: ErrorCode,
        #[case] name: &str,
        #[case] message: &str,
    ) {
        assert_eq!(code.as_str(), name);
        assert_eq!(code.message(), message);
    }

    #[test]
    fn every_message_fits_its_cap_uncut() {
        for code in ErrorCode::ALL {
            let text = BoundedText::<MESSAGE_MAX_CHARS>::new(code.message());

            assert_eq!(text.as_str(), code.message(), "{code:?}");
        }
    }

    fn request_id() -> BoundedText<REQUEST_ID_MAX_CHARS> {
        BoundedText::new("test-request-id")
    }

    fn validation_error() -> ErrorResponse {
        ErrorResponse {
            error: ErrorBody::new(
                ErrorCode::ValidationFailed,
                request_id(),
                Some(vec![FieldError::new("name", "length is lower than 3")]),
            ),
        }
    }

    #[test]
    fn a_body_takes_its_codes_message() {
        let body = ErrorBody::new(ErrorCode::Busy, request_id(), None);

        assert_eq!(body.code.known(), Some(ErrorCode::Busy));
        assert_eq!(body.message.as_str(), ErrorCode::Busy.message());
    }

    #[test]
    fn the_wire_shape_is_the_envelope() {
        assert_eq!(
            serde_json::to_value(validation_error()).unwrap(),
            json!({
                "error": {
                    "code": "validation_failed",
                    "message": "Some fields are invalid.",
                    "request_id": "test-request-id",
                    "fields": [{ "path": "name", "message": "length is lower than 3" }],
                }
            })
        );
    }

    #[test]
    fn fields_are_left_out_when_there_are_none() {
        let response = ErrorResponse {
            error: ErrorBody::new(ErrorCode::NotFound, request_id(), None),
        };

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({
                "error": {
                    "code": "not_found",
                    "message": "The resource doesn't exist.",
                    "request_id": "test-request-id",
                }
            })
        );
    }

    #[rstest]
    #[case::with_fields(validation_error())]
    #[case::without_fields(ErrorResponse {
        error: ErrorBody::new(ErrorCode::Internal, request_id(), None),
    })]
    fn every_response_round_trips(#[case] response: ErrorResponse) {
        let json = serde_json::to_string(&response).unwrap();

        assert_eq!(
            serde_json::from_str::<ErrorResponse>(&json).unwrap(),
            response
        );
    }

    #[test]
    fn an_extra_field_is_tolerated() {
        let json = json!({
            "error": {
                "code": "conflict",
                "message": "The request conflicts with the current state.",
                "request_id": "test-request-id",
                "hint": "added by a newer server",
                "fields": [{ "path": "name", "message": "taken", "rule": "unique" }],
            },
            "trace": "added by a newer server",
        });

        let response: ErrorResponse = serde_json::from_value(json).unwrap();

        assert_eq!(response.error.code.known(), Some(ErrorCode::Conflict));
        assert_eq!(
            response.error.fields,
            Some(vec![FieldError::new("name", "taken")])
        );
    }

    #[test]
    fn an_unknown_code_reads_back_with_its_message_and_request_id() {
        let json = json!({
            "error": {
                "code": "teapot",
                "message": "I'm a teapot.",
                "request_id": "test-request-id",
            }
        });

        let response: ErrorResponse = serde_json::from_value(json).unwrap();

        assert_eq!(response.error.code.known(), None);
        assert_eq!(response.error.code.as_str(), "teapot");
        assert_eq!(response.error.message.as_str(), "I'm a teapot.");
        assert_eq!(response.error.request_id.as_str(), "test-request-id");
    }

    #[test]
    fn every_text_is_cleaned_and_capped_when_read() {
        let json = json!({
            "error": {
                "code": "internal",
                "message": "x".repeat(10_000),
                "request_id": "id\nfake log line",
                "fields": [{ "path": "p".repeat(1_000), "message": "a\u{7}b" }],
            }
        });

        let response: ErrorResponse = serde_json::from_value(json).unwrap();

        let error = response.error;
        assert_eq!(error.message.as_str(), "x".repeat(MESSAGE_MAX_CHARS));
        assert_eq!(error.request_id.as_str(), "id | fake log line");
        let field = error.fields.unwrap().remove(0);
        assert_eq!(field.path.as_str(), "p".repeat(FIELD_PATH_MAX_CHARS));
        assert_eq!(field.message.as_str(), "ab");
    }
}
