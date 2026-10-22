//! The session under every [`Client`](crate::Client): its tokens, the
//! single-flight refresh, the session's end, and every route (Plan.md
//! P8.1, ADR-0016, ADR-0017).
//!
//! # The tokens
//! - **The access token** lives only here, with a generation number that
//!   grows with every one the client adopts.
//! - **The refresh token** lives only in the [`SessionStore`]: a refresh
//!   loads it, sends it, and saves the new one.
//! - **A pending step**, the password step's mfa or setup token or a 2FA
//!   replacement's setup token, lives only here, one at a time.
//!
//! # Refreshing: one at a time, only on a 401 `unauthorized`
//! An authenticated request that gets a 401 `unauthorized` asks
//! [`after_unauthorized`] what to do:
//! - **A refresh replaced the token it used meanwhile:** retry once with the
//!   current token, without refreshing again.
//! - **It used the current token:** refresh, then retry once. Refreshes are
//!   serialized by the refresh lock, and a refresh that finds a newer token
//!   than the one its caller used returns that token instead, so many 401s
//!   at once cost one rotation.
//! - **It was already retried with the newest token:** the session was
//!   revoked (a password change elsewhere, a disabled user); it ends.
//!
//! A request without an access token, as after a start, refreshes first,
//! and counts as retried. Only authenticated routes refresh: a login's 401
//! `invalid_credentials`, a step-up's 403 and a setup route's 401 never do.
//!
//! # The session's end, and the store gate
//! The session ends locally on a refused refresh, on a 401 after the retry,
//! on an empty store, and at every logout, even an unreachable server's;
//! never on a transient failure. Ending clears the store, the access token
//! and the pending step.
//!
//! **An end never races a save** (ADR-0017). A second lock, the store gate,
//! guards the session's epoch and every store write; the lock order is
//! always the refresh lock, then the gate, and ending takes only the gate.
//! - Every end takes the gate, bumps the epoch, clears the store and drops
//!   the tokens, without waiting for a refresh's network call.
//! - Every save (a refresh's, a login's) records the epoch when it starts,
//!   and after its POST saves and adopts the new tokens under the gate only
//!   if the epoch is unchanged.
//! - Otherwise it saves nothing, its caller gets [`ClientError::NoSession`],
//!   and it revokes the session its POST just created with that session's
//!   own access token, best effort, so the server keeps no orphan when it
//!   can be reached.
//!
//! A refresh loads its token under the gate and marks it as out until it
//! finishes; a logout that finds a token out leaves the server's side to
//! that refresh, since sending the same token again would be reuse.
//!
//! Every holder of either lock does bounded work: each store call has its
//! own timeout, and each request reqwest's.
//!
//! # A dropped caller never stops a save
//! A refresh and a code step run inside the future of the call that needs
//! them, which its caller may drop (a `select!`, a timeout). Once their
//! request may have reached the server, the server has replaced the stored
//! refresh token (a refresh) or started a session (a code step), so losing
//! the answer would leave a replaced token behind, whose next use is reuse,
//! or an orphaned session (ADR-0017, the user's review).
//!
//! So from that point on, the request, the save and the adoption run in a
//! task of their own (`tokio::spawn`), over an `Arc` of the core, and the
//! caller awaits its answer. A dropped caller drops only its wait; the task
//! saves and adopts the new pair, or, after an end, revokes it (the epoch
//! rule). A refresh's task holds the refresh lock and the refresh-out mark
//! until it ends, so refreshes stay one at a time, and a caller that arrives
//! meanwhile finds its result. The task is bounded by its request's and its
//! store calls' timeouts; its only owner is its caller's wait, an exception
//! CLAUDE.md lists. One that ends without an answer (a panic, a runtime
//! shutting down) is [`ClientError::Interrupted`].

use core::error::Error;
use core::fmt::Write as _;
use core::time::Duration;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use fleet_api_types::accounts::{
    AccountPage, AccountResponse, ChangeGrantRequest, CreateGrantRequest,
    CreateOfflineAccountRequest, CreatedOfflineAccountResponse, GrantPage, GrantResponse,
    LinkFlowResponse, LinkStartedResponse,
};
use fleet_api_types::auth::{
    LoginMfaRequest, LoginRequest, LoginResponse, LoginStatus, RefreshRequest, RegisterRequest,
    RegisterResponse, StepUpRequest, TokenResponse,
};
use fleet_api_types::bots::{BotModeRequest, BotPage, BotPatchRequest, BotResponse};
use fleet_api_types::chat::{
    ChatCommandRequest, ChatPage, ChatPageQuery, ChatSendRequest, ChatSendResponse,
};
#[cfg(feature = "dev-console")]
use fleet_api_types::events::EventTicketResponse;
use fleet_api_types::invites::{CreateInviteRequest, CreatedInviteResponse, InvitePage};
use fleet_api_types::me::{MeResponse, PasswordChangeRequest};
use fleet_api_types::mfa::{
    CODE_MAX_CHARS, MfaConfirmRequest, MfaConfirmResponse, MfaEnrollResponse, MfaResetResponse,
};
use fleet_api_types::modes::{CreateModeRequest, ModePage, ModeResponse, UpdateModeRequest};
use fleet_api_types::sessions::SessionPage;
use fleet_api_types::users::{UserPage, UserPatchRequest, UserResponse};
use fleet_api_types::{
    AccountId, BotId, ErrorCode, ErrorResponse, InviteId, LinkFlowId, ModeId, PageQuery,
    SecretText, SessionId, UserId,
};
use reqwest::{Method, Url};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::client::LoginStep;
use crate::error::{ApiFailure, ClientError, Unexpected};
#[cfg(feature = "dev-console")]
use crate::events::EventsHandshake;
use crate::store::{SessionStore, StoreError, StoreOp};
use crate::transport::{Incoming, Outgoing, Transport, TransportError};
use crate::url::ServerUrl;

/// A bot's lifecycle call: its path segment, and the statuses that answer
/// it (Plan.md P11.2; ADR-0019, S50).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BotCall {
    /// `start`: always 200.
    Start,
    /// `stop`: always 200.
    Stop,
    /// `restart`: 202, sent to its agent.
    Restart,
    /// `reset`: 202 when sent, 200 when its stored state was cleared.
    Reset,
    /// `resume`: as `reset`.
    Resume,
}

impl BotCall {
    /// Its path segment, after `/bots/{id}/`.
    const fn segment(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Reset => "reset",
            Self::Resume => "resume",
        }
    }

    /// The statuses that answer it with the bot.
    const fn statuses(self) -> &'static [u16] {
        match self {
            Self::Start | Self::Stop => &[200],
            Self::Restart => &[202],
            Self::Reset | Self::Resume => &[200, 202],
        }
    }
}

/// What to do after a 401 `unauthorized`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Next {
    /// A refresh replaced the token used: retry with the current one.
    RetryWithCurrent,
    /// Refresh, then retry.
    Refresh,
    /// The session was revoked: end it.
    EndSession,
}

/// The rule after a 401 `unauthorized`: `used` is the generation of the
/// token the request sent (`None` without one), `current` the generation
/// the client holds now, and `retried` whether the request was already
/// retried with the newest token. See the [module docs](self).
#[must_use]
pub(crate) fn after_unauthorized(used: Option<u64>, current: Option<u64>, retried: bool) -> Next {
    if retried {
        return Next::EndSession;
    }
    match current {
        Some(current) if Some(current) != used => Next::RetryWithCurrent,
        _ => Next::Refresh,
    }
}

/// Why the session ended, for the log.
#[derive(Debug, Clone, Copy)]
enum EndReason {
    RefreshRefused,
    Revoked,
    StoreEmpty,
    LoggedOut,
    Discarded,
}

impl EndReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::RefreshRefused => "refresh_refused",
            Self::Revoked => "revoked",
            Self::StoreEmpty => "store_empty",
            Self::LoggedOut => "logged_out",
            Self::Discarded => "discarded",
        }
    }
}

/// The session and the routes, over a transport; see the
/// [module docs](self).
pub(crate) struct Core<S, T> {
    server: ServerUrl,
    transport: T,
    store: S,
    store_timeout: Duration,
    state: Mutex<State>,
    /// Serializes refreshes; held across the refresh's request, by the
    /// refresh's own task.
    refresh_lock: Arc<AsyncMutex<()>>,
    /// Guards the epoch and every store write; never held across a request.
    gate: AsyncMutex<()>,
}

/// What the session holds. Behind a std mutex that's never held across an
/// await.
#[derive(Default)]
struct State {
    access: Option<Access>,
    pending: Option<Pending>,
    /// Bumped by every end, so a save that started before it is dropped.
    epoch: u64,
    /// The last access token's generation.
    generations: u64,
    /// The last pending step's number.
    pending_serial: u64,
    /// A refresh holds the stored token outside the store.
    refresh_out: bool,
}

/// An access token and its generation.
#[derive(Clone)]
struct Access {
    token: SecretString,
    generation: u64,
}

/// A pending step's token and its number.
#[derive(Clone)]
struct Pending {
    kind: StepKind,
    token: SecretString,
    serial: u64,
}

/// Which token a step holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepKind {
    /// The password step's mfa token, for `POST /auth/login/mfa`.
    Mfa,
    /// A setup token, for enroll and confirm.
    Setup,
}

/// Clears `refresh_out` when the refresh that set it ends, whichever way it
/// ends. Its task owns it.
struct RefreshOut<S, T>(Arc<Core<S, T>>);

impl<S, T> Drop for RefreshOut<S, T> {
    fn drop(&mut self) {
        self.0.state().refresh_out = false;
    }
}

impl<S, T> Core<S, T> {
    /// A session over `transport` and `store`, with no tokens yet; a stored
    /// refresh token is used by the first authenticated call.
    pub(crate) fn new(server: ServerUrl, transport: T, store: S, store_timeout: Duration) -> Self {
        Self {
            server,
            transport,
            store,
            store_timeout,
            state: Mutex::new(State::default()),
            refresh_lock: Arc::new(AsyncMutex::new(())),
            gate: AsyncMutex::new(()),
        }
    }

    pub(crate) fn server(&self) -> &ServerUrl {
        &self.server
    }

    /// The state. A thread that panicked while holding it left whole values
    /// behind, so a poisoned lock is used as it is.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn current_access(&self) -> Option<Access> {
        self.state().access.clone()
    }

    /// The pending step's token and number, if it's of `kind`.
    fn pending(&self, kind: StepKind) -> Result<(SecretString, u64), ClientError> {
        match &self.state().pending {
            Some(pending) if pending.kind == kind => Ok((pending.token.clone(), pending.serial)),
            _ => Err(ClientError::NoPendingStep),
        }
    }

    /// Makes `token` the pending step.
    fn set_pending(&self, kind: StepKind, token: SecretString) {
        let mut state = self.state();
        state.pending_serial = state.pending_serial.wrapping_add(1);
        state.pending = Some(Pending {
            kind,
            token,
            serial: state.pending_serial,
        });
    }

    /// Drops the pending step numbered `serial`, unless another replaced it.
    fn drop_pending(&self, serial: u64) {
        let mut state = self.state();
        if state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.serial == serial)
        {
            state.pending = None;
        }
    }
}

impl<S: SessionStore, T: Transport> Core<S, T> {
    // ---- The routes ----

    pub(crate) async fn health(&self, which: &str) -> Result<(), ClientError> {
        let answer = self
            .send(Method::GET, self.server.health(which), None, None)
            .await?;
        expect_empty(answer, 200)
    }

    pub(crate) async fn login(&self, request: &LoginRequest) -> Result<LoginStep, ClientError> {
        if self.current_access().is_some()
            || self.load().await.map_err(ClientError::Store)?.is_some()
        {
            return Err(ClientError::AlreadyLoggedIn);
        }
        let answer = self
            .send(
                Method::POST,
                self.server.api(&["auth", "login"]),
                None,
                Some(json(request)),
            )
            .await?;
        let response: LoginResponse = expect_json(answer, 200)?;
        let expires_in = Duration::from_secs(response.expires_in.into());
        let (kind, step) = match response.status.known() {
            Some(LoginStatus::MfaRequired) => {
                (StepKind::Mfa, LoginStep::MfaRequired { expires_in })
            }
            Some(LoginStatus::MfaSetupRequired) => {
                (StepKind::Setup, LoginStep::SetupRequired { expires_in })
            }
            None => {
                return Err(ClientError::UnexpectedResponse {
                    status: 200,
                    reason: Unexpected::UnknownLoginStatus,
                });
            }
        };
        self.set_pending(kind, response.token.into_secret());
        Ok(step)
    }

    pub(crate) async fn login_mfa(
        self: &Arc<Self>,
        code: SecretText<CODE_MAX_CHARS>,
    ) -> Result<(), ClientError> {
        let (mfa_token, serial) = self.pending(StepKind::Mfa)?;
        let epoch = self.state().epoch;
        let request = LoginMfaRequest {
            mfa_token: SecretText::new(mfa_token.expose_secret().to_owned()),
            code,
        };
        // A code step that reaches the server starts a session there: the
        // rest runs in a task of its own (see the module docs).
        let session = Arc::clone(self);
        outcome(tokio::spawn(async move {
            session.code_step(epoch, serial, &request).await
        }))
        .await
    }

    /// Ends the session here and sends nothing: the store is cleared and the
    /// access token and the pending step are dropped, under the gate, so a
    /// save in flight drops its pair (the epoch rule). Unlike every other
    /// end, a failed clear is returned, since the caller must not go on as
    /// if no token were stored.
    pub(crate) async fn discard(&self) -> Result<(), ClientError> {
        let _gate = self.gate.lock().await;
        self.drop_state();
        let cleared = self.clear().await;
        info!(reason = EndReason::Discarded.as_str(), "the session ended");
        cleared.map_err(ClientError::Store)
    }

