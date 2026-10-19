//! A bot's chat routes (Plan.md P11.3, Appendix B; ADR-0020, S3–S8).
//!
//! - **`POST /api/v1/bots/{id}/chat`** (authed) sends plain chat or a command
//!   on the allowlist: Control (`SendChat`). It never sends a command off the
//!   allowlist: a Manage holder gets 403 `command_needs_step_up`, the pointer
//!   to the next route, before any chat limit is spent (S4).
//! - **`POST /api/v1/bots/{id}/command`** (step-up) sends a command: Control
//!   for an allowlisted one, Manage for any other, as `/chat` authorizes it;
//!   a line that isn't a command is 422 (S8).
//!
//! Both answer in the extractors' order: 401, the per-user 429, the step-up's
//! 403, a malformed path's 404, the body's 400/415/422, an unseen bot's 404,
//! 403, the pointer, the chat 429s (the user's, then the bot's, spent
//! whatever then happens, so neither tells a stranger the bot exists), then
//! what the send answers: 200 when the bot's agent said the bot sent it, 202
//! when no answer came in time, 409 or 429 with why (S5). The line is stored
//! as the user's and recorded either way once it's sent (S7).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fleet_api_types::chat::{
    ChatCommandRequest, ChatSendOutcome, ChatSendRequest, ChatSendResponse,
};
use fleet_api_types::{ErrorResponse, SafeInt};
use fleet_core::authz::Permission;
use fleet_core::bot::ChatRefusal;
use fleet_core::chat::ChatMessage;
use fleet_core::id::BotId;
use fleet_proto::control::ChatFailedReason;
use tokio::time::Instant;

use super::acting;
use super::bots::{facts, writing};
use crate::app::bots::{ChatOutcome, ChatRoute, ChatSendDone, ChatSendError};
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, SteppedUpUser, ValidJson, ValidPath};
use crate::http::rate_limit::{ClientIp, RateLimits};
use crate::http::router::AppState;

