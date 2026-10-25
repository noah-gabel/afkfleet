//! The server's router (Plan.md P6.7, P7.11, Appendix B): every route,
//! registered through the route registry ([`super::routes`]), behind the
//! middleware stack.
//!
//! | Route | Access | Handler |
//! |---|---|---|
//! | `GET /health/live` | public | always 200: the process runs and answers |
//! | `GET /health/ready` | public | 200 when the database answers ([`HealthService::ready`]) |
//! | `POST /api/v1/auth/login` | public (Login) | the password step: an mfa or setup token, never real tokens |
//! | `POST /api/v1/auth/login/mfa` | public (Login) | the code step: the session's tokens |
//! | `POST /api/v1/auth/refresh` | public (Refresh) | a new token pair |
//! | `POST /api/v1/auth/logout` | user | ends this session |
//! | `POST /api/v1/auth/step-up` | user (Login) | the password again: step-up actions for 5 minutes |
//! | `POST /api/v1/auth/register` | public (Register) | an account from an invite; no tokens |
//! | `POST /api/v1/me/mfa/enroll` | setup token | a new TOTP secret, waiting for confirm |
//! | `POST /api/v1/me/mfa/confirm` | setup token | confirms it; answers the recovery codes |
//! | `POST /api/v1/me/mfa/reset` | user + step-up | a setup token that replaces the authenticator |
//! | `GET /api/v1/me` | user | the user's profile and the wrong codes since the previous login |
//! | `POST /api/v1/me/password` | user + step-up (Login) | a new password; every other session ends |
//! | `GET /api/v1/me/sessions` | user | one page of the user's live sessions |
//! | `DELETE /api/v1/me/sessions/{id}` | user | ends another of the user's sessions |
//! | `GET /api/v1/users` | user (Admin+) | one page of users |
//! | `PATCH /api/v1/users/{id}` | user + step-up (the Owner; Admins for a Member's `disabled`) | one change: a role or `disabled` |
//! | `POST /api/v1/invites` | user + step-up (Admin+; Admin invites the Owner) | a new invite and its code |
//! | `GET /api/v1/invites` | user (Admin+) | one page of invites |
//! | `DELETE /api/v1/invites/{id}` | user + step-up (as creating) | revokes a pending invite |
//! | `POST /api/v1/accounts/link` | user | starts a Microsoft link flow; its code, shown this once |
//! | `GET /api/v1/accounts/link/{flow_id}` | user (the flow's creator) | where the flow stands |
//! | `POST /api/v1/accounts/offline` | user (while `[accounts] allow_offline` is on) | a new offline account |
//! | `GET /api/v1/accounts` | user | one page of the accounts the user may see |
//! | `GET /api/v1/accounts/awaiting-approval` | user | one page of the pending links the user may review |
//! | `GET /api/v1/accounts/{id}` | user (View) | one account |
//! | `DELETE /api/v1/accounts/{id}` | user + step-up (Manage) | deletes the account and its grants |
//! | `POST /api/v1/accounts/{id}/approve` | user + step-up (above the owner) | approves a pending link |
//! | `POST /api/v1/accounts/{id}/refuse` | user + step-up (above the owner) | refuses a pending link, deleting it |
//! | `POST /api/v1/accounts/{id}/relink` | user (Manage) | starts a link flow that links the account again |
//! | `GET /api/v1/accounts/{id}/grants` | user (Manage) | one page of the account's grants |
//! | `POST /api/v1/accounts/{id}/grants` | user + step-up (Manage) | shares the account with a user named in the body |
//! | `PUT /api/v1/accounts/{id}/grants/{user}` | user + step-up (Manage) | changes a grant |
//! | `DELETE /api/v1/accounts/{id}/grants/{user}` | user + step-up (Manage) | revokes a grant |
//! | `POST /api/v1/agents/enrollment-tokens` | user + step-up (the Owner; while `[grpc]` is configured) | a new enrollment token, in its bundle, shown this once |
//! | `GET /api/v1/agents` | user (Admin+) | one page of agents, with their status |
//! | `POST /api/v1/agents/{id}/disable` | user + step-up (Admin+) | disables an agent for good: every certificate revoked, its stream ended |
//! | `POST /api/v1/agents/{id}/release` | user + step-up (Admin+) | releases a disabled agent's bots, on the caller's word that its host is stopped |
//! | `GET /api/v1/audit` | user (Admin+) | one page of the audit log, newest first, each entry as stored |
//! | `GET /api/v1/bots` | user | one page of the bots the user may see, each under its account's field rule |
//! | `GET /api/v1/bots/{id}` | user (View) | one bot, with what the server knows of its state |
//! | `PATCH /api/v1/bots/{id}` | user + step-up (Manage) | the Minecraft server the bot joins |
//! | `PUT /api/v1/bots/{id}/mode` | user (Control; a mode with a command off the allowlist: Manage) | the bot's mode |
//! | `POST /api/v1/bots/{id}/start` | user (Control) | the bot should run: 200 |
//! | `POST /api/v1/bots/{id}/stop` | user (Control) | the bot should stop: 200 |
//! | `POST /api/v1/bots/{id}/restart` | user (Control) | a restart, sent to the bot's agent: 202 |
//! | `POST /api/v1/bots/{id}/reset` | user (Control) | a failed bot reset: 202 sent to its agent, 200 cleared |
//! | `POST /api/v1/bots/{id}/resume` | user (Control) | a paused bot resumed: 202 sent to its agent, 200 cleared |
//! | `GET /api/v1/bots/{id}/chat` | user (View) | one page of the bot's chat, newest first, each line under its field rules |
//! | `POST /api/v1/bots/{id}/chat` | user (Control; a command off the allowlist: pointed to `/command`) | a line for the bot to send: 200 sent, 202 pending |
//! | `POST /api/v1/bots/{id}/command` | user + step-up (Control for an allowlisted command, Manage for any other) | a command for the bot to send: 200 sent, 202 pending |
//! | `GET /api/v1/modes` | user | one page of the modes the user may see, each under its field rules |
//! | `POST /api/v1/modes` | user (a shared mode: Admin+; steps sending a command off the allowlist: pointed to `/with-commands`) | a new mode of the user's: 201 |
//! | `POST /api/v1/modes/with-commands` | user + step-up (a shared mode: Admin+) | a new mode of the user's, whatever its steps send: 201 |
//! | `PUT /api/v1/modes/{id}` | user (the right to edit it, both as it is and as it will be; steps sending a command off the allowlist: pointed to `/with-commands`) | the mode as it should be, at the version read: 200 |
//! | `PUT /api/v1/modes/{id}/with-commands` | user + step-up (the right to edit it, both ways) | the mode as it should be, whatever its steps send: 200 |
//! | `DELETE /api/v1/modes/{id}` | user + step-up (the right to delete it) | deletes a mode no bot uses: 204 |
//! | `POST /api/v1/events/ticket` | user | a single-use ticket for one live events socket of this session, 30 s |
//! | `GET /api/v1/events` | user, with a ticket of this session | the live events socket (101); any other ticket 404, the user's sockets all open 429 |
//! | `GET /api/openapi.json` | public | the `OpenAPI` document, only in dev mode ([`openapi`](super::openapi)) |
//!
//! The health routes have no `/api/v1` prefix. Both answer `200` with an
//! empty body; a failed readiness check answers like any store failure:
//! `busy` (503) when the database is busy, `internal` (500) otherwise, with
//! nothing of the cause in the response.
//!
//! Every API route is registered through the registry, so it can't exist
//! without its access, its rate-limit class and its entry in the `OpenAPI`
//! document. The document's own route isn't listed in it.