    pub(crate) async fn logout(&self) -> Result<(), ClientError> {
        let (access, refresh, refresh_out) = {
            let _gate = self.gate.lock().await;
            let (access, refresh_out) = {
                let state = self.state();
                (state.access.clone(), state.refresh_out)
            };
            let refresh = self.load().await.unwrap_or_else(|error| {
                debug!(error = %chain(&error), "the refresh token can't be read for the logout");
                None
            });
            let held = access.is_some() || refresh.is_some();
            self.end_under_gate().await;
            if !held {
                return Ok(());
            }
            info!(reason = EndReason::LoggedOut.as_str(), "the session ended");
            (access, refresh, refresh_out)
        };
        let logout = self.server.api(&["auth", "logout"]);
        if let Some(access) = access {
            let answer = self
                .send(Method::POST, logout.clone(), Some(access.token), None)
                .await?;
            if !is_unauthorized(&answer) {
                return expect_empty(answer, 204);
            }
        }
        // A refresh that holds the token revokes its own new session (the
        // epoch rule); sending the token again would be reuse.
        let (Some(refresh), false) = (refresh, refresh_out) else {
            return Ok(());
        };
        // No later refresh can load it: the store is empty.
        let answer = self
            .send(
                Method::POST,
                self.server.api(&["auth", "refresh"]),
                None,
                Some(refresh_body(&refresh)),
            )
            .await?;
        if is_unauthorized(&answer) {
            return Ok(());
        }
        let tokens: TokenResponse = expect_json(answer, 200)?;
        let answer = self
            .send(
                Method::POST,
                logout,
                Some(tokens.access_token.into_secret()),
                None,
            )
            .await?;
        if is_unauthorized(&answer) {
            return Ok(());
        }
        expect_empty(answer, 204)
    }

    pub(crate) async fn step_up(
        self: &Arc<Self>,
        request: &StepUpRequest,
    ) -> Result<(), ClientError> {
        let answer = self
            .authed(
                Method::POST,
                self.server.api(&["auth", "step-up"]),
                Some(json(request)),
            )
            .await?;
        expect_empty(answer, 204)
    }

    pub(crate) async fn register(
        &self,
        request: &RegisterRequest,
    ) -> Result<RegisterResponse, ClientError> {
        let answer = self
            .send(
                Method::POST,
                self.server.api(&["auth", "register"]),
                None,
                Some(json(request)),
            )
            .await?;
        expect_json(answer, 201)
    }

    pub(crate) async fn me(self: &Arc<Self>) -> Result<MeResponse, ClientError> {
        let answer = self
            .authed(Method::GET, self.server.api(&["me"]), None)
            .await?;
        expect_json(answer, 200)
    }

    pub(crate) async fn change_password(
        self: &Arc<Self>,
        request: &PasswordChangeRequest,
    ) -> Result<(), ClientError> {
        let answer = self
            .authed(
                Method::POST,
                self.server.api(&["me", "password"]),
                Some(json(request)),
            )
            .await?;
        expect_empty(answer, 204)
    }

    pub(crate) async fn sessions(
        self: &Arc<Self>,
        query: &PageQuery<SessionId>,
    ) -> Result<SessionPage, ClientError> {
        let url = with_page(self.server.api(&["me", "sessions"]), query);
        let answer = self.authed(Method::GET, url, None).await?;
        expect_json(answer, 200)
    }

    pub(crate) async fn revoke_session(self: &Arc<Self>, id: SessionId) -> Result<(), ClientError> {
        let url = self.server.api(&["me", "sessions", &id.to_string()]);
        let answer = self.authed(Method::DELETE, url, None).await?;
        expect_empty(answer, 204)
    }

    pub(crate) async fn mfa_enroll(&self) -> Result<MfaEnrollResponse, ClientError> {
        let answer = self.with_setup(&["me", "mfa", "enroll"], None).await?;
        expect_json(answer, 200)
    }

    pub(crate) async fn mfa_confirm(
        &self,
        code: SecretText<CODE_MAX_CHARS>,
    ) -> Result<MfaConfirmResponse, ClientError> {
        let request = MfaConfirmRequest { code };
        let (answer, serial) = self
            .with_setup_serial(&["me", "mfa", "confirm"], Some(json(&request)))
            .await?;
        let codes = expect_json(answer, 200)?;
        self.drop_pending(serial);
        Ok(codes)
    }

    pub(crate) async fn mfa_reset(self: &Arc<Self>) -> Result<Duration, ClientError> {
        let answer = self
            .authed(Method::POST, self.server.api(&["me", "mfa", "reset"]), None)
            .await?;
        let response: MfaResetResponse = expect_json(answer, 200)?;
        self.set_pending(StepKind::Setup, response.setup_token.into_secret());
        Ok(Duration::from_secs(response.expires_in.into()))
    }

    pub(crate) async fn users(
        self: &Arc<Self>,
        query: &PageQuery<UserId>,
    ) -> Result<UserPage, ClientError> {
        let url = with_page(self.server.api(&["users"]), query);
        let answer = self.authed(Method::GET, url, None).await?;
        expect_json(answer, 200)
    }

    pub(crate) async fn patch_user(
        self: &Arc<Self>,
        id: UserId,
        request: &UserPatchRequest,
    ) -> Result<UserResponse, ClientError> {
        let url = self.server.api(&["users", &id.to_string()]);
        let answer = self.authed(Method::PATCH, url, Some(json(request))).await?;
        expect_json(answer, 200)
    }

    pub(crate) async fn create_invite(
        self: &Arc<Self>,
        request: &CreateInviteRequest,
    ) -> Result<CreatedInviteResponse, ClientError> {
        let answer = self
            .authed(
                Method::POST,
                self.server.api(&["invites"]),
                Some(json(request)),
            )
            .await?;
        expect_json(answer, 201)
    }

    pub(crate) async fn invites(
        self: &Arc<Self>,
        query: &PageQuery<InviteId>,
    ) -> Result<InvitePage, ClientError> {
        let url = with_page(self.server.api(&["invites"]), query);
        let answer = self.authed(Method::GET, url, None).await?;
        expect_json(answer, 200)
    }

    pub(crate) async fn revoke_invite(self: &Arc<Self>, id: InviteId) -> Result<(), ClientError> {
        let url = self.server.api(&["invites", &id.to_string()]);
        let answer = self.authed(Method::DELETE, url, None).await?;
        expect_empty(answer, 204)
    }

    // ---- Microsoft accounts (Plan.md P9.4, P9.6, P9.7) ----

    pub(crate) async fn start_link(self: &Arc<Self>) -> Result<LinkStartedResponse, ClientError> {
        let url = self.server.api(&["accounts", "link"]);
        let answer = self.authed(Method::POST, url, None).await?;
        expect_json(answer, 201)
    }

    pub(crate) async fn link_status(
        self: &Arc<Self>,
        flow: LinkFlowId,
    ) -> Result<LinkFlowResponse, ClientError> {
        let url = self.server.api(&["accounts", "link", &flow.to_string()]);
        let answer = self.authed(Method::GET, url, None).await?;
        expect_json(answer, 200)
    }

    pub(crate) async fn create_offline_account(
        self: &Arc<Self>,
        request: &CreateOfflineAccountRequest,
    ) -> Result<CreatedOfflineAccountResponse, ClientError> {
        let url = self.server.api(&["accounts", "offline"]);
        let answer = self.authed(Method::POST, url, Some(json(request))).await?;
        expect_json(answer, 201)
    }

    pub(crate) async fn accounts(
        self: &Arc<Self>,
        query: &PageQuery<AccountId>,
    ) -> Result<AccountPage, ClientError> {
        let url = with_page(self.server.api(&["accounts"]), query);
        let answer = self.authed(Method::GET, url, None).await?;
        expect_json(answer, 200)
    }

    pub(crate) async fn accounts_awaiting_approval(
        self: &Arc<Self>,
        query: &PageQuery<AccountId>,
    ) -> Result<AccountPage, ClientError> {
        let url = with_page(self.server.api(&["accounts", "awaiting-approval"]), query);
        let answer = self.authed(Method::GET, url, None).await?;
        expect_json(answer, 200)
    }

    pub(crate) async fn account(
        self: &Arc<Self>,
        id: AccountId,
    ) -> Result<AccountResponse, ClientError> {
        let url = self.server.api(&["accounts", &id.to_string()]);
        let answer = self.authed(Method::GET, url, None).await?;
        expect_json(answer, 200)
    }

    pub(crate) async fn delete_account(self: &Arc<Self>, id: AccountId) -> Result<(), ClientError> {
        let url = self.server.api(&["accounts", &id.to_string()]);
        let answer = self.authed(Method::DELETE, url, None).await?;
        expect_empty(answer, 204)
    }

    pub(crate) async fn approve_account(
        self: &Arc<Self>,
        id: AccountId,
    ) -> Result<AccountResponse, ClientError> {
        let url = self.server.api(&["accounts", &id.to_string(), "approve"]);
        let answer = self.authed(Method::POST, url, None).await?;
        expect_json(answer, 200)
    }

    pub(crate) async fn refuse_account(self: &Arc<Self>, id: AccountId) -> Result<(), ClientError> {
        let url = self.server.api(&["accounts", &id.to_string(), "refuse"]);
        let answer = self.authed(Method::POST, url, None).await?;
        expect_empty(answer, 204)
    }

    pub(crate) async fn relink_account(
        self: &Arc<Self>,
        id: AccountId,
    ) -> Result<LinkStartedResponse, ClientError> {
        let url = self.server.api(&["accounts", &id.to_string(), "relink"]);
        let answer = self.authed(Method::POST, url, None).await?;
        expect_json(answer, 201)
    }

    pub(crate) async fn account_grants(
        self: &Arc<Self>,
        id: AccountId,
        query: &PageQuery<UserId>,
    ) -> Result<GrantPage, ClientError> {
        let url = with_page(
            self.server.api(&["accounts", &id.to_string(), "grants"]),
            query,
        );
        let answer = self.authed(Method::GET, url, None).await?;
        expect_json(answer, 200)
    }

    pub(crate) async fn create_grant(
        self: &Arc<Self>,
        id: AccountId,
        request: &CreateGrantRequest,
    ) -> Result<GrantResponse, ClientError> {
        let url = self.server.api(&["accounts", &id.to_string(), "grants"]);
        let answer = self.authed(Method::POST, url, Some(json(request))).await?;
        expect_json(answer, 201)
    }

    pub(crate) async fn change_grant(
        self: &Arc<Self>,
        id: AccountId,
        user: UserId,
        request: &ChangeGrantRequest,
    ) -> Result<GrantResponse, ClientError> {
        let url = self.grant_url(id, user);
        let answer = self.authed(Method::PUT, url, Some(json(request))).await?;
        expect_json(answer, 200)
    }

    pub(crate) async fn revoke_grant(
        self: &Arc<Self>,
        id: AccountId,
        user: UserId,
    ) -> Result<(), ClientError> {
        let url = self.grant_url(id, user);
        let answer = self.authed(Method::DELETE, url, None).await?;
        expect_empty(answer, 204)
    }

    /// `/accounts/{id}/grants/{user}`.
    fn grant_url(&self, id: AccountId, user: UserId) -> Url {
        self.server
            .api(&["accounts", &id.to_string(), "grants", &user.to_string()])
    }

    // ---- Bots (Plan.md P11.2) ----

    pub(crate) async fn bots(
        self: &Arc<Self>,
        query: &PageQuery<BotId>,
    ) -> Result<BotPage, ClientError> {
        let url = with_page(self.server.api(&["bots"]), query);
        let answer = self.authed(Method::GET, url, None).await?;
        expect_json(answer, 200)
    }

    pub(crate) async fn bot(self: &Arc<Self>, id: BotId) -> Result<BotResponse, ClientError> {
        let url = self.server.api(&["bots", &id.to_string()]);
        let answer = self.authed(Method::GET, url, None).await?;
        expect_json(answer, 200)
    }

    pub(crate) async fn patch_bot(
        self: &Arc<Self>,
        id: BotId,
        request: &BotPatchRequest,
    ) -> Result<BotResponse, ClientError> {
        let url = self.server.api(&["bots", &id.to_string()]);
        let answer = self.authed(Method::PATCH, url, Some(json(request))).await?;
        expect_json(answer, 200)
    }

    pub(crate) async fn set_bot_mode(
        self: &Arc<Self>,
        id: BotId,
        request: &BotModeRequest,
    ) -> Result<BotResponse, ClientError> {
        let url = self.server.api(&["bots", &id.to_string(), "mode"]);
        let answer = self.authed(Method::PUT, url, Some(json(request))).await?;
        expect_json(answer, 200)
    }

    pub(crate) async fn control_bot(
        self: &Arc<Self>,
        id: BotId,
        call: BotCall,
    ) -> Result<BotResponse, ClientError> {
        let url = self.server.api(&["bots", &id.to_string(), call.segment()]);
        let answer = self.authed(Method::POST, url, None).await?;
        expect_json_of(answer, call.statuses())
    }

    // ---- Chat (Plan.md P11.3) ----

    pub(crate) async fn send_chat(
        self: &Arc<Self>,
        id: BotId,
        request: &ChatSendRequest,
    ) -> Result<ChatSendResponse, ClientError> {
        self.send_line(id, "chat", json(request)).await
    }

    pub(crate) async fn send_command(
        self: &Arc<Self>,
        id: BotId,
        request: &ChatCommandRequest,
    ) -> Result<ChatSendResponse, ClientError> {
        self.send_line(id, "command", json(request)).await
    }

    /// One page of the bot `id`'s chat, newest first, as the caller may see
    /// it (200).
    pub(crate) async fn chat(
        self: &Arc<Self>,
        id: BotId,
        query: &ChatPageQuery,
    ) -> Result<ChatPage, ClientError> {
        let url = with_chat_page(self.server.api(&["bots", &id.to_string(), "chat"]), query);
        let answer = self.authed(Method::GET, url, None).await?;
        expect_json(answer, 200)
    }

    /// Posts a line's `body` to the bot `id`'s `segment`: 200 when its agent
    /// said the bot sent it, 202 when no answer came in time.
    async fn send_line(
        self: &Arc<Self>,
        id: BotId,
        segment: &str,
        body: Vec<u8>,
    ) -> Result<ChatSendResponse, ClientError> {
        let url = self.server.api(&["bots", &id.to_string(), segment]);
        let answer = self.authed(Method::POST, url, Some(body)).await?;
        expect_json_of(answer, &[200, 202])
    }

    // ---- Modes (Plan.md P11.4) ----

