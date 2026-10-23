//! A bot's chat routes (Plan.md P11.3, Appendix B; ADR-0020, S3–S11).
//!
//! - **`GET /api/v1/bots/{id}/chat`** (authed) pages the bot's chat newest
//!   first for a viewer who may see the bot (View), each line under its
//!   field rules ([`super::chat_line`]): a command's arguments only for
//!   Manage holders, who sent a user's line only for viewers who see the
//!   account's owner, `mine` on the viewer's own lines. It answers in the
//!   extractors' order: 401, the per-user 429, a malformed path's 404, the
//!   query's 400/422, then an unseen bot's 404.
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
//! as the user's and recorded either way once it's sent (S7). The wait
//! counts from the request's [`Arrival`], as the request timeout does.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fleet_api_types::chat::{
    ChatCommandRequest, ChatFailure, ChatPage, ChatPageQuery, ChatSendOutcome, ChatSendRequest,
    ChatSendResponse,
};
use fleet_api_types::{ErrorResponse, SafeInt};
use fleet_core::authz::{Permission, effective_level};
use fleet_core::bot::ChatRefusal;
use fleet_core::chat::ChatMessage;
use fleet_core::id::BotId;
use tokio::time::Instant;

use super::bots::{facts, writing};
use super::{acting, chat_failure, chat_line};
use crate::app::bots::{ChatHistoryError, ChatOutcome, ChatRoute, ChatSendDone, ChatSendError};
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, SteppedUpUser, ValidJson, ValidPath, ValidQuery};
use crate::http::middleware::Arrival;
use crate::http::rate_limit::{ClientIp, RateLimits};
use crate::http::router::AppState;
use crate::ports::chat::ChatMessageId;
use crate::ports::page::{Page, PageLimit};

