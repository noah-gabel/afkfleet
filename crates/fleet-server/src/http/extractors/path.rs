//! [`ValidPath`]: a route's path parameter, read (Plan.md P7.6, P7.9,
//! ADR-0016).
//!
//! It reads the parameter with axum's `Path`, as an ID type that accepts
//! only a version 7 UUID. Anything else is 404 `not_found`: no resource can
//! have such an ID, so it answers like an unknown one. axum's own rejection
//! never reaches the client; a refusal is one `debug` line with its kind.

use axum::extract::rejection::PathRejection;
use axum::extract::{FromRequestParts, Path};
use axum::http::request::Parts;
use serde::de::DeserializeOwned;
use tracing::debug;

use crate::http::error::ApiError;

/// A path parameter that read as `T`.
#[derive(Debug)]
pub struct ValidPath<T>(pub T);

impl<S, T> FromRequestParts<S> for ValidPath<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        let Path(value) = Path::<T>::from_request_parts(parts, state)
            .await
            .map_err(|rejection| refuse(&rejection))?;
        Ok(Self(value))
    }
}

/// The answer to a path parameter axum couldn't read as `T`.
fn refuse(rejection: &PathRejection) -> ApiError {
    let kind = match rejection {
        PathRejection::FailedToDeserializePathParams(_) => "not an ID",
        _ => "missing",
    };
    debug!(kind, "a path parameter was refused");
    ApiError::NotFound
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use fleet_core::id::UserId;
    use tower::ServiceExt as _;

    use super::*;

    async fn status(uri: &str) -> StatusCode {
        let app = Router::new().route(
            "/users/{id}",
            get(|ValidPath(id): ValidPath<UserId>| async move { id.to_string() }),
        );
        app.oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn a_v7_id_is_read() {
        assert_eq!(
            status("/users/018bcfe5-6800-7101-8101-010101010101").await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn anything_else_is_not_found() {
        for uri in [
            "/users/not-an-id",
            "/users/550e8400-e29b-41d4-a716-446655440000",
            "/users/018BCFE5-6800-7101-8101-010101010101x",
            "/users/%00",
        ] {
            assert_eq!(status(uri).await, StatusCode::NOT_FOUND, "{uri}");
        }
    }

    #[test]
    fn a_missing_parameter_is_not_found_too() {
        let rejection = PathRejection::from(axum::extract::rejection::MissingPathParams::default());

        assert!(matches!(refuse(&rejection), ApiError::NotFound));
    }
}