    /// One page of the modes the caller may see, newest first (200).
    pub(crate) async fn modes(
        self: &Arc<Self>,
        query: &PageQuery<ModeId>,
    ) -> Result<ModePage, ClientError> {
        let url = with_page(self.server.api(&["modes"]), query);
        let answer = self.authed(Method::GET, url, None).await?;
        expect_json(answer, 200)
    }

    /// A new mode through `POST /modes`, or its step-up twin when
    /// `with_commands` (201).
    pub(crate) async fn create_mode(
        self: &Arc<Self>,
        request: &CreateModeRequest,
        with_commands: bool,
    ) -> Result<ModeResponse, ClientError> {
        let url = if with_commands {
            self.server.api(&["modes", "with-commands"])
        } else {
            self.server.api(&["modes"])
        };
        let answer = self.authed(Method::POST, url, Some(json(request))).await?;
        expect_json(answer, 201)
    }

    /// The mode `id` edited through `PUT /modes/{id}`, or its step-up twin
    /// when `with_commands` (200).
    pub(crate) async fn update_mode(
        self: &Arc<Self>,
        id: ModeId,
        request: &UpdateModeRequest,
        with_commands: bool,
    ) -> Result<ModeResponse, ClientError> {
        let id = id.to_string();
        let url = if with_commands {
            self.server.api(&["modes", &id, "with-commands"])
        } else {
            self.server.api(&["modes", &id])
        };
        let answer = self.authed(Method::PUT, url, Some(json(request))).await?;
        expect_json(answer, 200)
    }

    /// The mode `id` deleted through `DELETE /modes/{id}` (204).
    pub(crate) async fn delete_mode(self: &Arc<Self>, id: ModeId) -> Result<(), ClientError> {
        let url = self.server.api(&["modes", &id.to_string()]);
        let answer = self.authed(Method::DELETE, url, None).await?;
        expect_empty(answer, 204)
    }

    /// `POST /events/ticket`, and the socket's URL and bearer with it
    /// (`dev-console` only; ADR-0020, U15).
    #[cfg(feature = "dev-console")]
    pub(crate) async fn events_handshake(self: &Arc<Self>) -> Result<EventsHandshake, ClientError> {
        let answer = self
            .authed(Method::POST, self.server.api(&["events", "ticket"]), None)
            .await?;
        let ticket: EventTicketResponse = expect_json(answer, 200)?;
        // The token the ticket's request went with, or a newer one of the
        // same session: the ticket belongs to the session.
        let access = self.current_access().ok_or(ClientError::NoSession)?;
        let url = self.server.events(ticket.ticket.expose());
        Ok(EventsHandshake::new(url.as_str(), &access.token))
    }

    // ---- Sending ----

    /// Sends one request.
    async fn send(
        &self,
        method: Method,
        url: Url,
        bearer: Option<SecretString>,
        body: Option<Vec<u8>>,
    ) -> Result<Incoming, ClientError> {
        self.transport
            .send(Outgoing {
                method,
                url,
                bearer,
                body,
            })
            .await
            .map_err(|error| match error {
                TransportError::Unreachable(cause) => ClientError::Unreachable(cause),
                TransportError::Certificate(cause) => ClientError::Certificate(cause),
                TransportError::TimedOut(cause) => ClientError::TimedOut(cause),
                TransportError::TooLarge => ClientError::TooLarge,
            })
    }

    /// Sends an authenticated request with the session's rules; see the
    /// [module docs](self).
    async fn authed(
        self: &Arc<Self>,
        method: Method,
        url: Url,
        body: Option<Vec<u8>>,
    ) -> Result<Incoming, ClientError> {
        let (mut access, mut retried) = match self.current_access() {
            Some(access) => (access, false),
            None => (self.refresh(None).await?, true),
        };
        loop {
            let answer = self
                .send(
                    method.clone(),
                    url.clone(),
                    Some(access.token.clone()),
                    body.clone(),
                )
                .await?;
            if !is_unauthorized(&answer) {
                return Ok(answer);
            }
            let current = self.current_access();
            let next = after_unauthorized(
                Some(access.generation),
                current.as_ref().map(|current| current.generation),
                retried,
            );
            access = match (next, current) {
                (Next::RetryWithCurrent, Some(current)) => {
                    debug!("retrying with the newer access token");
                    current
                }
                (Next::EndSession, _) => {
                    self.end(EndReason::Revoked).await;
                    return Err(ClientError::NoSession);
                }
                (Next::Refresh | Next::RetryWithCurrent, _) => {
                    self.refresh(Some(access.generation)).await?
                }
            };
            retried = true;
        }
    }

    /// Sends a request with the pending setup step's token. A 401 there
    /// means the token expired or was used: the step is dropped, and
    /// nothing refreshes.
    async fn with_setup(
        &self,
        path: &[&str],
        body: Option<Vec<u8>>,
    ) -> Result<Incoming, ClientError> {
        self.with_setup_serial(path, body)
            .await
            .map(|(answer, _)| answer)
    }

    async fn with_setup_serial(
        &self,
        path: &[&str],
        body: Option<Vec<u8>>,
    ) -> Result<(Incoming, u64), ClientError> {
        let (token, serial) = self.pending(StepKind::Setup)?;
        let answer = self
            .send(Method::POST, self.server.api(path), Some(token), body)
            .await?;
        if is_unauthorized(&answer) {
            self.drop_pending(serial);
        }
        Ok((answer, serial))
    }

    // ---- Refreshing and ending ----

    /// Refreshes, one at a time, after a 401 for the token of generation
    /// `used` (`None`: no access token). A refresh that finished meanwhile
    /// answers its token instead.
    async fn refresh(self: &Arc<Self>, used: Option<u64>) -> Result<Access, ClientError> {
        // Owned, so the refresh's task holds it until the refresh ends.
        let serial = Arc::clone(&self.refresh_lock).lock_owned().await;
        if let Some(current) = self.current_access()
            && Some(current.generation) != used
        {
            return Ok(current);
        }
        let (epoch, token, out) = {
            let _gate = self.gate.lock().await;
            let epoch = self.state().epoch;
            let Some(token) = self.load().await.map_err(ClientError::Store)? else {
                self.end_under_gate().await;
                info!(reason = EndReason::StoreEmpty.as_str(), "the session ended");
                return Err(ClientError::NoSession);
            };
            self.state().refresh_out = true;
            (epoch, token, RefreshOut(Arc::clone(self)))
        };
        // From here on the request may reach the server, which then replaces
        // the stored token: the rest runs in a task of its own (see the
        // module docs).
        let session = Arc::clone(self);
        outcome(tokio::spawn(async move {
            let _serial = serial;
            let _out = out;
            session.rotate(epoch, &token).await
        }))
        .await
    }

    /// A refresh's request, then its save and adoption under the epoch rule;
    /// its own task runs it.
    async fn rotate(&self, epoch: u64, token: &SecretString) -> Result<Access, ClientError> {
        let answer = self
            .send(
                Method::POST,
                self.server.api(&["auth", "refresh"]),
                None,
                Some(refresh_body(token)),
            )
            .await?;
        if is_unauthorized(&answer) {
            self.end(EndReason::RefreshRefused).await;
            return Err(ClientError::NoSession);
        }
        let tokens: TokenResponse = expect_json(answer, 200)?;
        let access = self.adopt(epoch, tokens).await?;
        debug!("the access token was refreshed");
        Ok(access)
    }

    /// A code step's request, then its save and adoption under the epoch
    /// rule; its own task runs it.
    async fn code_step(
        &self,
        epoch: u64,
        serial: u64,
        request: &LoginMfaRequest,
    ) -> Result<(), ClientError> {
        let answer = self
            .send(
                Method::POST,
                self.server.api(&["auth", "login", "mfa"]),
                None,
                Some(json(request)),
            )
            .await?;
        let tokens: TokenResponse = expect_json(answer, 200)?;
        self.adopt(epoch, tokens).await?;
        self.drop_pending(serial);
        info!("the session started");
        Ok(())
    }

    /// Saves and adopts a new pair, under the gate, if no end came since
    /// `epoch`; otherwise revokes the session it belongs to and answers
    /// [`ClientError::NoSession`].
    async fn adopt(&self, epoch: u64, tokens: TokenResponse) -> Result<Access, ClientError> {
        let access_token = tokens.access_token.into_secret();
        let refresh_token = tokens.refresh_token.into_secret();
        let gate = self.gate.lock().await;
        let current_epoch = self.state().epoch;
        if current_epoch != epoch {
            drop(gate);
            self.revoke(access_token).await;
            return Err(ClientError::NoSession);
        }
        if let Err(error) = self.save(&refresh_token).await {
            warn!(error = %chain(&error), "the new refresh token can't be saved, so the stored one is removed and the session ends at the next refresh");
            self.clear_or_log().await;
        }
        let mut state = self.state();
        state.generations = state.generations.wrapping_add(1);
        let access = Access {
            token: access_token,
            generation: state.generations,
        };
        state.access = Some(access.clone());
        Ok(access)
    }

    /// Ends the session and logs why.
    async fn end(&self, reason: EndReason) {
        let _gate = self.gate.lock().await;
        self.end_under_gate().await;
        info!(reason = reason.as_str(), "the session ended");
    }

    /// Ends the session; the caller holds the gate.
    async fn end_under_gate(&self) {
        self.drop_state();
        self.clear_or_log().await;
    }

    /// Bumps the epoch and drops the access token and the pending step; the
    /// caller holds the gate and clears the store next.
    fn drop_state(&self) {
        let mut state = self.state();
        state.epoch = state.epoch.wrapping_add(1);
        state.access = None;
        state.pending = None;
    }

    /// Revokes the session of `access_token` on the server, best effort.
    async fn revoke(&self, access_token: SecretString) {
        let url = self.server.api(&["auth", "logout"]);
        match self.send(Method::POST, url, Some(access_token), None).await {
            Ok(answer) if answer.status == 204 => {}
            Ok(answer) => debug!(
                status = answer.status,
                "the orphaned session wasn't revoked"
            ),
            Err(error) => debug!(error = %chain(&error), "the orphaned session wasn't revoked"),
        }
    }

    // ---- The store, each call bounded ----

    async fn load(&self) -> Result<Option<SecretString>, StoreError> {
        tokio::time::timeout(self.store_timeout, self.store.load())
            .await
            .unwrap_or_else(|_| Err(StoreError::timed_out(StoreOp::Load)))
    }

    async fn save(&self, token: &SecretString) -> Result<(), StoreError> {
        tokio::time::timeout(self.store_timeout, self.store.save(token))
            .await
            .unwrap_or_else(|_| Err(StoreError::timed_out(StoreOp::Save)))
    }

    async fn clear(&self) -> Result<(), StoreError> {
        tokio::time::timeout(self.store_timeout, self.store.clear())
            .await
            .unwrap_or_else(|_| Err(StoreError::timed_out(StoreOp::Clear)))
    }

    /// Clears the store; a failure is logged at `error`, since a replaced
    /// token may stay behind, whose use would end the session on the
    /// server.
    async fn clear_or_log(&self) {
        if let Err(error) = self.clear().await {
            error!(error = %chain(&error), "the session store can't be cleared, so a replaced refresh token may remain; remove it from the keychain");
        }
    }
}

// ---- Bodies and answers ----

/// The answer of a task the client spawned for work that must finish even
/// when its caller is dropped. A task that ended without one is
/// [`ClientError::Interrupted`].
async fn outcome<R>(task: JoinHandle<Result<R, ClientError>>) -> Result<R, ClientError> {
    task.await
        .unwrap_or_else(|error| Err(ClientError::Interrupted(error)))
}

/// `request` as JSON. These DTOs hold only strings, numbers and booleans,
/// which `serde_json` always writes; an empty body would be refused by the
/// server anyway.
fn json(request: &impl Serialize) -> Vec<u8> {
    serde_json::to_vec(request).unwrap_or_default()
}

/// The body of `POST /auth/refresh`.
fn refresh_body(token: &SecretString) -> Vec<u8> {
    json(&RefreshRequest {
        refresh_token: SecretText::new(token.expose_secret().to_owned()),
    })
}

/// `url` with the chat page's parameters, if it has any.
fn with_chat_page(mut url: Url, query: &ChatPageQuery) -> Url {
    if query.before.is_some() || query.limit.is_some() {
        let mut pairs = url.query_pairs_mut();
        if let Some(before) = query.before {
            pairs.append_pair("before", &before.to_string());
        }
        if let Some(limit) = query.limit {
            pairs.append_pair("limit", &limit.to_string());
        }
    }
    url
}

/// `url` with `query`'s parameters, if it has any.
fn with_page<I: ToString>(mut url: Url, query: &PageQuery<I>) -> Url {
    if query.before.is_some() || query.limit.is_some() {
        let mut pairs = url.query_pairs_mut();
        if let Some(before) = &query.before {
            pairs.append_pair("before", &before.to_string());
        }
        if let Some(limit) = query.limit {
            pairs.append_pair("limit", &limit.to_string());
        }
    }
    url
}

/// Whether `answer` is a 401 `unauthorized`: the access token (or the
/// refresh or setup token) isn't valid any more.
fn is_unauthorized(answer: &Incoming) -> bool {
    answer.status == 401
        && serde_json::from_slice::<ErrorResponse>(&answer.body)
            .is_ok_and(|envelope| envelope.error.code.known() == Some(ErrorCode::Unauthorized))
}

/// The route's answer for `status`, read as `R`.
fn expect_json<R: DeserializeOwned>(answer: Incoming, status: u16) -> Result<R, ClientError> {
    expect_json_of(answer, &[status])
}

/// The route's answer for any of `statuses`, read as `R`.
fn expect_json_of<R: DeserializeOwned>(
    answer: Incoming,
    statuses: &[u16],
) -> Result<R, ClientError> {
    let answer = with_status_of(answer, statuses)?;
    serde_json::from_slice(&answer.body).map_err(|error| ClientError::UnexpectedResponse {
        status: answer.status,
        reason: if error.is_syntax() || error.is_eof() {
            Unexpected::NotJson
        } else {
            Unexpected::Body
        },
    })
}

/// The route's answer for `status`, without a body to read.
fn expect_empty(answer: Incoming, status: u16) -> Result<(), ClientError> {
    with_status(answer, status).map(drop)
}

