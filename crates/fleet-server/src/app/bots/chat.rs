//! A user's chat line to a bot (Plan.md P11.3; ADR-0020, S4–S8).
//!
//! [`BotService::send_chat`] carries out both chat routes:
//! - **One write transaction** reads the acting user, the bot and its
//!   account again, asks `authorize()` again (`SendChat` with the
//!   allowlist's check of the line), and judges whether the bot can take a
//!   line now ([`judge_chat`]); a refusal writes and records nothing. It then
//!   mints the line's [`ChatRequestId`], appends the line to the bot's chat
//!   as the user's (S6, U3), and commits with `bot.send_chat` or
//!   `bot.send_command`, whose metadata never holds the text (S6), all before
//!   anything is sent.
//! - **Then it subscribes to the bus and sends** the line to the stream that
//!   holds the bot, checked and queued at once as a lifecycle command is. A
//!   refusal after the commit keeps the row and the entry: the row records
//!   what the user asked to send, never whether it went out (S7).
//! - **It waits** for the agent's `ChatSent` or `ChatFailed` until the
//!   earlier of the send plus [`ChatWait::ANSWER`] and the request's arrival
//!   plus the request timeout less [`ChatWait::MARGIN`] (S5). The arrival is
//!   the instant the request timeout counts from too, so its 408 can't fire
//!   after a line was sent (PR #60's review). The answer
//!   matches the request, the bot and the agent, on any of the agent's
//!   streams, since an answer may come after a reconnect. Nothing in time,
//!   or a subscriber that fell behind, is pending: the line may yet go out.

use core::time::Duration;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditError, AuditMetadata};
use fleet_core::authz::{CommandCheck, Permission, authorize};
use fleet_core::bot::{ChatFacts, ChatRefusal, judge_chat};
use fleet_core::chat::ChatMessage;
use fleet_core::id::{AgentId, BotId, ChatRequestId};
use fleet_core::system::{MintError, mint};
use fleet_core::value::Username;
use fleet_proto::control::ChatFailedReason;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{Instant, timeout_at};
use tracing::{debug, info};

use super::{BotService, BotWriteError, reread};
use crate::app::events::{BusEvent, LiveEvent, LiveEventKind, NotSent};
use crate::app::users::{ActingUser, Rechecked};
use crate::ports::chat::{ChatEntry, ChatMessageId, ChatRecord, ListedChat, NewChatMessage};
use crate::ports::live_agents::CommandRefused;

/// Which route a line came through, which its audit entry names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRoute {
    /// `POST /bots/{id}/chat`: plain chat or an allowlisted command.
    Chat,
    /// `POST /bots/{id}/command`: a command, after a step-up.
    Command,
}

impl ChatRoute {
    /// Whether this route takes `message`, whose allowlist check is `check`
    /// (S4, S8): `/chat` takes plain chat and allowlisted commands, never a
    /// command off the allowlist; `/command` takes commands only.
    #[must_use]
    pub fn takes(self, message: &ChatMessage, check: CommandCheck) -> bool {
        match self {
            Self::Chat => !check.needs_manage(),
            Self::Command => message.is_command(),
        }
    }
}

/// What came of a line the server sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatOutcome {
    /// The bot's agent said the bot sent it.
    Sent,
    /// No answer came in time, or the wait fell behind the bus.
    Pending,
}

/// A line that went to its bot's agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatSendDone {
    /// The chat request's ID.
    pub request: ChatRequestId,
    /// The line's row in the bot's chat.
    pub message: ChatMessageId,
    /// What came of it.
    pub outcome: ChatOutcome,
}

