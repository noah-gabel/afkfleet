//! The servers' settings routes (Plan.md P11.9, Appendix B; ADR-0020, U54,
//! U58, S74–S80, R134, R135): Admins and the Owner read them, after a
//! step-up write them, and try unsaved chat formats on a line.
//!
//! - **Reading** (`GET /api/v1/server-settings`): `authorize()` asks
//!   `ViewServerSettings` first (403 for a Member), then one page, newest
//!   first, of at most 10 rows, so a page of rows at their caps fits a
//!   client's answer cap (`ServerSettingsPageQuery`).
//! - **Creating** (`POST /api/v1/server-settings`, after a step-up): a
//!   `CreateServerSettingsRequest`, whose every rule garde checked, at
//!   least one text unless a chat format is set (R140); the server is keyed
//!   (S74) and the texts and formats built by fleet-core.
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
//! - **Previewing** (`POST /api/v1/server-settings/chat-format-preview`, no
//!   step-up, U58): `ConfigureServerSettings` (403), then which of the
//!   request's formats, validated as on save, fits its line first, cleaned
//!   as received chat is (R134). Nothing is read, written or recorded.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use core::num::NonZeroU32;

use fleet_api_types::server_settings::{
    ChatFormatsInput, ConflictTextResponse, ConflictTextsInput, CreateServerSettingsRequest,
    PreviewChatFormatsRequest, PreviewChatFormatsResponse, SERVER_TAKEN, ServerSettingsPage,
    ServerSettingsPageQuery, ServerSettingsResponse, UpdateServerSettingsRequest, holds_something,
};
use fleet_api_types::{BoundedText, ErrorResponse, FieldError};
use fleet_core::authz::{Permission, ResourceContext};
use fleet_core::chat::ChatFormats;
use fleet_core::disconnect::ConflictTexts;
use fleet_core::id::ServerSettingsId;
use fleet_core::value::{ServerAddress, ServerKey};

use super::{acting, timestamp};
use crate::app::server_settings::{
    CreateServerSettingsError, DeleteServerSettingsError, ServerSettingsEdit,
    UpdateServerSettingsError,
};
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, SteppedUpUser, ValidJson, ValidPath, ValidQuery};
use crate::http::rate_limit::ClientIp;
use crate::http::router::AppState;
use crate::ports::page::{Page, PageLimit};
use crate::ports::server_settings::StoredServerSettings;