/// `answer` if its status is `status`; otherwise what it means.
fn with_status(answer: Incoming, status: u16) -> Result<Incoming, ClientError> {
    with_status_of(answer, &[status])
}

/// `answer` if its status is one of `statuses`; otherwise what it means.
fn with_status_of(answer: Incoming, statuses: &[u16]) -> Result<Incoming, ClientError> {
    if statuses.contains(&answer.status) {
        Ok(answer)
    } else if (200..300).contains(&answer.status) {
        Err(ClientError::UnexpectedResponse {
            status: answer.status,
            reason: Unexpected::Status,
        })
    } else {
        Err(failure(&answer))
    }
}

/// What a status other than the route's success means: the server's
/// refusal in its envelope, a gateway whose server is down, or something
/// this client doesn't understand.
pub(crate) fn failure(answer: &Incoming) -> ClientError {
    if (300..400).contains(&answer.status) {
        return ClientError::UnexpectedResponse {
            status: answer.status,
            reason: Unexpected::Redirect,
        };
    }
    match serde_json::from_slice::<ErrorResponse>(&answer.body) {
        Ok(envelope) => ClientError::Api(ApiFailure {
            status: answer.status,
            code: envelope.error.code,
            message: envelope.error.message,
            request_id: envelope.error.request_id,
            fields: envelope.error.fields.unwrap_or_default(),
            retry_after: answer.retry_after,
        }),
        Err(_) if matches!(answer.status, 502..=504) => ClientError::Unreachable(None),
        Err(_) => ClientError::UnexpectedResponse {
            status: answer.status,
            reason: Unexpected::NoEnvelope,
        },
    }
}