/// Why a line wasn't sent, or what the agent answered.
#[derive(Debug, thiserror::Error)]
pub enum ChatSendError {
    /// What any bot write can fail with: the session gone, the bot gone or
    /// hidden, `authorize()` refusing, the line unwritable, the audit or the
    /// database.
    #[error(transparent)]
    Write(#[from] BotWriteError),
    /// The bot can't take a line now: before anything was written, or after
    /// the commit when its stream no longer held it.
    #[error(transparent)]
    Refused(ChatRefusal),
    /// The bot's agent answered that the bot didn't send it.
    #[error("the bot's agent refused the line: {0:?}")]
    Failed(ChatFailedReason),
    /// The request's ID couldn't be minted.
    #[error("the chat request's ID can't be minted")]
    Mint(#[source] MintError),
    /// The line isn't one its route takes ([`ChatRoute::takes`]): the
    /// handler's own check is missing, a bug (S10). Nothing was read,
    /// written or sent.
    #[error(
        "a line its route doesn't take reached the service through {0:?}: the handler's check is missing"
    )]
    WrongRoute(ChatRoute),
}

/// How long a send waits for its agent's answer (S5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatWait {
    answer: Duration,
    budget: Duration,
}

impl ChatWait {
    /// How long after the send an answer is waited for: refusals and
    /// `ChatSent` come within milliseconds, and the agent's own 5 s job
    /// timeout answers within it.
    pub const ANSWER: Duration = Duration::from_secs(6);
    /// What the wait leaves of the request timeout, for the answer itself.
    pub const MARGIN: Duration = Duration::from_secs(2);

    /// The wait under the HTTP layer's `request_timeout`.
    #[must_use]
    pub const fn new(request_timeout: Duration) -> Self {
        Self {
            answer: Self::ANSWER,
            budget: request_timeout.saturating_sub(Self::MARGIN),
        }
    }

    /// The same wait, with `answer` after the send instead of
    /// [`ChatWait::ANSWER`]: for tests that wait for no answer.
    #[must_use]
    pub const fn with_answer(self, answer: Duration) -> Self {
        Self { answer, ..self }
    }

    /// When the wait of a line sent at `sent`, for a request that arrived at
    /// `started`, ends.
    #[must_use]
    pub fn deadline(self, started: Instant, sent: Instant) -> Instant {
        (sent + self.answer).min(started + self.budget)
    }
}

impl Default for ChatWait {
    /// Under the default request timeout of 15 s.
    fn default() -> Self {
        Self::new(Duration::from_secs(15))
    }
}

/// What the wait heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Sent,
    Failed(ChatFailedReason),
    Pending,
}

impl BotService {
    /// What `message` needs as a command: Manage when it's a command off
    /// `[chat] command_allowlist`, Control otherwise (`SendChat`).
    #[must_use]
    pub fn command_check(&self, message: &ChatMessage) -> CommandCheck {
        self.allowlist.check(message)
    }

