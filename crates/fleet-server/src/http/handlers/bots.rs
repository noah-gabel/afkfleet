//! The bot routes (Plan.md P11.2, Appendix B; ADR-0019, S46–S56): the bots
//! a caller may see, the server and the mode they set, and the five
//! lifecycle calls.
//!
//! - **Reading** (`GET /api/v1/bots`, `GET /api/v1/bots/{id}`) shows what
//!   the caller may see (`ViewBot`); a bot they may not see is 404, like an
//!   unknown one. Its account shows as `GET /api/v1/accounts/{id}` shows it
//!   to them (who owns it and since when only for Admins, the Owner and
//!   Manage holders), and which agent runs it only to Admins and the Owner
//!   (`ViewAgents`).
//! - **The server address** (`PATCH /api/v1/bots/{id}`) needs Manage
//!   (`ConfigureBot`) and a step-up (S47). Where it points isn't checked
//!   here: the agent checks the address it resolves before it connects
//!   (S46).
//! - **The mode** (`PUT /api/v1/bots/{id}/mode`) needs Control, or Manage
//!   for a mode that sends a command off the allowlist, and a mode the caller
//!   may see; an unknown or hidden mode is one 422.
//! - **The lifecycle calls** (`POST /api/v1/bots/{id}/start`, `/stop`,
//!   `/restart`, `/reset`, `/resume`) need Control (`ControlBot`). Start and
//!   stop answer 200 with the bot; a command sent to the bot's agent 202; one
//!   that can't act now 409 with why (S50).
//! - **The bot's own limit** (`[rate_limits] bot_actions_per_bot_per_min`)
//!   is spent by every call that makes its account join a server (a new
//!   address, start, restart, reset, resume; never stop or the mode), right
//!   after the caller is authorized, so a stranger never learns that the
//!   bot exists (S52).
//!
//! Every write is authorized here, and again in the service's transaction,
//! on what it read there.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fleet_api_types::agents::AgentStatus as ApiAgentStatus;
use fleet_api_types::bots::{
    BotAgent, BotModeRequest, BotPage, BotPatchRequest, BotResponse, BotStateKind, BotStateReason,
    BotStatus, BotStatusKind, DesiredState, LastDisconnect, UnplacedReason,
};
use fleet_api_types::{BoundedText, ErrorResponse, FieldError, PageQuery};
use fleet_core::authz::{
    Actor, GrantLevel, Permission, ResourceContext, authorize, effective_level,
};
use fleet_core::bot::{BotState, CommandRefusal, LastState, Lifecycle, ShownState};
use fleet_core::id::BotId;
use fleet_core::value::ServerAddress;

use super::{account_response, acting, page, timestamp};
use crate::app::bots::{BotAction, BotView, BotWriteError, Controlled, ListBotsError};
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, SteppedUpUser, ValidJson, ValidPath, ValidQuery};
use crate::http::rate_limit::ClientIp;
use crate::http::router::AppState;
use crate::ports::page::Paged;

/// The field message of a mode that doesn't exist, or that the caller may
/// not see: one text for both, so modes can't be probed.
pub(super) const UNUSABLE_MODE: &str = "isn't a mode you can use";

/// `GET /api/v1/bots`: the bots the caller may see, newest first.
#[utoipa::path(
    get,
    path = "/api/v1/bots",
    operation_id = "list_bots",
    tag = "bots",
    params(PageQuery<BotId>),
    responses(
        (status = 200, description = "One page of the bots the caller may see, newest first: those of their own accounts, of those shared with them, every Member's for an Admin, and all of them for the Owner.", body = BotPage),
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
    ValidQuery(query): ValidQuery<PageQuery<BotId>>,
) -> Result<Json<BotPage>, ApiError> {
    let actor = user.actor();
    let listed = state
        .bots
        .list(&actor, page(&query)?)
        .await
        .map_err(|error| match error {
            ListBotsError::Store(error) => ApiError::from(error),
            other @ ListBotsError::Disagreement => ApiError::internal(other),
        })?;
    bot_page(&actor, &listed)
}