/// `error` with its sources, joined by ": ".
fn chain(error: &dyn Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let _ = write!(text, ": {cause}");
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use fleet_api_types::ErrorCode;
    use rstest::rstest;
    use serde_json::{Value, json};
    use tokio::sync::Notify;
    use tokio::time::timeout;

    use super::*;
    use crate::store::{MemoryStore, StoreError, StoreOp};
    use crate::transport::{Outgoing, TransportError};

    /// Bounds every wait of these tests, so a hang fails with a clear
    /// message instead of running until CI's job timeout.
    const WAIT: Duration = Duration::from_secs(5);

    const ME: &str = "GET /api/v1/me";
    const REFRESH: &str = "POST /api/v1/auth/refresh";
    const LOGOUT: &str = "POST /api/v1/auth/logout";
    const LOGIN: &str = "POST /api/v1/auth/login";
    const LOGIN_MFA: &str = "POST /api/v1/auth/login/mfa";
    const STEP_UP: &str = "POST /api/v1/auth/step-up";
    const ENROLL: &str = "POST /api/v1/me/mfa/enroll";
    const CONFIRM: &str = "POST /api/v1/me/mfa/confirm";
    const RESET: &str = "POST /api/v1/me/mfa/reset";

    fn answer(status: u16, body: &Value) -> Incoming {
        Incoming {
            status,
            retry_after: None,
            body: serde_json::to_vec(body).unwrap(),
        }
    }

    fn raw(status: u16, body: &[u8]) -> Incoming {
        Incoming {
            status,
            retry_after: None,
            body: body.to_vec(),
        }
    }

    /// The server's error envelope with `code`.
    fn refusal(status: u16, code: &str) -> Incoming {
        answer(
            status,
            &json!({ "error": { "code": code, "message": "Refused.", "request_id": "r-1" } }),
        )
    }

    /// A session's tokens, numbered `n`.
    fn tokens(n: u32) -> Incoming {
        answer(
            200,
            &json!({
                "access_token": format!("afk_at_{n}"),
                "refresh_token": format!("afk_rt_{n}"),
                "access_expires_in": 900,
                "refresh_expires_in": 604_800,
            }),
        )
    }

    fn profile() -> Incoming {
        answer(
            200,
            &json!({
                "id": "01900000-0000-7000-8000-000000000001",
                "username": "alice",
                "role": "member",
                "created_at": "2026-10-12T10:00:00.000Z",
                "password_changed_at": "2026-10-12T10:00:00.000Z",
                "recovery_codes_left": 10,
                "failed_code_attempts": { "count": 0 },
            }),
        )
    }

    fn profile_with_status(status: u16) -> Incoming {
        Incoming {
            status,
            ..profile()
        }
    }

    /// An answer the test holds until it releases it.
    #[derive(Clone, Default)]
    struct Gate {
        arrived: Arc<Notify>,
        release: Arc<Notify>,
    }

    struct Reply {
        gate: Option<Gate>,
        result: Result<Incoming, TransportError>,
    }

    /// What the transport was sent.
    #[derive(Debug, Clone, PartialEq)]
    struct Seen {
        route: String,
        bearer: Option<String>,
        body: Option<Value>,
    }

    /// A transport that answers each route from its script, in order, and
    /// records what it was sent.
    #[derive(Default)]
    struct Scripted {
        replies: Mutex<HashMap<String, VecDeque<Reply>>>,
        seen: Mutex<Vec<Seen>>,
    }

    impl Scripted {
        fn push(&self, route: &str, reply: Reply) {
            self.replies
                .lock()
                .unwrap()
                .entry(route.to_owned())
                .or_default()
                .push_back(reply);
        }

        fn on(&self, route: &str, result: Result<Incoming, TransportError>) {
            self.push(route, Reply { gate: None, result });
        }

        fn held(&self, route: &str, incoming: Incoming) -> Gate {
            let gate = Gate::default();
            self.push(
                route,
                Reply {
                    gate: Some(gate.clone()),
                    result: Ok(incoming),
                },
            );
            gate
        }

        fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }

        fn routes(&self) -> Vec<String> {
            self.seen().into_iter().map(|seen| seen.route).collect()
        }

        fn bearers(&self, route: &str) -> Vec<Option<String>> {
            self.seen()
                .into_iter()
                .filter(|seen| seen.route == route)
                .map(|seen| seen.bearer)
                .collect()
        }
    }

    impl Transport for Scripted {
        async fn send(&self, request: Outgoing) -> Result<Incoming, TransportError> {
            let route = format!("{} {}", request.method, request.url.path());
            self.seen.lock().unwrap().push(Seen {
                route: route.clone(),
                bearer: request
                    .bearer
                    .as_ref()
                    .map(|token| token.expose_secret().to_owned()),
                body: request
                    .body
                    .as_ref()
                    .map(|body| serde_json::from_slice(body).unwrap()),
            });
            let reply = self
                .replies
                .lock()
                .unwrap()
                .get_mut(&route)
                .and_then(VecDeque::pop_front);
            let reply = reply.unwrap_or_else(|| panic!("no answer scripted for {route}"));
            if let Some(gate) = reply.gate {
                gate.arrived.notify_one();
                gate.release.notified().await;
            }
            reply.result
        }
    }

    #[derive(Default)]
    struct Flags {
        fail_clear: AtomicBool,
        fail_save: AtomicBool,
        hang_load: AtomicBool,
        saves: AtomicUsize,
        cleared: Notify,
    }

    /// A [`MemoryStore`] whose saves can fail and whose loads can hang, and
    /// which counts its saves and signals each clear. Clones share all.
    #[derive(Clone, Default)]
    struct TestStore {
        inner: MemoryStore,
        flags: Arc<Flags>,
    }

    impl TestStore {
        async fn holding(token: &str) -> Self {
            let store = Self::default();
            store.inner.save(&SecretString::from(token)).await.unwrap();
            store
        }

        async fn token(&self) -> Option<String> {
            self.inner
                .load()
                .await
                .unwrap()
                .map(|token| token.expose_secret().to_owned())
        }

        fn saves(&self) -> usize {
            self.flags.saves.load(Ordering::SeqCst)
        }
    }

    impl SessionStore for TestStore {
        async fn load(&self) -> Result<Option<SecretString>, StoreError> {
            if self.flags.hang_load.load(Ordering::SeqCst) {
                core::future::pending::<()>().await;
            }
            self.inner.load().await
        }

        async fn save(&self, refresh_token: &SecretString) -> Result<(), StoreError> {
            self.flags.saves.fetch_add(1, Ordering::SeqCst);
            if self.flags.fail_save.load(Ordering::SeqCst) {
                return Err(StoreError::new(StoreOp::Save, "the keychain is locked"));
            }
            self.inner.save(refresh_token).await
        }

        async fn clear(&self) -> Result<(), StoreError> {
            let cleared = if self.flags.fail_clear.load(Ordering::SeqCst) {
                Err(StoreError::new(StoreOp::Clear, "the keychain is locked"))
            } else {
                self.inner.clear().await
            };
            self.flags.cleared.notify_one();
            cleared
        }
    }

    type TestCore = Core<TestStore, Scripted>;

    fn core(store: &TestStore) -> Arc<TestCore> {
        Arc::new(Core::new(
            ServerUrl::parse("https://example.com").unwrap(),
            Scripted::default(),
            store.clone(),
            Duration::from_millis(100),
        ))
    }

    /// Gives `core` the access token `token` as generation `generation`, as
    /// a login or a refresh would have.
    fn give_access(core: &TestCore, token: &str, generation: u64) {
        let mut state = core.state();
        state.access = Some(Access {
            token: SecretString::from(token),
            generation,
        });
        state.generations = generation;
    }

    fn access(core: &TestCore) -> Option<String> {
        core.state()
            .access
            .as_ref()
            .map(|access| access.token.expose_secret().to_owned())
    }

    fn give_step(core: &TestCore, kind: StepKind, token: &str) {
        let mut state = core.state();
        state.pending_serial += 1;
        state.pending = Some(Pending {
            kind,
            token: SecretString::from(token),
            serial: state.pending_serial,
        });
    }

    fn has_step(core: &TestCore) -> bool {
        core.state().pending.is_some()
    }

    fn code(text: &str) -> SecretText<CODE_MAX_CHARS> {
        SecretText::new(text.to_owned())
    }

    fn login_request() -> LoginRequest {
        serde_json::from_value(json!({ "username": "alice", "password": "x" })).unwrap()
    }

    fn is_api(error: &ClientError, status: u16, code: ErrorCode) -> bool {
        error
            .api()
            .is_some_and(|failure| failure.status == status && failure.is(code))
    }

    fn count(routes: &[String], route: &str) -> usize {
        routes.iter().filter(|seen| *seen == route).count()
    }

    /// Lets the runtime's other tasks run until `done` holds, at most 1000
    /// turns, so a test whose task never finishes fails on its assertions
    /// instead of hanging.
    async fn settle(done: impl Fn() -> bool) {
        for _ in 0..1000 {
            if done() {
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    #[rstest]
    #[case::same_token(Some(1), Some(1), false, Next::Refresh)]
    #[case::replaced(Some(1), Some(2), false, Next::RetryWithCurrent)]
    #[case::ended_meanwhile(Some(1), None, false, Next::Refresh)]
    #[case::refreshed_first_by_another(None, Some(2), false, Next::RetryWithCurrent)]
    #[case::none_at_all(None, None, false, Next::Refresh)]
    #[case::retried_with_a_newer(Some(1), Some(2), true, Next::EndSession)]
    #[case::retried_with_the_same(Some(1), Some(1), true, Next::EndSession)]
    fn after_a_401_the_rule_decides(
        #[case] used: Option<u64>,
        #[case] current: Option<u64>,
        #[case] retried: bool,
        #[case] next: Next,
    ) {
        assert_eq!(after_unauthorized(used, current, retried), next);
    }

    #[tokio::test]
    async fn two_401s_at_once_give_one_refresh_and_both_retry_with_its_token() {
        let store = TestStore::holding("afk_rt_0").await;
        let core = core(&store);
        give_access(&core, "afk_at_0", 1);
        let first = core.transport.held(ME, refusal(401, "unauthorized"));
        let second = core.transport.held(ME, refusal(401, "unauthorized"));
        core.transport.on(REFRESH, Ok(tokens(1)));
        core.transport.on(ME, Ok(profile()));
        core.transport.on(ME, Ok(profile()));

        let (a, b, ()) = timeout(WAIT, async {
            tokio::join!(core.me(), core.me(), async {
                first.arrived.notified().await;
                second.arrived.notified().await;
                first.release.notify_one();
                second.release.notify_one();
            })
        })
        .await
        .unwrap();

        assert!(a.is_ok() && b.is_ok(), "{a:?} {b:?}");
        assert_eq!(count(&core.transport.routes(), REFRESH), 1);
        let at = |n: u32| Some(format!("afk_at_{n}"));
        assert_eq!(core.transport.bearers(ME), [at(0), at(0), at(1), at(1)]);
        assert_eq!(store.token().await.as_deref(), Some("afk_rt_1"));
        assert_eq!(store.saves(), 1);
    }

    #[tokio::test]
    async fn a_401_for_a_token_a_refresh_replaced_retries_with_the_current_one() {
        let store = TestStore::holding("afk_rt_1").await;
        let core = core(&store);
        give_access(&core, "afk_at_0", 1);
        let held = core.transport.held(ME, refusal(401, "unauthorized"));
        core.transport.on(ME, Ok(profile()));

        let (result, ()) = timeout(WAIT, async {
            tokio::join!(core.me(), async {
                held.arrived.notified().await;
                give_access(&core, "afk_at_1", 2);
                held.release.notify_one();
            })
        })
        .await
        .unwrap();

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(core.transport.routes(), [ME, ME]);
        assert_eq!(
            core.transport.bearers(ME),
            [Some("afk_at_0".to_owned()), Some("afk_at_1".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_request_without_an_access_token_refreshes_first() {
        let store = TestStore::holding("afk_rt_0").await;
        let core = core(&store);
        core.transport.on(REFRESH, Ok(tokens(1)));
        core.transport.on(ME, Ok(profile()));

        let result = core.me().await;

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(core.transport.routes(), [REFRESH, ME]);
        assert_eq!(
            core.transport.seen()[0].body,
            Some(json!({ "refresh_token": "afk_rt_0" }))
        );
        assert_eq!(access(&core).as_deref(), Some("afk_at_1"));
        assert_eq!(store.token().await.as_deref(), Some("afk_rt_1"));
    }

    #[tokio::test]
    async fn no_access_token_and_an_empty_store_send_nothing() {
        let store = TestStore::default();
        let core = core(&store);

        let result = core.me().await;

        assert!(matches!(result, Err(ClientError::NoSession)), "{result:?}");
        assert_eq!(core.transport.routes(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_401_after_the_retry_ends_the_session() {
        let store = TestStore::holding("afk_rt_0").await;
        let core = core(&store);
        give_access(&core, "afk_at_0", 1);
        core.transport.on(ME, Ok(refusal(401, "unauthorized")));
        core.transport.on(REFRESH, Ok(tokens(1)));
        core.transport.on(ME, Ok(refusal(401, "unauthorized")));

        let result = core.me().await;

        assert!(matches!(result, Err(ClientError::NoSession)), "{result:?}");
        assert_eq!(core.transport.routes(), [ME, REFRESH, ME]);
        assert_eq!(store.token().await, None);
        assert_eq!(access(&core), None);
    }

    #[tokio::test]
    async fn a_401_right_after_refreshing_first_ends_the_session() {
        let store = TestStore::holding("afk_rt_0").await;
        let core = core(&store);
        core.transport.on(REFRESH, Ok(tokens(1)));
        core.transport.on(ME, Ok(refusal(401, "unauthorized")));

        let result = core.me().await;

        assert!(matches!(result, Err(ClientError::NoSession)), "{result:?}");
        assert_eq!(core.transport.routes(), [REFRESH, ME]);
        assert_eq!(store.token().await, None);
    }

    #[tokio::test]
    async fn a_refused_refresh_ends_the_session() {
        let store = TestStore::holding("afk_rt_0").await;
        let core = core(&store);
        give_access(&core, "afk_at_0", 1);
        core.transport.on(ME, Ok(refusal(401, "unauthorized")));
        core.transport.on(REFRESH, Ok(refusal(401, "unauthorized")));

        let result = core.me().await;

        assert!(matches!(result, Err(ClientError::NoSession)), "{result:?}");
        assert_eq!(store.token().await, None);
        assert_eq!(access(&core), None);
    }

    #[rstest]
    #[case::unreachable(Err(TransportError::Unreachable(None)))]
    #[case::timed_out(Err(TransportError::TimedOut(None)))]
    #[case::rate_limited(Ok(refusal(429, "rate_limited")))]
    #[case::busy(Ok(refusal(503, "busy")))]
    #[tokio::test]
    async fn a_transient_failure_of_the_refresh_keeps_the_session(
        #[case] failure: Result<Incoming, TransportError>,
    ) {
        let store = TestStore::holding("afk_rt_0").await;
        let core = core(&store);
        give_access(&core, "afk_at_0", 1);
        core.transport.on(ME, Ok(refusal(401, "unauthorized")));
        core.transport.on(REFRESH, failure);

        let result = core.me().await;

        assert!(
            !matches!(result, Ok(_) | Err(ClientError::NoSession)),
            "{result:?}"
        );
        assert_eq!(store.token().await.as_deref(), Some("afk_rt_0"));
        assert_eq!(access(&core).as_deref(), Some("afk_at_0"));
    }

    #[tokio::test]
    async fn login_is_refused_while_a_session_is_held() {
        let stored = TestStore::holding("afk_rt_0").await;
        let in_memory = TestStore::default();
        let with_stored = core(&stored);
        let with_access = core(&in_memory);
        give_access(&with_access, "afk_at_0", 1);

        let first = with_stored.login(&login_request()).await;
        let second = with_access.login(&login_request()).await;

        assert!(
            matches!(first, Err(ClientError::AlreadyLoggedIn)),
            "{first:?}"
        );
        assert!(
            matches!(second, Err(ClientError::AlreadyLoggedIn)),
            "{second:?}"
        );
        assert_eq!(with_stored.transport.routes(), Vec::<String>::new());
        assert_eq!(with_access.transport.routes(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_login_keeps_its_step_token_and_the_code_step_starts_the_session() {
        let store = TestStore::default();
        let core = core(&store);
        core.transport.on(
            LOGIN,
            Ok(answer(
                200,
                &json!({ "status": "mfa_required", "token": "afk_mt_1", "expires_in": 300 }),
            )),
        );
        core.transport.on(LOGIN_MFA, Ok(tokens(1)));

        let step = core.login(&login_request()).await.unwrap();
        core.login_mfa(code("123456")).await.unwrap();

        assert_eq!(
            step,
            LoginStep::MfaRequired {
                expires_in: Duration::from_secs(300)
            }
        );
        assert_eq!(
            core.transport.seen()[1].body,
            Some(json!({ "mfa_token": "afk_mt_1", "code": "123456" }))
        );
        assert_eq!(access(&core).as_deref(), Some("afk_at_1"));
        assert_eq!(store.token().await.as_deref(), Some("afk_rt_1"));
        assert!(!has_step(&core));
    }

    #[tokio::test]
    async fn a_setup_step_is_kept_for_enroll_and_confirm() {
        let store = TestStore::default();
        let core = core(&store);
        core.transport.on(
            LOGIN,
            Ok(answer(
                200,
                &json!({ "status": "mfa_setup_required", "token": "afk_st_1", "expires_in": 600 }),
            )),
        );
        core.transport.on(
            ENROLL,
            Ok(answer(
                200,
                &json!({ "otpauth_uri": "otpauth://totp/x", "qr_svg": "data:x", "secret": "ABC" }),
            )),
        );

        let step = core.login(&login_request()).await.unwrap();
        core.mfa_enroll().await.unwrap();

        assert_eq!(
            step,
            LoginStep::SetupRequired {
                expires_in: Duration::from_secs(600)
            }
        );
        assert_eq!(
            core.transport.bearers(ENROLL),
            [Some("afk_st_1".to_owned())]
        );
        assert!(has_step(&core));
    }

    #[tokio::test]
    async fn a_login_status_this_version_doesnt_know_is_unexpected() {
        let store = TestStore::default();
        let core = core(&store);
        core.transport.on(
            LOGIN,
            Ok(answer(
                200,
                &json!({ "status": "passkey_required", "token": "afk_mt_1", "expires_in": 300 }),
            )),
        );

        let result = core.login(&login_request()).await;

        assert!(
            matches!(
                result,
                Err(ClientError::UnexpectedResponse {
                    status: 200,
                    reason: Unexpected::UnknownLoginStatus
                })
            ),
            "{result:?}"
        );
        assert!(!has_step(&core));
    }

    #[tokio::test]
    async fn a_failed_save_clears_the_store_and_the_call_goes_on() {
        let store = TestStore::holding("afk_rt_0").await;
        store.flags.fail_save.store(true, Ordering::SeqCst);
        let core = core(&store);
        give_access(&core, "afk_at_0", 1);
        core.transport.on(ME, Ok(refusal(401, "unauthorized")));
        core.transport.on(REFRESH, Ok(tokens(1)));
        core.transport.on(ME, Ok(profile()));

        let result = core.me().await;

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(store.saves(), 1);
        assert_eq!(store.token().await, None);
        assert_eq!(access(&core).as_deref(), Some("afk_at_1"));
    }

    #[tokio::test]
    async fn logout_revokes_the_session_with_its_access_token() {
        let store = TestStore::holding("afk_rt_0").await;
        let core = core(&store);
        give_access(&core, "afk_at_0", 1);
        core.transport.on(LOGOUT, Ok(raw(204, b"")));

        core.logout().await.unwrap();

        assert_eq!(
            core.transport.bearers(LOGOUT),
            [Some("afk_at_0".to_owned())]
        );
        assert_eq!(store.token().await, None);
        assert_eq!(access(&core), None);
    }

    #[tokio::test]
    async fn logout_while_unreachable_still_ends_the_session() {
        let store = TestStore::holding("afk_rt_0").await;
        let core = core(&store);
        give_access(&core, "afk_at_0", 1);
        give_step(&core, StepKind::Setup, "afk_st_1");
        core.transport
            .on(LOGOUT, Err(TransportError::Unreachable(None)));

        let result = core.logout().await;

        assert!(
            matches!(result, Err(ClientError::Unreachable(_))),
            "{result:?}"
        );
        assert_eq!(store.token().await, None);
        assert_eq!(access(&core), None);
        assert!(!has_step(&core));
    }

    #[tokio::test]
    async fn logout_falls_back_to_the_refresh_token_when_the_access_token_is_refused() {
        let store = TestStore::holding("afk_rt_0").await;
        let core = core(&store);
        give_access(&core, "afk_at_0", 1);
        core.transport.on(LOGOUT, Ok(refusal(401, "unauthorized")));
        core.transport.on(REFRESH, Ok(tokens(1)));
        core.transport.on(LOGOUT, Ok(raw(204, b"")));

        core.logout().await.unwrap();

        assert_eq!(core.transport.routes(), [LOGOUT, REFRESH, LOGOUT]);
        assert_eq!(
            core.transport.bearers(LOGOUT),
            [Some("afk_at_0".to_owned()), Some("afk_at_1".to_owned())]
        );
        assert_eq!(store.token().await, None);
        assert_eq!(store.saves(), 0);
        assert_eq!(access(&core), None);
    }

    #[tokio::test]
    async fn logout_after_a_start_refreshes_once_and_revokes_with_the_answer() {
        let store = TestStore::holding("afk_rt_0").await;
        let core = core(&store);
        core.transport.on(REFRESH, Ok(tokens(1)));
        core.transport.on(LOGOUT, Ok(raw(204, b"")));

        core.logout().await.unwrap();

        assert_eq!(core.transport.routes(), [REFRESH, LOGOUT]);
        assert_eq!(
            core.transport.bearers(LOGOUT),
            [Some("afk_at_1".to_owned())]
        );
        assert_eq!(store.token().await, None);
        assert_eq!(store.saves(), 0);
    }

    #[tokio::test]
    async fn logout_without_a_session_sends_nothing() {
        let store = TestStore::default();
        let core = core(&store);

        core.logout().await.unwrap();

        assert_eq!(core.transport.routes(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn logout_during_a_refresh_wins_and_the_refresh_saves_nothing() {
        let store = TestStore::holding("afk_rt_0").await;
        let core = core(&store);
        let held = core.transport.held(REFRESH, tokens(1));
        core.transport.on(LOGOUT, Ok(raw(204, b"")));

        let (me, logout, ()) = timeout(WAIT, async {
            tokio::join!(
                core.me(),
                async {
                    held.arrived.notified().await;
                    core.logout().await
                },
                async {
                    store.flags.cleared.notified().await;
                    held.release.notify_one();
                }
            )
        })
        .await
        .unwrap();

        assert!(matches!(me, Err(ClientError::NoSession)), "{me:?}");
        assert!(logout.is_ok(), "{logout:?}");
        assert_eq!(store.token().await, None);
        assert_eq!(store.saves(), 0);
        assert_eq!(access(&core), None);
        assert_eq!(core.transport.routes(), [REFRESH, LOGOUT]);
        assert_eq!(
            core.transport.bearers(LOGOUT),
            [Some("afk_at_1".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_discard_clears_the_store_and_everything_held_and_sends_nothing() {
        let store = TestStore::holding("afk_rt_0").await;
        let core = core(&store);
        give_access(&core, "afk_at_0", 1);
        give_step(&core, StepKind::Setup, "afk_st_1");

        core.discard().await.unwrap();

        assert_eq!(store.token().await, None);
        assert_eq!(access(&core), None);
        assert!(!has_step(&core));
        assert_eq!(core.transport.routes(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_discard_whose_clear_fails_answers_store_but_still_drops_the_tokens() {
        let store = TestStore::holding("afk_rt_0").await;
        store.flags.fail_clear.store(true, Ordering::SeqCst);
        let core = core(&store);
        give_access(&core, "afk_at_0", 1);

        let result = core.discard().await;

        assert!(
            matches!(&result, Err(ClientError::Store(error)) if error.op() == StoreOp::Clear),
            "{result:?}"
        );
        assert_eq!(access(&core), None);
        assert_eq!(core.transport.routes(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn after_a_discard_a_request_answers_no_session_and_sends_nothing() {
        let store = TestStore::holding("afk_rt_0").await;
        let core = core(&store);

        core.discard().await.unwrap();
        let me = core.me().await;

        assert!(matches!(me, Err(ClientError::NoSession)), "{me:?}");
        assert_eq!(core.transport.routes(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_discard_during_a_refresh_wins_and_the_refresh_saves_nothing() {
        let store = TestStore::holding("afk_rt_0").await;
        let core = core(&store);
        let held = core.transport.held(REFRESH, tokens(1));
        core.transport.on(LOGOUT, Ok(raw(204, b"")));

        let (me, discard, ()) = timeout(WAIT, async {
            tokio::join!(
                core.me(),
                async {
                    held.arrived.notified().await;
                    core.discard().await
                },
                async {
                    store.flags.cleared.notified().await;
                    held.release.notify_one();
                }
            )
        })
        .await
        .unwrap();

        assert!(matches!(me, Err(ClientError::NoSession)), "{me:?}");
        assert!(discard.is_ok(), "{discard:?}");
        assert_eq!(store.token().await, None);
        assert_eq!(store.saves(), 0);
        assert_eq!(access(&core), None);
        // The session the refresh's answer belongs to is revoked on the
        // server, with its own access token (the epoch rule).
        assert_eq!(core.transport.routes(), [REFRESH, LOGOUT]);
        assert_eq!(
            core.transport.bearers(LOGOUT),
            [Some("afk_at_1".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_code_step_whose_session_ended_before_its_save_saves_nothing() {
        let store = TestStore::default();
        let core = core(&store);
        give_step(&core, StepKind::Mfa, "afk_mt_1");
        let held = core.transport.held(LOGIN_MFA, tokens(1));
        core.transport.on(LOGOUT, Ok(raw(204, b"")));

        let (login, logout, ()) = timeout(WAIT, async {
            tokio::join!(
                core.login_mfa(code("123456")),
                async {
                    held.arrived.notified().await;
                    core.logout().await
                },
                async {
                    store.flags.cleared.notified().await;
                    held.release.notify_one();
                }
            )
        })
        .await
        .unwrap();

        assert!(matches!(login, Err(ClientError::NoSession)), "{login:?}");
        assert!(logout.is_ok(), "{logout:?}");
        assert_eq!(store.token().await, None);
        assert_eq!(store.saves(), 0);
        assert_eq!(access(&core), None);
        assert_eq!(
            core.transport.bearers(LOGOUT),
            [Some("afk_at_1".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_401_on_a_setup_route_drops_the_step_and_never_refreshes() {
        let store = TestStore::holding("afk_rt_0").await;
        let core = core(&store);
        give_step(&core, StepKind::Setup, "afk_st_1");
        core.transport.on(ENROLL, Ok(refusal(401, "unauthorized")));

        let first = core.mfa_enroll().await.unwrap_err();
        let second = core.mfa_enroll().await.unwrap_err();

        assert!(is_api(&first, 401, ErrorCode::Unauthorized), "{first:?}");
        assert!(matches!(second, ClientError::NoPendingStep), "{second:?}");
        assert_eq!(core.transport.routes(), [ENROLL]);
        assert_eq!(store.token().await.as_deref(), Some("afk_rt_0"));
    }

    #[tokio::test]
    async fn a_wrong_code_keeps_the_step_for_another_try_and_never_refreshes() {
        let store = TestStore::default();
        let core = core(&store);
        give_step(&core, StepKind::Mfa, "afk_mt_1");
        core.transport
            .on(LOGIN_MFA, Ok(refusal(401, "invalid_credentials")));
        core.transport
            .on(LOGIN_MFA, Ok(refusal(401, "invalid_credentials")));

        let first = core.login_mfa(code("111111")).await.unwrap_err();
        let second = core.login_mfa(code("222222")).await.unwrap_err();

        assert!(
            is_api(&first, 401, ErrorCode::InvalidCredentials),
            "{first:?}"
        );
        assert!(
            is_api(&second, 401, ErrorCode::InvalidCredentials),
            "{second:?}"
        );
        assert_eq!(core.transport.routes(), [LOGIN_MFA, LOGIN_MFA]);
        assert!(has_step(&core));
    }

    #[tokio::test]
    async fn a_confirm_with_a_wrong_code_keeps_the_step() {
        let store = TestStore::default();
        let core = core(&store);
        give_step(&core, StepKind::Setup, "afk_st_1");
        core.transport
            .on(CONFIRM, Ok(refusal(422, "validation_failed")));

        let refused = core.mfa_confirm(code("000000")).await.unwrap_err();

        assert!(
            is_api(&refused, 422, ErrorCode::ValidationFailed),
            "{refused:?}"
        );
        assert!(has_step(&core));
    }

    #[tokio::test]
    async fn a_step_needs_its_own_kind() {
        let store = TestStore::default();
        let with_mfa = core(&store);
        give_step(&with_mfa, StepKind::Mfa, "afk_mt_1");
        let with_setup = core(&store);
        give_step(&with_setup, StepKind::Setup, "afk_st_1");
        let without = core(&store);

        let enroll = with_mfa.mfa_enroll().await.unwrap_err();
        let confirm = with_mfa.mfa_confirm(code("123456")).await.unwrap_err();
        let login = with_setup.login_mfa(code("123456")).await.unwrap_err();
        let none = without.login_mfa(code("123456")).await.unwrap_err();

        for error in [enroll, confirm, login, none] {
            assert!(matches!(error, ClientError::NoPendingStep), "{error:?}");
        }
        assert_eq!(with_mfa.transport.routes(), Vec::<String>::new());
        assert_eq!(with_setup.transport.routes(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_403_at_step_up_never_refreshes_or_ends_the_session() {
        let store = TestStore::holding("afk_rt_0").await;
        let core = core(&store);
        give_access(&core, "afk_at_0", 1);
        core.transport.on(STEP_UP, Ok(refusal(403, "forbidden")));
        let request: StepUpRequest =
            serde_json::from_value(json!({ "password": "wrong" })).unwrap();

        let refused = core.step_up(&request).await.unwrap_err();

        assert!(is_api(&refused, 403, ErrorCode::Forbidden), "{refused:?}");
        assert_eq!(core.transport.routes(), [STEP_UP]);
        assert_eq!(store.token().await.as_deref(), Some("afk_rt_0"));
        assert_eq!(access(&core).as_deref(), Some("afk_at_0"));
    }

    #[tokio::test]
    async fn invalid_credentials_at_login_never_refreshes() {
        let store = TestStore::default();
        let core = core(&store);
        core.transport
            .on(LOGIN, Ok(refusal(401, "invalid_credentials")));

        let refused = core.login(&login_request()).await.unwrap_err();

        assert!(
            is_api(&refused, 401, ErrorCode::InvalidCredentials),
            "{refused:?}"
        );
        assert_eq!(core.transport.routes(), [LOGIN]);
        assert!(!has_step(&core));
    }

    #[tokio::test]
    async fn a_reset_makes_its_setup_token_the_step_and_confirm_ends_it() {
        let store = TestStore::holding("afk_rt_0").await;
        let core = core(&store);
        give_access(&core, "afk_at_0", 1);
        core.transport.on(
            RESET,
            Ok(answer(
                200,
                &json!({ "setup_token": "afk_st_2", "expires_in": 600 }),
            )),
        );
        core.transport.on(
            ENROLL,
            Ok(answer(
                200,
                &json!({ "otpauth_uri": "otpauth://totp/x", "qr_svg": "data:x", "secret": "ABC" }),
            )),
        );
        core.transport.on(
            CONFIRM,
            Ok(answer(
                200,
                &json!({ "recovery_codes": ["abcd-efgh-ijkl-mnop-qrst"] }),
            )),
        );

        let expires_in = core.mfa_reset().await.unwrap();
        core.mfa_enroll().await.unwrap();
        let codes = core.mfa_confirm(code("123456")).await.unwrap();
        let after = core.mfa_enroll().await.unwrap_err();

        assert_eq!(expires_in, Duration::from_secs(600));
        assert_eq!(core.transport.bearers(RESET), [Some("afk_at_0".to_owned())]);
        assert_eq!(
            core.transport.bearers(ENROLL),
            [Some("afk_st_2".to_owned())]
        );
        assert_eq!(
            core.transport.bearers(CONFIRM),
            [Some("afk_st_2".to_owned())]
        );
        assert_eq!(codes.recovery_codes.len(), 1);
        assert!(matches!(after, ClientError::NoPendingStep), "{after:?}");
        assert_eq!(access(&core).as_deref(), Some("afk_at_0"));
    }

    #[tokio::test]
    async fn a_store_that_hangs_times_out() {
        let store = TestStore::holding("afk_rt_0").await;
        store.flags.hang_load.store(true, Ordering::SeqCst);
        let core = core(&store);

        let result = timeout(WAIT, core.me()).await.unwrap();

        assert!(
            matches!(&result, Err(ClientError::Store(error)) if error.op() == StoreOp::Load),
            "{result:?}"
        );
        assert_eq!(core.transport.routes(), Vec::<String>::new());
    }

    #[test]
    fn an_envelope_is_read_with_its_fields_and_wait() {
        let mut incoming = answer(
            429,
            &json!({ "error": {
                "code": "rate_limited",
                "message": "Too many requests.",
                "request_id": "r-7",
                "fields": [{ "path": "code", "message": "is wrong" }],
            } }),
        );
        incoming.retry_after = Some(Duration::from_secs(7));

        let ClientError::Api(failure) = failure(&incoming) else {
            panic!("not an API failure");
        };

        assert_eq!(failure.status, 429);
        assert!(failure.is(ErrorCode::RateLimited));
        assert_eq!(failure.message.as_str(), "Too many requests.");
        assert_eq!(failure.request_id.as_str(), "r-7");
        assert_eq!(failure.fields.len(), 1);
        assert_eq!(failure.retry_after, Some(Duration::from_secs(7)));
    }

    #[test]
    fn a_code_this_version_doesnt_know_still_reads() {
        let ClientError::Api(failure) = failure(&refusal(418, "teapot_mode")) else {
            panic!("not an API failure");
        };

        assert_eq!(failure.code.known(), None);
        assert_eq!(failure.code.as_str(), "teapot_mode");
    }

    #[rstest]
    #[case::bad_gateway(502)]
    #[case::unavailable(503)]
    #[case::gateway_timeout(504)]
    fn a_gateway_error_without_the_envelope_is_unreachable(#[case] status: u16) {
        let error = failure(&raw(status, b"<html>Bad Gateway</html>"));

        assert!(matches!(error, ClientError::Unreachable(None)), "{error:?}");
    }

    #[rstest]
    #[case::redirect(302, b"" as &[u8], Unexpected::Redirect)]
    #[case::moved(
        301,
        br#"{"error":{"code":"not_found","message":"x","request_id":"r"}}"#,
        Unexpected::Redirect
    )]
    #[case::html(400, b"<html>Bad Request</html>", Unexpected::NoEnvelope)]
    #[case::other_json(500, br#"{"detail":"oops"}"#, Unexpected::NoEnvelope)]
    fn anything_else_without_the_envelope_is_unexpected(
        #[case] status: u16,
        #[case] body: &[u8],
        #[case] reason: Unexpected,
    ) {
        let error = failure(&raw(status, body));

        assert!(
            matches!(
                error,
                ClientError::UnexpectedResponse { status: s, reason: r } if s == status && r == reason
            ),
            "{error:?}"
        );
    }

    #[rstest]
    #[case::not_json(raw(200, b"<html>"), Unexpected::NotJson)]
    #[case::empty(raw(200, b""), Unexpected::NotJson)]
    #[case::wrong_shape(answer(200, &json!({ "id": 1 })), Unexpected::Body)]
    #[case::created(profile_with_status(201), Unexpected::Status)]
    #[tokio::test]
    async fn a_success_the_route_cant_answer_is_unexpected(
        #[case] incoming: Incoming,
        #[case] reason: Unexpected,
    ) {
        let store = TestStore::default();
        let core = core(&store);
        give_access(&core, "afk_at_0", 1);
        core.transport.on(ME, Ok(incoming));

        let error = core.me().await.unwrap_err();

        assert!(
            matches!(error, ClientError::UnexpectedResponse { reason: r, .. } if r == reason),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_dropped_caller_never_stops_its_refresh_from_saving() {
        let store = TestStore::holding("afk_rt_0").await;
        let core = core(&store);
        give_access(&core, "afk_at_0", 1);
        core.transport.on(ME, Ok(refusal(401, "unauthorized")));
        let held = core.transport.held(REFRESH, tokens(1));
        core.transport.on(ME, Ok(profile()));

        timeout(WAIT, async {
            tokio::select! {
                result = core.me() => panic!("the call ended first: {result:?}"),
                () = held.arrived.notified() => {}
            }
        })
        .await
        .unwrap();
        held.release.notify_one();
        settle(|| access(&core).as_deref() == Some("afk_at_1")).await;

        assert_eq!(store.token().await.as_deref(), Some("afk_rt_1"));
        assert_eq!(access(&core).as_deref(), Some("afk_at_1"));
        let me = core.me().await;
        assert!(me.is_ok(), "{me:?}");
        assert_eq!(core.transport.routes(), [ME, REFRESH, ME]);
        assert_eq!(
            core.transport.bearers(ME),
            [Some("afk_at_0".to_owned()), Some("afk_at_1".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_dropped_caller_never_stops_its_code_step_from_saving() {
        let store = TestStore::default();
        let core = core(&store);
        give_step(&core, StepKind::Mfa, "afk_mt_1");
        let held = core.transport.held(LOGIN_MFA, tokens(1));
        core.transport.on(ME, Ok(profile()));

        timeout(WAIT, async {
            tokio::select! {
                result = core.login_mfa(code("123456")) => panic!("the call ended first: {result:?}"),
                () = held.arrived.notified() => {}
            }
        })
        .await
        .unwrap();
        held.release.notify_one();
        settle(|| access(&core).is_some()).await;

        assert_eq!(store.token().await.as_deref(), Some("afk_rt_1"));
        assert_eq!(access(&core).as_deref(), Some("afk_at_1"));
        assert!(!has_step(&core));
        let me = core.me().await;
        assert!(me.is_ok(), "{me:?}");
        assert_eq!(core.transport.routes(), [LOGIN_MFA, ME]);
        assert_eq!(core.transport.bearers(ME), [Some("afk_at_1".to_owned())]);
    }

    #[tokio::test]
    async fn a_task_that_ends_without_an_answer_is_interrupted() {
        let task = tokio::spawn(async { panic!("the task failed") });

        let error = outcome::<()>(task).await.unwrap_err();

        assert!(matches!(error, ClientError::Interrupted(_)), "{error:?}");
    }

    // ---- Microsoft accounts ----

    const ACCOUNT: &str = "01900000-0000-7000-8000-00000000000a";
    const FLOW: &str = "01900000-0000-7000-8000-00000000000b";
    const GRANTEE: &str = "01900000-0000-7000-8000-00000000000c";
    const BOT: &str = "01900000-0000-7000-8000-00000000000d";
    const MODE: &str = "01900000-0000-7000-8000-00000000000e";

    fn id<T: core::str::FromStr>(text: &str) -> T
    where
        T::Err: core::fmt::Debug,
    {
        text.parse().unwrap()
    }

    fn at() -> fleet_api_types::Timestamp {
        serde_json::from_value(json!("2026-10-15T10:00:00.000Z")).unwrap()
    }

    fn label(user: &str, name: &str) -> fleet_api_types::users::UserLabel {
        fleet_api_types::users::UserLabel {
            id: id(user),
            username: fleet_api_types::BoundedText::new(name),
        }
    }

    fn started() -> LinkStartedResponse {
        LinkStartedResponse {
            flow_id: id(FLOW),
            user_code: SecretText::new("B7X4-KDLQ".to_owned()),
            verification_uri: fleet_api_types::BoundedText::new("https://www.microsoft.com/link"),
            expires_at: at(),
        }
    }

    fn an_account() -> AccountResponse {
        use fleet_api_types::accounts::{AccountKind, AccountStatus, GrantLevel};
        AccountResponse {
            id: id(ACCOUNT),
            name: fleet_api_types::BoundedText::new("AfkBot1"),
            kind: AccountKind::Microsoft.into(),
            status: AccountStatus::Pending.into(),
            reason: None,
            level: GrantLevel::Manage.into(),
            owner: Some(label("01900000-0000-7000-8000-000000000001", "alice")),
            linked_at: Some(at()),
        }
    }

    fn a_grant() -> GrantResponse {
        GrantResponse {
            user: label(GRANTEE, "carol"),
            level: fleet_api_types::accounts::GrantLevel::Control.into(),
            granted_by: label("01900000-0000-7000-8000-000000000001", "alice"),
            granted_at: at(),
        }
    }

    fn a_bot() -> BotResponse {
        use fleet_api_types::bots::{BotStatus, BotStatusKind, DesiredState, UnplacedReason};
        BotResponse {
            id: id(BOT),
            account: an_account(),
            server_address: Some(fleet_api_types::BoundedText::new("play.example.com")),
            mode_id: id(MODE),
            desired_state: DesiredState::Running.into(),
            status: BotStatus {
                why: Some(UnplacedReason::Pending.into()),
                ..BotStatus::of(BotStatusKind::Unplaced)
            },
            agent: None,
        }
    }

    fn first_page<I>() -> PageQuery<I> {
        PageQuery {
            before: None,
            limit: None,
        }
    }

    /// `value` as the server's answer with `status`.
    fn dto(status: u16, value: &impl Serialize) -> Incoming {
        answer(status, &serde_json::to_value(value).unwrap())
    }

    /// A core in a live session, holding `afk_at_0`.
    async fn signed_in() -> Arc<TestCore> {
        let store = TestStore::holding("afk_rt_0").await;
        let core = core(&store);
        give_access(&core, "afk_at_0", 1);
        core
    }

    fn sent(route: &str, body: Option<Value>) -> Seen {
        Seen {
            route: route.to_owned(),
            bearer: Some("afk_at_0".to_owned()),
            body,
        }
    }

    #[tokio::test]
    async fn a_link_and_a_link_again_post_without_a_body_and_read_the_201() {
        let core = signed_in().await;
        let link = "POST /api/v1/accounts/link";
        let again = format!("POST /api/v1/accounts/{ACCOUNT}/relink");
        core.transport.on(link, Ok(dto(201, &started())));
        core.transport.on(&again, Ok(dto(201, &started())));

        let new = core.start_link().await.unwrap();
        let relinked = core.relink_account(id(ACCOUNT)).await.unwrap();

        for answer in [new, relinked] {
            assert_eq!(answer.flow_id, id(FLOW));
            assert_eq!(answer.user_code.expose(), "B7X4-KDLQ");
            assert_eq!(
                answer.verification_uri.as_str(),
                "https://www.microsoft.com/link"
            );
            assert_eq!(answer.expires_at, at());
        }
        assert_eq!(
            core.transport.seen(),
            [sent(link, None), sent(&again, None)]
        );
    }

    #[tokio::test]
    async fn a_link_start_answered_with_200_is_unexpected() {
        let core = signed_in().await;
        core.transport
            .on("POST /api/v1/accounts/link", Ok(dto(200, &started())));

        let error = core.start_link().await.unwrap_err();

        assert!(
            matches!(
                error,
                ClientError::UnexpectedResponse {
                    status: 200,
                    reason: Unexpected::Status
                }
            ),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn the_link_status_reads_each_state_and_one_this_version_doesnt_know() {
        use fleet_api_types::accounts::{AccountStatus, LinkFlowState, RefusalReason};
        let core = signed_in().await;
        let route = format!("GET /api/v1/accounts/link/{FLOW}");
        let states = [
            LinkFlowResponse {
                status: LinkFlowState::Waiting.into(),
                expires_at: Some(at()),
                reason: None,
                account_id: None,
                account_status: None,
                name: None,
            },
            LinkFlowResponse {
                status: LinkFlowState::Refused.into(),
                expires_at: None,
                reason: Some(RefusalReason::NotOwned.into()),
                account_id: None,
                account_status: None,
                name: None,
            },
            LinkFlowResponse {
                status: LinkFlowState::Linked.into(),
                expires_at: None,
                reason: None,
                account_id: Some(id(ACCOUNT)),
                account_status: Some(AccountStatus::Pending.into()),
                name: Some(fleet_api_types::BoundedText::new("AfkBot1")),
            },
        ];
        for state in &states {
            core.transport.on(&route, Ok(dto(200, state)));
        }
        core.transport
            .on(&route, Ok(answer(200, &json!({ "status": "brand_new" }))));

        let mut read = Vec::new();
        for _ in 0..4 {
            read.push(core.link_status(id(FLOW)).await.unwrap());
        }

        assert_eq!(read.get(..3), Some(&states[..]));
        let unknown = read.get(3).unwrap();
        assert_eq!(unknown.status.known(), None);
        assert_eq!(unknown.status.as_str(), "brand_new");
        assert_eq!(
            core.transport.routes(),
            [route.clone(), route.clone(), route.clone(), route]
        );
    }

    #[tokio::test]
    async fn the_account_reads_go_to_their_routes() {
        let core = signed_in().await;
        let page = AccountPage {
            items: vec![an_account()],
            next: Some(id(ACCOUNT)),
        };
        let one = format!("GET /api/v1/accounts/{ACCOUNT}");
        core.transport
            .on("GET /api/v1/accounts", Ok(dto(200, &page)));
        core.transport.on(
            "GET /api/v1/accounts/awaiting-approval",
            Ok(dto(200, &page)),
        );
        core.transport.on(&one, Ok(dto(200, &an_account())));

        let listed = core.accounts(&first_page()).await.unwrap();
        let awaiting = core
            .accounts_awaiting_approval(&first_page())
            .await
            .unwrap();
        let read = core.account(id(ACCOUNT)).await.unwrap();

        assert_eq!(listed, page);
        assert_eq!(awaiting, page);
        assert_eq!(read, an_account());
        assert_eq!(
            core.transport.seen(),
            [
                sent("GET /api/v1/accounts", None),
                sent("GET /api/v1/accounts/awaiting-approval", None),
                sent(&one, None)
            ]
        );
    }

    #[tokio::test]
    async fn the_account_writes_send_their_method_path_and_body() {
        use fleet_api_types::accounts::{AccountKind, AccountStatus};
        let core = signed_in().await;
        let created = CreatedOfflineAccountResponse {
            id: id(ACCOUNT),
            name: fleet_api_types::BoundedText::new("AfkBot1"),
            kind: AccountKind::Offline.into(),
            status: AccountStatus::Active.into(),
        };
        let delete = format!("DELETE /api/v1/accounts/{ACCOUNT}");
        let approve = format!("POST /api/v1/accounts/{ACCOUNT}/approve");
        let refuse = format!("POST /api/v1/accounts/{ACCOUNT}/refuse");
        core.transport
            .on("POST /api/v1/accounts/offline", Ok(dto(201, &created)));
        core.transport.on(&delete, Ok(raw(204, b"")));
        core.transport.on(&approve, Ok(dto(200, &an_account())));
        core.transport.on(&refuse, Ok(raw(204, b"")));
        let request: CreateOfflineAccountRequest =
            serde_json::from_value(json!({ "name": "AfkBot1" })).unwrap();

        let made = core.create_offline_account(&request).await.unwrap();
        core.delete_account(id(ACCOUNT)).await.unwrap();
        let approved = core.approve_account(id(ACCOUNT)).await.unwrap();
        core.refuse_account(id(ACCOUNT)).await.unwrap();

        assert_eq!(made, created);
        assert_eq!(approved, an_account());
        assert_eq!(
            core.transport.seen(),
            [
                sent(
                    "POST /api/v1/accounts/offline",
                    Some(json!({ "name": "AfkBot1" }))
                ),
                sent(&delete, None),
                sent(&approve, None),
                sent(&refuse, None)
            ]
        );
    }

    #[tokio::test]
    async fn the_grant_routes_send_their_method_path_and_body() {
        let core = signed_in().await;
        let page = GrantPage {
            items: vec![a_grant()],
            next: Some(id(GRANTEE)),
        };
        let list = format!("GET /api/v1/accounts/{ACCOUNT}/grants");
        let create = format!("POST /api/v1/accounts/{ACCOUNT}/grants");
        let change = format!("PUT /api/v1/accounts/{ACCOUNT}/grants/{GRANTEE}");
        let revoke = format!("DELETE /api/v1/accounts/{ACCOUNT}/grants/{GRANTEE}");
        core.transport.on(&list, Ok(dto(200, &page)));
        core.transport.on(&create, Ok(dto(201, &a_grant())));
        core.transport.on(&change, Ok(dto(200, &a_grant())));
        core.transport.on(&revoke, Ok(raw(204, b"")));
        let grant: CreateGrantRequest =
            serde_json::from_value(json!({ "username": "carol", "level": "control" })).unwrap();
        let level: ChangeGrantRequest = serde_json::from_value(json!({ "level": "view" })).unwrap();

        let listed = core
            .account_grants(id(ACCOUNT), &first_page())
            .await
            .unwrap();
        let created = core.create_grant(id(ACCOUNT), &grant).await.unwrap();
        let changed = core
            .change_grant(id(ACCOUNT), id(GRANTEE), &level)
            .await
            .unwrap();
        core.revoke_grant(id(ACCOUNT), id(GRANTEE)).await.unwrap();

        assert_eq!(listed, page);
        assert_eq!((created, changed), (a_grant(), a_grant()));
        assert_eq!(
            core.transport.seen(),
            [
                sent(&list, None),
                sent(
                    &create,
                    Some(json!({ "username": "carol", "level": "control" }))
                ),
                sent(&change, Some(json!({ "level": "view" }))),
                sent(&revoke, None)
            ]
        );
    }

    #[tokio::test]
    async fn a_refused_account_write_comes_back_with_the_servers_code() {
        let core = signed_in().await;
        let delete = format!("DELETE /api/v1/accounts/{ACCOUNT}");
        core.transport
            .on(&delete, Ok(refusal(403, "step_up_required")));

        let error = core.delete_account(id(ACCOUNT)).await.unwrap_err();

        assert!(is_api(&error, 403, ErrorCode::StepUpRequired), "{error:?}");
        assert_eq!(core.transport.routes(), [delete]);
    }

    #[tokio::test]
    async fn the_bot_reads_go_to_their_routes() {
        let core = signed_in().await;
        let page = BotPage {
            items: vec![a_bot()],
            next: Some(id(BOT)),
        };
        let one = format!("GET /api/v1/bots/{BOT}");
        core.transport.on("GET /api/v1/bots", Ok(dto(200, &page)));
        core.transport.on(&one, Ok(dto(200, &a_bot())));

        let listed = core.bots(&first_page()).await.unwrap();
        let read = core.bot(id(BOT)).await.unwrap();

        assert_eq!(listed, page);
        assert_eq!(read, a_bot());
        assert_eq!(
            core.transport.seen(),
            [sent("GET /api/v1/bots", None), sent(&one, None)]
        );
    }

    #[tokio::test]
    async fn the_bot_settings_send_their_method_path_and_body() {
        let core = signed_in().await;
        let patch = format!("PATCH /api/v1/bots/{BOT}");
        let mode = format!("PUT /api/v1/bots/{BOT}/mode");
        core.transport.on(&patch, Ok(dto(200, &a_bot())));
        core.transport.on(&mode, Ok(dto(200, &a_bot())));
        let address: BotPatchRequest =
            serde_json::from_value(json!({ "server_address": "play.example.com:25566" })).unwrap();
        let chosen: BotModeRequest = serde_json::from_value(json!({ "mode_id": MODE })).unwrap();

        let patched = core.patch_bot(id(BOT), &address).await.unwrap();
        let moded = core.set_bot_mode(id(BOT), &chosen).await.unwrap();

        assert_eq!((patched, moded), (a_bot(), a_bot()));
        assert_eq!(
            core.transport.seen(),
            [
                sent(
                    &patch,
                    Some(json!({ "server_address": "play.example.com:25566" }))
                ),
                sent(&mode, Some(json!({ "mode_id": MODE })))
            ]
        );
    }

    #[rstest]
    #[case::start(BotCall::Start, "start", 200)]
    #[case::stop(BotCall::Stop, "stop", 200)]
    #[case::restart_sent(BotCall::Restart, "restart", 202)]
    #[case::reset_sent(BotCall::Reset, "reset", 202)]
    #[case::reset_cleared(BotCall::Reset, "reset", 200)]
    #[case::resume_sent(BotCall::Resume, "resume", 202)]
    #[case::resume_cleared(BotCall::Resume, "resume", 200)]
    #[tokio::test]
    async fn a_lifecycle_call_posts_without_a_body_and_reads_the_bot_from_each_answer_it_may_get(
        #[case] call: BotCall,
        #[case] segment: &str,
        #[case] status: u16,
    ) {
        let core = signed_in().await;
        let route = format!("POST /api/v1/bots/{BOT}/{segment}");
        core.transport.on(&route, Ok(dto(status, &a_bot())));

        let answered = core.control_bot(id(BOT), call).await.unwrap();

        assert_eq!(answered, a_bot());
        assert_eq!(core.transport.seen(), [sent(&route, None)]);
    }

    #[rstest]
    #[case::start_accepted(BotCall::Start, "start", 202)]
    #[case::stop_accepted(BotCall::Stop, "stop", 202)]
    #[case::restart_done(BotCall::Restart, "restart", 200)]
    #[case::reset_created(BotCall::Reset, "reset", 201)]
    #[case::resume_empty(BotCall::Resume, "resume", 204)]
    #[tokio::test]
    async fn a_lifecycle_answer_its_call_never_gets_is_unexpected(
        #[case] call: BotCall,
        #[case] segment: &str,
        #[case] status: u16,
    ) {
        let core = signed_in().await;
        let route = format!("POST /api/v1/bots/{BOT}/{segment}");
        core.transport.on(&route, Ok(dto(status, &a_bot())));

        let error = core.control_bot(id(BOT), call).await.unwrap_err();

        assert!(
            matches!(
                error,
                ClientError::UnexpectedResponse { status: s, reason: Unexpected::Status } if s == status
            ),
            "{error:?}"
        );
    }

    fn a_chat_page() -> ChatPage {
        use fleet_api_types::chat::{ChatDirection, ChatLine, ChatLineKind};
        ChatPage {
            items: vec![ChatLine {
                id: fleet_api_types::SafeInt::try_new(41).unwrap(),
                at: serde_json::from_value(json!("2027-01-15T08:00:00.000Z")).unwrap(),
                direction: ChatDirection::Out.into(),
                kind: ChatLineKind::User.into(),
                text: fleet_api_types::BoundedText::new("/login"),
                truncated: false,
                arguments_hidden: true,
                sender: None,
                sent_by: None,
                mine: true,
            }],
            next: Some(fleet_api_types::SafeInt::try_new(41).unwrap()),
        }
    }

    #[tokio::test]
    async fn the_history_is_read_from_its_route() {
        let core = signed_in().await;
        let route = format!("GET /api/v1/bots/{BOT}/chat");
        core.transport.on(&route, Ok(dto(200, &a_chat_page())));

        let page = core
            .chat(
                id(BOT),
                &ChatPageQuery {
                    before: None,
                    limit: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(page, a_chat_page());
        assert_eq!(core.transport.seen(), [sent(&route, None)]);
    }

    #[rstest]
    #[case::not_found(404, ErrorCode::NotFound)]
    #[case::out_of_range(422, ErrorCode::ValidationFailed)]
    #[case::too_many(429, ErrorCode::RateLimited)]
    #[tokio::test]
    async fn a_refused_history_comes_back_with_the_servers_code(
        #[case] status: u16,
        #[case] code: ErrorCode,
    ) {
        use fleet_api_types::OpenEnum as _;
        let core = signed_in().await;
        let route = format!("GET /api/v1/bots/{BOT}/chat");
        core.transport
            .on(&route, Ok(refusal(status, code.as_str())));

        let error = core
            .chat(
                id(BOT),
                &ChatPageQuery {
                    before: None,
                    limit: Some(0),
                },
            )
            .await
            .unwrap_err();

        assert!(is_api(&error, status, code), "{error:?}");
    }

    /// A mode as the server answers it.
    fn a_mode() -> ModeResponse {
        serde_json::from_value(json!({
            "id": "018bcfe5-6800-7cdc-8dcd-cdcdcdcdcdcd",
            "name": "Greeter",
            "visibility": "private",
            "ownership": "mine",
            "editable": true,
            "deletable": true,
            "visibility_editable": false,
            "version": 1,
            "created_at": "2027-01-15T08:00:00.000Z",
            "updated_at": "2027-01-15T08:00:00.000Z",
            "steps": [{ "action": { "type": "send_chat", "message": "/login", "arguments_hidden": true }, "schedule": { "type": "at_start" }, "probability": 100 }],
        }))
        .unwrap()
    }

    /// A new private mode that swings.
    fn a_new_mode() -> CreateModeRequest {
        serde_json::from_value(json!({ "name": "Greeter", "visibility": "private", "steps": [
            { "action": { "type": "swing_arm" }, "schedule": { "type": "every", "interval_ms": 20_000, "jitter_ms": 0 }, "probability": 100 },
        ] }))
        .unwrap()
    }

    #[tokio::test]
    async fn the_modes_are_read_from_their_route() {
        let core = signed_in().await;
        let route = "GET /api/v1/modes";
        let page = ModePage {
            items: vec![a_mode()],
            next: None,
        };
        core.transport.on(route, Ok(dto(200, &page)));

        let read = core
            .modes(&PageQuery {
                before: None,
                limit: None,
            })
            .await
            .unwrap();

        assert_eq!(read, page);
        assert_eq!(core.transport.seen(), [sent(route, None)]);
    }

    #[rstest]
    #[case::plain(false, "POST /api/v1/modes")]
    #[case::with_commands(true, "POST /api/v1/modes/with-commands")]
    #[tokio::test]
    async fn a_mode_is_created_through_its_route(#[case] with_commands: bool, #[case] route: &str) {
        let core = signed_in().await;
        core.transport.on(route, Ok(dto(201, &a_mode())));

        let created = core
            .create_mode(&a_new_mode(), with_commands)
            .await
            .unwrap();

        assert_eq!(created, a_mode());
        assert_eq!(
            core.transport.seen(),
            [sent(
                route,
                Some(serde_json::to_value(a_new_mode()).unwrap())
            )]
        );
    }

    #[tokio::test]
    async fn a_create_answered_with_another_success_is_unexpected() {
        let core = signed_in().await;
        core.transport
            .on("POST /api/v1/modes", Ok(dto(200, &a_mode())));

        let error = core.create_mode(&a_new_mode(), false).await.unwrap_err();

        assert!(
            matches!(
                error,
                ClientError::UnexpectedResponse {
                    status: 200,
                    reason: Unexpected::Status
                }
            ),
            "{error:?}"
        );
    }

    #[rstest]
    #[case::pointer(403, ErrorCode::ModeNeedsStepUp)]
    #[case::step_up(403, ErrorCode::StepUpRequired)]
    #[case::cap(409, ErrorCode::Conflict)]
    #[case::field(422, ErrorCode::ValidationFailed)]
    #[case::limited(429, ErrorCode::RateLimited)]
    #[tokio::test]
    async fn a_refused_create_comes_back_with_the_servers_code(
        #[case] status: u16,
        #[case] code: ErrorCode,
    ) {
        use fleet_api_types::OpenEnum as _;
        let core = signed_in().await;
        core.transport
            .on("POST /api/v1/modes", Ok(refusal(status, code.as_str())));

        let error = core.create_mode(&a_new_mode(), false).await.unwrap_err();

        assert!(is_api(&error, status, code), "{error:?}");
    }

    /// An edit of the mode `a_mode()` answers, at version 1.
    fn an_edit() -> UpdateModeRequest {
        serde_json::from_value(
            json!({ "version": 1, "name": "Greeter", "visibility": "shared", "steps": [] }),
        )
        .unwrap()
    }

    /// The route of the mode `a_mode()` answers.
    const A_MODE: &str = "/api/v1/modes/018bcfe5-6800-7cdc-8dcd-cdcdcdcdcdcd";

    #[rstest]
    #[case::plain(false, "")]
    #[case::with_commands(true, "/with-commands")]
    #[tokio::test]
    async fn a_mode_is_edited_through_its_route(#[case] with_commands: bool, #[case] tail: &str) {
        let core = signed_in().await;
        let route = format!("PUT {A_MODE}{tail}");
        core.transport.on(&route, Ok(dto(200, &a_mode())));

        let edited = core
            .update_mode(a_mode().id, &an_edit(), with_commands)
            .await
            .unwrap();

        assert_eq!(edited, a_mode());
        assert_eq!(
            core.transport.seen(),
            [sent(&route, Some(serde_json::to_value(an_edit()).unwrap()))]
        );
    }

    #[tokio::test]
    async fn a_mode_is_deleted_through_its_route() {
        let core = signed_in().await;
        let route = format!("DELETE {A_MODE}");
        core.transport.on(&route, Ok(raw(204, b"")));

        core.delete_mode(a_mode().id).await.unwrap();

        assert_eq!(core.transport.seen(), [sent(&route, None)]);
    }

    #[tokio::test]
    async fn an_edit_or_a_delete_answered_with_another_success_is_unexpected() {
        let core = signed_in().await;
        core.transport
            .on(&format!("PUT {A_MODE}"), Ok(dto(201, &a_mode())));
        core.transport
            .on(&format!("DELETE {A_MODE}"), Ok(dto(200, &a_mode())));

        let edit = core
            .update_mode(a_mode().id, &an_edit(), false)
            .await
            .unwrap_err();
        let delete = core.delete_mode(a_mode().id).await.unwrap_err();

        for (error, expected) in [(edit, 201), (delete, 200)] {
            assert!(
                matches!(
                    error,
                    ClientError::UnexpectedResponse {
                        status,
                        reason: Unexpected::Status
                    } if status == expected
                ),
                "{error:?}"
            );
        }
    }

    #[rstest]
    #[case::pointer("PUT", 403, ErrorCode::ModeNeedsStepUp)]
    #[case::changed("PUT", 409, ErrorCode::ModeChanged)]
    #[case::in_use_by_others("PUT", 409, ErrorCode::ModeInUseByOthers)]
    #[case::step_up("DELETE", 403, ErrorCode::StepUpRequired)]
    #[case::in_use("DELETE", 409, ErrorCode::ModeInUse)]
    #[tokio::test]
    async fn a_refused_edit_or_delete_comes_back_with_the_servers_code(
        #[case] method: &str,
        #[case] status: u16,
        #[case] code: ErrorCode,
    ) {
        use fleet_api_types::OpenEnum as _;
        let core = signed_in().await;
        core.transport.on(
            &format!("{method} {A_MODE}"),
            Ok(refusal(status, code.as_str())),
        );

        let error = if method == "PUT" {
            core.update_mode(a_mode().id, &an_edit(), false)
                .await
                .map(|_| ())
                .unwrap_err()
        } else {
            core.delete_mode(a_mode().id).await.unwrap_err()
        };

        assert!(is_api(&error, status, code), "{error:?}");
    }

    #[rstest]
    #[case::the_first_page(None, None, None)]
    #[case::a_cursor(Some(41), None, Some("before=41"))]
    #[case::a_limit(None, Some(10), Some("limit=10"))]
    #[case::both(Some(41), Some(10), Some("before=41&limit=10"))]
    fn a_history_query_carries_only_what_it_holds(
        #[case] before: Option<i64>,
        #[case] limit: Option<u32>,
        #[case] query: Option<&str>,
    ) {
        let url: Url = "https://fleet.example.com/api/v1/bots/x/chat"
            .parse()
            .unwrap();
        let chat = ChatPageQuery {
            before: before.map(|cursor| fleet_api_types::SafeInt::try_new(cursor).unwrap()),
            limit,
        };

        let built = with_chat_page(url, &chat);

        assert_eq!(built.query(), query);
    }

    fn a_chat_answer(outcome: fleet_api_types::chat::ChatSendOutcome) -> ChatSendResponse {
        ChatSendResponse {
            request_id: id("01900000-0000-7000-8000-0000000000c1"),
            message_id: fleet_api_types::SafeInt::try_new(41).unwrap(),
            outcome: outcome.into(),
        }
    }

    #[rstest]
    #[case::chat_sent("chat", 200)]
    #[case::chat_pending("chat", 202)]
    #[case::command_sent("command", 200)]
    #[case::command_pending("command", 202)]
    #[tokio::test]
    async fn a_line_posts_its_body_to_its_route_and_reads_either_answer(
        #[case] segment: &str,
        #[case] status: u16,
    ) {
        use fleet_api_types::chat::ChatSendOutcome;
        let core = signed_in().await;
        let route = format!("POST /api/v1/bots/{BOT}/{segment}");
        let outcome = if status == 200 {
            ChatSendOutcome::Sent
        } else {
            ChatSendOutcome::Pending
        };
        core.transport
            .on(&route, Ok(dto(status, &a_chat_answer(outcome))));

        let answered = if segment == "chat" {
            core.send_chat(
                id(BOT),
                &ChatSendRequest {
                    message: "/spawn now".to_owned(),
                },
            )
            .await
        } else {
            core.send_command(
                id(BOT),
                &ChatCommandRequest {
                    message: "/spawn now".to_owned(),
                },
            )
            .await
        };

        assert_eq!(answered.unwrap(), a_chat_answer(outcome));
        assert_eq!(
            core.transport.seen(),
            [sent(&route, Some(json!({ "message": "/spawn now" })))]
        );
    }

    #[rstest]
    #[case::created(201)]
    #[case::empty(204)]
    #[tokio::test]
    async fn a_chat_answer_a_line_never_gets_is_unexpected(#[case] status: u16) {
        use fleet_api_types::chat::ChatSendOutcome;
        let core = signed_in().await;
        let route = format!("POST /api/v1/bots/{BOT}/chat");
        core.transport.on(
            &route,
            Ok(dto(status, &a_chat_answer(ChatSendOutcome::Sent))),
        );

        let error = core
            .send_chat(
                id(BOT),
                &ChatSendRequest {
                    message: "hello".to_owned(),
                },
            )
            .await
            .unwrap_err();

        assert!(
            matches!(
                error,
                ClientError::UnexpectedResponse { status: s, reason: Unexpected::Status } if s == status
            ),
            "{error:?}"
        );
    }

    #[rstest]
    #[case::pointer(403, ErrorCode::CommandNeedsStepUp)]
    #[case::not_online(409, ErrorCode::BotNotOnline)]
    #[case::unavailable(409, ErrorCode::ChatUnavailable)]
    #[case::limited(429, ErrorCode::RateLimited)]
    #[tokio::test]
    async fn a_refused_line_comes_back_with_the_servers_code_and_wait(
        #[case] status: u16,
        #[case] code: ErrorCode,
    ) {
        use fleet_api_types::OpenEnum as _;
        let core = signed_in().await;
        let route = format!("POST /api/v1/bots/{BOT}/chat");
        let mut refused = refusal(status, code.as_str());
        refused.retry_after = (status == 429).then_some(Duration::from_secs(3));
        core.transport.on(&route, Ok(refused));

        let error = core
            .send_chat(
                id(BOT),
                &ChatSendRequest {
                    message: "hello".to_owned(),
                },
            )
            .await
            .unwrap_err();

        assert!(is_api(&error, status, code), "{error:?}");
        let ClientError::Api(failure) = error else {
            panic!("not an API failure");
        };
        assert_eq!(
            failure.retry_after,
            (status == 429).then_some(Duration::from_secs(3))
        );
    }

    #[rstest]
    #[case::unreachable(BotCall::Restart, "restart", ErrorCode::BotUnreachable)]
    #[case::not_running(BotCall::Restart, "restart", ErrorCode::BotNotRunning)]
    #[case::not_failed(BotCall::Reset, "reset", ErrorCode::BotNotFailed)]
    #[case::not_paused(BotCall::Resume, "resume", ErrorCode::BotNotPaused)]
    #[tokio::test]
    async fn a_refused_lifecycle_call_comes_back_with_the_servers_409_code(
        #[case] call: BotCall,
        #[case] segment: &str,
        #[case] code: ErrorCode,
    ) {
        use fleet_api_types::OpenEnum as _;
        let core = signed_in().await;
        let route = format!("POST /api/v1/bots/{BOT}/{segment}");
        core.transport.on(&route, Ok(refusal(409, code.as_str())));

        let error = core.control_bot(id(BOT), call).await.unwrap_err();

        assert!(is_api(&error, 409, code), "{error:?}");
        assert_eq!(core.transport.routes(), [route]);
    }
}
