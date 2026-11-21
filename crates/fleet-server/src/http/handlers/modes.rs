//! The mode routes (Plan.md P11.4, Appendix B; ADR-0020, S12–S19).
//!
//! - **Reading** (`GET /api/v1/modes`) lists the modes the caller may see
//!   (`ViewMode`), newest first: the built-in and shared ones, their own,
//!   every private one for the Owner, and Members' for an Admin. Each is
//!   built for its viewer by [`super::mode_response`]: who owns it only for
//!   Admins and the Owner, what the viewer may do with it, and a chat step's
//!   command arguments only for a viewer who may edit it (S14, S15).
//! - **Creating** (`POST /api/v1/modes`, and its twin `POST
//!   /api/v1/modes/with-commands`) takes a
//!   `CreateModeRequest`, whose every rule garde checked (S12). `authorize()`
//!   asks `CreateMode` (a shared mode needs an Admin or the Owner); then the
//!   plain route points a mode whose steps send a command off the allowlist
//!   to the twin (403 `mode_needs_step_up`, named from when the twin needed
//!   a step-up; ADR-0028), after `forbidden`, so only a caller the twin
//!   would take ever sees the pointer (S16). The service
//!   checks it all again in its transaction: the cap (409 `conflict`, S19)
//!   and a taken name (422 on `name`, S13).
//! - **Editing** (`PUT /api/v1/modes/{id}`, and its twin `PUT
//!   /api/v1/modes/{id}/with-commands`) takes an
//!   `UpdateModeRequest`: the whole mode and the `version` read. A mode the
//!   caller may not see is 404; `authorize()` asks `UpdateMode` with the
//!   mode's visibility, then with the requested one (403), so a switch needs
//!   the right both ways; then the pointer, as for creating (S16). The
//!   service checks it all again in its transaction, in R3's order: an edit
//!   that shows the same answers the mode unchanged (S21), a stale
//!   `version` is 409 `mode_changed` (S18), a taken name 422 on `name`, and
//!   a bot using the mode that the caller couldn't set it on 409
//!   `mode_in_use_by_others` (S17).
//! - **A scripted mode** (ADR-0036, D13 to D15): its body is `script` beside
//!   an empty `steps` (422 at `steps` otherwise). `authorize()` asks with
//!   the kind the request names before anything reads the script, so a
//!   Member's is a 403 that no validator and no compiler ever saw. Its
//!   fleet-core rules are 422s at their fields under `script`; then the
//!   service compiles it in `spawn_blocking`, never running a hook, and a
//!   text that isn't a program is a 422 at `script.source` with Rhai's
//!   message, its line and its column; a compiler that fails is the generic
//!   500 with its request ID. An edit that makes the mode a script's while a
//!   bot that uses it is on an agent that doesn't run it is 409
//!   `agent_too_old`.
//! - **Deleting** (`DELETE /api/v1/modes/{id}`): a mode
//!   the caller may not see is 404, one they may not delete 403 (a built-in
//!   one for everyone); the service checks again in its transaction and
//!   refuses a mode any bot uses (409 `mode_in_use`, S17). 204.

use std::net::IpAddr;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use core::num::NonZeroU32;

use fleet_api_types::modes::{
    CreateModeRequest, MODE_NAME_TAKEN, MODE_SCRIPT_HAS_NO_STEPS, ModePage, ModeResponse,
    ModeSteps, UpdateModeRequest,
};
use fleet_api_types::script::{self, ModeScriptRequest};
use fleet_api_types::{ErrorResponse, FieldError, PageQuery};
use fleet_core::authz::{ModeKind, Permission, ResourceContext};
use fleet_core::id::ModeId;
use fleet_core::mode::{ModeDefinition, ModeName};

use super::{acting, mode_response, page};
use crate::app::modes::{
    CreateModeError, DeleteModeError, ListModesError, ModeEdit, ModeRoute, ScriptCheckError,
    UpdateModeError,
};
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, ValidJson, ValidPath, ValidQuery};
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
        .map(|listed| {
            let check = state.modes.command_check(&listed.mode.definition);
            mode_response(&actor, listed, check)
        })
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
        (status = 403, description = "`mode_needs_step_up`: the steps send a command off the allowlist, which only `POST /modes/with-commands` takes; `forbidden`: a shared mode or a scripted one, each of which needs an Admin or the Owner.", body = ErrorResponse),
        (status = 409, description = "`conflict`: the caller already owns 50 modes.", body = ErrorResponse),
        (status = 422, description = "A field breaks its rule, named at its own path (`name`, `steps[3].action.message`, `script.messages[1].text`): one error, the earliest first. A script that isn't a program is `script.source` with Rhai's message, its `line` and its `column`.", body = ErrorResponse),
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

