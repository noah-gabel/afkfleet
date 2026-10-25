//! The account routes (Plan.md P9.4, P9.7, Appendix B, ADR-0018): `POST
//! /api/v1/accounts/link` starts a Microsoft link flow, `GET
//! /api/v1/accounts/link/{flow_id}` reads where it stands, and `POST
//! /api/v1/accounts/offline` creates an offline account.
//!
//! - **Starting** is every role's (`LinkAccount`); the service applies the
//!   caps and the Member quota. The answer carries the code the user types
//!   at Microsoft's page: this once, and to this user only.
//! - **Reading** a flow is its creator's alone (`UsePersonal`): an unknown,
//!   replaced or long finished flow, and another user's, are all 404 with
//!   the same body.
//! - **An offline account** is every role's (`LinkAccount`), while `[accounts]
//!   allow_offline` is on. With it off, the route answers 404 like an
//!   unknown one, whatever the body, since the body is read only after that
//!   check.
//!
//! P9.6 (ADR-0018, group E) adds the accounts themselves:
//! - **Listing** (`GET /api/v1/accounts`) shows what the caller may see,
//!   and `GET /api/v1/accounts/awaiting-approval` the pending links they may
//!   review, for the app's count on Home. An account the caller may not
//!   see is 404, like an unknown one (`ViewBot`). Who owns an account and
//!   since when shows only to Admins, the Owner and Manage holders.
//! - **Approving and refusing** a pending link (`ReviewLink`) and
//!   **deleting** an account (`DeleteAccount`) need a step-up; each is
//!   authorized here and again in the service's transaction.
//! - **Linking again** (`RelinkAccount`) starts a link flow like
//!   `POST /accounts/link`, with the same caps; no step-up, since it can't
//!   move the account to another Minecraft profile.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fleet_api_types::accounts::{
    AccountKind, AccountPage, AccountResponse, AccountStatus, CreateOfflineAccountRequest,
    CreatedOfflineAccountResponse, LinkFlowResponse, LinkFlowState, LinkStartedResponse,
    RefusalReason,
};
use fleet_api_types::{BoundedText, ErrorResponse, PageQuery, SecretText};
use fleet_core::authz::{Actor, Permission, ResourceContext};
use fleet_core::id::{AccountId, LinkFlowId};
use fleet_core::value::McUsername;

use super::{account_response, acting, page, timestamp};
use crate::app::accounts::{
    CreateOfflineError, DeleteAccountError, ListAccountsError, ReviewError,
};
use crate::app::link_flows::{
    ALL_BUSY_RETRY_AFTER, IN_PROGRESS_RETRY_AFTER, LinkFlowStatus, LinkStartError, StartedFlow,
};
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, SteppedUpUser, ValidJson, ValidPath, ValidQuery};
use crate::http::rate_limit::ClientIp;
use crate::http::router::AppState;
use crate::ports::accounts::ListedAccount;
use crate::ports::msauth::Failure;
use crate::ports::page::Paged;

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
    Ok((StatusCode::CREATED, Json(started_response(&started)?)))
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
        LinkStartError::OverQuota | LinkStartError::NotRelinkable => ApiError::Conflict,
        LinkStartError::AccountGone => ApiError::NotFound,
        LinkStartError::Denied(error) => ApiError::from(error),
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

/// `POST /api/v1/accounts/offline`: a new offline account, while the
/// server allows them.
#[utoipa::path(
    post,
    path = "/api/v1/accounts/offline",
    operation_id = "create_offline_account",
    tag = "accounts",
    request_body = CreateOfflineAccountRequest,
    responses(
        (status = 201, description = "The offline account, active at once.", body = CreatedOfflineAccountResponse),
        (status = 400, description = "A body that isn't the request's shape.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 404, description = "The server doesn't allow offline accounts (`[accounts] allow_offline`).", body = ErrorResponse),
        (status = 409, description = "The name is taken, in any case, or the Member owns their quota of accounts already.", body = ErrorResponse),
        (status = 415, description = "A body that isn't JSON.", body = ErrorResponse),
        (status = 422, description = "A name that isn't a Minecraft name.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn create_offline(
    user: AuthUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    body: Result<ValidJson<CreateOfflineAccountRequest>, ApiError>,
) -> Result<(StatusCode, Json<CreatedOfflineAccountResponse>), ApiError> {
    if !state.accounts.offline_allowed() {
        return Err(ApiError::NotFound);
    }
    let ValidJson(request) = body?;
    user.require(Permission::LinkAccount, &ResourceContext::Global)?;
    // garde checked it's a Minecraft name.
    let name: McUsername = request.name.expose().parse().map_err(ApiError::internal)?;
    let account = state
        .accounts
        .create_offline(&acting(&user, ip), name)
        .await
        .map_err(|error| match error {
            CreateOfflineError::Disabled => ApiError::NotFound,
            CreateOfflineError::SessionGone => ApiError::Unauthorized,
            CreateOfflineError::OverQuota | CreateOfflineError::Taken => ApiError::Conflict,
            CreateOfflineError::Store(error) => ApiError::from(error),
            other @ (CreateOfflineError::Mint(_) | CreateOfflineError::Audit(_)) => {
                ApiError::internal(other)
            }
        })?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedOfflineAccountResponse {
            id: account.id,
            name: BoundedText::new(account.name.as_str()),
            kind: AccountKind::from(account.credential.kind()).into(),
            status: AccountStatus::from(account.status()).into(),
        }),
    ))
}

