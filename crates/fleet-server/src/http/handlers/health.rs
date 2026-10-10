//! The health checks (Plan.md P6.7, ADR-0015): `GET /health/live` and
//! `GET /health/ready`, without the `/api/v1` prefix.
//!
//! Both answer `200` with an empty body, so they reveal nothing about the
//! server. A failed readiness check answers like any store failure, through
//! `From<StoreError> for ApiError`: a busy database gives `busy` (503,
//! `Retry-After: 1`, logged at `warn`), any other failure `internal` (500,
//! its source chain logged at `error`). The trace layer logs both routes at
//! `debug`, so a probe every few seconds doesn't flood the log.

use axum::extract::State;
use axum::http::StatusCode;

use crate::http::error::ApiError;
use crate::http::router::AppState;

/// The liveness check's path.
pub(crate) const LIVE: &str = "/health/live";

/// The readiness check's path.
pub(crate) const READY: &str = "/health/ready";

/// `GET /health/live`: the process runs and answers requests. It checks
/// nothing else, so it never fails while the server can answer at all.
pub(crate) async fn live() -> StatusCode {
    StatusCode::OK
}

/// `GET /health/ready`: the server can serve requests, which means the
/// database answers.
pub(crate) async fn ready(State(state): State<AppState>) -> Result<StatusCode, ApiError> {
    state.health.ready().await?;
    Ok(StatusCode::OK)
}