/// `POST /api/v1/modes/with-commands`: a new mode of the caller's,
/// whatever commands its steps send.
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
        (status = 403, description = "A shared mode or a scripted one, each of which needs an Admin or the Owner.", body = ErrorResponse),
        (status = 409, description = "`conflict`: the caller already owns 50 modes.", body = ErrorResponse),
        (status = 422, description = "A field breaks its rule, named at its own path (`name`, `steps[3].action.message`, `script.messages[1].text`): one error, the earliest first. A script that isn't a program is `script.source` with Rhai's message, its `line` and its `column`.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn create_with_commands(
    user: AuthUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidJson(request): ValidJson<CreateModeRequest>,
) -> Result<(StatusCode, Json<ModeResponse>), ApiError> {
    creating(&user, &state, ip, &request, ModeRoute::WithCommands).await
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
    let name = ModeName::try_from(request.name.as_str()).map_err(ApiError::internal)?;
    let visibility = request.visibility.into();
    // Before anything reads a script: a Member's never reaches a validator
    // or the compiler (ADR-0036, D15).
    user.require(
        Permission::CreateMode {
            visibility,
            kind: requested_kind(request.script.as_ref()),
        },
        &ResourceContext::Global,
    )?;
    let definition = requested(&request.steps, request.script.as_ref())?;
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
            CreateModeError::Script(error) => not_a_program(error),
            other @ (CreateModeError::WrongRoute { .. }
            | CreateModeError::Mint(_)
            | CreateModeError::Audit(_)) => ApiError::internal(other),
        })?;
    Ok((
        StatusCode::CREATED,
        Json(mode_response(
            &user.actor(),
            &created,
            state.modes.command_check(&created.mode.definition),
        )?),
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
        (status = 403, description = "`mode_needs_step_up`: the steps send a command off the allowlist, which only `PUT /modes/{id}/with-commands` takes; `forbidden`: the caller may see the mode but not edit it, or not with this visibility, or it's a scripted mode before or after and the caller isn't an Admin or the Owner. A built-in mode is read-only.", body = ErrorResponse),
        (status = 404, description = "No such mode, or one the caller may not see.", body = ErrorResponse),
        (status = 409, description = "`mode_changed`: the mode changed since the caller read `version`; `mode_in_use_by_others`: a bot uses the mode that the caller couldn't set the edited mode on; `agent_too_old`: the edit makes it a scripted mode, and a bot that uses it is on an agent that doesn't run its script API level.", body = ErrorResponse),
        (status = 422, description = "A field breaks its rule, named at its own path (`version`, `name`, `steps[3].action.message`, `script.leash`): one error, the earliest first. A script that isn't a program is `script.source` with Rhai's message, its `line` and its `column`.", body = ErrorResponse),
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

/// `PUT /api/v1/modes/{id}/with-commands`: the mode as it should be,
/// whatever commands its steps send.
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
        (status = 403, description = "The caller may see the mode but not edit it, or not with this visibility, or it's a scripted mode before or after and the caller isn't an Admin or the Owner. A built-in mode is read-only.", body = ErrorResponse),
        (status = 404, description = "No such mode, or one the caller may not see.", body = ErrorResponse),
        (status = 409, description = "`mode_changed`: the mode changed since the caller read `version`; `mode_in_use_by_others`: a bot uses the mode that the caller couldn't set the edited mode on; `agent_too_old`: the edit makes it a scripted mode, and a bot that uses it is on an agent that doesn't run its script API level.", body = ErrorResponse),
        (status = 422, description = "A field breaks its rule, named at its own path (`version`, `name`, `steps[3].action.message`, `script.leash`): one error, the earliest first. A script that isn't a program is `script.source` with Rhai's message, its `line` and its `column`.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn update_with_commands(
    user: AuthUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<ModeId>,
    ValidJson(request): ValidJson<UpdateModeRequest>,
) -> Result<Json<ModeResponse>, ApiError> {
    editing(&user, &state, ip, id, request, ModeRoute::WithCommands).await
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
            kind: ModeKind::of(&listed.mode.definition),
        },
        &context,
    )?;
    // Before anything reads a script, as for creating (ADR-0036, D15).
    user.require(
        Permission::UpdateMode {
            visibility,
            kind: requested_kind(request.script.as_ref()),
        },
        &context,
    )?;
    let definition = requested(&request.steps, request.script.as_ref())?;
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
            UpdateModeError::AgentTooOld => ApiError::AgentTooOld,
            UpdateModeError::Store(error) => ApiError::from(error),
            UpdateModeError::Script(error) => not_a_program(error),
            other @ (UpdateModeError::WrongRoute { .. }
            | UpdateModeError::VersionExhausted
            | UpdateModeError::Lost
            | UpdateModeError::Audit(_)) => ApiError::internal(other),
        })?;
    let check = state.modes.command_check(&edited.mode().mode.definition);
    Ok(Json(mode_response(&user.actor(), edited.mode(), check)?))
}

