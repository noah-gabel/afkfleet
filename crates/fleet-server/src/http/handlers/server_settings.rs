//! The servers' settings routes (Plan.md P11.9, Appendix B; ADR-0020, U54,
//! S74–S78): Admins and the Owner read them, and after a step-up write them.
//!
//! - **Reading** (`GET /api/v1/server-settings`): `authorize()` asks
//!   `ViewServerSettings` first (403 for a Member), then one page, newest
//!   first.
//! - **Creating** (`POST /api/v1/server-settings`, after a step-up): a
//!   `CreateServerSettingsRequest`, whose every rule garde checked; the
//!   server is keyed (S74) and the texts built by fleet-core.
//!   `ConfigureServerSettings` first (403), then the service checks it all
//!   again in its transaction: the cap (409 `conflict`, S78) and a server
//!   that has settings already (422 on `server`).
//! - **Editing** (`PUT /api/v1/server-settings/{id}`, after a step-up): an
//!   `UpdateServerSettingsRequest`, the whole settings and the `version`
//!   read. `ConfigureServerSettings` first (403), so a Member's answer never
//!   depends on whether the row exists (S75); then a missing row is 404, an
//!   edit that changes nothing answers the row as it is, a stale `version`
//!   is 409 `server_settings_changed`, and another row's server 422 on
//!   `server`.
//! - **Deleting** (`DELETE /api/v1/server-settings/{id}`, after a step-up):
//!   `ConfigureServerSettings` first, then 404 for a missing row. 204.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use core::num::NonZeroU32;

use fleet_api_types::server_settings::{
    ConflictTextResponse, CreateServerSettingsRequest, SERVER_TAKEN, ServerSettingsPage,
    ServerSettingsResponse, UpdateServerSettingsRequest,
};
use fleet_api_types::{BoundedText, ErrorResponse, FieldError, PageQuery};
use fleet_core::authz::{Permission, ResourceContext};
use fleet_core::disconnect::ConflictTexts;
use fleet_core::id::ServerSettingsId;
use fleet_core::value::{ServerAddress, ServerKey};

use super::{acting, page, timestamp};
use crate::app::server_settings::{
    CreateServerSettingsError, DeleteServerSettingsError, ServerSettingsEdit,
    UpdateServerSettingsError,
};
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, SteppedUpUser, ValidJson, ValidPath, ValidQuery};
use crate::http::rate_limit::ClientIp;
use crate::http::router::AppState;
use crate::ports::server_settings::StoredServerSettings;

