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
//! - **Editing** (`PUT /api/v1/modes/{id}`, and `PUT
//!   /api/v1/modes/{id}/with-commands` after a step-up) takes an
//!   `UpdateModeRequest`: the whole mode and the `version` read. A mode the
//!   caller may not see is 404; `authorize()` asks `UpdateMode` with the
//!   mode's visibility, then with the requested one (403), so a switch needs
//!   the right both ways; then the pointer, as for creating (S16). The
//!   service checks it all again in its transaction, in R3's order: an edit
//!   that shows the same answers the mode unchanged (S21), a stale
//!   `version` is 409 `mode_changed` (S18), a taken name 422 on `name`, and
//!   a bot using the mode that the caller couldn't set it on 409
//!   `mode_in_use_by_others` (S17).

use std::net::IpAddr;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use core::num::NonZeroU32;

use fleet_api_types::modes::{
    CreateModeRequest, MODE_NAME_TAKEN, ModePage, ModeResponse, UpdateModeRequest,
};
use fleet_api_types::{ErrorResponse, FieldError, PageQuery};
use fleet_core::authz::{Permission, ResourceContext};
use fleet_core::id::ModeId;
use fleet_core::mode::ModeName;

use super::{acting, mode_response, page};
use crate::app::modes::{CreateModeError, ListModesError, ModeEdit, ModeRoute, UpdateModeError};
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, SteppedUpUser, ValidJson, ValidPath, ValidQuery};
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

/// `PUT /api/v1/modes/{id}`: the mode as it should be, whose steps send no
/// command off the allowlist.
#[utoipa::path(
    put,
    path = "/api/v1/modes/{id}",
    operation_id = "update_mode",
    tag = "modes",
    params(("id" = String, Path, description = "The mode's ID.")),
    request_body = UpdateModeRequest,
    responses(
        (status = 200, description = "The mode as it now is, as the caller sees it: at the next version, or unchanged when the edit shows the same as the stored mode.", body = ModeResponse),
        (status = 400, description = "A body of the wrong shape: an unknown field or `type`, a value of the wrong type, or a number its type can't hold. The answer never repeats what it read.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "`mode_needs_step_up`: the steps send a command off the allowlist, which only `PUT /modes/{id}/with-commands` takes, after a step-up; `forbidden`: the caller may see the mode but not edit it, or not with this visibility. A built-in mode is read-only.", body = ErrorResponse),
        (status = 404, description = "No such mode, or one the caller may not see.", body = ErrorResponse),
        (status = 409, description = "`mode_changed`: the mode changed since the caller read `version`; `mode_in_use_by_others`: a bot uses the mode that the caller couldn't set the edited mode on.", body = ErrorResponse),
        (status = 422, description = "A field breaks its rule, named at its own path (`version`, `name`, `steps[3].action.message`): one error, the earliest step's first.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn update(
    user: AuthUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<ModeId>,
    ValidJson(request): ValidJson<UpdateModeRequest>,
) -> Result<Json<ModeResponse>, ApiError> {
    editing(&user, &state, ip, id, request, ModeRoute::Plain).await
}

/// `PUT /api/v1/modes/{id}/with-commands`: the mode as it should be, after
/// a step-up, whatever commands its steps send.
#[utoipa::path(
    put,
    path = "/api/v1/modes/{id}/with-commands",
    operation_id = "update_mode_with_commands",
    tag = "modes",
    params(("id" = String, Path, description = "The mode's ID.")),
    request_body = UpdateModeRequest,
    responses(
        (status = 200, description = "The mode as it now is, as the caller sees it: at the next version, or unchanged when the edit shows the same as the stored mode.", body = ModeResponse),
        (status = 400, description = "A body of the wrong shape: an unknown field or `type`, a value of the wrong type, or a number its type can't hold. The answer never repeats what it read.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or the caller may see the mode but not edit it, or not with this visibility. A built-in mode is read-only.", body = ErrorResponse),
        (status = 404, description = "No such mode, or one the caller may not see.", body = ErrorResponse),
        (status = 409, description = "`mode_changed`: the mode changed since the caller read `version`; `mode_in_use_by_others`: a bot uses the mode that the caller couldn't set the edited mode on.", body = ErrorResponse),
        (status = 422, description = "A field breaks its rule, named at its own path (`version`, `name`, `steps[3].action.message`): one error, the earliest step's first.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn update_with_commands(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<ModeId>,
    ValidJson(request): ValidJson<UpdateModeRequest>,
) -> Result<Json<ModeResponse>, ApiError> {
    editing(
        user.user(),
        &state,
        ip,
        id,
        request,
        ModeRoute::WithCommands,
    )
    .await
}

/// `request` for the mode `id` through `route` for `user` from `ip`: the
/// mode found and authorized both ways, pointed to the twin when the plain
/// route can't take it, then edited.
async fn editing(
    user: &AuthUser,
    state: &AppState,
    ip: IpAddr,
    id: ModeId,
    request: UpdateModeRequest,
    route: ModeRoute,
) -> Result<Json<ModeResponse>, ApiError> {
    // garde checked every rule already, so a refusal here is a bug.
    let definition = request
        .steps
        .definition()
        .map_err(|_| ApiError::internal("the steps' rules already hold: garde checked them"))?;
    let name = ModeName::try_from(request.name.as_str()).map_err(ApiError::internal)?;
    let version = NonZeroU32::new(request.version)
        .ok_or_else(|| ApiError::internal("the version is at least 1: garde checked it"))?;
    let visibility = request.visibility.into();
    let Some(listed) = state.modes.find(id).await? else {
        return Err(ApiError::NotFound);
    };
    let context = listed.context();
    user.require(
        Permission::UpdateMode {
            visibility: listed.mode.visibility,
        },
        &context,
    )?;
    user.require(Permission::UpdateMode { visibility }, &context)?;
    // The pointer comes after `forbidden`: only a caller the twin would take
    // ever sees it (S16).
    if !route.takes(state.modes.command_check(&definition)) {
        return Err(ApiError::ModeNeedsStepUp);
    }
    let edit = ModeEdit {
        version,
        name,
        visibility,
        definition,
    };
    let edited = state
        .modes
        .update(&acting(user, ip), route, id, edit)
        .await
        .map_err(|error| match error {
            UpdateModeError::SessionGone => ApiError::Unauthorized,
            UpdateModeError::NotFound => ApiError::NotFound,
            UpdateModeError::Denied(error) => ApiError::from(error),
            UpdateModeError::Changed => ApiError::ModeChanged,
            UpdateModeError::NameTaken => {
                ApiError::Validation(vec![FieldError::new("name", MODE_NAME_TAKEN)])
            }
            UpdateModeError::InUseByOthers => ApiError::ModeInUseByOthers,
            UpdateModeError::Store(error) => ApiError::from(error),
            other @ (UpdateModeError::WrongRoute { .. }
            | UpdateModeError::VersionExhausted
            | UpdateModeError::Lost
            | UpdateModeError::Audit(_)) => ApiError::internal(other),
        })?;
    Ok(Json(mode_response(&user.actor(), edited.mode())?))
}
