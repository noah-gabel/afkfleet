//! The Owner's live events (Plan.md P11.7; ADR-0020, S58): fleet-client's
//! own socket, read by a task of its own on the case's runtime, so pings are
//! answered while the case waits on an API call. The task keeps every update
//! and then logs one line for it, the kind and the bot's ID, so each update
//! wakes [`Logs::until`](super::logs::Logs::until).
//!
//! A step compares frames with what `GET /bots/{id}` answers at that moment
//! ([`World::live_settles`](super::world::World::live_settles)): at each wake
//! only the newest `bot` frame not yet looked at gets a GET, and the frames
//! before it in that wake are skipped, so a repeated frame is just another
//! one, and nothing polls the API.

use std::sync::{Arc, Mutex};

use fleet_api_types::bots::BotResponse;
use fleet_api_types::chat::ChatLine;
use fleet_api_types::events::{EventMessage, EventMessageKind};
use fleet_api_types::ids::ChatRequestId;
use fleet_client::{LiveEnd, LiveEvents, LiveStop, LiveUpdate, MemoryStore, RefetchReason};
use fleet_core::id::BotId;
use tokio::task::JoinSet;
use tracing::debug;

use super::logs::PATIENCE;

/// One update, as the case keeps it.
#[derive(Debug, Clone)]
pub(crate) enum Update {
    /// Fetch everything again.
    Refetch(RefetchReason),
    /// An event.
    Event(Box<EventMessage>),
    /// The socket went down: why, and its wait.
    Disconnected(String),
    /// The socket ended.
    Ended(LiveEnd),
}

impl Update {
    /// The update's kind, for its log line.
    fn kind(&self) -> String {
        match self {
            Self::Refetch(_) => "refetch".to_owned(),
            Self::Event(event) => event.kind.as_str().to_owned(),
            Self::Disconnected(_) => "disconnected".to_owned(),
            Self::Ended(_) => "ended".to_owned(),
        }
    }

    /// The bot an event is about.
    fn bot(&self) -> Option<BotId> {
        match self {
            Self::Event(event) => event.bot_id,
            _ => None,
        }
    }
}

/// The Owner's socket and what it said; see the [module docs](self).
#[derive(Debug)]
pub(crate) struct Live {
    seen: Arc<Mutex<Vec<Update>>>,
    /// How many of the updates a step has looked at.
    compared: Mutex<usize>,
    stop: LiveStop,
    reader: JoinSet<()>,
}

impl Live {
    /// Reads `live` in a task of its own until it ends.
    pub(crate) fn open(mut live: LiveEvents<MemoryStore>) -> Self {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = live.stopper();
        let mut reader = JoinSet::new();
        let kept = Arc::clone(&seen);
        reader.spawn(async move {
            loop {
                // No timeout: the socket waits for the server's next frame,
                // and `Live::close` stops it.
                let update = match live.recv().await {
                    Ok(LiveUpdate::Refetch(reason)) => Update::Refetch(reason),
                    Ok(LiveUpdate::Event(event)) => Update::Event(event),
                    Ok(LiveUpdate::Disconnected { why, retry_in }) => {
                        Update::Disconnected(format!("{why:?}, again in {retry_in:?}"))
                    }
                    Err(end) => Update::Ended(end),
                };
                let ended = matches!(update, Update::Ended(_));
                let (kind, bot) = (update.kind(), update.bot());
                kept.lock().unwrap().push(update);
                // After the update is kept, so the wait it wakes finds it.
                debug!(kind = %kind, bot_id = ?bot, "live update");
                if ended {
                    return;
                }
            }
        });
        Self {
            seen,
            compared: Mutex::new(0),
            stop,
            reader,
        }
    }

    /// Every update so far.
    pub(crate) fn updates(&self) -> Vec<Update> {
        self.seen.lock().unwrap().clone()
    }

    /// How many times the socket asked to fetch everything.
    pub(crate) fn refetches(&self) -> usize {
        self.updates()
            .iter()
            .filter(|update| matches!(update, Update::Refetch(_)))
            .count()
    }

    /// Each time `updates` say the socket went down: why, and its wait.
    pub(crate) fn drops(updates: &[Update]) -> Vec<String> {
        updates
            .iter()
            .filter_map(|update| match update {
                Update::Disconnected(why) => Some(why.clone()),
                _ => None,
            })
            .collect()
    }

    /// The newest `bot` frame of `bot` that no step has looked at yet; every
    /// update so far counts as looked at from now on.
    pub(crate) fn newest_bot_frame(&self, bot: BotId) -> Option<BotResponse> {
        let seen = self.seen.lock().unwrap();
        let mut compared = self.compared.lock().unwrap();
        let fresh = seen.get(*compared..).unwrap_or_default();
        *compared = seen.len();
        fresh.iter().rev().find_map(|update| match update {
            Update::Event(event)
                if event.kind.known() == Some(EventMessageKind::Bot)
                    && event.bot_id == Some(bot) =>
            {
                event.bot.clone()
            }
            _ => None,
        })
    }

    /// The chat lines the socket brought for `bot` from its server, in order:
    /// those no user's request sent.
    pub(crate) fn lines_in(&self, bot: BotId) -> Vec<ChatLine> {
        self.chat(bot)
            .into_iter()
            .filter(|(_, request, _)| request.is_none())
            .map(|(_, _, line)| line)
            .collect()
    }

    /// Where `request`'s `chat` frame and its `chat_sent` came for `bot`,
    /// as indexes among the updates, with the frame's line.
    pub(crate) fn sent(
        &self,
        bot: BotId,
        request: ChatRequestId,
    ) -> (Option<(usize, ChatLine)>, Option<usize>) {
        let line = self
            .chat(bot)
            .into_iter()
            .find(|(_, of, _)| *of == Some(request))
            .map(|(at, _, line)| (at, line));
        let sent = self.updates().iter().position(|update| match update {
            Update::Event(event) => {
                event.kind.known() == Some(EventMessageKind::ChatSent)
                    && event.bot_id == Some(bot)
                    && event.request == Some(request)
            }
            _ => false,
        });
        (line, sent)
    }

    /// `bot`'s `chat` frames: where each came, its request, its line.
    fn chat(&self, bot: BotId) -> Vec<(usize, Option<ChatRequestId>, ChatLine)> {
        self.updates()
            .into_iter()
            .enumerate()
            .filter_map(|(at, update)| match update {
                Update::Event(event)
                    if event.kind.known() == Some(EventMessageKind::Chat)
                        && event.bot_id == Some(bot) =>
                {
                    Some((at, event.request, event.line?))
                }
                _ => None,
            })
            .collect()
    }

    /// Stops the socket, which sends its close, and waits for its reader.
    pub(crate) async fn close(mut self) {
        self.stop.stop();
        tokio::time::timeout(PATIENCE, self.reader.join_next())
            .await
            .expect("the socket's reader ends in time")
            .expect("a reader")
            .expect("the reader ran to its end");
        let last = self.updates().pop();
        assert!(
            matches!(last, Some(Update::Ended(LiveEnd::Stopped))),
            "{last:?}"
        );
    }
}