    /// Sends `message` to the bot `id` for `by`, through `route`, whose
    /// request arrived at `started`; see the [module docs](self).
    ///
    /// # Errors
    /// [`ChatSendError::WrongRoute`] for a line `route` doesn't take, before
    /// anything else; [`ChatSendError::Write`] for what any bot write fails
    /// with, [`ChatSendError::Refused`] for a bot that can't take a line
    /// now, [`ChatSendError::Failed`] with the agent's reason, and
    /// [`ChatSendError::Mint`].
    pub async fn send_chat(
        &self,
        by: &ActingUser,
        id: BotId,
        message: &ChatMessage,
        route: ChatRoute,
        started: Instant,
    ) -> Result<ChatSendDone, ChatSendError> {
        let check = self.allowlist.check(message);
        // The handler checked it already; only a bug gets here (S10), and
        // nothing is read, written or sent for it.
        if !route.takes(message, check) {
            return Err(ChatSendError::WrongRoute(route));
        }
        let mut tx = self.store.write().await.map_err(BotWriteError::from)?;
        let (Rechecked { actor, username }, bot, account) = reread(tx.as_mut(), by, id).await?;
        authorize(&actor, Permission::SendChat(check), &account.context())
            .map_err(BotWriteError::Denied)?;
        let reached = self.reach(&bot).await.map_err(BotWriteError::from)?;
        // A refusal drops the transaction: nothing is written or recorded.
        judge_chat(ChatFacts {
            desired: bot.desired,
            reach: reached.reach,
            known: reached.known,
        })
        .map_err(ChatSendError::Refused)?;
        // The judge let it through, so the bot's stream holds it.
        let (Some(agent), Some(generation)) = (bot.agent, reached.stream) else {
            return Err(ChatSendError::Refused(ChatRefusal::Unreachable));
        };
        let now = self.clock.now();
        let request: ChatRequestId =
            mint(now, self.random.as_ref()).map_err(ChatSendError::Mint)?;
        let row = NewChatMessage {
            bot: bot.id,
            agent,
            at: now,
            entry: ChatEntry::UserSent {
                message: message.clone(),
                by: Some(actor.id),
            },
        };
        // The write transaction holds the bot's row as read: it names this
        // agent, so the line is stored.
        let Some(stored) = tx.chat().append(&row).await.map_err(BotWriteError::from)? else {
            return Err(ChatSendError::Refused(ChatRefusal::Unreachable));
        };
        let (action, metadata) =
            recorded(route, message, check, request, stored).map_err(BotWriteError::from)?;
        // Recorded with its row before it's sent: nothing reaches an agent
        // unaudited, and a refusal after the commit keeps both (S7).
        tx.commit(self.entry(by, action, bot.id, metadata))
            .await
            .map_err(BotWriteError::from)?;
        // On the bus once stored and before it's sent, with its request, so
        // every viewer has the line before its outcome (S34).
        let events = LineEvents {
            bot: bot.id,
            agent,
            generation,
            at: now,
            request,
        };
        self.events
            .publish(events.line(stored, row.entry, username));
        // Subscribed before the send, so no answer can come before the wait
        // listens.
        let mut answers = self.events.subscribe();
        match self
            .live
            .send_chat(agent, generation, bot.id, request, message)
        {
            Ok(()) => {}
            Err(
                refused @ (CommandRefused::NotHeld
                | CommandRefused::NotConnected
                | CommandRefused::QueueFull),
            ) => {
                // The answer, 409 `bot_unreachable`, doesn't tell this from a
                // refusal before the commit; the row and the entry stay (S7).
                debug!(user_id = %by.actor.id, bot_id = %bot.id, agent_id = %agent, chat_request = %request, chat_line = stored.get(), refused = ?refused, "a user's chat line was stored and recorded, but its stream no longer took it");
                self.events.publish(events.not_sent(NotSent::Unreachable));
                return Err(ChatSendError::Refused(ChatRefusal::Unreachable));
            }
            Err(CommandRefused::Unsendable) => {
                // The sender's answer is the internal error; viewers learn
                // only that it failed (R35).
                self.events.publish(events.not_sent(NotSent::Unsendable));
                return Err(ChatSendError::Write(BotWriteError::Unsendable));
            }
        }
        info!(user_id = %by.actor.id, bot_id = %bot.id, agent_id = %agent, chat_request = %request, route = ?route, "a user's chat line was sent to a bot's agent");
        let deadline = self.chat_wait.deadline(started, Instant::now());
        let outcome = match answer(&mut answers, request, bot.id, agent, deadline).await {
            Answer::Sent => ChatOutcome::Sent,
            Answer::Pending => ChatOutcome::Pending,
            Answer::Failed(reason) => {
                debug!(bot_id = %bot.id, chat_request = %request, reason = ?reason, "a user's chat line was refused by the bot's agent");
                return Err(ChatSendError::Failed(reason));
            }
        };
        debug!(bot_id = %bot.id, chat_request = %request, outcome = ?outcome, "a user's chat line was answered");
        Ok(ChatSendDone {
            request,
            message: stored,
            outcome,
        })
    }
}

/// What a user's line puts on the bus (S34, R35), from its bot's agent as
/// the send found it.
struct LineEvents {
    bot: BotId,
    agent: AgentId,
    generation: u64,
    at: DateTime<Utc>,
    request: ChatRequestId,
}

