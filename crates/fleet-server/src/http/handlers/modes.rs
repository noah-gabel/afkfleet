//! The mode routes (Plan.md P11.4, Appendix B; ADR-0020, S12–S19).
//!
//! - **Reading** (`GET /api/v1/modes`) lists the modes the caller may see
//!   (`ViewMode`), newest first: the built-in and shared ones, their own,
//!   every private one for the Owner, and Members' for an Admin. Each is
//!   built for its viewer by [`super::mode_response`]: who owns it only for
//!   Admins and the Owner, what the viewer may do with it, and a chat step's
//!   command arguments only for a viewer who may edit it (S14, S15).

use axum::Json;
use axum::extract::State;
use fleet_api_types::modes::ModePage;
use fleet_api_types::{ErrorResponse, PageQuery};
use fleet_core::id::ModeId;

use super::{mode_response, page};
use crate::app::modes::ListModesError;
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, ValidQuery};
use crate::http::router::AppState;

/// `GET /api/v1/modes`: the modes the caller may see, newest first.
#[utoipa::path(
    get,
    path = "/api/v1/modes",
    operation_id = "list_modes",
    tag = "modes",
    params(PageQuery<ModeId>),
    responses(
        (status = 200, description = "One page of the modes the caller may see, newest first: the built-in and shared ones, their own, every Member's private one for an Admin, and every one for the Owner. A chat step's command shows its arguments only to a caller who may edit the mode.", body = ModePage),
        (status = 400, description = "A query that doesn't parse.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 422, description = "A `limit` outside 1 to 100.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn list(
    user: AuthUser,
    State(state): State<AppState>,
    ValidQuery(query): ValidQuery<PageQuery<ModeId>>,
) -> Result<Json<ModePage>, ApiError> {
    let actor = user.actor();
    let listed = state
        .modes
        .list(&actor, page(&query)?)
        .await
        .map_err(|error| match error {
            ListModesError::Store(error) => ApiError::from(error),
            other @ ListModesError::Disagreement => ApiError::internal(other),
        })?;
    let items = listed
        .items
        .iter()
        .map(|listed| mode_response(&actor, listed))
        .collect::<Result<_, _>>()?;
    Ok(Json(ModePage {
        items,
        next: listed.next,
    }))
}