use axum::Router;
use utoipa_axum::routes;

use super::handlers::{
    accounts, agents, audit, auth, bots, chat, events, grants, health, invites, me, mfa, modes,
    users,
};
use super::middleware::RequestIds;
use super::openapi::ApiDocs;
use super::rate_limit::RateLimits;
use super::routes::{RateClass, Routes};
use super::ws::LiveEvents;
use crate::app::accounts::AccountService;
use crate::app::agents::AgentService;
use crate::app::audit::AuditService;
use crate::app::auth::TokenService;
use crate::app::bots::BotService;
use crate::app::enrollment::EnrollmentTokenService;
use crate::app::grants::GrantService;
use crate::app::health::HealthService;
use crate::app::invites::InviteService;
use crate::app::link_flows::LinkFlowService;
use crate::app::login::LoginService;
use crate::app::mfa::MfaService;
use crate::app::modes::ModeService;
use crate::app::users::UserService;
use crate::config::HttpConfig;

/// What the handlers share: the services.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The readiness check.
    pub(crate) health: HealthService,
    /// Sessions and their tokens: `AuthUser` authenticates every authed
    /// request through it.
    pub(crate) tokens: TokenService,
    /// The request limits: `AuthUser` spends each authed request's per-user
    /// limit, and the router's layers the others.
    pub(crate) limits: RateLimits,
    /// The second factor, and the login's challenges, which `SetupToken`
    /// reads.
    pub(crate) mfa: MfaService,
    /// Logging in and stepping up.
    pub(crate) login: LoginService,
    /// The users: the admins' list and changes.
    pub(crate) users: UserService,
    /// The invites to register.
    pub(crate) invites: InviteService,
    /// The Microsoft link flows.
    pub(crate) link_flows: LinkFlowService,
    /// The accounts beside the link flows: offline ones, the lists, the
    /// reviews and deleting.
    pub(crate) accounts: AccountService,
    /// The grants that share accounts.
    pub(crate) grants: GrantService,
    /// The enrollment tokens, while the control plane is on (`[grpc]`);
    /// without it, their route answers 404.
    pub(crate) enrollment_tokens: Option<EnrollmentTokenService>,
    /// The agents: their list, and disabling one, with `[grpc]` or without.
    pub(crate) agents: AgentService,
    /// The bots: what each viewer sees of them, and what users change.
    pub(crate) bots: BotService,
    /// The modes: what each viewer may see of them.
    pub(crate) modes: ModeService,
    /// The live events: the tickets, the bus and the sockets (P11.5).
    pub(crate) events: LiveEvents,
    /// The audit log, read by Admins and the Owner (P11.9).
    pub(crate) audit: AuditService,
}