impl LineEvents {
    /// The line, stored as row `id` from `entry`, sent by `username`.
    fn line(&self, id: ChatMessageId, entry: ChatEntry, username: Username) -> LiveEvent {
        self.event(LiveEventKind::Chat {
            line: ListedChat {
                record: ChatRecord {
                    id,
                    bot: self.bot,
                    at: self.at,
                    entry,
                },
                sent_by: Some(username),
            },
            request: Some(self.request),
        })
    }

    /// The line stored, and not sent, for `why`.
    const fn not_sent(&self, why: NotSent) -> LiveEvent {
        self.event(LiveEventKind::ChatNotSent {
            request: self.request,
            why,
        })
    }

    const fn event(&self, kind: LiveEventKind) -> LiveEvent {
        LiveEvent {
            bot: self.bot,
            agent: self.agent,
            generation: self.generation,
            at: self.at,
            kind,
        }
    }
}

/// A line's audit action and metadata (S6): the route's action, the request
/// and the row, whether it's a command and whether the allowlist lists it,
/// and only a listed command's name. Never the text or its length.
fn recorded(
    route: ChatRoute,
    message: &ChatMessage,
    check: CommandCheck,
    request: ChatRequestId,
    stored: ChatMessageId,
) -> Result<(AuditAction, AuditMetadata), AuditError> {
    let action = match route {
        ChatRoute::Chat => AuditAction::BOT_SEND_CHAT,
        ChatRoute::Command => AuditAction::BOT_SEND_COMMAND,
    };
    let metadata = AuditMetadata::new()
        .id("request", request)?
        .int("message", stored.get())?;
    let metadata = match message.command_name() {
        None => metadata.text("command", "none")?,
        Some(_) if check.needs_manage() => metadata.text("command", "unlisted")?,
        Some(name) => metadata
            .text("command", "allowlisted")?
            .text("command_name", &format!("/{name}"))?,
    };
    Ok((action, metadata))
}

