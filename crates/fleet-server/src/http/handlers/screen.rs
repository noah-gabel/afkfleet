//! What a bot sees, on demand (Plan.md P16.20; ADR-0036, M14):
//! `GET /api/v1/bots/{id}/screen`.
//!
//! - **Who:** anyone who may see the bot (`ViewBot`), the level that reads
//!   its chat; below it the bot is 404, like an unknown one.
//! - **Its limit:** each look asks the bot's agent, so it spends the
//!   caller's own `[rate_limits] screen_reads_per_user_per_min`, right after
//!   they're authorized, so a stranger learns nothing of the bot.
//! - **Never a hang:** the answer comes within the service's wait whatever
//!   the agent does; an agent of the last release is never asked, and a bot
//!   that isn't online isn't either. It reads only: no audit entry.

use axum::Json;
use axum::extract::State;
use fleet_api_types::ErrorResponse;
use fleet_api_types::screen::{BotScreenKind, BotScreenResponse};
use fleet_core::authz::{Permission, authorize};
use fleet_core::id::BotId;
use fleet_proto::control::Seen;

use crate::app::bots::{BotSight, ScreenError};
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, ValidPath};
use crate::http::router::AppState;

/// `GET /api/v1/bots/{id}/screen`: what the bot sees, asked of its agent.
#[utoipa::path(
    get,
    path = "/api/v1/bots/{id}/screen",
    operation_id = "get_bot_screen",
    tag = "bots",
    params(("id" = String, Path, description = "The bot's ID.")),
    responses(
        (status = 200, description = "What the bot sees, as its agent read it: `screen` with the screen it has open, or `inventory` with its own slots when none is; each slot with its number and, in a grid, its row and column, as a script names them. Or why that can't be said: `offline` (it isn't in a world on a live agent), `agent_too_old` (its agent runs a version from before script API level 2, which isn't asked: it needs the next release), `no_answer` (its agent didn't answer within 5 s). Every text is the Minecraft server's, as plain text.", body = BotScreenResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 404, description = "No such bot, or one the caller may not see.", body = ErrorResponse),
        (status = 429, description = "Too many looks: the caller's own `screen_reads_per_user_per_min`.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = 503, description = "`busy`: the bot's agent is looking at this bot already, or as many looks wait on its stream as it takes.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn screen(
    user: AuthUser,
    State(state): State<AppState>,
    ValidPath(id): ValidPath<BotId>,
) -> Result<Json<BotScreenResponse>, ApiError> {
    let actor = user.actor();
    let Some(view) = state.bots.view(&actor, id).await? else {
        return Err(ApiError::NotFound);
    };
    authorize(&actor, Permission::ViewBot, &view.account.context()).map_err(ApiError::from)?;
    state.limits.check_screen_read(actor.id)?;
    let sight = state
        .bots
        .screen(&actor, id)
        .await
        .map_err(|error| match error {
            // Between the two reads the bot went, or the caller's view of it.
            ScreenError::NotFound | ScreenError::Denied(_) => ApiError::NotFound,
            ScreenError::Busy => ApiError::busy(),
            ScreenError::Store(error) => ApiError::from(error),
            other @ ScreenError::Unsendable => ApiError::internal(other),
        })?;
    Ok(Json(answer(sight)))
}

/// `sight` as the API answers it.
fn answer(sight: BotSight) -> BotScreenResponse {
    match sight {
        BotSight::Seen(seen) => {
            let Seen {
                inventory,
                screen,
                sign,
            } = *seen;
            let sign = sign.map(|sign| (sign.at, sign.side));
            match screen {
                Some(screen) => BotScreenResponse::of_screen(&screen, sign),
                None => BotScreenResponse::of_inventory(&inventory, sign),
            }
        }
        BotSight::Offline => BotScreenResponse::unseen(BotScreenKind::Offline),
        BotSight::AgentTooOld => BotScreenResponse::unseen(BotScreenKind::AgentTooOld),
        BotSight::NoAnswer => BotScreenResponse::unseen(BotScreenKind::NoAnswer),
    }
}
