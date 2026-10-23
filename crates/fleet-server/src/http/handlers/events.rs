//! `POST /api/v1/events/ticket` and `GET /api/v1/events`, the live events
//! socket (Plan.md P11.5, §7.4, Appendix B; ADR-0020, S28, S29, S35, R10,
//! R17).
//!
//! - **The ticket** is the user's own (`UsePersonal`), for one socket of
//!   this session within 30 s; minting it stores nothing, so no audit entry.
//! - **The connect** needs the session's access token and a ticket issued
//!   to that very session, and checks them in this order:
//!   1. `AuthUser` (401, the per-user 429), before anything is read;
//!   2. the sockets' owner counts the connect from here, so a shutdown
//!      waits for it; once the shutdown began, 503 `busy`;
//!   3. the upgrade's headers: 405 for a method that isn't `GET`, 400 for a
//!      request that isn't an upgrade, before the ticket is spent;
//!   4. the query: 400 for a parameter that isn't `ticket`, or one given
//!      twice;
//!   5. the ticket, taken at once (single use), then whose it is: every
//!      ticket that isn't a live one of this session gets the same 404, with
//!      one `debug` line naming why, never the ticket;
//!   6. the session again, subscribed to the commit count first, so a
//!      revocation committed meanwhile is never missed (404 too; a store
//!      failure 503 or 500; 503 `busy` when the server stops meanwhile);
//!   7. the user's place among their sockets: 429 `too_many_connections`;
//!   8. the upgrade, with the socket's limits; the socket's task runs on
//!      the sockets' owner.
//!
//!   The ticket is in the URL, so no log line ever holds it: the trace
//!   layer logs the route's template, and nothing here logs the query.
//! - **What a socket sends** ([`Viewer`], ADR-0020, S30, S32–S34,
//!   R28–R35): each event built for its viewer right before its send,
//!   through the very helpers the REST answers use, so a frame shows what
//!   `GET /bots/{id}` would show them then:
//!   1. the gate: once the commit count moved past the rights it judged on,
//!      it reads them again (one read for every socket), sends `bot_removed`
//!      for each bot the viewer no longer sees, and reads the ones they now
//!      do; every read of bots it holds is then read again;
//!   2. a viewer whose own role, flag or account changed is sent nothing
//!      more: their session check closes the socket (R31);
//!   3. an agent's event on a bot they can't see is dropped by a lookup,
//!      with no read and no frame;
//!   4. a bot's state and the server's hints are read together, in one
//!      query, at the first one's place (R33), and each read decides its
//!      frame: `bot` when the viewer may see the bot, `bot_removed` when they
//!      saw it and no longer may.

use core::time::Duration;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::response::Response;
use fleet_api_types::bots::{BotStateKind, BotStateReason};
use fleet_api_types::chat::ChatFailure;
use fleet_api_types::events::{EventMessage, EventTicketResponse, EventsQuery};
use fleet_api_types::{ErrorResponse, SecretText};
use fleet_core::authz::{Actor, GrantLevel, Permission, ResourceContext, Role, authorize};
use fleet_core::bot::{BotNotification, LastState};
use fleet_core::id::{BotId, UserId};
use secrecy::ExposeSecret as _;
use tokio::time::Instant;
use tracing::{Instrument as _, debug, info_span};

use super::bots::bot_response;
use super::{chat_failure, chat_line, mode_response, timestamp};
use crate::app::auth::{SocketCheckError, SocketSession};
use crate::app::bots::{BotService, BotView};
use crate::app::events::{BusEvent, LiveEvent, LiveEventKind, LiveRights, NotSent, Rights};
use crate::app::modes::ModeService;
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, ValidQuery};
use crate::http::router::AppState;
use crate::http::ws::{
    MAX_CLIENT_FRAME, MAX_WRITE_BUFFER, MERGE_MAX, OpenSocket, Project, READ_BUFFER, Step,
    Unprojected, refused_upgrade, run,
};
use crate::ports::live_hints::ModeEvent;

