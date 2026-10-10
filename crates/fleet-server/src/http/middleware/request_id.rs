//! The request-ID layer: every request gets an ID the server mints, never
//! one a client chose (Plan.md P6.6, ADR-0015).
//!
//! The layer removes every `x-request-id` the client sent, mints a version 7
//! [`RequestId`] from the [`Clock`] and [`SecureRandom`] ports, and sets it
//! on the request (for [`render_errors`](crate::http::error::render_errors)
//! and the trace layer's span) and on the response, replacing any value a
//! handler set. A client therefore can't pick the ID that is logged and
//! echoed back: no fake or colliding IDs, no text of its own in the log.
//!
//! If no ID can be minted (the OS's randomness failed, or the clock is
//! outside a version 7 ID's range), the request is refused with the internal
//! error, whose body says `"unknown"`, and the cause is logged at `error`:
//! a human must act. The inner layers and the handler never run.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use fleet_core::id::RequestId;
use fleet_core::system::{Clock, MintError, SecureRandom, mint};
use tracing::error;

use crate::http::error::{ApiError, REQUEST_ID_HEADER, chain, render_without_id};

/// Where the request-ID layer mints each request's ID: the server's clock and
/// its secure random source.
#[derive(Debug, Clone)]
pub struct RequestIds {
    /// The ID's creation time.
    clock: Arc<dyn Clock>,
    /// The ID's random bytes.
    random: Arc<dyn SecureRandom>,
}

impl RequestIds {
    /// Mints from `clock` and `random`: the server wires its `SystemClock`
    /// and `OsRandom`, tests a `ManualClock` and a `SeededRandom`.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, random: Arc<dyn SecureRandom>) -> Self {
        Self { clock, random }
    }

    /// A fresh ID and its header value.
    fn mint(&self) -> Result<(RequestId, HeaderValue), NoRequestId> {
        let id: RequestId = mint(self.clock.now(), self.random.as_ref())?;
        let value = HeaderValue::try_from(id.to_string()).map_err(|_| NoRequestId::Header)?;
        Ok((id, value))
    }
}

/// Why a request got no ID.
#[derive(Debug, thiserror::Error)]
enum NoRequestId {
    /// Minting failed.
    #[error(transparent)]
    Mint(#[from] MintError),
    /// The ID isn't a valid header value, which a hyphenated UUID always is.
    #[error("the request ID isn't a valid header value")]
    Header,
}

/// The middleware; see the [module docs](self).
pub(super) async fn set_request_id(
    State(ids): State<RequestIds>,
    mut request: Request,
    next: Next,
) -> Response {
    request.headers_mut().remove(REQUEST_ID_HEADER);
    let (id, value) = match ids.mint() {
        Ok(minted) => minted,
        Err(failure) => {
            error!(
                error = %chain(&failure),
                "a request was refused: no request ID could be minted"
            );
            return render_without_id(ApiError::internal(failure));
        }
    };
    request
        .headers_mut()
        .insert(REQUEST_ID_HEADER, value.clone());
    request.extensions_mut().insert(id);
    let mut response = next.run(request).await;
    response.headers_mut().insert(REQUEST_ID_HEADER, value);
    response
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use fleet_core::system::RandomError;
    use fleet_testkit::system::{ManualClock, SeededRandom};

    use super::*;

    fn ids(clock: ManualClock, random: SeededRandom) -> RequestIds {
        RequestIds::new(Arc::new(clock), Arc::new(random))
    }

    #[test]
    fn the_header_value_is_the_ids_canonical_form() {
        let ids = ids(ManualClock::new(DateTime::UNIX_EPOCH), SeededRandom::new(1));

        let (id, value) = ids.mint().unwrap();

        assert_eq!(value.to_str().unwrap(), id.to_string());
    }

    #[test]
    fn the_same_clock_and_seed_mint_the_same_id() {
        let at = DateTime::from_timestamp_millis(1_700_000_000_000).unwrap();
        let first = ids(ManualClock::new(at), SeededRandom::new(9));
        let second = ids(ManualClock::new(at), SeededRandom::new(9));

        assert_eq!(first.mint().unwrap().0, second.mint().unwrap().0);
    }

    #[test]
    fn a_failing_random_source_mints_nothing() {
        let random = SeededRandom::new(1);
        random.fail_next(RandomError::Unavailable);
        let ids = ids(ManualClock::new(DateTime::UNIX_EPOCH), random);

        let failure = ids.mint().unwrap_err();

        assert!(
            matches!(failure, NoRequestId::Mint(MintError::Random(_))),
            "{failure:?}"
        );
    }

    #[test]
    fn a_time_before_1970_mints_nothing() {
        let before = DateTime::from_timestamp_millis(-1).unwrap();
        let ids = ids(ManualClock::new(before), SeededRandom::new(1));

        let failure = ids.mint().unwrap_err();

        assert!(
            matches!(failure, NoRequestId::Mint(MintError::Id(_))),
            "{failure:?}"
        );
    }
}
