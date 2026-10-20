//! The mode routes (Plan.md P11.4, Appendix B; ADR-0020, S12–S19).
//!
//! - **Reading** (`GET /api/v1/modes`) lists the modes the caller may see
//!   (`ViewMode`), newest first: the built-in and shared ones, their own,
//!   every private one for the Owner, and Members' for an Admin. Each is
//!   built for its viewer by [`super::mode_response`]: who owns it only for
//!   Admins and the Owner, what the viewer may do with it, and a chat step's
//!   command arguments only for a viewer who may edit it (S14, S15).
//! - **Creating** (`POST /api/v1/modes`, and `POST
//!   /api/v1/modes/with-commands` after a step-up) takes a
//!   `CreateModeRequest`, whose every rule garde checked (S12). `authorize()`
//!   asks `CreateMode` (a shared mode needs an Admin or the Owner); then the
//!   plain route points a mode whose steps send a command off the allowlist
//!   to the twin (403 `mode_needs_step_up`), after `forbidden`, so only a
//!   caller the twin would take ever sees the pointer (S16). The service
//!   checks it all again in its transaction: the cap (409 `conflict`, S19)
//!   and a taken name (422 on `name`, S13).

use std::net::IpAddr;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fleet_api_types::modes::{CreateModeRequest, MODE_NAME_TAKEN, ModePage, ModeResponse};
use fleet_api_types::{ErrorResponse, FieldError, PageQuery};
use fleet_core::authz::{Permission, ResourceContext};
use fleet_core::id::ModeId;
use fleet_core::mode::ModeName;

use super::{acting, mode_response, page};
use crate::app::modes::{CreateModeError, ListModesError, ModeRoute};
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, SteppedUpUser, ValidJson, ValidQuery};
use crate::http::rate_limit::ClientIp;
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

/// `POST /api/v1/modes`: a new mode of the caller's, whose steps send no
/// command off the allowlist.
#[utoipa::path(
    post,
    path = "/api/v1/modes",
    operation_id = "create_mode",
    tag = "modes",
    request_body = CreateModeRequest,
    responses(
        (status = 201, description = "The new mode, as its creator sees it.", body = ModeResponse),
        (status = 400, description = "A body of the wrong shape: an unknown field or `type`, a value of the wrong type, or a number its type can't hold. The answer never repeats what it read.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "`mode_needs_step_up`: the steps send a command off the allowlist, which only `POST /modes/with-commands` takes, after a step-up; `forbidden`: a shared mode, which needs an Admin or the Owner.", body = ErrorResponse),
        (status = 409, description = "`conflict`: the caller already owns 50 modes.", body = ErrorResponse),
        (status = 422, description = "A field breaks its rule, named at its own path (`name`, `steps[3].action.message`): one error, the earliest step's first.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn create(
    user: AuthUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidJson(request): ValidJson<CreateModeRequest>,
) -> Result<(StatusCode, Json<ModeResponse>), ApiError> {
    creating(&user, &state, ip, &request, ModeRoute::Plain).await
}

/// `POST /api/v1/modes/with-commands`: a new mode of the caller's, after a
/// step-up, whatever commands its steps send.
#[utoipa::path(
    post,
    path = "/api/v1/modes/with-commands",
    operation_id = "create_mode_with_commands",
    tag = "modes",
    request_body = CreateModeRequest,
    responses(
        (status = 201, description = "The new mode, as its creator sees it.", body = ModeResponse),
        (status = 400, description = "A body of the wrong shape: an unknown field or `type`, a value of the wrong type, or a number its type can't hold. The answer never repeats what it read.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or a shared mode, which needs an Admin or the Owner.", body = ErrorResponse),
        (status = 409, description = "`conflict`: the caller already owns 50 modes.", body = ErrorResponse),
        (status = 422, description = "A field breaks its rule, named at its own path (`name`, `steps[3].action.message`): one error, the earliest step's first.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn create_with_commands(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidJson(request): ValidJson<CreateModeRequest>,
) -> Result<(StatusCode, Json<ModeResponse>), ApiError> {
    creating(user.user(), &state, ip, &request, ModeRoute::WithCommands).await
}

/// `request` through `route` for `user` from `ip`: authorized, pointed to
/// the twin when the plain route can't take it, then created.
async fn creating(
    user: &AuthUser,
    state: &AppState,
    ip: IpAddr,
    request: &CreateModeRequest,
    route: ModeRoute,
) -> Result<(StatusCode, Json<ModeResponse>), ApiError> {
    // garde checked every rule already, so a refusal here is a bug.
    let definition = request
        .steps
        .definition()
        .map_err(|_| ApiError::internal("the steps' rules already hold: garde checked them"))?;
    let name = ModeName::try_from(request.name.as_str()).map_err(ApiError::internal)?;
    let visibility = request.visibility.into();
    user.require(
        Permission::CreateMode { visibility },
        &ResourceContext::Global,
    )?;
    // The pointer comes after `forbidden`: only a caller the twin would take
    // ever sees it (S16).
    if !route.takes(state.modes.command_check(&definition)) {
        return Err(ApiError::ModeNeedsStepUp);
    }
    let created = state
        .modes
        .create(&acting(user, ip), route, name, visibility, definition)
        .await
        .map_err(|error| match error {
            CreateModeError::SessionGone => ApiError::Unauthorized,
            CreateModeError::Denied(error) => ApiError::from(error),
            CreateModeError::CapReached => ApiError::Conflict,
            CreateModeError::NameTaken => {
                ApiError::Validation(vec![FieldError::new("name", MODE_NAME_TAKEN)])
            }
            CreateModeError::Store(error) => ApiError::from(error),
            other @ (CreateModeError::WrongRoute { .. }
            | CreateModeError::Mint(_)
            | CreateModeError::Audit(_)) => ApiError::internal(other),
        })?;
    Ok((
        StatusCode::CREATED,
        Json(mode_response(&user.actor(), &created)?),
    ))
}