/// Every service the handlers share, by name, so each one is a line of its
/// own where the state is built.
#[derive(Debug, Clone)]
pub struct Services {
    /// The readiness check.
    pub health: HealthService,
    /// Sessions and their tokens.
    pub tokens: TokenService,
    /// The request limits.
    pub limits: RateLimits,
    /// The second factor and the login's challenges.
    pub mfa: MfaService,
    /// Logging in and stepping up.
    pub login: LoginService,
    /// The users: the admins' list and changes.
    pub users: UserService,
    /// The invites to register.
    pub invites: InviteService,
    /// The Microsoft link flows.
    pub link_flows: LinkFlowService,
    /// The accounts beside the link flows: offline ones, the lists, the
    /// reviews and deleting.
    pub accounts: AccountService,
    /// The grants that share accounts.
    pub grants: GrantService,
    /// The enrollment tokens, while the control plane is on (`[grpc]`).
    pub enrollment_tokens: Option<EnrollmentTokenService>,
    /// The agents: their list, and disabling one.
    pub agents: AgentService,
    /// The bots: what each viewer sees of them, and what users change.
    pub bots: BotService,
    /// The modes: what each viewer may see of them.
    pub modes: ModeService,
    /// The live events: the tickets, the bus and the sockets.
    pub events: LiveEvents,
    /// The audit log, read by Admins and the Owner.
    pub audit: AuditService,
}

impl AppState {
    /// The state over the server's `services`.
    #[must_use]
    pub fn new(services: Services) -> Self {
        let Services {
            health,
            tokens,
            limits,
            mfa,
            login,
            users,
            invites,
            link_flows,
            accounts,
            grants,
            enrollment_tokens,
            agents,
            bots,
            modes,
            events,
            audit,
        } = services;
        Self {
            health,
            tokens,
            limits,
            mfa,
            login,
            users,
            invites,
            link_flows,
            accounts,
            grants,
            enrollment_tokens,
            agents,
            bots,
            modes,
            events,
            audit,
        }
    }
}

