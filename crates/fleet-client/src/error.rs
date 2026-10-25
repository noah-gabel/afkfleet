//! The client's errors (ADR-0017): [`ClientError`] for a call, with the
//! server's own refusals as [`ApiFailure`], and [`BuildError`] for
//! [`Client::new`](crate::Client::new).
//!
//! No error's `Display` holds a token, a body or a URL: reqwest's errors are
//! kept as sources without their URL, and the server's message is its fixed
//! text, cleaned and capped by [`BoundedText`].

use core::error::Error;
use core::fmt;
use core::time::Duration;

use fleet_api_types::error::{MESSAGE_MAX_CHARS, REQUEST_ID_MAX_CHARS};
use fleet_api_types::{BoundedText, ErrorCode, FieldError, Open};
use thiserror::Error;
use tokio::task::JoinError;

use crate::store::StoreError;

/// What a transport error carries as its source.
pub(crate) type Cause = Box<dyn Error + Send + Sync>;

/// A failed call.
#[derive(Debug, Error)]
pub enum ClientError {
    /// The server can't be reached: DNS, the connection, a reset, or a 502,
    /// 503 or 504 without the API's error envelope (a reverse proxy whose
    /// server is down).
    #[error("the server can't be reached")]
    Unreachable(#[source] Option<Cause>),
    /// The server's certificate was refused: it isn't trusted, or it isn't
    /// for this host.
    #[error("the server's certificate isn't trusted")]
    Certificate(#[source] Cause),
    /// Connecting or the whole request took too long.
    #[error("the server didn't answer in time")]
    TimedOut(#[source] Option<Cause>),
    /// The response body is larger than
    /// [`MAX_RESPONSE_BYTES`](crate::MAX_RESPONSE_BYTES).
    #[error("the server's answer is larger than 1 MiB")]
    TooLarge,
    /// The response isn't one this client understands.
    #[error("the server's answer (status {status}) isn't one this client understands: {reason}")]
    UnexpectedResponse {
        /// Its HTTP status.
        status: u16,
        /// What's wrong with it.
        reason: Unexpected,
    },
    /// The server refused the request with its error envelope.
    #[error("{0}")]
    Api(ApiFailure),
    /// There's no session: log in. A refused refresh, a revoked session or
    /// an empty store ends one.
    #[error("there's no session: log in")]
    NoSession,
    /// The call needs a pending login or 2FA step, and there's none (or it's
    /// the other kind).
    #[error("no login or 2FA step is pending")]
    NoPendingStep,
    /// `login` while a session is held: log out first.
    #[error("a session is already held: log out first")]
    AlreadyLoggedIn,
    /// The session store failed.
    #[error("the session store failed")]
    Store(#[source] StoreError),
    /// A refresh or a code step ended without an answer: its task panicked,
    /// or the runtime is shutting down. Whatever it did before that stands.
    #[error("the refresh or the code step ended without an answer")]
    Interrupted(#[source] JoinError),
}

impl ClientError {
    /// The server's refusal, if this is one.
    #[must_use]
    pub fn api(&self) -> Option<&ApiFailure> {
        match self {
            Self::Api(failure) => Some(failure),
            _ => None,
        }
    }
}

/// What's wrong with an [`UnexpectedResponse`](ClientError::UnexpectedResponse).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unexpected {
    /// The body isn't JSON.
    NotJson,
    /// An error status without the API's error envelope.
    NoEnvelope,
    /// A redirect, which the API never answers and the client never follows.
    Redirect,
    /// A success status the route never answers.
    Status,
    /// JSON that isn't the route's answer.
    Body,
    /// A login status this version doesn't know.
    UnknownLoginStatus,
    /// A 101 that doesn't answer the live events socket's upgrade: no
    /// upgrade to `websocket`, a `Sec-WebSocket-Accept` that isn't the
    /// key's, or an extension or subprotocol never offered (ADR-0020, U25).
    Handshake,
}

impl fmt::Display for Unexpected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotJson => "its body isn't JSON",
            Self::NoEnvelope => "it isn't the API's error envelope",
            Self::Redirect => "it's a redirect, which the API never answers",
            Self::Status => "the route never answers this status",
            Self::Body => "its body isn't the route's answer",
            Self::UnknownLoginStatus => "it names a login step this version doesn't know",
            Self::Handshake => "its 101 doesn't answer the live events upgrade",
        })
    }
}