/// `POST /api/v1/bots/{id}/chat`: a line for the bot to send.
#[utoipa::path(
    post,
    path = "/api/v1/bots/{id}/chat",
    operation_id = "send_bot_chat",
    tag = "bots",
    params(("id" = String, Path, description = "The bot's ID.")),
    request_body = ChatSendRequest,
    responses(
        (status = 200, description = "The bot's agent said the bot sent the line. The line is in the bot's chat as the caller's, which records what was asked, not that it reached the Minecraft server.", body = ChatSendResponse),
        (status = 202, description = "The line went to the bot's agent, and no answer came in time: it may yet be sent or refused.", body = ChatSendResponse),
        (status = 400, description = "A body of the wrong shape.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "`command_needs_step_up`: a command off the allowlist, which only `POST /bots/{id}/command` sends, after a step-up; `forbidden`: the caller may see the bot but not chat through it (Control), or not send that command (Manage).", body = ErrorResponse),
        (status = 404, description = "No such bot, or one the caller may not see.", body = ErrorResponse),
        (status = 409, description = "`bot_not_online`: the bot isn't online; `chat_unavailable`: its Minecraft server takes only signed chat, and the bot can't sign this line; `bot_unreachable`: its agent can't be reached, or didn't get the line through.", body = ErrorResponse),
        (status = 422, description = "A line that's empty, more than one line, too long, or holds `§` or text-direction marks.", body = ErrorResponse),
        (status = 429, description = "Too many requests, too many lines from the caller or to this bot, or the bot's agent is at its chat rate.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn send(
    user: AuthUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<BotId>,
    ValidJson(request): ValidJson<ChatSendRequest>,
) -> Result<(StatusCode, Json<ChatSendResponse>), ApiError> {
    let started = Instant::now();
    let message = line(&request.message)?;
    send_line(&user, &state, ip, id, &message, ChatRoute::Chat, started).await
}

/// `POST /api/v1/bots/{id}/command`: a command for the bot to send, after a
/// step-up.
#[utoipa::path(
    post,
    path = "/api/v1/bots/{id}/command",
    operation_id = "send_bot_command",
    tag = "bots",
    params(("id" = String, Path, description = "The bot's ID.")),
    request_body = ChatCommandRequest,
    responses(
        (status = 200, description = "The bot's agent said the bot sent the command. It's in the bot's chat as the caller's, which records what was asked, not that it reached the Minecraft server.", body = ChatSendResponse),
        (status = 202, description = "The command went to the bot's agent, and no answer came in time: it may yet be sent or refused.", body = ChatSendResponse),
        (status = 400, description = "A body of the wrong shape.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or the caller may see the bot but not send this command: Control for one on the allowlist, Manage for any other.", body = ErrorResponse),
        (status = 404, description = "No such bot, or one the caller may not see.", body = ErrorResponse),
        (status = 409, description = "`bot_not_online`: the bot isn't online; `chat_unavailable`: its Minecraft server takes only signed chat, and the bot can't sign this command; `bot_unreachable`: its agent can't be reached, or didn't get the command through.", body = ErrorResponse),
        (status = 422, description = "A line that isn't a command, or that's empty, more than one line, too long, or holds `§` or text-direction marks.", body = ErrorResponse),
        (status = 429, description = "Too many requests, too many lines from the caller or to this bot, or the bot's agent is at its chat rate.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn command(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<BotId>,
    ValidJson(request): ValidJson<ChatCommandRequest>,
) -> Result<(StatusCode, Json<ChatSendResponse>), ApiError> {
    let started = Instant::now();
    let message = line(&request.message)?;
    send_line(
        user.user(),
        &state,
        ip,
        id,
        &message,
        ChatRoute::Command,
        started,
    )
    .await
}

/// One line through `route`: authorized (`SendChat` with the allowlist's
/// check), pointed to `/command` when `/chat` got an unlisted command, the
/// user's and the bot's chat limits spent, then sent and answered.
async fn send_line(
    user: &AuthUser,
    state: &AppState,
    ip: std::net::IpAddr,
    id: BotId,
    message: &ChatMessage,
    route: ChatRoute,
    started: Instant,
) -> Result<(StatusCode, Json<ChatSendResponse>), ApiError> {
    let actor = user.actor();
    let check = state.bots.command_check(message);
    let view = facts(state, &actor, id).await?;
    user.require(Permission::SendChat(check), &view.account.context())?;
    if route == ChatRoute::Chat && check.needs_manage() {
        return Err(ApiError::CommandNeedsStepUp);
    }
    state.limits.check_chat_user(actor.id)?;
    state.limits.check_chat_bot(id)?;
    let done = state
        .bots
        .send_chat(&acting(user, ip), id, message, route, started)
        .await
        .map_err(|error| chatting(error, &state.limits))?;
    answered(done)
}

/// The line garde already checked, as a `ChatMessage`.
fn line(text: &str) -> Result<ChatMessage, ApiError> {
    ChatMessage::try_from(text)
        .map_err(|_| ApiError::internal("the rule already holds: garde checked it"))
}

/// A sent line's answer: 200 when its agent said it went out, 202 pending.
fn answered(done: ChatSendDone) -> Result<(StatusCode, Json<ChatSendResponse>), ApiError> {
    let (status, outcome) = match done.outcome {
        ChatOutcome::Sent => (StatusCode::OK, ChatSendOutcome::Sent),
        ChatOutcome::Pending => (StatusCode::ACCEPTED, ChatSendOutcome::Pending),
    };
    Ok((
        status,
        Json(ChatSendResponse {
            request_id: done.request,
            message_id: SafeInt::try_new(done.message.get()).map_err(ApiError::internal)?,
            outcome: outcome.into(),
        }),
    ))
}

/// A line that wasn't sent, or that its agent refused, as the API answers
/// it (S5): one match over the agent's reasons, without a wildcard, so a
/// reason the protocol adds can't go unanswered.
fn chatting(error: ChatSendError, limits: &RateLimits) -> ApiError {
    match error {
        ChatSendError::Write(error) => writing(error),
        ChatSendError::Refused(ChatRefusal::NotOnline) => ApiError::BotNotOnline,
        ChatSendError::Refused(ChatRefusal::Unreachable) => ApiError::BotUnreachable,
        ChatSendError::Failed(reason) => match reason {
            ChatFailedReason::NotOnline
            | ChatFailedReason::Disconnected
            | ChatFailedReason::NotInWorld => ApiError::BotNotOnline,
            ChatFailedReason::ChatUnavailable => ApiError::ChatUnavailable,
            ChatFailedReason::RateLimited | ChatFailedReason::QueueFull => ApiError::RateLimited {
                retry_after: limits.chat_retry_after(),
            },
            ChatFailedReason::TimedOut
            | ChatFailedReason::SessionBusy
            | ChatFailedReason::AgentBusy
            | ChatFailedReason::UnknownBot
            | ChatFailedReason::ShuttingDown => ApiError::BotUnreachable,
        },
        other @ ChatSendError::Mint(_) => ApiError::internal(other),
    }
}
