//! [`ValidJson`]: a request body, read and checked (Plan.md P7.6, ADR-0015,
//! ADR-0016).
//!
//! It reads the body with axum's `Json`, then checks garde's rules. How each
//! failure answers:
//!
//! | Failure | Answer |
//! |---|---|
//! | No `Content-Type: application/json` | 415 `unsupported_media_type` |
//! | Malformed JSON | 400 `bad_request` |
//! | Well-formed JSON of the wrong shape: an unknown, missing or mistyped field | 400 `bad_request` |
//! | A body that can't be read | 400 `bad_request`, or 413 past the body limit |
//! | A garde rule broken | 422 `validation_failed`, one field error per rule |
//!
//! A wrong shape is a client bug, so it's a 400 without details; only the
//! request DTOs' rules give the field errors a client can show. serde's
//! message never reaches a response or a log line, since it can quote the
//! value (`invalid type: integer 123456`): a refusal is one `debug` line
//! with its kind, and the line and column where serde stopped.

use std::error::Error;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Request};
use axum::http::StatusCode;
use garde::Validate;
use serde::de::DeserializeOwned;
use tracing::debug;

use crate::http::error::ApiError;

/// A JSON request body that passed its DTO's garde rules.
#[derive(Debug)]
pub struct ValidJson<T>(pub T);

impl<S, T> FromRequest<S> for ValidJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Validate,
    T::Context: Default,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, ApiError> {
        let Json(value) = Json::<T>::from_request(request, state)
            .await
            .map_err(|rejection| refuse(&rejection))?;
        value.validate()?;
        Ok(Self(value))
    }
}

/// The answer to a body axum couldn't read as `T`, with its one `debug`
/// line; see the [module docs](self).
fn refuse(rejection: &JsonRejection) -> ApiError {
    let (kind, error) = match rejection {
        JsonRejection::MissingJsonContentType(_) => {
            ("no JSON content type", ApiError::UnsupportedMediaType)
        }
        JsonRejection::JsonSyntaxError(_) => ("malformed JSON", ApiError::BadRequest),
        JsonRejection::JsonDataError(_) => ("the wrong shape", ApiError::BadRequest),
        JsonRejection::BytesRejection(bytes) if bytes.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            ("too large", ApiError::PayloadTooLarge)
        }
        _ => ("unreadable", ApiError::BadRequest),
    };
    let position = position(rejection);
    debug!(
        kind,
        line = position.map(|(line, _)| line),
        column = position.map(|(_, column)| column),
        "a request body was refused"
    );
    error
}

/// Where serde stopped reading, if a serde error is in `error`'s chain.
fn position(error: &(dyn Error + 'static)) -> Option<(usize, usize)> {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(serde) = error.downcast_ref::<serde_json::Error>() {
            return Some((serde.line(), serde.column()));
        }
        current = error.source();
    }
    None
}
