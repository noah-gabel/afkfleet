//! The account routes (Plan.md P9.4, Appendix B, ADR-0018): `POST
//! /api/v1/accounts/link` starts a Microsoft link flow, and `GET
//! /api/v1/accounts/link/{flow_id}` reads where it stands.
//!
//! - **Starting** is every role's (`LinkAccount`); the service applies the
//!   caps and the Member quota. The answer carries the code the user types
//!   at Microsoft's page: this once, and to this user only.
//! - **Reading** a flow is its creator's alone (`UsePersonal`): an unknown,
//!   replaced or long finished flow, and another user's, are all 404 with
//!   the same body.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fleet_api_types::accounts::{
    AccountStatus, LinkFlowResponse, LinkFlowState, LinkStartedResponse, RefusalReason,
};
use fleet_api_types::{BoundedText, ErrorResponse, SecretText};
use fleet_core::authz::{Permission, ResourceContext};
use fleet_core::id::LinkFlowId;

use super::{acting, timestamp};
use crate::app::link_flows::{
    ALL_BUSY_RETRY_AFTER, IN_PROGRESS_RETRY_AFTER, LinkFlowStatus, LinkStartError,
};
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, ValidPath};
use crate::http::rate_limit::ClientIp;
use crate::http::router::AppState;
use crate::ports::msauth::Failure;

/// `POST /api/v1/accounts/link`: starts a Microsoft link flow, and answers
/// its code, shown this once.
#[utoipa::path(
    post,
    path = "/api/v1/accounts/link",
    operation_id = "start_link",
    tag = "accounts",
    responses(
        (status = 201, description = "The flow started: open the link and enter the code.", body = LinkStartedResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 409, description = "The Member owns their quota of accounts already.", body = ErrorResponse),
        (status = 429, description = "Too many requests, the user's daily cap of link flows, or the user's flow is finishing or starting.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = 503, description = "Every place for a link flow is taken, Microsoft didn't answer, or the server is busy.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn start_link(
    user: AuthUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
) -> Result<(StatusCode, Json<LinkStartedResponse>), ApiError> {
    user.require(Permission::LinkAccount, &ResourceContext::Global)?;
    let started = state
        .link_flows
        .start(&acting(&user, ip))
        .await
        .map_err(refused)?;
    Ok((
        StatusCode::CREATED,
        Json(LinkStartedResponse {
            flow_id: started.id,
            user_code: SecretText::new(started.user_code.as_str().to_owned()),
            verification_uri: BoundedText::new(started.verification_uri.as_str()),
            expires_at: timestamp(started.expires_at)?,
        }),
    ))
}

/// A start that didn't happen, as the API answers it (ADR-0018).
fn refused(error: LinkStartError) -> ApiError {
    match error {
        LinkStartError::ShuttingDown => ApiError::busy(),
        LinkStartError::InProgress => ApiError::RateLimited {
            retry_after: IN_PROGRESS_RETRY_AFTER,
        },
        // Every place taken, or Microsoft away for a moment: try later.
        LinkStartError::AllBusy
        | LinkStartError::Microsoft(
            Failure::Unreachable
            | Failure::TimedOut
            | Failure::RateLimited
            | Failure::Unavailable { .. },
        ) => ApiError::Busy {
            retry_after: ALL_BUSY_RETRY_AFTER,
        },
        LinkStartError::DailyCap { retry_after } => ApiError::RateLimited { retry_after },
        LinkStartError::OverQuota => ApiError::Conflict,
        LinkStartError::SessionGone => ApiError::Unauthorized,
        LinkStartError::Store(error) => ApiError::from(error),
        other @ (LinkStartError::Microsoft(Failure::Certificate | Failure::Unexpected { .. })
        | LinkStartError::Mint(_)
        | LinkStartError::Audit(_)) => ApiError::internal(other),
    }
}

/// `GET /api/v1/accounts/link/{flow_id}`: where the caller's flow stands.
#[utoipa::path(
    get,
    path = "/api/v1/accounts/link/{flow_id}",
    operation_id = "link_status",
    tag = "accounts",
    params(("flow_id" = String, Path, description = "The flow's ID, from `POST /accounts/link`.")),
    responses(
        (status = 200, description = "Where the flow stands.", body = LinkFlowResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 404, description = "No such flow, one that ended more than 15 minutes ago or was replaced, or another user's.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn link_status(
    user: AuthUser,
    State(state): State<AppState>,
    ValidPath(id): ValidPath<LinkFlowId>,
) -> Result<Json<LinkFlowResponse>, ApiError> {
    let view = state.link_flows.status(id).ok_or(ApiError::NotFound)?;
    user.require(
        Permission::UsePersonal,
        &ResourceContext::Personal {
            owner: view.creator,
        },
    )?;
    Ok(Json(flow_response(view.status)?))
}

/// A flow's status as the API shows it.
fn flow_response(status: LinkFlowStatus) -> Result<LinkFlowResponse, ApiError> {
    let mut response = LinkFlowResponse {
        status: LinkFlowState::Failed.into(),
        expires_at: None,
        reason: None,
        account_id: None,
        account_status: None,
        name: None,
    };
    let state = match status {
        LinkFlowStatus::Waiting { expires_at } => {
            response.expires_at = Some(timestamp(expires_at)?);
            LinkFlowState::Waiting
        }
        LinkFlowStatus::Finishing => LinkFlowState::Finishing,
        LinkFlowStatus::Declined => LinkFlowState::Declined,
        LinkFlowStatus::Expired => LinkFlowState::Expired,
        LinkFlowStatus::Taken => LinkFlowState::Taken,
        LinkFlowStatus::OverQuota => LinkFlowState::OverQuota,
        LinkFlowStatus::Refused(reason) => {
            response.reason = Some(RefusalReason::from(reason).into());
            LinkFlowState::Refused
        }
        LinkFlowStatus::Failed => LinkFlowState::Failed,
        LinkFlowStatus::Linked {
            account,
            status,
            name,
        } => {
            response.account_id = Some(account);
            response.account_status = Some(AccountStatus::from(status).into());
            response.name = Some(BoundedText::new(name.as_str()));
            LinkFlowState::Linked
        }
    };
    response.status = state.into();
    Ok(response)
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use fleet_api_types::OpenEnum as _;
    use fleet_core::account::{self, RefusalReason as CoreReason};
    use fleet_core::id::AccountId;

    use super::*;

    #[test]
    fn every_status_has_the_state_of_its_name() {
        let at = DateTime::from_timestamp_millis(1_800_000_000_000).unwrap();
        let statuses = [
            LinkFlowStatus::Waiting { expires_at: at },
            LinkFlowStatus::Finishing,
            LinkFlowStatus::Declined,
            LinkFlowStatus::Expired,
            LinkFlowStatus::Taken,
            LinkFlowStatus::OverQuota,
            LinkFlowStatus::Refused(CoreReason::NotOwned),
            LinkFlowStatus::Failed,
            LinkFlowStatus::Linked {
                account: AccountId::new_v7(at, [1; 10]).unwrap(),
                status: account::AccountStatus::Pending,
                name: "AfkBot1".parse().unwrap(),
            },
        ];

        for status in statuses {
            let name = status.as_str();
            let response = flow_response(status).unwrap();

            assert_eq!(
                response.status.known().map(LinkFlowState::as_str),
                Some(name)
            );
        }
        assert_eq!(LinkFlowState::ALL.len(), 9);
    }
}
