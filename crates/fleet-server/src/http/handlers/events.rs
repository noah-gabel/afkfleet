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
//!      with no read and no frame; events taken on rights a commit outran
//!      wait, unjudged, until the gate read them again, so a hidden bot's
//!      event never makes, joins or moves anything the viewer is sent (R47);
//!   4. a bot's state and the server's hints are read together, in one
//!      query, at the first one's place (R33), and each read decides its
//!      frame: `bot` when the viewer may see the bot, `bot_removed` when they
//!      saw it and no longer may;
//!   5. a mode's events are read once, at the first one's place, and the
//!      viewer saw it when they could see it under any of their `was` (R49).

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
use fleet_core::id::{BotId, ModeId};
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
    MAX_CLIENT_FRAME, MAX_WRITE_BUFFER, MERGE_MAX, OpenSocket, Project, READ_BUFFER, Step, Taken,
    Unprojected, refused_upgrade, run,
};

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
    slots: VecDeque<Queued>,
    /// Events taken on rights a commit outran, in the bus's order: judged
    /// once the gate read the rights again (R47).
    unjudged: VecDeque<Arc<BusEvent>>,
    /// Whether the events being judged are a replay's, so the slots they
    /// make say so (R70).
    replaying: bool,
    bots: BotService,
    modes: ModeService,
}

/// A slot and where it came from: a replay of events that waited unjudged,
/// or anything else (the gate, an event taken live). A replay runs once no
/// slot the last one made is still queued (R61, R70).
#[derive(Debug)]
struct Queued {
    slot: Slot,
    replayed: bool,
}

