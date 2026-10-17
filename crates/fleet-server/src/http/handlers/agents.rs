//! The agents' routes (Plan.md P10.3, P10.10, Appendix B, ADR-0019):
//! `POST /api/v1/agents/enrollment-tokens`, `GET /api/v1/agents` and
//! `POST /api/v1/agents/{id}/disable`.
//!
//! - **Creating an enrollment token** is the Owner's and needs a step-up
//!   (`SteppedUpUser`, the handler's first extractor), like the admin writes
//!   of §7.2. It's authorized here and again in the service's transaction.
//!   The route takes no input; the token lives 15 minutes, and at most 20
//!   are pending. The bundle (the token and the CA's fingerprint) is in this
//!   one answer.
//! - **While the control plane is off** (`[grpc]` isn't configured), the
//!   route answers 404 like an unknown one, after the step-up check, as
//!   `POST /accounts/offline` does while it's switched off.
//! - **Listing agents** is Admin+ (`ViewAgents`): every agent, enabled or
//!   not, newest first, with its status.
//! - **Disabling an agent** is Admin+ (`DisableAgent`) and needs a step-up,
//!   authorized before anything is read and again in the service's
//!   transaction (S31). It's final: every certificate revoked and the live
//!   stream ended before the answer. An agent disabled already answers as
//!   it is. It works with the control plane off too: an agent enrolled
//!   before can still be disabled.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fleet_api_types::agents::{
    AgentPage, AgentResponse, AgentStatus, CreatedEnrollmentTokenResponse,
};
use fleet_api_types::{BoundedText, ErrorResponse, PageQuery};
use fleet_core::authz::{Permission, ResourceContext};
use fleet_core::id::AgentId;

use super::{acting, page, secret_text, timestamp};
use crate::app::agents::{AgentView, DisableAgentError, Disabler};
use crate::app::enrollment::{CreateTokenError, Creator};
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, SteppedUpUser, ValidPath, ValidQuery};
use crate::http::rate_limit::ClientIp;
use crate::http::router::AppState;

/// `POST /api/v1/agents/enrollment-tokens`: a new enrollment token, in its
/// bundle, shown this once. Needs a step-up.
#[utoipa::path(
    post,
    path = "/api/v1/agents/enrollment-tokens",
    operation_id = "create_enrollment_token",
    tag = "agents",
    responses(
        (status = 201, description = "The token, in its bundle: pipe it into `afkfleet-agent enroll` within 15 minutes.", body = CreatedEnrollmentTokenResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or only the Owner creates enrollment tokens.", body = ErrorResponse),
        (status = 404, description = "The server's control plane is off: `[grpc]` isn't configured.", body = ErrorResponse),
        (status = 409, description = "20 enrollment tokens are pending already.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn create_enrollment_token(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
) -> Result<(StatusCode, Json<CreatedEnrollmentTokenResponse>), ApiError> {
    let Some(enrollment) = state.enrollment_tokens.as_ref() else {
        return Err(ApiError::NotFound);
    };
    let user = user.user();
    user.require(Permission::EnrollAgent, &ResourceContext::Global)?;
    let created = enrollment
        .create(Creator::Owner(&acting(user, ip)))
        .await
        .map_err(|error| match error {
            CreateTokenError::SessionGone => ApiError::Unauthorized,
            CreateTokenError::Denied => ApiError::Forbidden,
            CreateTokenError::CapReached => ApiError::Conflict,
            CreateTokenError::Store(error) => ApiError::from(error),
            other @ CreateTokenError::Internal(_) => ApiError::internal(other),
        })?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedEnrollmentTokenResponse {
            id: created.id,
            bundle: secret_text(&created.bundle.to_text()),
            ca_fingerprint: BoundedText::new(&created.bundle.ca_fingerprint().to_string()),
            expires_at: timestamp(created.expires_at)?,
        }),
    ))
}

/// An agent as the API shows it.
fn agent_response(view: &AgentView) -> Result<AgentResponse, ApiError> {
    let agent = &view.listed.agent;
    Ok(AgentResponse {
        id: agent.id,
        name: BoundedText::new(agent.name.as_str()),
        enabled: agent.enabled,
        status: AgentStatus::from(view.status).into(),
        bots: view.listed.bots,
        capacity: agent.capacity.map(|capacity| u32::from(capacity.get())),
        version: agent
            .version
            .as_ref()
            .map(|version| BoundedText::new(version.as_str())),
        connected_since: view.connected_since.map(timestamp).transpose()?,
        last_seen_at: view.last_seen_at.map(timestamp).transpose()?,
        not_after: view.listed.not_after.map(timestamp).transpose()?,
        created_at: timestamp(agent.created_at)?,
    })
}

/// `GET /api/v1/agents`: one page of agents, newest first.
#[utoipa::path(
    get,
    path = "/api/v1/agents",
    operation_id = "list_agents",
    tag = "agents",
    params(PageQuery<AgentId>),
    responses(
        (status = 200, description = "One page of agents, newest first, with their status.", body = AgentPage),
        (status = 400, description = "A query that doesn't parse.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Only Admins and the Owner list agents.", body = ErrorResponse),
        (status = 422, description = "A `limit` outside 1 to 100.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn list_agents(
    user: AuthUser,
    State(state): State<AppState>,
    ValidQuery(query): ValidQuery<PageQuery<AgentId>>,
) -> Result<Json<AgentPage>, ApiError> {
    user.require(Permission::ViewAgents, &ResourceContext::Global)?;
    let listed = state.agents.list(page(&query)?).await?;
    Ok(Json(AgentPage {
        items: listed
            .items
            .iter()
            .map(agent_response)
            .collect::<Result<_, _>>()?,
        next: listed.next,
    }))
}

/// `POST /api/v1/agents/{id}/disable`: disables an agent for good, revoking
/// every certificate and ending its stream. Needs a step-up.
#[utoipa::path(
    post,
    path = "/api/v1/agents/{id}/disable",
    operation_id = "disable_agent",
    tag = "agents",
    params(("id" = String, Path, description = "The agent's ID.")),
    responses(
        (status = 200, description = "The agent, disabled: every certificate revoked and its stream ended. An agent disabled already answers as it is.", body = AgentResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or only Admins and the Owner disable agents.", body = ErrorResponse),
        (status = 404, description = "No such agent.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn disable_agent(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<AgentId>,
) -> Result<Json<AgentResponse>, ApiError> {
    let user = user.user();
    user.require(Permission::DisableAgent, &ResourceContext::Global)?;
    let disabled = state
        .agents
        .disable(Disabler::Admin(&acting(user, ip)), id)
        .await
        .map_err(|error| match error {
            DisableAgentError::NotFound => ApiError::NotFound,
            DisableAgentError::SessionGone => ApiError::Unauthorized,
            DisableAgentError::Denied(error) => ApiError::from(error),
            DisableAgentError::Store(error) => ApiError::from(error),
            other @ DisableAgentError::Audit(_) => ApiError::internal(other),
        })?;
    Ok(Json(agent_response(&disabled.view)?))
}
