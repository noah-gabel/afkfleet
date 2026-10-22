//! `POST /api/v1/events/ticket` and `GET /api/v1/events`, the live events
//! socket (Plan.md P11.5, §7.4, Appendix B; ADR-0020, S28, S29, S35, R10,
//! R17).
//!
//! - **The ticket** is the user's own (`UsePersonal`), for one socket of
//!   this session within 30 s; minting it stores nothing, so no audit entry.
//! - **The connect** needs the session's access token and a ticket issued
//!   to that very session, and checks them in this order:
//!   1. `AuthUser` (401, the per-user 429), before anything is read;
//!   2. the upgrade's headers: 405 for a method that isn't `GET`, 400 for a
//!      request that isn't an upgrade, before the ticket is spent;
//!   3. the query: 400 for a parameter that isn't `ticket`, or one given
//!      twice;
//!   4. the ticket, taken at once (single use), then whose it is: every
//!      ticket that isn't a live one of this session gets the same 404, with
//!      one `debug` line naming why, never the ticket;
//!   5. the session again (404 too; a store failure 503 or 500);
//!   6. the user's place among their sockets: 429 `too_many_connections`;
//!   7. the upgrade, with the socket's limits; the socket's task runs on
//!      the sockets' owner from the start.
//!
//!   The ticket is in the URL, so no log line ever holds it: the trace
//!   layer logs the route's template, and nothing here logs the query.

use axum::Json;
use axum::extract::State;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::response::Response;
use fleet_api_types::events::{EventTicketResponse, EventsQuery};
use fleet_api_types::{ErrorResponse, SecretText};
use fleet_core::authz::{Permission, ResourceContext};
use secrecy::ExposeSecret as _;
use tracing::{Instrument as _, debug, info_span};

use crate::app::auth::{SocketCheckError, SocketSession};
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, ValidQuery};
use crate::http::router::AppState;
use crate::http::ws::{
    MAX_CLIENT_FRAME, MAX_WRITE_BUFFER, OpenSocket, READ_BUFFER, refused_upgrade, run,
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
        (status = 400, description = "Not a WebSocket upgrade, or a query that isn't one `ticket`.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 404, description = "Not a live ticket of this session: unknown, used, expired, or another session's.", body = ErrorResponse),
        (status = 429, description = "Too many requests, or `too_many_connections`: the user's sockets are all open.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
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
    ValidQuery(query): ValidQuery<EventsQuery>,
) -> Result<Response, ApiError> {
    let upgrade = upgrade.map_err(|rejection| refused_upgrade(&rejection))?;
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
    state
        .tokens
        .check_socket(&session)
        .await
        .map_err(|error| match error {
            SocketCheckError::Refused(reason) => refuse(&user, reason),
            SocketCheckError::Store(error) => ApiError::from(error),
        })?;
    let slot = state.limits.open_socket(actor.id)?;
    let owned = state.events.sockets.token();
    let open = OpenSocket {
        session,
        slot,
        events: state.events.clone(),
    };
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
            sockets.spawn(run(socket, open).instrument(span));
            // Counted by the tracker until its task is spawned.
            drop(owned);
        }))
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
