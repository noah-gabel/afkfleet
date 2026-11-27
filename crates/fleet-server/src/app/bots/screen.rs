//! What a bot sees, on demand (Plan.md P16.20; ADR-0036, M14).
//!
//! [`BotService::screen`] reads the bot as its viewer sees it and asks
//! `authorize()` for `ViewBot`: a bot the viewer may not see is an unknown
//! one. It asks the bot's agent only when the live stream of that agent
//! holds the bot assigned, the bot was last reported online on it, and the
//! agent runs script API level 2: an agent of the last release reads a
//! screen request as a message it doesn't know and ends its stream, so it's
//! never sent one. The answer comes from that stream only, within
//! [`SCREEN_WAIT`]; whatever the agent does, the viewer gets an answer by
//! then. It reads only: nothing is written or recorded.

use core::time::Duration;

use fleet_core::authz::{Actor, AuthzError, Permission, authorize};
use fleet_core::bot::{LastState, Reach};
use fleet_core::id::BotId;
use fleet_core::script::ScriptApiLevel;
use fleet_proto::control::{ScreenOutcome, ScreenUnseen, Seen};
use tracing::{debug, error};

use super::BotService;
use crate::ports::live_agents::ScreenRefused;
use crate::ports::store::StoreError;

/// How long a look at a bot's screen waits for its agent: the agent's own
/// bound, 2 s, and a second's margin each way for its conductor and the
/// stream.
pub const SCREEN_WAIT: Duration = Duration::from_secs(5);

/// What a viewer learns of what a bot sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotSight {
    /// What it sees, as its agent read it.
    Seen(Box<Seen>),
    /// It isn't in a world on a live agent: no agent holds it, its agent's
    /// stream is down, or it's stopped, connecting or between worlds.
    Offline,
    /// Its agent runs a version from before script API level 2, which
    /// can't be asked: it needs the next release.
    AgentTooOld,
    /// Its agent didn't answer within [`SCREEN_WAIT`], or its session
    /// didn't.
    NoAnswer,
}

/// Why a bot's screen wasn't looked at.
#[derive(Debug, thiserror::Error)]
pub enum ScreenError {
    /// No such bot.
    #[error("no such bot")]
    NotFound,
    /// `authorize()` refused: a bot the viewer may not see.
    #[error(transparent)]
    Denied(AuthzError),
    /// As many looks wait on its agent's stream as it takes, or the agent
    /// looks at this bot already: try again in a moment.
    #[error("the bot's agent is busy looking")]
    Busy,
    /// The request couldn't be written, which a valid one never is: a bug.
    #[error("the screen request can't be written")]
    Unsendable,
    /// The database.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl BotService {
    /// The service, waiting `screen_wait` for a bot's agent to say what the
    /// bot sees, instead of [`SCREEN_WAIT`]: a test's shorter bound.
    #[must_use]
    pub fn with_screen_wait(self, screen_wait: Duration) -> Self {
        Self {
            screen_wait,
            ..self
        }
    }

    /// What the bot `id` sees, for `viewer`; see the [module docs](self).
    ///
    /// # Errors
    /// [`ScreenError::NotFound`] for an unknown bot,
    /// [`ScreenError::Denied`] for one the viewer may not see,
    /// [`ScreenError::Busy`], [`ScreenError::Unsendable`] and
    /// [`ScreenError::Store`].
    pub async fn screen(&self, viewer: &Actor, id: BotId) -> Result<BotSight, ScreenError> {
        let Some(listed) = self.store.bots().listed(id, viewer.id).await? else {
            return Err(ScreenError::NotFound);
        };
        authorize(viewer, Permission::ViewBot, &listed.account.context())
            .map_err(ScreenError::Denied)?;
        let bot = &listed.record.bot;
        let reached = self.reach(bot).await?;
        let (Reach::Held, Some(agent), Some(generation)) =
            (reached.reach, bot.agent, reached.stream)
        else {
            return Ok(BotSight::Offline);
        };
        if !matches!(reached.known, Some(LastState::Online)) {
            return Ok(BotSight::Offline);
        }
        // Checked here so the stream is never asked; the registry refuses
        // it too, at the moment of sending.
        let too_old = self
            .live
            .live(agent)
            .is_none_or(|stream| !ScriptApiLevel::V2.runs_on(stream.script_api));
        if too_old {
            return Ok(BotSight::AgentTooOld);
        }
        let wait = match self.live.read_screen(agent, generation, bot.id) {
            Ok(wait) => wait,
            Err(
                ScreenRefused::NotHeld | ScreenRefused::NotConnected | ScreenRefused::QueueFull,
            ) => {
                return Ok(BotSight::Offline);
            }
            Err(ScreenRefused::AgentTooOld) => return Ok(BotSight::AgentTooOld),
            Err(ScreenRefused::Busy) => return Err(ScreenError::Busy),
            Err(ScreenRefused::Unsendable) => {
                error!(bot_id = %bot.id, agent_id = %agent, "a screen request couldn't be written");
                return Err(ScreenError::Unsendable);
            }
        };
        debug!(user_id = %viewer.id, bot_id = %bot.id, agent_id = %agent, "a bot's agent was asked what the bot sees");
        match tokio::time::timeout(self.screen_wait, wait).await {
            Ok(Ok(ScreenOutcome::Seen(seen))) => Ok(BotSight::Seen(seen)),
            Ok(Ok(ScreenOutcome::Unseen(ScreenUnseen::NotOnline | ScreenUnseen::UnknownBot))) => {
                Ok(BotSight::Offline)
            }
            Ok(Ok(ScreenOutcome::Unseen(ScreenUnseen::Busy))) => Err(ScreenError::Busy),
            // Unanswered: the agent's session didn't, it didn't in time, or
            // its stream ended meanwhile.
            Ok(Ok(ScreenOutcome::Unseen(ScreenUnseen::TimedOut)) | Err(_)) | Err(_) => {
                Ok(BotSight::NoAnswer)
            }
        }
    }
}