/// `GET /api/v1/bots/{id}/chat`: one page of the bot's chat, newest first.
#[utoipa::path(
    get,
    path = "/api/v1/bots/{id}/chat",
    operation_id = "get_bot_chat",
    tag = "bots",
    params(("id" = String, Path, description = "The bot's ID."), ChatPageQuery),
    responses(
        (status = 200, description = "One page of the bot's chat, newest first by ID, which is the order the server stored the lines in. Each line is as the caller may see it: a command's arguments only for Manage holders, and who had a user's line sent only for callers who see the account's owner.", body = ChatPage),
        (status = 400, description = "A query that doesn't parse: an unknown parameter, one given twice, or a value that isn't a whole number the parameter can hold.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 404, description = "No such bot, or one the caller may not see.", body = ErrorResponse),
        (status = 422, description = "A `limit` outside 1 to 100, or a `before` below 1.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn history(
    user: AuthUser,
    State(state): State<AppState>,
    ValidPath(id): ValidPath<BotId>,
    ValidQuery(query): ValidQuery<ChatPageQuery>,
) -> Result<Json<ChatPage>, ApiError> {
    let actor = user.actor();
    let history = state
        .bots
        .chat(&actor, id, chat_page(&query)?)
        .await
        .map_err(reading)?;
    let level = effective_level(&actor, history.account.owner, history.account.grant)
        .ok_or_else(|| ApiError::internal("a bot the viewer may not see was answered"))?;
    let items = history
        .lines
        .items
        .iter()
        .map(|line| chat_line(&actor, level, line))
        .collect::<Result<_, _>>()?;
    let next = history
        .lines
        .next
        .map(|cursor| SafeInt::try_new(cursor.get()))
        .transpose()
        .map_err(ApiError::internal)?;
    Ok(Json(ChatPage { items, next }))
}

/// The page the chat's query asks for. garde has checked both values, so
/// one out of range is a bug.
fn chat_page(query: &ChatPageQuery) -> Result<Page<ChatMessageId>, ApiError> {
    Ok(Page {
        before: query.before.map(|cursor| ChatMessageId::new(cursor.get())),
        limit: PageLimit::new(query.limit_or_default()).map_err(ApiError::internal)?,
    })
}

/// A history that couldn't be read, as the API answers it: an unknown bot
/// and one the caller may not see alike, 404.
fn reading(error: ChatHistoryError) -> ApiError {
    match error {
        ChatHistoryError::NotFound => ApiError::NotFound,
        ChatHistoryError::Denied(denied) => ApiError::from(denied),
        ChatHistoryError::Store(store) => ApiError::from(store),
    }
}

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
        (status = 422, description = "A line that's empty, too long, or holds a line break or another control character, `§` or text-direction marks.", body = ErrorResponse),
        (status = 429, description = "Too many requests, too many lines from the caller or to this bot, or the bot's agent is at its chat rate.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn send(
    user: AuthUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    arrival: Arrival,
    ValidPath(id): ValidPath<BotId>,
    ValidJson(request): ValidJson<ChatSendRequest>,
) -> Result<(StatusCode, Json<ChatSendResponse>), ApiError> {
    let message = line(&request.message)?;
    send_line(
        &user,
        &state,
        ip,
        id,
        &message,
        ChatRoute::Chat,
        arrival.instant(),
    )
    .await
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
        (status = 422, description = "A line that isn't a command, or that's empty, too long, or holds a line break or another control character, `§` or text-direction marks.", body = ErrorResponse),
        (status = 429, description = "Too many requests, too many lines from the caller or to this bot, or the bot's agent is at its chat rate.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn command(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    arrival: Arrival,
    ValidPath(id): ValidPath<BotId>,
    ValidJson(request): ValidJson<ChatCommandRequest>,
) -> Result<(StatusCode, Json<ChatSendResponse>), ApiError> {
    let message = line(&request.message)?;
    send_line(
        user.user(),
        &state,
        ip,
        id,
        &message,
        ChatRoute::Command,
        arrival.instant(),
    )
    .await
}

/// One line through `route`, whose request arrived at `started`: authorized
/// (`SendChat` with the allowlist's check), pointed to `/command` when
/// `/chat` got an unlisted command, the user's and the bot's chat limits
/// spent, then sent and answered within the wait counted from the arrival.
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
/// it (S5): the agent's reasons through [`chat_failure`], the match the live
/// `chat_failed` shares (S34).
fn chatting(error: ChatSendError, limits: &RateLimits) -> ApiError {
    match error {
        ChatSendError::Write(error) => writing(error),
        ChatSendError::Refused(ChatRefusal::NotOnline) => ApiError::BotNotOnline,
        ChatSendError::Refused(ChatRefusal::Unreachable) => ApiError::BotUnreachable,
        ChatSendError::Failed(reason) => match chat_failure(reason) {
            ChatFailure::BotNotOnline => ApiError::BotNotOnline,
            ChatFailure::ChatUnavailable => ApiError::ChatUnavailable,
            ChatFailure::RateLimited => ApiError::RateLimited {
                retry_after: limits.chat_retry_after(),
            },
            ChatFailure::BotUnreachable => ApiError::BotUnreachable,
        },
        // Only a missing check in a handler here gets a line to the service
        // through a route that doesn't take it: a bug (S10).
        other @ (ChatSendError::Mint(_) | ChatSendError::WrongRoute(_)) => {
            ApiError::internal(other)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::DateTime;
    use fleet_proto::control::ChatFailedReason;
    use fleet_testkit::system::ManualClock;

    use super::*;
    use crate::config::RateLimitConfig;

    /// A line the service refused for its route reached it only because the
    /// handler's check is missing: a bug, the internal error (S10).
    #[test]
    fn a_line_off_its_routes_rule_is_the_internal_error() {
        let limits = RateLimits::new(
            &RateLimitConfig::default(),
            Arc::new(ManualClock::new(DateTime::UNIX_EPOCH)),
        );

        for route in [ChatRoute::Chat, ChatRoute::Command] {
            let answered = chatting(ChatSendError::WrongRoute(route), &limits);

            assert!(
                matches!(answered, ApiError::Internal(_)),
                "{route:?}: {answered:?}"
            );
        }
    }

    /// S34: a live `chat_failed` names a failure with the very code the
    /// `POST` answers it with.
    #[test]
    fn a_failures_code_on_the_socket_is_the_posts_answer() {
        let limits = RateLimits::new(
            &RateLimitConfig::default(),
            Arc::new(ManualClock::new(DateTime::UNIX_EPOCH)),
        );
        let reasons = [
            ChatFailedReason::NotOnline,
            ChatFailedReason::Disconnected,
            ChatFailedReason::NotInWorld,
            ChatFailedReason::ChatUnavailable,
            ChatFailedReason::RateLimited,
            ChatFailedReason::QueueFull,
            ChatFailedReason::TimedOut,
            ChatFailedReason::SessionBusy,
            ChatFailedReason::AgentBusy,
            ChatFailedReason::UnknownBot,
            ChatFailedReason::ShuttingDown,
        ];

        for reason in reasons {
            let answered = chatting(ChatSendError::Failed(reason), &limits);

            assert_eq!(
                fleet_api_types::OpenEnum::as_str(super::super::chat_failure(reason)),
                fleet_api_types::OpenEnum::as_str(answered.code()),
                "{reason:?}"
            );
        }
    }
}