/// What a request's body is made of, read before the script itself: a
/// request with `script` asks to write a scripted mode.
const fn requested_kind(script: Option<&ModeScriptRequest>) -> ModeKind {
    match script {
        Some(_) => ModeKind::Script,
        None => ModeKind::Steps,
    }
}

/// The mode a request's body makes: its steps, or its script beside empty
/// steps. garde checked the steps' rules already, so a refusal there is a
/// bug; a script's rules are each a 422 at its field under `script`, with
/// the line and the column of a place in its text.
fn requested(
    steps: &ModeSteps,
    script: Option<&ModeScriptRequest>,
) -> Result<ModeDefinition, ApiError> {
    let Some(script) = script else {
        return steps
            .definition()
            .map_err(|_| ApiError::internal("the steps' rules already hold: garde checked them"));
    };
    if !steps.0.is_empty() {
        return Err(ApiError::Validation(vec![FieldError::new(
            "steps",
            MODE_SCRIPT_HAS_NO_STEPS,
        )]));
    }
    script
        .script()
        .map(ModeDefinition::scripted)
        .map_err(|error| ApiError::Validation(vec![FieldError::from(error)]))
}

/// A script that isn't a program, as the caller's own mistake: 422 at
/// `script.source` with Rhai's message, its line and its column. A compiler
/// that failed is an internal fault: the generic 500 with its request ID.
fn not_a_program(error: ScriptCheckError) -> ApiError {
    match error {
        ScriptCheckError::NotAProgram(error) => {
            ApiError::Validation(vec![script::not_a_program(&error)])
        }
        other @ ScriptCheckError::CompilerFailed(_) => ApiError::internal(other),
    }
}

/// `DELETE /api/v1/modes/{id}`: deletes a mode no bot uses.
#[utoipa::path(
    delete,
    path = "/api/v1/modes/{id}",
    operation_id = "delete_mode",
    tag = "modes",
    params(("id" = String, Path, description = "The mode's ID.")),
    responses(
        (status = 204, description = "The mode is deleted."),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "The caller may see the mode but not delete it. A built-in mode is read-only.", body = ErrorResponse),
        (status = 404, description = "No such mode, or one the caller may not see.", body = ErrorResponse),
        (status = 409, description = "`mode_in_use`: a bot uses the mode; switch every bot to another mode first.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn delete(
    user: AuthUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<ModeId>,
) -> Result<StatusCode, ApiError> {
    let Some(listed) = state.modes.find(id).await? else {
        return Err(ApiError::NotFound);
    };
    user.require(Permission::DeleteMode, &listed.context())?;
    state
        .modes
        .delete(&acting(&user, ip), id)
        .await
        .map_err(|error| match error {
            DeleteModeError::SessionGone => ApiError::Unauthorized,
            DeleteModeError::NotFound => ApiError::NotFound,
            DeleteModeError::Denied(error) => ApiError::from(error),
            DeleteModeError::InUse => ApiError::ModeInUse,
            DeleteModeError::Store(error) => ApiError::from(error),
            other @ (DeleteModeError::Lost | DeleteModeError::Audit(_)) => {
                ApiError::internal(other)
            }
        })?;
    Ok(StatusCode::NO_CONTENT)
}
