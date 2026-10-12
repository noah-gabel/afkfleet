//! [`ValidQuery`]: a request's query string, read and checked (Plan.md P7.6,
//! P7.9, ADR-0016).
//!
//! It reads the query with axum's `Query`, then checks garde's rules, like
//! [`super::ValidJson`] does for a body:
//!
//! | Failure | Answer |
//! |---|---|
//! | A query that doesn't parse: an unknown or repeated parameter, a value of the wrong type (a `before` that isn't a version 7 UUID, a `limit` that isn't a number) | 400 `bad_request` |
//! | A garde rule broken (a `limit` outside 1 to 100) | 422 `validation_failed`, one field error per rule |
//!
//! serde's message never reaches a response or a log line, since it can
//! quote the value: a refusal is one `debug` line.

use axum::extract::{FromRequestParts, Query};
use axum::http::request::Parts;
use garde::Validate;
use serde::de::DeserializeOwned;
use tracing::debug;

use crate::http::error::ApiError;

/// A query string that read as `T` and passed its garde rules.
#[derive(Debug)]
pub struct ValidQuery<T>(pub T);

impl<S, T> FromRequestParts<S> for ValidQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Validate,
    T::Context: Default,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        let Query(value) = Query::<T>::from_request_parts(parts, state)
            .await
            .map_err(|_| {
                debug!("a query string was refused");
                ApiError::BadRequest
            })?;
        value.validate()?;
        Ok(Self(value))
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use fleet_api_types::PageQuery;
    use fleet_core::id::UserId;
    use tower::ServiceExt as _;

    use super::*;

    async fn status(uri: &str) -> StatusCode {
        let app = Router::new().route(
            "/users",
            get(
                |ValidQuery(query): ValidQuery<PageQuery<UserId>>| async move {
                    format!("{:?} {:?}", query.before, query.limit)
                },
            ),
        );
        app.oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn a_valid_query_is_read() {
        for uri in [
            "/users",
            "/users?limit=1",
            "/users?limit=100",
            "/users?before=018bcfe5-6800-7101-8101-010101010101&limit=20",
        ] {
            assert_eq!(status(uri).await, StatusCode::OK, "{uri}");
        }
    }

    #[tokio::test]
    async fn a_query_that_doesnt_parse_is_a_bad_request() {
        for uri in [
            "/users?page=2",
            "/users?limit=ten",
            "/users?limit=-1",
            "/users?before=not-an-id",
            "/users?before=550e8400-e29b-41d4-a716-446655440000",
            "/users?limit=1&limit=2",
        ] {
            assert_eq!(status(uri).await, StatusCode::BAD_REQUEST, "{uri}");
        }
    }

    #[tokio::test]
    async fn a_limit_outside_its_range_breaks_its_rule() {
        for uri in ["/users?limit=0", "/users?limit=101"] {
            assert_eq!(status(uri).await, StatusCode::UNPROCESSABLE_ENTITY, "{uri}");
        }
    }
}