/// `POST /api/v1/events/ticket`: a ticket for one live events socket of this
/// session.
#[utoipa::path(
    post,
    path = "/api/v1/events/ticket",
    operation_id = "event_ticket",
    tag = "events",
    responses(
        (status = 200, description = "A single-use ticket for `GET /api/v1/events`, with this session's access token, within 30 s. A secret: never log or show it.", body = EventTicketResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn ticket(
    user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<EventTicketResponse>, ApiError> {
    let owner = user.actor().id;
    user.require(
        Permission::UsePersonal,
        &ResourceContext::Personal { owner },
    )?;
    let issued = state
        .events
        .tickets
        .issue(owner, user.session_id())
        .map_err(ApiError::internal)?;
    Ok(Json(EventTicketResponse {
        ticket: SecretText::new(issued.ticket.expose_secret().to_owned()),
        expires_in: u32::try_from(issued.expires_in).map_err(ApiError::internal)?,
    }))
}

/// `GET /api/v1/events?ticket=…`: the live events socket, its checks in
/// the order this module's docs give.
#[utoipa::path(
    get,
    path = "/api/v1/events",
    operation_id = "events",
    tag = "events",
    params(EventsQuery),
    responses(
        (status = 101, description = "The socket: JSON text frames, one `EventMessage` each, `ready` first; the client sends only pongs and a close (`fleet_api_types::events`)."),
        (status = 400, description = "Not a WebSocket upgrade, or (after the upgrade's checks) a query that isn't one `ticket`.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 404, description = "One answer for every ticket that opens nothing: malformed, unknown, used, expired, another user's or another session's; and for a session that no longer counts (revoked, expired, its user disabled or given another role). Mint a new ticket, and back off before trying again.", body = ErrorResponse),
        (status = 405, description = "A method that isn't `GET` (a `HEAD`).", body = ErrorResponse),
        (status = 429, description = "Too many requests, or `too_many_connections`: the user's sockets are all open.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = 503, description = "`busy`: the server is shutting down, or the session couldn't be checked.", body = ErrorResponse),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
    extensions(
        ("x-websocket-message" = json!({ "$ref": "#/components/schemas/EventMessage" })),
    ),
)]
pub(crate) async fn connect(
    user: AuthUser,
    State(state): State<AppState>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
    query: Result<ValidQuery<EventsQuery>, ApiError>,
) -> Result<Response, ApiError> {
    // Counted by the sockets' owner from the start, so a shutdown waits for
    // this connect, and one that began already turns it away (R21).
    let owned = state.events.sockets.token();
    let stop = state.events.sockets.stop();
    if stop.is_cancelled() {
        return Err(ApiError::busy());
    }
    let upgrade = upgrade.map_err(|rejection| refused_upgrade(&rejection))?;
    // Only after the upgrade's own refusals (R10).
    let ValidQuery(query) = query?;
    let actor = user.actor();
    let grant = state
        .events
        .tickets
        .take(query.ticket.expose())
        .map_err(|refusal| refuse(&user, &refusal.to_string()))?;
    user.require(
        Permission::UsePersonal,
        &ResourceContext::Personal { owner: grant.user },
    )
    .map_err(|_| refuse(&user, "another user's ticket"))?;
    if grant.session != user.session_id() {
        return Err(refuse(&user, "another session's ticket"));
    }
    let session = SocketSession {
        actor,
        session_id: user.session_id(),
        times: user.session_times(),
    };
    // Subscribed before the check: a commit after it wakes the socket (R18).
    let commits = state.tokens.socket_commits();
    let checked = tokio::select! {
        biased;
        // Both are cancel-safe: the check only reads.
        () = stop.cancelled() => return Err(ApiError::busy()),
        checked = state.tokens.check_socket(&session) => checked,
    };
    checked.map_err(|error| match error {
        SocketCheckError::Refused(reason) => refuse(&user, reason),
        SocketCheckError::Store(error) => ApiError::from(error),
    })?;
    let slot = state.limits.open_socket(actor.id)?;
    let open = OpenSocket {
        session,
        slot,
        commits,
        // Counted here, so the upgrade's own time is part of the lifetime.
        deadline: socket_deadline(state.tokens.socket_time_left(session.times)),
        events: state.events.clone(),
        tokens: state.tokens.clone(),
    };
    let viewer = Viewer::new(
        actor,
        state.events.rights.clone(),
        state.bots.clone(),
        state.modes.clone(),
    );
    let sockets = state.events.sockets.clone();
    let span = info_span!("events_socket", user_id = %actor.id, session_id = %session.session_id);
    Ok(upgrade
        .read_buffer_size(READ_BUFFER)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_WRITE_BUFFER)
        .max_message_size(MAX_CLIENT_FRAME)
        .max_frame_size(MAX_CLIENT_FRAME)
        .on_failed_upgrade(|error| {
            debug!(error = %error, "a live events socket's upgrade failed");
        })
        .on_upgrade(move |socket| async move {
            sockets.spawn(run(socket, open, viewer).instrument(span));
            // Counted by the tracker until its task is spawned.
            drop(owned);
        }))
}

/// When a socket with `time_left` must close: now plus that, or now when it
/// overflows the clock.
fn socket_deadline(time_left: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(time_left).unwrap_or(now)
}

/// One socket's viewer, and what the socket owes them; see the
/// [module docs](self).
#[derive(Debug)]
pub(crate) struct Viewer {
    actor: Actor,
    live: LiveRights,
    /// The rights it judges on.
    rights: Arc<Rights>,
    /// The bots the viewer is taken to know: what they could see at the
    /// open or the last `resync`, and every bot sent since, until it's
    /// removed (R32).
    seen: BTreeSet<BotId>,
    slots: VecDeque<Slot>,
    bots: BotService,
    modes: ModeService,
}

/// One thing a socket owes its viewer, in the bus's order.
#[derive(Debug)]
enum Slot {
    /// The viewer no longer sees this bot: `bot_removed`.
    Removed(BotId),
    /// Bots whose reads may have changed, read together when their turn
    /// comes.
    Bots(BotReads),
    /// An agent's event about one bot.
    Agent(Arc<BusEvent>),
    /// A mode that was created, changed or deleted, read when its turn
    /// comes.
    Mode(ModeEvent),
    /// Another user's role changed: an Admin viewer's sight may have (R11).
    RoleChanged(UserId),
}

/// Bots to read, and, once read, the views not sent yet.
#[derive(Debug, Default)]
struct BotReads {
    ids: BTreeSet<BotId>,
    /// `None` until read, and again once a commit may have changed them.
    views: Option<BTreeMap<BotId, BotView>>,
}

impl Viewer {
    /// The viewer `actor`, judged on `live`, their bots read through `bots`
    /// and their modes through `modes`.
    pub(crate) fn new(
        actor: Actor,
        live: LiveRights,
        bots: BotService,
        modes: ModeService,
    ) -> Self {
        Self {
            actor,
            live,
            rights: Arc::default(),
            seen: BTreeSet::new(),
            slots: VecDeque::new(),
            bots,
            modes,
        }
    }

    /// The mode `event` names, read now (S33): `mode` as `GET /modes` lists
    /// it when the viewer may see it now, else `mode_removed` when they could
    /// before the write, judged on what it read, else nothing.
    async fn mode_step(&self, event: ModeEvent) -> Result<Step, Unprojected> {
        let listed = self.modes.find(event.id).await.map_err(|error| {
            debug!(%error, "a live events socket couldn't read a mode");
            Unprojected
        })?;
        let seen_now = listed.filter(|listed| {
            authorize(&self.actor, Permission::ViewMode, &listed.context()).is_ok()
        });
        if let Some(listed) = seen_now {
            // A mode that doesn't build is the internal error a `GET /modes`
            // answers, logged there; the socket sends nothing.
            return Ok(
                mode_response(&self.actor, &listed).map_or(Step::Nothing, |shown| {
                    Step::Send(Box::new(EventMessage::mode(shown)))
                }),
            );
        }
        let saw = event.was.is_some_and(|was| {
            authorize(&self.actor, Permission::ViewMode, &was.context()).is_ok()
        });
        Ok(if saw {
            Step::Send(Box::new(EventMessage::mode_removed(event.id)))
        } else {
            Step::Nothing
        })
    }

    /// Reads the rights again once the commit count moved past them: sends
    /// `bot_removed` first for each bot the viewer no longer sees, then
    /// reads those they now do, and drops every read made before. Whether
    /// the rights are current: not when a commit outran the read (S43).
    async fn gate(&mut self) -> Result<bool, Unprojected> {
        let count = self.live.count();
        if count <= self.rights.at() {
            return Ok(true);
        }
        self.rights = self.live.at_least(count).await.map_err(|error| {
            debug!(%error, "a live events socket couldn't read its rights");
            Unprojected
        })?;
        if self.outrun() {
            // Older than a commit already made: the next turn reads again,
            // and only rights that held decide what was seen.
            return Ok(false);
        }
        for slot in &mut self.slots {
            if let Slot::Bots(reads) = slot {
                reads.views = None;
            }
        }
        let visible = self.rights.visible(&self.actor);
        let joined: BTreeSet<BotId> = visible.difference(&self.seen).copied().collect();
        if !joined.is_empty() {
            // A removal still waiting for a bot seen again never goes: its
            // read below says what it is now.
            self.slots
                .retain(|slot| !matches!(slot, Slot::Removed(bot) if joined.contains(bot)));
            self.slots.push_front(Slot::Bots(BotReads {
                ids: joined,
                views: None,
            }));
        }
        let left: Vec<BotId> = self.seen.difference(&visible).copied().collect();
        for bot in left.into_iter().rev() {
            self.seen.remove(&bot);
            self.slots.push_front(Slot::Removed(bot));
        }
        Ok(true)
    }

    /// Whether a commit was counted after the rights the viewer holds: a
    /// read made since may be older than it (S43).
    fn outrun(&self) -> bool {
        self.live.count() > self.rights.at()
    }

    /// Notes that `bots` may read differently: merged into the first
    /// unread read it holds, or a new one at the end.
    fn hint(&mut self, bots: impl IntoIterator<Item = BotId>) {
        let ids: BTreeSet<BotId> = bots
            .into_iter()
            .filter(|bot| self.rights.level(&self.actor, *bot).is_some() || self.seen.contains(bot))
            .collect();
        if ids.is_empty() {
            return;
        }
        let unread = self.slots.iter_mut().find_map(|slot| match slot {
            Slot::Bots(reads) if reads.views.is_none() => Some(reads),
            _ => None,
        });
        match unread {
            Some(reads) => reads.ids.extend(ids),
            None => self
                .slots
                .push_back(Slot::Bots(BotReads { ids, views: None })),
        }
    }

    /// The next bot of `reads`, read now with the rest if it wasn't yet.
    async fn bot_step(&mut self, mut reads: BotReads) -> Result<Step, Unprojected> {
        let mut views = if let Some(views) = reads.views.take() {
            views
        } else {
            // The bots the viewer may see now, and those they saw, for a
            // `bot_removed`.
            reads.ids.retain(|bot| {
                self.rights.level(&self.actor, *bot).is_some() || self.seen.contains(bot)
            });
            let ids: Vec<BotId> = reads.ids.iter().copied().collect();
            let views = self
                .bots
                .views(&self.actor, &ids)
                .await
                .map_err(|error| {
                    debug!(%error, "a live events socket couldn't read its bots");
                    Unprojected
                })?
                .into_iter()
                .map(|view| (view.record.bot.id, view))
                .collect();
            if self.outrun() {
                // A commit landed during the read: read again on the next
                // turn, under the rights it made (S43).
                self.slots.push_front(Slot::Bots(reads));
                return Ok(Step::Nothing);
            }
            views
        };
        let Some(bot) = reads.ids.pop_first() else {
            return Ok(Step::Nothing);
        };
        let view = views.remove(&bot);
        if !reads.ids.is_empty() {
            reads.views = Some(views);
            self.slots.push_front(Slot::Bots(reads));
        }
        Ok(self.bot_frame(bot, view))
    }

    /// `bot`'s frame from its fresh `view` (R32): `bot` when the viewer may
    /// see it, as `GET /bots/{id}` shows it, else `bot_removed` when they saw
    /// it.
    fn bot_frame(&mut self, bot: BotId, view: Option<BotView>) -> Step {
        let shown = view.filter(|view| {
            authorize(&self.actor, Permission::ViewBot, &view.account.context()).is_ok()
        });
        match shown {
            // A response that doesn't build is the internal error a `GET`
            // answers, logged there; the socket sends nothing for it.
            Some(view) => bot_response(&self.actor, &view).map_or(Step::Nothing, |response| {
                self.seen.insert(bot);
                Step::Send(Box::new(EventMessage::bot(response)))
            }),
            None if self.seen.remove(&bot) => Step::Send(Box::new(EventMessage::bot_removed(bot))),
            None => Step::Nothing,
        }
    }

    /// An agent's `event`, for a viewer who may see its bot now, at the
    /// level they hold on it now: a chat line through the history's
    /// `chat_line`, a failure's reason only at Control or more (S34).
    fn agent_step(&self, event: &LiveEvent) -> Step {
        let Some(level) = self.rights.level(&self.actor, event.bot) else {
            return Step::Nothing;
        };
        let control = level >= GrantLevel::Control;
        let message = match &event.kind {
            LiveEventKind::Died => match timestamp(event.at) {
                Ok(at) => EventMessage::bot_died(event.bot, at),
                Err(_) => return Step::Nothing,
            },
            LiveEventKind::Alert(notification) => {
                match (timestamp(event.at), alerted(*notification)) {
                    (Ok(at), Some((state, reason))) => {
                        EventMessage::bot_alert(event.bot, at, state, reason)
                    }
                    _ => return Step::Nothing,
                }
            }
            LiveEventKind::Chat { line, request } => {
                // A line that doesn't build is the internal error the
                // history answers, logged there; the socket sends nothing.
                match chat_line(&self.actor, level, line) {
                    Ok(shown) => EventMessage::chat(event.bot, shown, *request),
                    Err(_) => return Step::Nothing,
                }
            }
            LiveEventKind::ChatSent(request) => EventMessage::chat_sent(event.bot, *request),
            LiveEventKind::ChatFailed(request, reason) => EventMessage::chat_failed(
                event.bot,
                *request,
                control.then(|| chat_failure(*reason)),
            ),
            LiveEventKind::ChatNotSent { request, why } => {
                let failure = match why {
                    NotSent::Unreachable => control.then_some(ChatFailure::BotUnreachable),
                    NotSent::Unsendable => None,
                };
                EventMessage::chat_failed(event.bot, *request, failure)
            }
            // Merged into the bots' reads as it's taken.
            LiveEventKind::Status(_) => return Step::Nothing,
        };
        Step::Send(Box::new(message))
    }
}

impl Project for Viewer {
    async fn open(&mut self) -> Result<(), Unprojected> {
        self.rights = self
            .live
            .at_least(self.live.count())
            .await
            .map_err(|error| {
                debug!(%error, "a live events socket couldn't read its rights");
                Unprojected
            })?;
        self.seen = self.rights.visible(&self.actor);
        Ok(())
    }

    fn has_room(&self) -> bool {
        self.slots.len() < MERGE_MAX
    }

    fn take(&mut self, event: Arc<BusEvent>) {
        match &*event {
            BusEvent::Agent(agent) => {
                // A bot the viewer can't see: dropped by a lookup (S30).
                if self.rights.level(&self.actor, agent.bot).is_none() {
                    return;
                }
                if matches!(agent.kind, LiveEventKind::Status(_)) {
                    self.hint([agent.bot]);
                } else {
                    self.slots.push_back(Slot::Agent(event));
                }
            }
            BusEvent::Bots(bots) => self.hint(bots.iter().copied()),
            BusEvent::Mode(event) => self.slots.push_back(Slot::Mode(*event)),
            BusEvent::RoleChanged { user } => self.slots.push_back(Slot::RoleChanged(*user)),
        }
    }

    fn has_work(&self) -> bool {
        !self.slots.is_empty()
    }

    async fn next(&mut self) -> Result<Step, Unprojected> {
        if !self.gate().await? {
            return Ok(Step::Nothing);
        }
        if !self.rights.stands(&self.actor) {
            // Muted until the session check closes the socket (R31).
            self.slots.clear();
            return Ok(Step::Nothing);
        }
        match self.slots.pop_front() {
            None => Ok(Step::Nothing),
            Some(Slot::Removed(bot)) => Ok(Step::Send(Box::new(EventMessage::bot_removed(bot)))),
            Some(Slot::Bots(reads)) => self.bot_step(reads).await,
            Some(Slot::Agent(event)) => Ok(event
                .agent()
                .map_or(Step::Nothing, |event| self.agent_step(event))),
            Some(Slot::Mode(event)) => {
                let step = self.mode_step(event).await?;
                if self.outrun() {
                    // A commit landed during the read: read again on the
                    // next turn (S43).
                    self.slots.push_front(Slot::Mode(event));
                    return Ok(Step::Nothing);
                }
                Ok(step)
            }
            // Another Admin's sight follows a user's role (R11); the Owner
            // and Members see what they did, and a viewer's own role closes
            // their socket.
            Some(Slot::RoleChanged(user)) => {
                Ok(if user != self.actor.id && self.actor.role == Role::Admin {
                    Step::Resync
                } else {
                    Step::Nothing
                })
            }
        }
    }

    fn resynced(&mut self) {
        self.seen = self.rights.visible(&self.actor);
    }
}

/// A paused or failed bot's state and reason, as `GET /bots/{id}` names
/// them.
fn alerted(notification: BotNotification) -> Option<(BotStateKind, BotStateReason)> {
    let state = match notification {
        BotNotification::Paused { reason } => LastState::Paused(reason),
        BotNotification::Failed { reason } => LastState::Failed(reason),
    };
    Some((BotStateKind::from(state), BotStateReason::of(state)?))
}

/// The one answer to every ticket that doesn't open a socket, with its one
/// `debug` line: why, and whose request, never the ticket.
fn refuse(user: &AuthUser, reason: &str) -> ApiError {
    debug!(
        reason,
        user_id = %user.actor().id,
        session_id = %user.session_id(),
        "a live events ticket was refused"
    );
    ApiError::NotFound
}