/// `GET /api/v1/accounts`: the accounts the caller may see, newest first.
#[utoipa::path(
    get,
    path = "/api/v1/accounts",
    operation_id = "list_accounts",
    tag = "accounts",
    params(PageQuery<AccountId>),
    responses(
        (status = 200, description = "One page of the accounts the caller may see, newest first: their own, those shared with them, every Member's for an Admin, and all of them for the Owner.", body = AccountPage),
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
    ValidQuery(query): ValidQuery<PageQuery<AccountId>>,
) -> Result<Json<AccountPage>, ApiError> {
    let actor = user.actor();
    let listed = state
        .accounts
        .list(&actor, page(&query)?)
        .await
        .map_err(listing)?;
    account_page(&actor, &listed)
}

/// `GET /api/v1/accounts/awaiting-approval`: the pending links the caller
/// may approve or refuse, newest first.
#[utoipa::path(
    get,
    path = "/api/v1/accounts/awaiting-approval",
    operation_id = "accounts_awaiting_approval",
    tag = "accounts",
    params(PageQuery<AccountId>),
    responses(
        (status = 200, description = "One page of the pending links the caller may approve or refuse, newest first: Members' for an Admin, Members' and Admins' for the Owner, none for a Member.", body = AccountPage),
        (status = 400, description = "A query that doesn't parse.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 422, description = "A `limit` outside 1 to 100.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn awaiting_approval(
    user: AuthUser,
    State(state): State<AppState>,
    ValidQuery(query): ValidQuery<PageQuery<AccountId>>,
) -> Result<Json<AccountPage>, ApiError> {
    let actor = user.actor();
    let listed = state
        .accounts
        .awaiting_review(&actor, page(&query)?)
        .await
        .map_err(listing)?;
    account_page(&actor, &listed)
}

/// `GET /api/v1/accounts/{id}`: one account the caller may see.
#[utoipa::path(
    get,
    path = "/api/v1/accounts/{id}",
    operation_id = "get_account",
    tag = "accounts",
    params(("id" = String, Path, description = "The account's ID.")),
    responses(
        (status = 200, description = "The account, as the caller sees it.", body = AccountResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 404, description = "No such account, or one the caller may not see.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn get(
    user: AuthUser,
    State(state): State<AppState>,
    ValidPath(id): ValidPath<AccountId>,
) -> Result<Json<AccountResponse>, ApiError> {
    let actor = user.actor();
    let account = facts(&state, &actor, id).await?;
    user.require(Permission::ViewBot, &account.context())?;
    Ok(Json(account_response(&actor, &account)?))
}

/// `DELETE /api/v1/accounts/{id}`: deletes the account, with its grants.
/// Needs a step-up.
#[utoipa::path(
    delete,
    path = "/api/v1/accounts/{id}",
    operation_id = "delete_account",
    tag = "accounts",
    params(("id" = String, Path, description = "The account's ID.")),
    responses(
        (status = 204, description = "The account, its refresh token and its grants are deleted."),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or the caller may see the account but not delete it (Manage).", body = ErrorResponse),
        (status = 404, description = "No such account, or one the caller may not see.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn delete(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<AccountId>,
) -> Result<StatusCode, ApiError> {
    let user = user.user();
    let account = facts(&state, &user.actor(), id).await?;
    user.require(Permission::DeleteAccount, &account.context())?;
    state
        .accounts
        .delete(&acting(user, ip), id)
        .await
        .map_err(|error| match error {
            DeleteAccountError::NotFound => ApiError::NotFound,
            DeleteAccountError::SessionGone => ApiError::Unauthorized,
            DeleteAccountError::Denied(error) => ApiError::from(error),
            DeleteAccountError::Store(error) => ApiError::from(error),
            other @ DeleteAccountError::Audit(_) => ApiError::internal(other),
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/accounts/{id}/approve`: approves a pending link, which
/// makes the account usable. Needs a step-up.
#[utoipa::path(
    post,
    path = "/api/v1/accounts/{id}/approve",
    operation_id = "approve_account",
    tag = "accounts",
    params(("id" = String, Path, description = "The account's ID.")),
    responses(
        (status = 200, description = "The account is active, as the caller sees it now.", body = AccountResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or the caller may see the account but isn't above its owner (the Owner for Admins' links, an Admin for Members').", body = ErrorResponse),
        (status = 404, description = "No such account, or one the caller may not see.", body = ErrorResponse),
        (status = 409, description = "The account isn't pending.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn approve(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<AccountId>,
) -> Result<Json<AccountResponse>, ApiError> {
    let user = user.user();
    let actor = user.actor();
    let account = facts(&state, &actor, id).await?;
    user.require(Permission::ReviewLink, &account.context())?;
    let approved = state
        .accounts
        .approve(&acting(user, ip), id)
        .await
        .map_err(reviewing)?;
    Ok(Json(account_response(&actor, &approved)?))
}

/// `POST /api/v1/accounts/{id}/refuse`: refuses a pending link, which
/// deletes the account. Needs a step-up.
#[utoipa::path(
    post,
    path = "/api/v1/accounts/{id}/refuse",
    operation_id = "refuse_account",
    tag = "accounts",
    params(("id" = String, Path, description = "The account's ID.")),
    responses(
        (status = 204, description = "The link is refused: the account, its refresh token and its grants are deleted."),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or the caller may see the account but isn't above its owner (the Owner for Admins' links, an Admin for Members').", body = ErrorResponse),
        (status = 404, description = "No such account, or one the caller may not see.", body = ErrorResponse),
        (status = 409, description = "The account isn't pending.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn refuse(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<AccountId>,
) -> Result<StatusCode, ApiError> {
    let user = user.user();
    let account = facts(&state, &user.actor(), id).await?;
    user.require(Permission::ReviewLink, &account.context())?;
    state
        .accounts
        .refuse(&acting(user, ip), id)
        .await
        .map_err(reviewing)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/accounts/{id}/relink`: starts a Microsoft link flow that
/// links the account again, and answers its code, shown this once.
#[utoipa::path(
    post,
    path = "/api/v1/accounts/{id}/relink",
    operation_id = "relink_account",
    tag = "accounts",
    params(("id" = String, Path, description = "The account's ID.")),
    responses(
        (status = 201, description = "The flow started: open the link and enter the code, signing in with this account's Microsoft account. Read it with `GET /accounts/link/{flow_id}`.", body = LinkStartedResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "The caller may see the account but not link it again (Manage).", body = ErrorResponse),
        (status = 404, description = "No such account, or one the caller may not see.", body = ErrorResponse),
        (status = 409, description = "The account is pending (approve or refuse it first), or an offline one.", body = ErrorResponse),
        (status = 429, description = "Too many requests, the user's daily cap of link flows, or the user's flow is finishing or starting.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = 503, description = "Every place for a link flow is taken, Microsoft didn't answer, or the server is busy.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn relink(
    user: AuthUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<AccountId>,
) -> Result<(StatusCode, Json<LinkStartedResponse>), ApiError> {
    let account = facts(&state, &user.actor(), id).await?;
    user.require(Permission::RelinkAccount, &account.context())?;
    let started = state
        .link_flows
        .start_relink(&acting(&user, ip), id)
        .await
        .map_err(refused)?;
    Ok((StatusCode::CREATED, Json(started_response(&started)?)))
}

/// The account `id` as `actor` sees it, or 404.
async fn facts(state: &AppState, actor: &Actor, id: AccountId) -> Result<ListedAccount, ApiError> {
    state
        .accounts
        .view(actor, id)
        .await?
        .ok_or(ApiError::NotFound)
}

/// A list that couldn't be read, as the API answers it.
pub(super) fn listing(error: ListAccountsError) -> ApiError {
    match error {
        ListAccountsError::Store(error) => ApiError::from(error),
        other @ ListAccountsError::Disagreement => ApiError::internal(other),
    }
}

/// A review that didn't happen, as the API answers it.
fn reviewing(error: ReviewError) -> ApiError {
    match error {
        ReviewError::NotFound => ApiError::NotFound,
        ReviewError::SessionGone => ApiError::Unauthorized,
        ReviewError::Denied(error) => ApiError::from(error),
        ReviewError::NotPending => ApiError::Conflict,
        ReviewError::Store(error) => ApiError::from(error),
        other @ ReviewError::Audit(_) => ApiError::internal(other),
    }
}

/// A page of accounts as `viewer` sees them.
pub(super) fn account_page(
    viewer: &Actor,
    listed: &Paged<ListedAccount, AccountId>,
) -> Result<Json<AccountPage>, ApiError> {
    Ok(Json(AccountPage {
        items: listed
            .items
            .iter()
            .map(|account| account_response(viewer, account))
            .collect::<Result<_, _>>()?,
        next: listed.next,
    }))
}

/// A started flow's answer: its code, this once.
fn started_response(started: &StartedFlow) -> Result<LinkStartedResponse, ApiError> {
    Ok(LinkStartedResponse {
        flow_id: started.id,
        user_code: SecretText::new(started.user_code.as_str().to_owned()),
        verification_uri: BoundedText::new(started.verification_uri.as_str()),
        expires_at: timestamp(started.expires_at)?,
    })
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