/// `GET /api/v1/server-settings`: every server's settings, newest first.
#[utoipa::path(
    get,
    path = "/api/v1/server-settings",
    operation_id = "list_server_settings",
    tag = "server-settings",
    params(ServerSettingsPageQuery),
    responses(
        (status = 200, description = "One page of the servers' settings, newest first.", body = ServerSettingsPage),
        (status = 400, description = "A query that doesn't parse.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Only Admins and the Owner read the servers' settings.", body = ErrorResponse),
        (status = 422, description = "A `limit` outside 1 to 10.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn list(
    user: AuthUser,
    State(state): State<AppState>,
    ValidQuery(query): ValidQuery<ServerSettingsPageQuery>,
) -> Result<Json<ServerSettingsPage>, ApiError> {
    user.require(Permission::ViewServerSettings, &ResourceContext::Global)?;
    let listed = state.server_settings.list(settings_page(&query)?).await?;
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
        (status = 400, description = "A body of the wrong shape: an unknown field, a missing one, or a value of the wrong type. The answer never repeats what it read.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or the caller isn't an Admin or the Owner.", body = ErrorResponse),
        (status = 409, description = "`conflict`: 100 servers have settings already.", body = ErrorResponse),
        (status = 422, description = "A field breaks its rule, named at its own path (`server`, `conflict_texts`, `conflict_texts[3]`, `chat_formats`, `chat_formats[3]`): one error per field, a list's first; a server that has settings already is `server`'s.", body = ErrorResponse),
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
    let (server, texts, formats) = checked(
        &request.server,
        &request.conflict_texts,
        &request.chat_formats,
    )?;
    user.require(
        Permission::ConfigureServerSettings,
        &ResourceContext::Global,
    )?;
    let created = state
        .server_settings
        .create(&acting(user, ip), server, texts, formats)
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
        (status = 400, description = "A body of the wrong shape: an unknown field, a missing one, or a value of the wrong type. The answer never repeats what it read.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or the caller isn't an Admin or the Owner.", body = ErrorResponse),
        (status = 404, description = "No such settings.", body = ErrorResponse),
        (status = 409, description = "`server_settings_changed`: the settings changed since the caller read `version`.", body = ErrorResponse),
        (status = 422, description = "A field breaks its rule, named at its own path (`version`, `server`, `conflict_texts`, `conflict_texts[3]`, `chat_formats`, `chat_formats[3]`): one error per field, a list's first; a server another row has is `server`'s.", body = ErrorResponse),
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
    let (server, texts, formats) = checked(
        &request.server,
        &request.conflict_texts,
        &request.chat_formats,
    )?;
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
        chat_formats: formats,
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

/// The request's server, keyed, its texts and its formats. garde checked
/// them all already, at least one text or format among them, so a refusal
/// here is a bug.
fn checked(
    server: &str,
    texts: &ConflictTextsInput,
    formats: &ChatFormatsInput,
) -> Result<(ServerKey, ConflictTexts, ChatFormats), ApiError> {
    let address = ServerAddress::try_from(server).map_err(ApiError::internal)?;
    if !holds_something(texts, formats) {
        return Err(ApiError::internal(
            "a row holds a text or a format: garde checked it",
        ));
    }
    let texts = texts
        .texts()
        .map_err(|_| ApiError::internal("the texts' rules already hold: garde checked them"))?;
    let formats = formats
        .formats()
        .map_err(|_| ApiError::internal("the formats' rules already hold: garde checked them"))?;
    Ok((ServerKey::from(&address), texts, formats))
}

/// `POST /api/v1/server-settings/chat-format-preview`: which sender unsaved
/// chat formats read from a line.
#[utoipa::path(
    post,
    path = "/api/v1/server-settings/chat-format-preview",
    operation_id = "preview_chat_formats",
    tag = "server-settings",
    request_body = PreviewChatFormatsRequest,
    responses(
        (status = 200, description = "The sender the first format that fits the line reads, and that format's place counted from 0; an empty object when none fits. Nothing is stored or recorded.", body = PreviewChatFormatsResponse),
        (status = 400, description = "A body of the wrong shape: an unknown field, a missing one, or a value of the wrong type. The answer never repeats what it read.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Only Admins and the Owner try chat formats.", body = ErrorResponse),
        (status = 422, description = "A field breaks its rule, named at its own path (`chat_formats`, `chat_formats[3]`, `line`), the formats under the save's rules.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn preview(
    user: AuthUser,
    State(state): State<AppState>,
    ValidJson(request): ValidJson<PreviewChatFormatsRequest>,
) -> Result<Json<PreviewChatFormatsResponse>, ApiError> {
    user.require(
        Permission::ConfigureServerSettings,
        &ResourceContext::Global,
    )?;
    let formats = request
        .chat_formats
        .formats()
        .map_err(|_| ApiError::internal("the formats' rules already hold: garde checked them"))?;
    let found = state.server_settings.preview(&formats, &request.line);
    Ok(Json(PreviewChatFormatsResponse {
        sender: found
            .as_ref()
            .map(|found| BoundedText::new(found.sender().as_str())),
        format: found
            .map(|found| u32::try_from(found.format()))
            .transpose()
            .map_err(ApiError::internal)?,
    }))
}

/// The 422 of a server that has settings already.
fn taken() -> ApiError {
    ApiError::Validation(vec![FieldError::new("server", SERVER_TAKEN)])
}

/// The page the list's query asks for. garde has checked the limit, so one
/// out of range is a bug.
fn settings_page(query: &ServerSettingsPageQuery) -> Result<Page<ServerSettingsId>, ApiError> {
    Ok(Page {
        before: query.before,
        limit: PageLimit::new(query.limit_or_default()).map_err(ApiError::internal)?,
    })
}

/// `settings` as the API answers them: the server as its key, each text as
/// its lines, the formats as written.
fn settings_response(settings: &StoredServerSettings) -> Result<ServerSettingsResponse, ApiError> {
    Ok(ServerSettingsResponse {
        id: settings.id,
        server: BoundedText::new(&settings.server.to_string()),
        conflict_texts: ConflictTextResponse::list(&settings.conflict_texts),
        chat_formats: settings.chat_formats.iter().map(BoundedText::new).collect(),
        version: settings.version.get(),
        created_at: timestamp(settings.created_at)?,
        updated_at: timestamp(settings.updated_at)?,
    })
}
