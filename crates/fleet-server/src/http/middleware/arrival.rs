//! The arrival layer: each request's own instant, taken right outside the
//! request timeout (Plan.md P11.3; ADR-0020, PR #60's review, U6).
//!
//! A chat line's wait for its agent's answer ends before the request
//! timeout would, counted from the same moment the timeout counts from: the
//! request's arrival, never the handler's start, which comes only after the
//! session's lookup and the body. So the timeout's 408 can't fire after a
//! line was sent.
//!
//! The layer sets an [`Arrival`] on every request that doesn't carry one
//! yet. Nothing on the wire can set an extension, and no layer outside it
//! sets this one, so in the server every request gets its own; only a test
//! hands the router a request that already carries one, which is kept. A
//! handler takes it as an extractor, which fails closed: a request that
//! didn't pass the stack is the internal error.

use axum::extract::{FromRequestParts, Request};
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::Response;
use tokio::time::Instant;

use crate::http::error::ApiError;

/// When a request arrived, on tokio's clock, as the request timeout counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arrival(Instant);

impl Arrival {
    /// A request that arrived at `at`.
    #[must_use]
    pub const fn new(at: Instant) -> Self {
        Self(at)
    }

    /// The instant the request arrived.
    #[must_use]
    pub const fn instant(self) -> Instant {
        self.0
    }
}

/// A handler asked for a request's arrival on a request that didn't pass
/// the middleware stack. A wiring bug, answered as an internal error.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("the request has no arrival: it didn't pass the middleware stack")]
pub struct MissingArrival;

/// The middleware; see the [module docs](self).
pub(super) async fn set_arrival(mut request: Request, next: Next) -> Response {
    if request.extensions().get::<Arrival>().is_none() {
        request.extensions_mut().insert(Arrival(Instant::now()));
    }
    next.run(request).await
}

impl<S: Send + Sync> FromRequestParts<S> for Arrival {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, ApiError>> + Send {
        core::future::ready(
            parts
                .extensions
                .get::<Self>()
                .copied()
                .ok_or_else(|| ApiError::internal(MissingArrival)),
        )
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Request as HttpRequest;

    use super::*;

    fn parts(arrival: Option<Arrival>) -> Parts {
        let mut request = HttpRequest::builder().uri("/").body(()).unwrap();
        if let Some(arrival) = arrival {
            request.extensions_mut().insert(arrival);
        }
        request.into_parts().0
    }

    #[tokio::test(start_paused = true)]
    async fn a_handler_gets_the_arrival_the_stack_set() {
        let arrived = Arrival::new(Instant::now());
        tokio::time::advance(core::time::Duration::from_secs(1)).await;

        let got = Arrival::from_request_parts(&mut parts(Some(arrived)), &()).await;

        assert_eq!(got.unwrap(), arrived);
    }

    #[tokio::test]
    async fn a_request_without_an_arrival_is_the_internal_error() {
        let got = Arrival::from_request_parts(&mut parts(None), &()).await;

        assert!(matches!(got, Err(ApiError::Internal(_))), "{got:?}");
    }
}