/// `GET /api/v1/bots/{id}`: one bot the caller may see.
#[utoipa::path(
    get,
    path = "/api/v1/bots/{id}",
    operation_id = "get_bot",
    tag = "bots",
    params(("id" = String, Path, description = "The bot's ID.")),
    responses(
        (status = 200, description = "The bot, as the caller sees it.", body = BotResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 404, description = "No such bot, or one the caller may not see.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn get(
    user: AuthUser,
    State(state): State<AppState>,
    ValidPath(id): ValidPath<BotId>,
) -> Result<Json<BotResponse>, ApiError> {
    let actor = user.actor();
    let view = facts(&state, &actor, id).await?;
    user.require(Permission::ViewBot, &view.account.context())?;
    Ok(Json(bot_response(&actor, &view)?))
}

/// `PATCH /api/v1/bots/{id}`: the Minecraft server the bot joins. Needs
/// Manage and a step-up.
#[utoipa::path(
    patch,
    path = "/api/v1/bots/{id}",
    operation_id = "patch_bot",
    tag = "bots",
    params(("id" = String, Path, description = "The bot's ID.")),
    request_body = BotPatchRequest,
    responses(
        (status = 200, description = "The bot after the change, or as it was when it had that address already. Where the address points isn't checked here: that's the agent's, on the address it resolves before it connects (ADR-0019, S46).", body = BotResponse),
        (status = 400, description = "A body of the wrong shape.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or the caller may see the bot but not configure it (Manage).", body = ErrorResponse),
        (status = 404, description = "No such bot, or one the caller may not see.", body = ErrorResponse),
        (status = 422, description = "An address that isn't a host and an optional port.", body = ErrorResponse),
        (status = 429, description = "Too many requests, or too many actions on this bot.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn patch(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<BotId>,
    ValidJson(request): ValidJson<BotPatchRequest>,
) -> Result<Json<BotResponse>, ApiError> {
    let user = user.user();
    let actor = user.actor();
    let server = ServerAddress::try_from(request.server_address.as_str())
        .map_err(|_| ApiError::internal("the rule already holds: garde checked it"))?;
    let view = facts(&state, &actor, id).await?;
    user.require(Permission::ConfigureBot, &view.account.context())?;
    state.limits.check_bot_action(id)?;
    state
        .bots
        .set_server(&acting(user, ip), id, server)
        .await
        .map_err(writing)?;
    answered(&state, &actor, id).await
}

/// `PUT /api/v1/bots/{id}/mode`: the mode the bot runs.
#[utoipa::path(
    put,
    path = "/api/v1/bots/{id}/mode",
    operation_id = "set_bot_mode",
    tag = "bots",
    params(("id" = String, Path, description = "The bot's ID.")),
    request_body = BotModeRequest,
    responses(
        (status = 200, description = "The bot after the change, or as it was when it ran that mode already.", body = BotResponse),
        (status = 400, description = "A body of the wrong shape.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "The caller may see the bot but not switch it to this mode: Control, or Manage for a mode that sends a command off the allowlist.", body = ErrorResponse),
        (status = 404, description = "No such bot, or one the caller may not see.", body = ErrorResponse),
        (status = 422, description = "A mode that doesn't exist, or that the caller may not see.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn set_mode(
    user: AuthUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<BotId>,
    ValidJson(request): ValidJson<BotModeRequest>,
) -> Result<Json<BotResponse>, ApiError> {
    let actor = user.actor();
    state
        .bots
        .set_mode(&acting(&user, ip), id, request.mode_id)
        .await
        .map_err(writing)?;
    answered(&state, &actor, id).await
}

/// `POST /api/v1/bots/{id}/start`: the bot should run.
#[utoipa::path(
    post,
    path = "/api/v1/bots/{id}/start",
    operation_id = "start_bot",
    tag = "bots",
    params(("id" = String, Path, description = "The bot's ID.")),
    responses(
        (status = 200, description = "The bot, which should run: the server places it once it may, and a Paused or Failed one stays so until it's resumed or reset.", body = BotResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "The caller may see the bot but not control it (Control).", body = ErrorResponse),
        (status = 404, description = "No such bot, or one the caller may not see.", body = ErrorResponse),
        (status = 429, description = "Too many requests, or too many actions on this bot.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn start(
    user: AuthUser,
    state: State<AppState>,
    ip: ClientIp,
    id: ValidPath<BotId>,
) -> Result<(StatusCode, Json<BotResponse>), ApiError> {
    lifecycle(&user, state, ip, id, BotAction::Start).await
}

/// `POST /api/v1/bots/{id}/stop`: the bot should be stopped.
#[utoipa::path(
    post,
    path = "/api/v1/bots/{id}/stop",
    operation_id = "stop_bot",
    tag = "bots",
    params(("id" = String, Path, description = "The bot's ID.")),
    responses(
        (status = 200, description = "The bot, which should be stopped: its agent stops it; a Paused or Failed one keeps that state.", body = BotResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "The caller may see the bot but not control it (Control).", body = ErrorResponse),
        (status = 404, description = "No such bot, or one the caller may not see.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn stop(
    user: AuthUser,
    state: State<AppState>,
    ip: ClientIp,
    id: ValidPath<BotId>,
) -> Result<(StatusCode, Json<BotResponse>), ApiError> {
    lifecycle(&user, state, ip, id, BotAction::Stop).await
}

/// `POST /api/v1/bots/{id}/restart`: the bot's run starts again.
#[utoipa::path(
    post,
    path = "/api/v1/bots/{id}/restart",
    operation_id = "restart_bot",
    tag = "bots",
    params(("id" = String, Path, description = "The bot's ID.")),
    responses(
        (status = 202, description = "The command went to the bot's agent, which restarts its run; the bot is as it reads now.", body = BotResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "The caller may see the bot but not control it (Control).", body = ErrorResponse),
        (status = 404, description = "No such bot, or one the caller may not see.", body = ErrorResponse),
        (status = 409, description = "`bot_not_running`: the bot should be stopped, runs nowhere, or is paused or failed; `bot_unreachable`: its agent's stream doesn't hold it now.", body = ErrorResponse),
        (status = 429, description = "Too many requests, or too many actions on this bot.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn restart(
    user: AuthUser,
    state: State<AppState>,
    ip: ClientIp,
    id: ValidPath<BotId>,
) -> Result<(StatusCode, Json<BotResponse>), ApiError> {
    lifecycle(&user, state, ip, id, BotAction::Command(Lifecycle::Restart)).await
}

/// `POST /api/v1/bots/{id}/reset`: a failed bot tries again.
#[utoipa::path(
    post,
    path = "/api/v1/bots/{id}/reset",
    operation_id = "reset_bot",
    tag = "bots",
    params(("id" = String, Path, description = "The bot's ID.")),
    responses(
        (status = 200, description = "The bot ran nowhere: its stored failed state is cleared, and its next placement starts it.", body = BotResponse),
        (status = 202, description = "The command went to the bot's agent, which tries it again.", body = BotResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "The caller may see the bot but not control it (Control).", body = ErrorResponse),
        (status = 404, description = "No such bot, or one the caller may not see.", body = ErrorResponse),
        (status = 409, description = "`bot_not_failed`: the bot hasn't failed; `bot_unreachable`: its agent's stream doesn't hold it now.", body = ErrorResponse),
        (status = 429, description = "Too many requests, or too many actions on this bot.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn reset(
    user: AuthUser,
    state: State<AppState>,
    ip: ClientIp,
    id: ValidPath<BotId>,
) -> Result<(StatusCode, Json<BotResponse>), ApiError> {
    lifecycle(&user, state, ip, id, BotAction::Command(Lifecycle::Reset)).await
}

/// `POST /api/v1/bots/{id}/resume`: a paused bot connects again.
#[utoipa::path(
    post,
    path = "/api/v1/bots/{id}/resume",
    operation_id = "resume_bot",
    tag = "bots",
    params(("id" = String, Path, description = "The bot's ID.")),
    responses(
        (status = 200, description = "The bot ran nowhere: its stored paused state is cleared, and its next placement starts it.", body = BotResponse),
        (status = 202, description = "The command went to the bot's agent, which connects it again, even over a human still playing on the account.", body = BotResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "The caller may see the bot but not control it (Control).", body = ErrorResponse),
        (status = 404, description = "No such bot, or one the caller may not see.", body = ErrorResponse),
        (status = 409, description = "`bot_not_paused`: the bot isn't paused; `bot_unreachable`: its agent's stream doesn't hold it now.", body = ErrorResponse),
        (status = 429, description = "Too many requests, or too many actions on this bot.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn resume(
    user: AuthUser,
    state: State<AppState>,
    ip: ClientIp,
    id: ValidPath<BotId>,
) -> Result<(StatusCode, Json<BotResponse>), ApiError> {
    lifecycle(&user, state, ip, id, BotAction::Command(Lifecycle::Resume)).await
}

/// One lifecycle call: authorized (`ControlBot`), the bot's limit spent
/// unless it stops the bot, carried out, and answered with the bot as it
/// reads now.
async fn lifecycle(
    user: &AuthUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<BotId>,
    action: BotAction,
) -> Result<(StatusCode, Json<BotResponse>), ApiError> {
    let actor = user.actor();
    let view = facts(&state, &actor, id).await?;
    user.require(Permission::ControlBot, &view.account.context())?;
    if action != BotAction::Stop {
        state.limits.check_bot_action(id)?;
    }
    let done = state
        .bots
        .control(&acting(user, ip), id, action)
        .await
        .map_err(writing)?;
    let status = match done {
        Controlled::Sent => StatusCode::ACCEPTED,
        Controlled::Changed | Controlled::Unchanged | Controlled::Cleared => StatusCode::OK,
    };
    Ok((status, answered(&state, &actor, id).await?))
}

/// The bot `id` as `actor` reads it, or 404.
async fn facts(state: &AppState, actor: &Actor, id: BotId) -> Result<BotView, ApiError> {
    state.bots.view(actor, id).await?.ok_or(ApiError::NotFound)
}

/// A write's answer: the bot as `actor` reads it after the write; 404 when
/// it's gone meanwhile, or `actor` may no longer see it (`ViewBot`, as
/// `get` asks).
async fn answered(
    state: &AppState,
    actor: &Actor,
    id: BotId,
) -> Result<Json<BotResponse>, ApiError> {
    let view = facts(state, actor, id).await?;
    authorize(actor, Permission::ViewBot, &view.account.context())?;
    Ok(Json(bot_response(actor, &view)?))
}

/// A bot write that wasn't made, as the API answers it.
fn writing(error: BotWriteError) -> ApiError {
    match error {
        BotWriteError::NotFound => ApiError::NotFound,
        BotWriteError::SessionGone => ApiError::Unauthorized,
        BotWriteError::Denied(error) => ApiError::from(error),
        BotWriteError::UnusableMode => {
            ApiError::Validation(vec![FieldError::new("mode_id", UNUSABLE_MODE)])
        }
        BotWriteError::Refused(refusal) => match refusal {
            CommandRefusal::NotRunning => ApiError::BotNotRunning,
            CommandRefusal::NotPaused => ApiError::BotNotPaused,
            CommandRefusal::NotFailed => ApiError::BotNotFailed,
            CommandRefusal::Unreachable => ApiError::BotUnreachable,
        },
        BotWriteError::Store(error) => ApiError::from(error),
        other @ (BotWriteError::Unsendable | BotWriteError::Audit(_)) => ApiError::internal(other),
    }
}

/// A page of bots as `viewer` sees them.
fn bot_page(viewer: &Actor, listed: &Paged<BotView, BotId>) -> Result<Json<BotPage>, ApiError> {
    Ok(Json(BotPage {
        items: listed
            .items
            .iter()
            .map(|view| bot_response(viewer, view))
            .collect::<Result<_, _>>()?,
        next: listed.next,
    }))
}

/// A bot as `viewer` sees it (S54): its account under the accounts' field
/// rule, its server for Control holders and above (PR #54's review), and
/// its agent for Admins and the Owner only.
fn bot_response(viewer: &Actor, view: &BotView) -> Result<BotResponse, ApiError> {
    let bot = &view.record.bot;
    let sees_agents = authorize(viewer, Permission::ViewAgents, &ResourceContext::Global).is_ok();
    let sees_server = effective_level(viewer, view.account.owner, view.account.grant)
        .is_some_and(|level| level >= GrantLevel::Control);
    Ok(BotResponse {
        id: bot.id,
        account: account_response(viewer, &view.account)?,
        server_address: bot
            .server
            .as_ref()
            .filter(|_| sees_server)
            .map(|server| BoundedText::new(&server.to_string())),
        mode_id: bot.mode,
        desired_state: DesiredState::from(bot.desired).into(),
        status: bot_status(&view.shown)?,
        agent: view
            .agent
            .as_ref()
            .filter(|_| sees_agents)
            .map(|agent| BotAgent {
                id: agent.id,
                name: BoundedText::new(agent.name.as_str()),
                status: ApiAgentStatus::from(agent.status).into(),
            }),
    })
}

/// What the server knows of a bot's state, as the API shows it.
fn bot_status(shown: &ShownState) -> Result<BotStatus, ApiError> {
    Ok(match shown {
        ShownState::Reported(snapshot) => BotStatus {
            since: Some(timestamp(snapshot.since)?),
            online_since: match snapshot.state {
                BotState::Online { since, .. } => Some(timestamp(since)?),
                _ => None,
            },
            attempt: snapshot.attempt().map(core::num::NonZeroU32::get),
            restarting: match snapshot.state {
                BotState::Stopping { restart } => Some(restart),
                _ => None,
            },
            last_disconnect: snapshot.last_disconnect.as_ref().map(LastDisconnect::from),
            ..stated(BotStatusKind::Reported, LastState::from(snapshot.state))
        },
        ShownState::AwaitingReport { since } => BotStatus {
            since: Some(timestamp(*since)?),
            ..BotStatus::of(BotStatusKind::AwaitingReport)
        },
        ShownState::Unknown {
            agent_last_seen,
            last_known,
            ..
        } => {
            let status = match last_known {
                Some(last) => BotStatus {
                    since: Some(timestamp(last.at)?),
                    ..stated(BotStatusKind::Unknown, last.state)
                },
                None => BotStatus::of(BotStatusKind::Unknown),
            };
            BotStatus {
                agent_last_seen_at: agent_last_seen.map(timestamp).transpose()?,
                ..status
            }
        }
        ShownState::Unplaced { why, sticky } => {
            let state = sticky.map_or(LastState::Stopped, |sticky| {
                LastState::from(BotState::from(sticky))
            });
            BotStatus {
                why: Some(UnplacedReason::from(*why).into()),
                ..stated(BotStatusKind::Unplaced, state)
            }
        }
    })
}

/// A status of `kind` in `state`, with its reason when it has one.
fn stated(kind: BotStatusKind, state: LastState) -> BotStatus {
    BotStatus {
        state: Some(BotStateKind::from(state).into()),
        reason: BotStateReason::of(state).map(Into::into),
        ..BotStatus::of(kind)
    }
}