/// `GET /api/v1/server-settings`: every server's settings, newest first.
#[utoipa::path(
    get,
    path = "/api/v1/server-settings",
    operation_id = "list_server_settings",
    tag = "server-settings",
    params(PageQuery<ServerSettingsId>),
    responses(
        (status = 200, description = "One page of the servers' settings, newest first.", body = ServerSettingsPage),
        (status = 400, description = "A query that doesn't parse.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Only Admins and the Owner read the servers' settings.", body = ErrorResponse),
        (status = 422, description = "A `limit` outside 1 to 100.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn list(
    user: AuthUser,
    State(state): State<AppState>,
    ValidQuery(query): ValidQuery<PageQuery<ServerSettingsId>>,
) -> Result<Json<ServerSettingsPage>, ApiError> {
    user.require(Permission::ViewServerSettings, &ResourceContext::Global)?;
    let listed = state.server_settings.list(page(&query)?).await?;
    let items = listed
        .items
        .iter()
        .map(settings_response)
        .collect::<Result<_, _>>()?;
    Ok(Json(ServerSettingsPage {
        items,
        next: listed.next,
    }))
}

/// `POST /api/v1/server-settings`: a server's new settings, after a
/// step-up.
#[utoipa::path(
    post,
    path = "/api/v1/server-settings",
    operation_id = "create_server_settings",
    tag = "server-settings",
    request_body = CreateServerSettingsRequest,
    responses(
        (status = 201, description = "The new settings, their server as its key.", body = ServerSettingsResponse),
        (status = 400, description = "A body of the wrong shape: an unknown field, or a value of the wrong type. The answer never repeats what it read.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or the caller isn't an Admin or the Owner.", body = ErrorResponse),
        (status = 409, description = "`conflict`: 100 servers have settings already.", body = ErrorResponse),
        (status = 422, description = "A field breaks its rule, named at its own path (`server`, `conflict_texts`, `conflict_texts[3]`): one error per field, a list's first; a server that has settings already is `server`'s.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn create(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidJson(request): ValidJson<CreateServerSettingsRequest>,
) -> Result<(StatusCode, Json<ServerSettingsResponse>), ApiError> {
    let user = user.user();
    let (server, texts) = checked(&request.server, &request.conflict_texts.texts())?;
    user.require(
        Permission::ConfigureServerSettings,
        &ResourceContext::Global,
    )?;
    let created = state
        .server_settings
        .create(&acting(user, ip), server, texts)
        .await
        .map_err(|error| match error {
            CreateServerSettingsError::SessionGone => ApiError::Unauthorized,
            CreateServerSettingsError::Denied(error) => ApiError::from(error),
            CreateServerSettingsError::CapReached => ApiError::Conflict,
            CreateServerSettingsError::ServerTaken => taken(),
            CreateServerSettingsError::Store(error) => ApiError::from(error),
            other @ (CreateServerSettingsError::Mint(_) | CreateServerSettingsError::Audit(_)) => {
                ApiError::internal(other)
            }
        })?;
    Ok((StatusCode::CREATED, Json(settings_response(&created)?)))
}

/// `PUT /api/v1/server-settings/{id}`: the settings as they should be,
/// after a step-up.
#[utoipa::path(
    put,
    path = "/api/v1/server-settings/{id}",
    operation_id = "update_server_settings",
    tag = "server-settings",
    params(("id" = String, Path, description = "The settings' ID.")),
    request_body = UpdateServerSettingsRequest,
    responses(
        (status = 200, description = "The settings as they now are: at the next version, or unchanged when the edit asks for what's stored.", body = ServerSettingsResponse),
        (status = 400, description = "A body of the wrong shape: an unknown field, or a value of the wrong type. The answer never repeats what it read.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or the caller isn't an Admin or the Owner.", body = ErrorResponse),
        (status = 404, description = "No such settings.", body = ErrorResponse),
        (status = 409, description = "`server_settings_changed`: the settings changed since the caller read `version`.", body = ErrorResponse),
        (status = 422, description = "A field breaks its rule, named at its own path (`version`, `server`, `conflict_texts[3]`); a server another row has is `server`'s.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn update(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<ServerSettingsId>,
    ValidJson(request): ValidJson<UpdateServerSettingsRequest>,
) -> Result<Json<ServerSettingsResponse>, ApiError> {
    let user = user.user();
    let (server, texts) = checked(&request.server, &request.conflict_texts.texts())?;
    let version = NonZeroU32::new(request.version)
        .ok_or_else(|| ApiError::internal("the version is at least 1: garde checked it"))?;
    user.require(
        Permission::ConfigureServerSettings,
        &ResourceContext::Global,
    )?;
    let edit = ServerSettingsEdit {
        version,
        server,
        conflict_texts: texts,
    };
    let edited = state
        .server_settings
        .update(&acting(user, ip), id, edit)
        .await
        .map_err(|error| match error {
            UpdateServerSettingsError::SessionGone => ApiError::Unauthorized,
            UpdateServerSettingsError::Denied(error) => ApiError::from(error),
            UpdateServerSettingsError::NotFound => ApiError::NotFound,
            UpdateServerSettingsError::Changed => ApiError::ServerSettingsChanged,
            UpdateServerSettingsError::ServerTaken => taken(),
            UpdateServerSettingsError::Store(error) => ApiError::from(error),
            other @ (UpdateServerSettingsError::VersionExhausted
            | UpdateServerSettingsError::Lost
            | UpdateServerSettingsError::Audit(_)) => ApiError::internal(other),
        })?;
    Ok(Json(settings_response(edited.settings())?))
}

/// `DELETE /api/v1/server-settings/{id}`: deletes a server's settings,
/// after a step-up.
#[utoipa::path(
    delete,
    path = "/api/v1/server-settings/{id}",
    operation_id = "delete_server_settings",
    tag = "server-settings",
    params(("id" = String, Path, description = "The settings' ID.")),
    responses(
        (status = 204, description = "The settings are deleted: the server's bots count no kick text as a login any more."),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or the caller isn't an Admin or the Owner.", body = ErrorResponse),
        (status = 404, description = "No such settings.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn delete(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<ServerSettingsId>,
) -> Result<StatusCode, ApiError> {
    let user = user.user();
    user.require(
        Permission::ConfigureServerSettings,
        &ResourceContext::Global,
    )?;
    state
        .server_settings
        .delete(&acting(user, ip), id)
        .await
        .map_err(|error| match error {
            DeleteServerSettingsError::SessionGone => ApiError::Unauthorized,
            DeleteServerSettingsError::Denied(error) => ApiError::from(error),
            DeleteServerSettingsError::NotFound => ApiError::NotFound,
            DeleteServerSettingsError::Store(error) => ApiError::from(error),
            other @ (DeleteServerSettingsError::Lost | DeleteServerSettingsError::Audit(_)) => {
                ApiError::internal(other)
            }
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// The request's server, keyed, and its texts. garde checked both already,
/// so a refusal here is a bug.
fn checked<E>(
    server: &str,
    texts: &Result<ConflictTexts, E>,
) -> Result<(ServerKey, ConflictTexts), ApiError> {
    let address = ServerAddress::try_from(server).map_err(ApiError::internal)?;
    let texts = texts
        .as_ref()
        .map_err(|_| ApiError::internal("the texts' rules already hold: garde checked them"))?;
    Ok((ServerKey::from(&address), texts.clone()))
}

/// The 422 of a server that has settings already.
fn taken() -> ApiError {
    ApiError::Validation(vec![FieldError::new("server", SERVER_TAKEN)])
}

/// `settings` as the API answers them: the server as its key, each text as
/// its lines.
fn settings_response(settings: &StoredServerSettings) -> Result<ServerSettingsResponse, ApiError> {
    Ok(ServerSettingsResponse {
        id: settings.id,
        server: BoundedText::new(&settings.server.to_string()),
        conflict_texts: ConflictTextResponse::list(&settings.conflict_texts),
        version: settings.version.get(),
        created_at: timestamp(settings.created_at)?,
        updated_at: timestamp(settings.updated_at)?,
    })
}
