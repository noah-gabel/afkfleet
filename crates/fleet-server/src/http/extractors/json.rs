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

/// Where serde stopped reading, if its error is in `error`'s chain. axum
/// hands it over inside `serde_path_to_error`'s, whose own `source` skips
/// it, and bare only for what follows a whole value.
fn position(error: &(dyn Error + 'static)) -> Option<(usize, usize)> {
    let mut current = Some(error);
    while let Some(error) = current {
        let serde = error
            .downcast_ref::<serde_path_to_error::Error<serde_json::Error>>()
            .map(serde_path_to_error::Error::inner)
            .or_else(|| error.downcast_ref::<serde_json::Error>());
        if let Some(serde) = serde {
            return Some((serde.line(), serde.column()));
        }
        current = error.source();
    }
    None
}

#[cfg(test)]
mod tests {
    use std::io;

    use axum::Router;
    use axum::body::{Body, Bytes};
    use axum::extract::DefaultBodyLimit;
    use axum::http::header::CONTENT_TYPE;
    use axum::routing::post;
    use futures_util::stream;
    use serde::Deserialize;
    use serde_json::Value;
    use tower::ServiceExt as _;

    use super::super::test_logs::capture;
    use super::*;

    /// A body with one rule: `n` is a digit.
    #[derive(Debug, Deserialize, Validate)]
    #[serde(deny_unknown_fields)]
    struct Digit {
        #[garde(range(max = 9))]
        n: u8,
    }

    /// A refusal's line: its kind, and the line and column where serde
    /// stopped.
    type Refusal = (String, Option<u64>, Option<u64>);

    const JSON: Option<&str> = Some("application/json");

    /// The status `body` gets from a route that takes a [`Digit`] of at
    /// most 32 bytes, and its refusal's line, a `debug` one, if it left one.
    async fn answer(content_type: Option<&str>, body: Body) -> (StatusCode, Option<Refusal>) {
        let (logs, _guard) = capture();
        let app = Router::new()
            .route(
                "/",
                post(|ValidJson(digit): ValidJson<Digit>| async move { digit.n.to_string() }),
            )
            .layer(DefaultBodyLimit::max(32));
        let mut request = axum::http::Request::post("/");
        if let Some(content_type) = content_type {
            request = request.header(CONTENT_TYPE, content_type);
        }

        let response = app.oneshot(request.body(body).unwrap()).await.unwrap();

        let lines = logs.lines_with("a request body was refused").unwrap();
        assert!(lines.len() <= 1, "{}", logs.text());
        let refusal = lines.first().map(|line| {
            assert_eq!(line["level"], "DEBUG", "{line}");
            (
                line["kind"].as_str().unwrap().to_owned(),
                line.get("line").and_then(Value::as_u64),
                line.get("column").and_then(Value::as_u64),
            )
        });
        (response.status(), refusal)
    }

    fn refusal(kind: &str, line: Option<u64>, column: Option<u64>) -> Refusal {
        (kind.to_owned(), line, column)
    }

    #[tokio::test]
    async fn a_body_that_keeps_its_rules_is_read_and_leaves_no_line() {
        let answered = answer(JSON, Body::from(r#"{"n":3}"#)).await;

        assert_eq!(answered, (StatusCode::OK, None));
    }

    #[tokio::test]
    async fn a_broken_rule_is_a_422_and_not_a_refused_body() {
        let answered = answer(JSON, Body::from(r#"{"n":10}"#)).await;

        assert_eq!(answered, (StatusCode::UNPROCESSABLE_ENTITY, None));
    }

    #[tokio::test]
    async fn a_body_without_a_json_content_type_is_a_415() {
        let answered = answer(None, Body::from(r#"{"n":3}"#)).await;

        assert_eq!(
            answered,
            (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                Some(refusal("no JSON content type", None, None))
            )
        );
    }

    /// The body ends on its second line, after the colon.
    #[tokio::test]
    async fn malformed_json_is_a_400_whose_line_says_where_serde_stopped() {
        let answered = answer(JSON, Body::from("{\n  \"n\":")).await;

        assert_eq!(
            answered,
            (
                StatusCode::BAD_REQUEST,
                Some(refusal("malformed JSON", Some(2), Some(6)))
            )
        );
    }

    /// A string where a number belongs, on the body's second line, where
    /// serde stops behind it; an unknown field is the same kind, and serde
    /// stops behind its name.
    #[tokio::test]
    async fn the_wrong_shape_is_a_400_whose_line_says_where_serde_stopped() {
        let mistyped = answer(JSON, Body::from("{\n  \"n\": \"x\"\n}")).await;
        let unknown = answer(JSON, Body::from(r#"{"n":3,"m":4}"#)).await;

        assert_eq!(
            mistyped,
            (
                StatusCode::BAD_REQUEST,
                Some(refusal("the wrong shape", Some(2), Some(10)))
            )
        );
        assert_eq!(
            unknown,
            (
                StatusCode::BAD_REQUEST,
                Some(refusal("the wrong shape", Some(1), Some(10)))
            )
        );
    }

    /// axum hands this one over as serde's own error, not wrapped.
    #[tokio::test]
    async fn anything_after_the_value_is_malformed_json_with_its_place() {
        let answered = answer(JSON, Body::from(r#"{"n":3} x"#)).await;

        assert_eq!(
            answered,
            (
                StatusCode::BAD_REQUEST,
                Some(refusal("malformed JSON", Some(1), Some(9)))
            )
        );
    }

    #[tokio::test]
    async fn a_body_past_the_limit_is_a_413() {
        let body = format!(r#"{{"n":{}}}"#, "0".repeat(40));

        let answered = answer(JSON, Body::from(body)).await;

        assert_eq!(
            answered,
            (
                StatusCode::PAYLOAD_TOO_LARGE,
                Some(refusal("too large", None, None))
            )
        );
    }

    /// A body whose stream fails is no too large one: 400, as anything else
    /// that can't be read.
    #[tokio::test]
    async fn a_body_that_cant_be_read_is_a_400() {
        let cut_off = stream::once(async { Err::<Bytes, _>(io::Error::other("cut off")) });

        let answered = answer(JSON, Body::from_stream(cut_off)).await;

        assert_eq!(
            answered,
            (
                StatusCode::BAD_REQUEST,
                Some(refusal("unreadable", None, None))
            )
        );
    }
}