/// The server's refusal: its error envelope, with the status and the wait it
/// asks for. Code that branches on [`ApiFailure::code`] keeps a default
/// branch that shows the message and the request ID, since a newer server
/// may send a code this version doesn't know (ADR-0015).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiFailure {
    /// The HTTP status.
    pub status: u16,
    /// The error code.
    pub code: Open<ErrorCode>,
    /// The server's message.
    pub message: BoundedText<MESSAGE_MAX_CHARS>,
    /// The request's ID, for the server's log.
    pub request_id: BoundedText<REQUEST_ID_MAX_CHARS>,
    /// The fields a 422 refused, each with its message.
    pub fields: Vec<FieldError>,
    /// How long to wait before trying again: a 429's or a 503's
    /// `Retry-After`.
    pub retry_after: Option<Duration>,
}

impl ApiFailure {
    /// Whether the code is `code`.
    #[must_use]
    pub fn is(&self, code: ErrorCode) -> bool {
        self.code.known() == Some(code)
    }
}

impl fmt::Display for ApiFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({}, status {}, request {})",
            self.message,
            self.code.as_str(),
            self.status,
            self.request_id
        )
    }
}

/// The HTTP client couldn't be built, e.g. a custom CA reqwest refused.
#[derive(Debug, Error)]
#[error("the HTTP client can't be built")]
pub struct BuildError(#[source] pub(crate) Cause);

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(code: ErrorCode) -> ApiFailure {
        ApiFailure {
            status: 409,
            code: code.into(),
            message: BoundedText::new(code.message()),
            request_id: BoundedText::new("01900000-0000-7000-8000-000000000009"),
            fields: Vec::new(),
            retry_after: None,
        }
    }

    #[test]
    fn a_refusal_shows_its_message_code_status_and_request() {
        let shown = ClientError::Api(failure(ErrorCode::Conflict)).to_string();

        assert_eq!(
            shown,
            format!(
                "{} (conflict, status 409, request 01900000-0000-7000-8000-000000000009)",
                ErrorCode::Conflict.message()
            )
        );
    }

    #[test]
    fn is_compares_the_known_code() {
        let refusal = failure(ErrorCode::Conflict);

        assert!(refusal.is(ErrorCode::Conflict));
        assert!(!refusal.is(ErrorCode::NotFound));
        assert_eq!(ClientError::Api(refusal.clone()).api(), Some(&refusal));
        assert_eq!(ClientError::NoSession.api(), None);
    }

    #[test]
    fn every_other_error_has_a_fixed_text() {
        let shown = [
            ClientError::Unreachable(None),
            ClientError::Certificate("refused".into()),
            ClientError::TimedOut(None),
            ClientError::TooLarge,
            ClientError::UnexpectedResponse {
                status: 302,
                reason: Unexpected::Redirect,
            },
            ClientError::NoSession,
            ClientError::NoPendingStep,
            ClientError::AlreadyLoggedIn,
        ]
        .map(|error| error.to_string());

        assert_eq!(
            shown,
            [
                "the server can't be reached",
                "the server's certificate isn't trusted",
                "the server didn't answer in time",
                "the server's answer is larger than 1 MiB",
                "the server's answer (status 302) isn't one this client understands: it's a redirect, which the API never answers",
                "there's no session: log in",
                "no login or 2FA step is pending",
                "a session is already held: log out first",
            ]
        );
    }
}