impl Queued {
    /// `slot`, made by anything but a replay.
    const fn live(slot: Slot) -> Self {
        Self {
            slot,
            replayed: false,
        }
    }
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
    /// comes, and whether the viewer could see it under any `was` of its
    /// events, judged as each was taken (R49).
    Mode { id: ModeId, saw: bool },
    /// Another user's role changed, for an Admin viewer: their sight may
    /// have (R11).
    RoleChanged,
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
            unjudged: VecDeque::new(),
            replaying: false,
            bots,
            modes,
        }
    }

    /// `slot`, marked as made by a replay while one runs.
    const fn queued(&self, slot: Slot) -> Queued {
        Queued {
            slot,
            replayed: self.replaying,
        }
    }

    /// The mode `id`, read now (S33): `mode` as `GET /modes` lists it when
    /// the viewer may see it now, else `mode_removed` when they `saw` it
    /// before a write, else nothing.
    async fn mode_step(&self, id: ModeId, saw: bool) -> Result<Step, Unprojected> {
        let listed = self.modes.find(id).await.map_err(|error| {
            debug!(%error, "a live events socket couldn't read a mode");
            Unprojected
        })?;
        let seen_now = listed.filter(|listed| {
            authorize(&self.actor, Permission::ViewMode, &listed.context()).is_ok()
        });
        if let Some(listed) = seen_now {
            // A mode that doesn't build is the internal error a `GET /modes`
            // answers; the socket sends nothing.
            if let Ok(shown) = mode_response(&self.actor, &listed) {
                return Ok(Step::Send(Box::new(EventMessage::mode(shown))));
            }
            debug!(mode_id = %id, "a live events socket couldn't build a mode's frame");
            return Ok(Step::Nothing);
        }
        Ok(if saw {
            Step::Send(Box::new(EventMessage::mode_removed(id)))
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
        let visible = self.rights.visible(&self.actor);
        let joined: BTreeSet<BotId> = visible.difference(&self.seen).copied().collect();
        for queued in &mut self.slots {
            if let Slot::Bots(reads) = &mut queued.slot {
                reads.views = None;
                // Read below, at the front: once is enough.
                reads.ids.retain(|bot| !joined.contains(bot));
            }
        }
        if !joined.is_empty() {
            // A removal still waiting for a bot seen again never goes: its
            // read below says what it is now.
            drop_removals(&mut self.slots, &joined);
            self.slots.push_front(Queued::live(Slot::Bots(BotReads {
                ids: joined,
                views: None,
            })));
        }
        let left: Vec<BotId> = self.seen.difference(&visible).copied().collect();
        for bot in left.into_iter().rev() {
            self.seen.remove(&bot);
            self.slots.push_front(Queued::live(Slot::Removed(bot)));
        }
        Ok(true)
    }

    /// Whether a commit was counted after the rights the viewer holds: a
    /// read made since may be older than it (S43).
    fn outrun(&self) -> bool {
        self.live.count() > self.rights.at()
    }

    /// Whether the viewer may see `bot` now, or saw it, for a `bot_removed`.
    fn sees(&self, bot: BotId) -> bool {
        self.rights.level(&self.actor, bot).is_some() || self.seen.contains(&bot)
    }

    /// Notes that `ids`, bots the viewer sees or saw, may read differently:
    /// merged into the first unread read it holds, or a new one at the end.
    fn hint(&mut self, ids: BTreeSet<BotId>) {
        let unread = self
            .slots
            .iter_mut()
            .find_map(|queued| match &mut queued.slot {
                Slot::Bots(reads) if reads.views.is_none() => Some(reads),
                _ => None,
            });
        if let Some(reads) = unread {
            reads.ids.extend(ids);
        } else {
            let queued = self.queued(Slot::Bots(BotReads { ids, views: None }));
            self.slots.push_back(queued);
        }
    }

    /// The next bot of `reads`, read now with the rest if it wasn't yet; what
    /// it gives back keeps its slot's origin, `replayed`.
    async fn bot_step(&mut self, mut reads: BotReads, replayed: bool) -> Result<Step, Unprojected> {
        let mut views = if let Some(views) = reads.views.take() {
            views
        } else {
            // The bots the viewer may see now, and those they saw, for a
            // `bot_removed`.
            reads.ids.retain(|bot| self.sees(*bot));
            if reads.ids.is_empty() {
                // Every bot it held is hidden: no read, no frame.
                return Ok(Step::Nothing);
            }
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
                self.slots.push_front(Queued {
                    slot: Slot::Bots(reads),
                    replayed,
                });
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
            self.slots.push_front(Queued {
                slot: Slot::Bots(reads),
                replayed,
            });
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
            // answers; the socket sends nothing for it.
            Some(view) => {
                if let Ok(response) = bot_response(&self.actor, &view) {
                    self.seen.insert(bot);
                    Step::Send(Box::new(EventMessage::bot(response)))
                } else {
                    debug!(bot_id = %bot, "a live events socket couldn't build a bot's frame");
                    Step::Nothing
                }
            }
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
                // history answers; the socket sends nothing.
                let Ok(shown) = chat_line(&self.actor, level, line) else {
                    debug!(bot_id = %event.bot, "a live events socket couldn't build a chat line's frame");
                    return Step::Nothing;
                };
                EventMessage::chat(event.bot, shown, *request)
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

    /// Judges `event` on the rights the viewer holds, current when it was
    /// taken: what it makes or joins counts toward [`MERGE_MAX`], and one
    /// about a bot the viewer can't see makes nothing (S30). `Err` gives it
    /// back when a commit outran those rights while it was judged: it waits
    /// for the gate (R47).
    fn judge(&mut self, event: Arc<BusEvent>) -> Result<Taken, Arc<BusEvent>> {
        match &*event {
            BusEvent::Agent(agent) => {
                let bot = agent.bot;
                if self.rights.level(&self.actor, bot).is_none() {
                    // Dropped by a lookup, on rights as new as every commit
                    // counted so far.
                    return if self.outrun() {
                        Err(event)
                    } else {
                        Ok(Taken::Uncounted)
                    };
                }
                if matches!(agent.kind, LiveEventKind::Status(_)) {
                    self.hint(BTreeSet::from([bot]));
                } else {
                    let queued = self.queued(Slot::Agent(event));
                    self.slots.push_back(queued);
                }
                Ok(Taken::Counted)
            }
            BusEvent::Bots(bots) => {
                let ids: BTreeSet<BotId> =
                    bots.iter().copied().filter(|bot| self.sees(*bot)).collect();
                if bots.iter().any(|bot| !ids.contains(bot)) && self.outrun() {
                    return Err(event);
                }
                if ids.is_empty() {
                    return Ok(Taken::Uncounted);
                }
                self.hint(ids);
                Ok(Taken::Counted)
            }
            BusEvent::Mode(mode) => {
                // Judged now: `authorize` on `was` depends only on the viewer
                // and `was` (R49).
                let saw = mode.was.is_some_and(|was| {
                    authorize(&self.actor, Permission::ViewMode, &was.context()).is_ok()
                });
                // Read once, at its first place (R33): one waiting already is
                // read as the mode is then.
                let waiting = self
                    .slots
                    .iter_mut()
                    .find_map(|queued| match &mut queued.slot {
                        Slot::Mode { id, saw: queued } if *id == mode.id => Some(queued),
                        _ => None,
                    });
                if let Some(queued) = waiting {
                    *queued |= saw;
                } else {
                    let queued = self.queued(Slot::Mode { id: mode.id, saw });
                    self.slots.push_back(queued);
                }
                Ok(Taken::Counted)
            }
            // Another Admin's sight follows a user's role (R11); the Owner
            // and Members see what they did, and a viewer's own role closes
            // their socket.
            BusEvent::RoleChanged { user } => {
                if *user != self.actor.id && self.actor.role == Role::Admin {
                    let queued = self.queued(Slot::RoleChanged);
                    self.slots.push_back(queued);
                    Ok(Taken::Counted)
                } else {
                    Ok(Taken::Uncounted)
                }
            }
        }
    }

    /// Judges the events taken on rights a commit outran, in order and after
    /// everything taken before them, now that the gate read the rights again:
    /// until [`MERGE_MAX`] of them made or joined something to send, or
    /// another commit outran the rights; the rest wait until what this
    /// replay made went out (R47, R48, R61).
    fn replay(&mut self) {
        self.replaying = true;
        self.replay_events();
        self.replaying = false;
    }

    /// [`Self::replay`]'s loop, the slots it makes marked as a replay's.
    fn replay_events(&mut self) {
        let mut counted = 0_usize;
        while counted < MERGE_MAX {
            let Some(event) = self.unjudged.pop_front() else {
                break;
            };
            if self.outrun() {
                self.unjudged.push_front(event);
                break;
            }
            match self.judge(event) {
                Ok(Taken::Counted) => counted = counted.saturating_add(1),
                Ok(Taken::Uncounted) => {}
                Err(event) => {
                    self.unjudged.push_front(event);
                    break;
                }
            }
        }
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

    fn take(&mut self, event: Arc<BusEvent>) -> Taken {
        // Rights a commit outran may not show a bot the viewer sees now, nor
        // hide one they no longer do: the event waits, unjudged, behind any
        // that wait already, and the gate reads the rights before any is
        // judged (R47).
        if !self.unjudged.is_empty() || self.outrun() {
            self.unjudged.push_back(event);
            return Taken::Uncounted;
        }
        self.judge(event).unwrap_or_else(|event| {
            self.unjudged.push_back(event);
            Taken::Uncounted
        })
    }

    fn has_work(&self) -> bool {
        !self.slots.is_empty() || !self.unjudged.is_empty()
    }

    async fn next(&mut self) -> Result<Step, Unprojected> {
        // A replay makes slots only once those of the last one went out, so
        // the slots hold at most one wake's worth, whatever waits (R61); the
        // gate's own slots never hold it back (R70).
        let sent = !self.slots.iter().any(|queued| queued.replayed);
        if !self.gate().await? {
            return Ok(Step::Nothing);
        }
        if !self.rights.stands(&self.actor) {
            // Muted until the session check closes the socket (R31).
            self.slots.clear();
            self.unjudged.clear();
            return Ok(Step::Nothing);
        }
        // Right after the gate, before the bots it found joined are read: a
        // waiting hint of one of them merges into that read, so it comes once.
        if sent {
            self.replay();
        }
        let Some(Queued { slot, replayed }) = self.slots.pop_front() else {
            return Ok(Step::Nothing);
        };
        match slot {
            Slot::Removed(bot) => Ok(Step::Send(Box::new(EventMessage::bot_removed(bot)))),
            Slot::Bots(reads) => self.bot_step(reads, replayed).await,
            Slot::Agent(event) => Ok(event
                .agent()
                .map_or(Step::Nothing, |event| self.agent_step(event))),
            Slot::Mode { id, saw } => {
                let step = self.mode_step(id, saw).await?;
                if self.outrun() {
                    // A commit landed during the read: read again on the
                    // next turn (S43).
                    self.slots.push_front(Queued {
                        slot: Slot::Mode { id, saw },
                        replayed,
                    });
                    return Ok(Step::Nothing);
                }
                Ok(step)
            }
            Slot::RoleChanged => Ok(Step::Resync),
        }
    }

    fn resynced(&mut self) {
        self.seen = self.rights.visible(&self.actor);
        // The viewer fetches these bots now: a removal still waiting for one
        // of them would take it away again.
        drop_removals(&mut self.slots, &self.seen);
    }
}

/// Drops every `bot_removed` still waiting in `slots` for a bot of `shown`,
/// which the viewer is taken to see again (R41).
fn drop_removals(slots: &mut VecDeque<Queued>, shown: &BTreeSet<BotId>) {
    slots.retain(|queued| !matches!(queued.slot, Slot::Removed(bot) if shown.contains(&bot)));
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

#[cfg(test)]
mod tests {
    use fleet_core::authz::{ModeVisibility, UserRef};
    use fleet_core::id::{AccountId, UserId};
    use fleet_testkit::system::{ManualClock, SeededRandom};
    use sqlx::sqlite::SqliteConnectOptions;

    use super::*;
    use crate::app::events::EventBus;
    use crate::grpc::registry::AgentRegistry;
    use crate::infra::sqlite::{Database, DatabaseOptions};
    use crate::ports::bots::{Access, BotAccess, UserAccess};
    use crate::ports::live_hints::{ModeEvent, ModeSeen};
    use crate::ports::store::Store;

    fn at() -> chrono::DateTime<chrono::Utc> {
        "2027-01-15T08:00:00Z".parse().unwrap()
    }

    fn bot(n: u8) -> BotId {
        BotId::new_v7(at(), [n; 10]).unwrap()
    }

    fn account(n: u8) -> AccountId {
        AccountId::new_v7(at(), [n; 10]).unwrap()
    }

    /// `actor`'s viewer, over a fresh database it never reads, and that
    /// database, to close.
    async fn viewer(dir: &tempfile::TempDir, actor: Actor) -> (Viewer, Database) {
        // An empty file is an empty database; the read pool opens it read-only.
        let path = dir.path().join("viewer.db");
        std::fs::File::create(&path).unwrap();
        let db = Database::connect(
            SqliteConnectOptions::new().filename(&path),
            DatabaseOptions::default(),
        )
        .await
        .unwrap();
        let store: Arc<dyn Store> = Arc::new(db.clone());
        let clock = Arc::new(ManualClock::new(at()));
        let random = Arc::new(SeededRandom::new(1));
        let bots = BotService::new(
            clock.clone(),
            random.clone(),
            Arc::clone(&store),
            Arc::new(AgentRegistry::new(clock.clone())),
            EventBus::new(),
            true,
        );
        let modes = ModeService::new(clock, random, Arc::clone(&store));
        (Viewer::new(actor, LiveRights::new(store), bots, modes), db)
    }

    /// Events of two modes an Admin made, taken by a Member's viewer: each
    /// mode waits in one slot, and the Member saw it when they could under
    /// any `was` of its events, whatever their order (R49).
    #[tokio::test]
    async fn merged_mode_events_keep_whether_the_viewer_saw_it_under_any_was() {
        let dir = tempfile::tempdir().unwrap();
        let member = Actor {
            id: UserId::new_v7(at(), [1; 10]).unwrap(),
            role: Role::Member,
        };
        let (mut viewer, db) = viewer(&dir, member).await;
        let admin = UserRef {
            id: UserId::new_v7(at(), [2; 10]).unwrap(),
            role: Role::Admin,
        };
        let was = |visibility| {
            Some(ModeSeen {
                owner: Some(admin),
                visibility,
            })
        };
        let (first, second) = (
            ModeId::new_v7(at(), [3; 10]).unwrap(),
            ModeId::new_v7(at(), [4; 10]).unwrap(),
        );

        let taken: Vec<Taken> = [
            (first, ModeVisibility::Private),
            (second, ModeVisibility::Shared),
            (first, ModeVisibility::Shared),
            (second, ModeVisibility::Private),
        ]
        .into_iter()
        .map(|(id, visibility)| {
            viewer.take(Arc::new(BusEvent::Mode(ModeEvent {
                id,
                was: was(visibility),
            })))
        })
        .collect();

        let slots: Vec<(ModeId, bool)> = viewer
            .slots
            .iter()
            .filter_map(|queued| match &queued.slot {
                Slot::Mode { id, saw } => Some((*id, *saw)),
                _ => None,
            })
            .collect();
        assert_eq!(slots, [(first, true), (second, true)]);
        assert_eq!(viewer.slots.len(), 2);
        assert!(taken.iter().all(|taken| *taken == Taken::Counted));
        drop(viewer);
        db.close().await;
    }

    /// 200 mode events wait unjudged, as taken on rights a commit outran:
    /// a replay makes at most `MERGE_MAX` slots, and the next one waits
    /// until they went out, turn by turn, so the slots never hold more than
    /// one wake's worth (R61), and every event is judged in the end.
    #[tokio::test]
    async fn a_replay_adds_at_most_merge_max_slots_until_they_are_sent() {
        let dir = tempfile::tempdir().unwrap();
        let owner = UserId::new_v7(at(), [1; 10]).unwrap();
        let (mut viewer, db) = viewer(
            &dir,
            Actor {
                id: owner,
                role: Role::Owner,
            },
        )
        .await;
        viewer.rights = Arc::new(Rights::new(
            0,
            Access {
                bots: Vec::new(),
                grants: Vec::new(),
                users: vec![UserAccess {
                    user: owner,
                    role: Role::Owner,
                    enabled: true,
                }],
            },
        ));
        for n in 0..200_u8 {
            viewer
                .unjudged
                .push_back(Arc::new(BusEvent::Mode(ModeEvent {
                    id: ModeId::new_v7(at(), [n; 10]).unwrap(),
                    was: None,
                })));
        }

        let mut most = 0;
        let mut turns = 0;
        while viewer.has_work() && turns < 1000 {
            viewer.next().await.unwrap();
            most = most.max(viewer.slots.len());
            turns += 1;
        }

        assert!(most < MERGE_MAX, "the slots held {most}");
        assert!(!viewer.has_work(), "every event judged and sent");
        assert_eq!(turns, 200, "one slot a turn");
        drop(viewer);
        db.close().await;
    }

    /// Ten mode events wait unjudged while the gate keeps queueing its own
    /// slots ahead of them, a `bot_removed` every turn, as access granted
    /// and revoked around one viewer makes it: the replay still runs, since
    /// only slots a replay made hold the next one back, and every event is
    /// judged at once (R70).
    #[tokio::test]
    async fn a_replay_still_runs_while_the_gates_own_slots_keep_coming() {
        let dir = tempfile::tempdir().unwrap();
        let owner = UserId::new_v7(at(), [1; 10]).unwrap();
        let (mut viewer, db) = viewer(
            &dir,
            Actor {
                id: owner,
                role: Role::Owner,
            },
        )
        .await;
        viewer.rights = Arc::new(Rights::new(
            0,
            Access {
                bots: Vec::new(),
                grants: Vec::new(),
                users: vec![UserAccess {
                    user: owner,
                    role: Role::Owner,
                    enabled: true,
                }],
            },
        ));
        for n in 0..10_u8 {
            viewer
                .unjudged
                .push_back(Arc::new(BusEvent::Mode(ModeEvent {
                    id: ModeId::new_v7(at(), [n; 10]).unwrap(),
                    was: None,
                })));
        }

        let mut turns = 0_u8;
        while !viewer.unjudged.is_empty() && turns < 100 {
            // The gate's own slot, at the front, as it queues one for a bot
            // that left the viewer's sight.
            viewer
                .slots
                .push_front(Queued::live(Slot::Removed(bot(turns))));
            viewer.next().await.unwrap();
            turns += 1;
        }

        assert!(
            viewer.unjudged.is_empty(),
            "{} events still wait after {turns} turns",
            viewer.unjudged.len()
        );
        assert_eq!(turns, 1, "judged on the first turn");
        drop(viewer);
        db.close().await;
    }

    /// A resync while the removals of bots 2 and 3 wait, on rights that show
    /// bots 1 and 2: the viewer fetches bot 2 with the rest, so only bot 3's
    /// removal still goes.
    #[tokio::test]
    async fn a_resync_drops_a_waiting_removal_of_a_bot_the_viewer_sees_again() {
        let dir = tempfile::tempdir().unwrap();
        let owner = UserId::new_v7(at(), [1; 10]).unwrap();
        let (mut viewer, db) = viewer(
            &dir,
            Actor {
                id: owner,
                role: Role::Owner,
            },
        )
        .await;
        viewer.rights = Arc::new(Rights::new(
            0,
            Access {
                bots: vec![
                    BotAccess {
                        bot: bot(1),
                        account: account(1),
                        owner,
                    },
                    BotAccess {
                        bot: bot(2),
                        account: account(2),
                        owner,
                    },
                ],
                grants: Vec::new(),
                users: vec![UserAccess {
                    user: owner,
                    role: Role::Owner,
                    enabled: true,
                }],
            },
        ));
        viewer.seen = BTreeSet::from([bot(1)]);
        viewer.slots = VecDeque::from([
            Queued::live(Slot::Removed(bot(2))),
            Queued::live(Slot::Removed(bot(3))),
        ]);

        viewer.resynced();

        let waiting: Vec<BotId> = viewer
            .slots
            .iter()
            .filter_map(|queued| match &queued.slot {
                Slot::Removed(bot) => Some(*bot),
                _ => None,
            })
            .collect();
        assert_eq!(viewer.seen, BTreeSet::from([bot(1), bot(2)]));
        assert_eq!(waiting, [bot(3)]);
        drop(viewer);
        db.close().await;
    }
}