/// The agent's answer to `request` for `bot` from `agent`, on any stream,
/// heard on `answers` until `deadline`; anything else on the bus is skipped,
/// the server's own hints among it: only an agent's event answers (R13).
async fn answer(
    answers: &mut broadcast::Receiver<Arc<BusEvent>>,
    request: ChatRequestId,
    bot: BotId,
    agent: AgentId,
    deadline: Instant,
) -> Answer {
    loop {
        // `recv` is cancel-safe: the deadline drops it without losing an
        // event, and nothing is waited for after it.
        let event = match timeout_at(deadline, answers.recv()).await {
            Ok(Ok(event)) => event,
            // Nothing in time; or the wait fell behind the bus, so its
            // answer may be among the events it missed; or the bus is gone.
            Err(_) | Ok(Err(RecvError::Lagged(_) | RecvError::Closed)) => return Answer::Pending,
        };
        let Some(event) = event.agent() else {
            continue;
        };
        if event.bot != bot || event.agent != agent {
            continue;
        }
        match event.kind {
            LiveEventKind::ChatSent(answered) if answered == request => return Answer::Sent,
            LiveEventKind::ChatFailed(answered, reason) if answered == request => {
                return Answer::Failed(reason);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use fleet_core::bot::{BotSnapshot, BotState};

    use super::*;
    use crate::app::events::{EVENT_BUS_CAPACITY, EventBus, LiveEvent};

    fn at() -> DateTime<Utc> {
        DateTime::from_timestamp_millis(1_800_000_000_000).unwrap()
    }

    fn bot(n: u8) -> BotId {
        BotId::new_v7(at(), [n; 10]).unwrap()
    }

    fn agent(n: u8) -> AgentId {
        AgentId::new_v7(at(), [n; 10]).unwrap()
    }

    fn request(n: u8) -> ChatRequestId {
        ChatRequestId::new_v7(at(), [n; 10]).unwrap()
    }

    fn event(bot: BotId, agent: AgentId, generation: u64, kind: LiveEventKind) -> LiveEvent {
        LiveEvent {
            bot,
            agent,
            generation,
            at: at(),
            kind,
        }
    }

    fn status(bot: BotId) -> LiveEventKind {
        LiveEventKind::Status(BotSnapshot {
            bot_id: bot,
            state: BotState::Stopped,
            since: at(),
            last_disconnect: None,
        })
    }

    /// The answer the bus's `events` give `request` for bot 1 from agent 1,
    /// waiting 6 s.
    async fn heard(bus: &EventBus, events: Vec<LiveEvent>) -> Answer {
        let mut answers = bus.subscribe();
        for event in events {
            bus.publish(event);
        }
        let deadline = Instant::now() + ChatWait::ANSWER;
        answer(&mut answers, request(1), bot(1), agent(1), deadline).await
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_matches_its_request_bot_and_agent_on_any_stream() {
        let bus = EventBus::new();
        let others = vec![
            event(bot(1), agent(1), 1, LiveEventKind::ChatSent(request(2))),
            event(bot(2), agent(1), 1, LiveEventKind::ChatSent(request(1))),
            event(bot(1), agent(2), 1, LiveEventKind::ChatSent(request(1))),
            event(bot(1), agent(1), 1, status(bot(1))),
        ];
        let mut events = others.clone();
        events.push(event(
            bot(1),
            agent(1),
            7,
            LiveEventKind::ChatSent(request(1)),
        ));

        let sent = heard(&bus, events).await;
        let none = heard(&bus, others).await;

        assert_eq!(sent, Answer::Sent);
        assert_eq!(none, Answer::Pending);
    }

    #[tokio::test(start_paused = true)]
    async fn the_wait_reads_agent_events_only() {
        use crate::ports::live_hints::{LiveHints, ModeEvent};

        let bus = EventBus::new();
        let mut answers = bus.subscribe();
        let hint = |bus: &EventBus| {
            bus.bots(&[bot(1)]);
            bus.mode(ModeEvent {
                id: fleet_core::id::ModeId::new_v7(at(), [5; 10]).unwrap(),
                was: None,
            });
            bus.role_changed(fleet_core::id::UserId::new_v7(at(), [6; 10]).unwrap());
        };
        hint(&bus);
        bus.publish(event(
            bot(1),
            agent(1),
            1,
            LiveEventKind::ChatNotSent {
                request: request(1),
                why: crate::app::events::NotSent::Unreachable,
            },
        ));
        hint(&bus);
        bus.publish(event(
            bot(1),
            agent(1),
            1,
            LiveEventKind::ChatSent(request(1)),
        ));
        let deadline = Instant::now() + ChatWait::ANSWER;

        let heard = answer(&mut answers, request(1), bot(1), agent(1), deadline).await;

        assert_eq!(heard, Answer::Sent);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failure_answers_with_its_reason() {
        let bus = EventBus::new();
        let other = event(
            bot(1),
            agent(2),
            1,
            LiveEventKind::ChatFailed(request(1), ChatFailedReason::QueueFull),
        );
        let failed = event(
            bot(1),
            agent(1),
            2,
            LiveEventKind::ChatFailed(request(1), ChatFailedReason::RateLimited),
        );

        let heard = heard(&bus, vec![other, failed]).await;

        assert_eq!(heard, Answer::Failed(ChatFailedReason::RateLimited));
    }

    #[tokio::test(start_paused = true)]
    async fn no_answer_by_the_deadline_is_pending_exactly_at_it() {
        let bus = EventBus::new();
        let begun = Instant::now();

        let heard = heard(&bus, Vec::new()).await;

        assert_eq!(heard, Answer::Pending);
        assert_eq!(begun.elapsed(), ChatWait::ANSWER);
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_published_while_waiting_is_heard() {
        let bus = EventBus::new();
        let mut answers = bus.subscribe();
        let publisher = bus.clone();
        let later = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            publisher.publish(event(
                bot(1),
                agent(1),
                1,
                LiveEventKind::ChatSent(request(1)),
            ));
        });

        let heard = answer(
            &mut answers,
            request(1),
            bot(1),
            agent(1),
            Instant::now() + ChatWait::ANSWER,
        )
        .await;

        later.await.unwrap();
        assert_eq!(heard, Answer::Sent);
    }

    #[tokio::test(start_paused = true)]
    async fn a_wait_that_fell_behind_the_bus_is_pending() {
        let bus = EventBus::new();
        let mut events: Vec<LiveEvent> = (0..=EVENT_BUS_CAPACITY)
            .map(|_| event(bot(2), agent(1), 1, status(bot(2))))
            .collect();
        events.push(event(
            bot(1),
            agent(1),
            1,
            LiveEventKind::ChatSent(request(1)),
        ));
        let begun = Instant::now();

        let heard = heard(&bus, events).await;

        assert_eq!(heard, Answer::Pending);
        assert_eq!(begun.elapsed(), Duration::ZERO, "at once");
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_bus_is_pending_at_once() {
        let bus = EventBus::new();
        let mut answers = bus.subscribe();
        drop(bus);
        let begun = Instant::now();

        let heard = answer(
            &mut answers,
            request(1),
            bot(1),
            agent(1),
            begun + ChatWait::ANSWER,
        )
        .await;

        assert_eq!(heard, Answer::Pending);
        assert_eq!(begun.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn the_wait_ends_at_the_earlier_of_its_two_bounds() {
        let started = Instant::now();
        let sent = started + Duration::from_secs(1);

        let usual = ChatWait::new(Duration::from_secs(15)).deadline(started, sent);
        let short = ChatWait::new(Duration::from_secs(5)).deadline(started, sent);
        let shortest = ChatWait::new(Duration::from_secs(1)).deadline(started, sent);
        let tested = ChatWait::default()
            .with_answer(Duration::from_millis(50))
            .deadline(started, sent);

        assert_eq!(usual, sent + ChatWait::ANSWER, "6 s after the send");
        assert_eq!(
            short,
            started + Duration::from_secs(3),
            "the request's 5 s less 2"
        );
        assert_eq!(shortest, started, "no time left at all");
        assert_eq!(tested, sent + Duration::from_millis(50));
        assert_eq!(ChatWait::default(), ChatWait::new(Duration::from_secs(15)));
    }

    /// `/chat` takes plain chat and allowlisted commands, `/command` every
    /// command and nothing else (S4, S8, S10).
    #[rstest::rstest]
    #[case::plain_chat(ChatRoute::Chat, "hello", true)]
    #[case::a_later_slash(ChatRoute::Chat, "hello /op AfkBot2", true)]
    #[case::a_listed_command(ChatRoute::Chat, "/spawn here", true)]
    #[case::an_unlisted_command(ChatRoute::Chat, "/op AfkBot2", false)]
    #[case::a_lone_slash(ChatRoute::Chat, "/", false)]
    #[case::plain_chat_as_a_command(ChatRoute::Command, "hello", false)]
    #[case::a_later_slash_as_a_command(ChatRoute::Command, "hello /spawn", false)]
    #[case::a_listed_command_as_a_command(ChatRoute::Command, "/spawn here", true)]
    #[case::an_unlisted_command_as_a_command(ChatRoute::Command, "/op AfkBot2", true)]
    #[case::a_lone_slash_as_a_command(ChatRoute::Command, "/", true)]
    fn each_route_takes_only_its_lines(
        #[case] route: ChatRoute,
        #[case] text: &str,
        #[case] taken: bool,
    ) {
        let allowlist = fleet_core::authz::CommandAllowlist::try_new(&["/spawn"]).unwrap();
        let message = ChatMessage::try_from(text).unwrap();

        assert_eq!(
            route.takes(&message, allowlist.check(&message)),
            taken,
            "{route:?} {text:?}"
        );
    }
}