/// Adds every route of the API to `routes`; see the [module docs](self).
#[must_use]
pub fn api_routes(routes: Routes) -> Routes {
    routes
        .public_route(RateClass::Standard, routes!(health::live))
        .public_route(RateClass::Standard, routes!(health::ready))
        .public_route(RateClass::Login, routes!(auth::login))
        .public_route(RateClass::Login, routes!(auth::login_mfa))
        .public_route(RateClass::Refresh, routes!(auth::refresh))
        .authed_route(RateClass::Standard, routes!(auth::logout))
        .authed_route(RateClass::Login, routes!(auth::step_up))
        .setup_route(RateClass::Standard, routes!(mfa::enroll))
        .setup_route(RateClass::Standard, routes!(mfa::confirm))
        .step_up_route(RateClass::Standard, routes!(mfa::reset))
        .public_route(RateClass::Register, routes!(auth::register))
        .authed_route(RateClass::Standard, routes!(me::me))
        .step_up_route(RateClass::Login, routes!(me::change_password))
        .authed_route(RateClass::Standard, routes!(me::list_sessions))
        .authed_route(RateClass::Standard, routes!(me::revoke_session))
        .authed_route(RateClass::Standard, routes!(users::list))
        .step_up_route(RateClass::Standard, routes!(users::patch))
        .step_up_route(RateClass::Standard, routes!(invites::create))
        .authed_route(RateClass::Standard, routes!(invites::list))
        .step_up_route(RateClass::Standard, routes!(invites::revoke))
        .authed_route(RateClass::Standard, routes!(accounts::start_link))
        .authed_route(RateClass::Standard, routes!(accounts::link_status))
        .authed_route(RateClass::Standard, routes!(accounts::create_offline))
        .authed_route(RateClass::Standard, routes!(accounts::list))
        .authed_route(RateClass::Standard, routes!(accounts::awaiting_approval))
        .authed_route(RateClass::Standard, routes!(accounts::get))
        .step_up_route(RateClass::Standard, routes!(accounts::delete))
        .step_up_route(RateClass::Standard, routes!(accounts::approve))
        .step_up_route(RateClass::Standard, routes!(accounts::refuse))
        .authed_route(RateClass::Standard, routes!(accounts::relink))
        .authed_route(RateClass::Standard, routes!(grants::list))
        .step_up_route(RateClass::Standard, routes!(grants::create))
        .step_up_route(RateClass::Standard, routes!(grants::change))
        .step_up_route(RateClass::Standard, routes!(grants::revoke))
        .step_up_route(
            RateClass::Standard,
            routes!(agents::create_enrollment_token),
        )
        .authed_route(RateClass::Standard, routes!(agents::list_agents))
        .step_up_route(RateClass::Standard, routes!(agents::disable_agent))
        .step_up_route(RateClass::Standard, routes!(agents::release_agent))
        .authed_route(RateClass::Standard, routes!(audit::list))
        .authed_route(RateClass::Standard, routes!(bots::list))
        .authed_route(RateClass::Standard, routes!(bots::get))
        .step_up_route(RateClass::Standard, routes!(bots::patch))
        .authed_route(RateClass::Standard, routes!(bots::set_mode))
        .authed_route(RateClass::Standard, routes!(bots::start))
        .authed_route(RateClass::Standard, routes!(bots::stop))
        .authed_route(RateClass::Standard, routes!(bots::restart))
        .authed_route(RateClass::Standard, routes!(bots::reset))
        .authed_route(RateClass::Standard, routes!(bots::resume))
        .authed_route(RateClass::Standard, routes!(chat::history))
        .authed_route(RateClass::Standard, routes!(chat::send))
        .step_up_route(RateClass::Standard, routes!(chat::command))
        .authed_route(RateClass::Standard, routes!(modes::list))
        .authed_route(RateClass::Standard, routes!(modes::create))
        .step_up_route(RateClass::Standard, routes!(modes::create_with_commands))
        .authed_route(RateClass::Standard, routes!(modes::update))
        .step_up_route(RateClass::Standard, routes!(modes::update_with_commands))
        .step_up_route(RateClass::Standard, routes!(modes::delete))
        .authed_route(RateClass::Standard, routes!(events::ticket))
        .authed_route(RateClass::Standard, routes!(events::connect))
}

/// Every route with `state`, behind the middleware stack with the timeout,
/// the body limit and the trusted proxies from `http`, request IDs from `ids`
/// and the limits `state` carries. `docs` says
/// whether `GET /api/openapi.json` exists: only in dev mode.
pub fn router(state: AppState, http: &HttpConfig, ids: RequestIds, docs: ApiDocs) -> Router {
    let routes = Routes::new(&state.limits);
    api_routes(routes).finish(state, http, ids, docs).0
}
