//! [`Client`]: one method per endpoint of the API (Plan.md P8.1,
//! ADR-0017), over reqwest and a [`SessionStore`].
//!
//! The rules it keeps are the [crate docs](crate)': no token ever leaves
//! it, one refresh at a time and only on a 401 `unauthorized`, the session
//! ending locally on a refused refresh, a revoked session and every logout.
//!
//! The login is two steps: [`Client::login`] answers which step follows,
//! and keeps the step's token itself; [`Client::login_mfa`] takes the code,
//! or [`Client::mfa_enroll`] and [`Client::mfa_confirm`] set up 2FA first,
//! after which the user logs in again. [`Client::mfa_reset`] starts
//! replacing the authenticator in a session, with the same two calls after
//! it.

use core::fmt;
use core::time::Duration;
use std::sync::Arc;

use fleet_api_types::accounts::{
    AccountPage, AccountResponse, ChangeGrantRequest, CreateGrantRequest,
    CreateOfflineAccountRequest, CreatedOfflineAccountResponse, GrantPage, GrantResponse,
    HeldGrantPage, LinkFlowResponse, LinkStartedResponse,
};
use fleet_api_types::agents::{AgentPage, AgentResponse};
use fleet_api_types::audit::{AuditPage, AuditPageQuery};
use fleet_api_types::auth::{LoginRequest, RegisterRequest, RegisterResponse, StepUpRequest};
use fleet_api_types::bots::{BotModeRequest, BotPage, BotPatchRequest, BotResponse};
use fleet_api_types::chat::{
    ChatCommandRequest, ChatPage, ChatPageQuery, ChatSendRequest, ChatSendResponse,
};
use fleet_api_types::ids::AgentId;
use fleet_api_types::invites::{CreateInviteRequest, CreatedInviteResponse, InvitePage};
use fleet_api_types::me::{MeResponse, PasswordChangeRequest};
use fleet_api_types::mfa::{CODE_MAX_CHARS, MfaConfirmResponse, MfaEnrollResponse};
use fleet_api_types::modes::{CreateModeRequest, ModePage, ModeResponse, UpdateModeRequest};
use fleet_api_types::server_settings::{
    CreateServerSettingsRequest, ServerSettingsPage, ServerSettingsPageQuery,
    ServerSettingsResponse, UpdateServerSettingsRequest,
};
use fleet_api_types::sessions::SessionPage;
use fleet_api_types::users::{UserPage, UserPatchRequest, UserResponse};
use fleet_api_types::{
    AccountId, BotId, InviteId, LinkFlowId, ModeId, PageQuery, SecretText, ServerSettingsId,
    SessionId, UserId,
};

use crate::error::{BuildError, ClientError};
use crate::events::LiveEvents;
use crate::session::{BotCall, Core};
use crate::store::SessionStore;
use crate::transport::{ReqwestTransport, Timeouts};
use crate::trust::Trust;
use crate::url::ServerUrl;

/// Which server, and which certificates it may present.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// The server's address.
    pub server: ServerUrl,
    /// The certificates the client accepts.
    pub trust: Trust,
}

/// The step that follows the password: [`Client::login`]'s answer. The
/// step's token stays in the client; `expires_in` says when to restart the
/// login, since an expired token answers like a wrong code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginStep {
    /// Send a code from the authenticator app, or a recovery code, with
    /// [`Client::login_mfa`].
    MfaRequired {
        /// How long the step's token lives.
        expires_in: Duration,
    },
    /// The user has no 2FA yet: [`Client::mfa_enroll`], then
    /// [`Client::mfa_confirm`], then log in again.
    SetupRequired {
        /// How long the step's token lives.
        expires_in: Duration,
    },
}

/// What a restart, a reset or a resume did, as its status said (ADR-0020,
/// S73).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotCommandEffect {
    /// 202: sent to the bot's agent; its state changes when the agent
    /// reports back.
    Sent,
    /// 200: the bot runs nowhere, and its stored Paused or Failed state was
    /// cleared; no agent was involved.
    Cleared,
}

/// [`Client::restart_bot`]'s, [`Client::reset_bot`]'s and
/// [`Client::resume_bot`]'s answer.
#[derive(Debug, Clone, PartialEq)]
pub struct BotCommandAnswer {
    /// The bot as it reads after the call.
    pub bot: BotResponse,
    /// What the call did.
    pub effect: BotCommandEffect,
}

/// The API's client; see the [module docs](self). Clones share one session.
pub struct Client<S> {
    core: Arc<Core<S, ReqwestTransport>>,
}

impl<S: SessionStore> Client<S> {
    /// A client of `config`'s server, whose refresh token lives in `store`.
    /// A session stored there from an earlier run is used by the first
    /// authenticated call.
    ///
    /// # Errors
    /// [`BuildError`] when reqwest can't build its HTTP client, e.g. it
    /// refuses the custom CA.
    pub fn new(config: ClientConfig, store: S) -> Result<Self, BuildError> {
        let timeouts = Timeouts::default();
        let transport = ReqwestTransport::new(&config.trust, timeouts)?;
        Ok(Self {
            core: Arc::new(Core::new(config.server, transport, store, timeouts.store)),
        })
    }

    /// The server's address.
    #[must_use]
    pub fn server(&self) -> &ServerUrl {
        self.core.server()
    }

    /// `GET /health/live`: whether the server's process answers.
    ///
    /// # Errors
    /// [`ClientError`] when it doesn't, or answers something else than 200.
    pub async fn health_live(&self) -> Result<(), ClientError> {
        self.core.health("live").await
    }

    /// `GET /health/ready`: whether the server can serve requests, its
    /// database included.
    ///
    /// # Errors
    /// [`ClientError`] when it can't, e.g. 503 `busy`.
    pub async fn health_ready(&self) -> Result<(), ClientError> {
        self.core.health("ready").await
    }

    /// `POST /auth/login`: the password step. The step's token stays in the
    /// client for the next call.
    ///
    /// # Errors
    /// [`ClientError::AlreadyLoggedIn`] while a session is held, before any
    /// request; the server's 401 `invalid_credentials`, 422 or 429 as
    /// [`ClientError::Api`]; any transport error.
    pub async fn login(&self, request: &LoginRequest) -> Result<LoginStep, ClientError> {
        self.core.login(request).await
    }

    /// `POST /auth/login/mfa`: the code step after [`LoginStep::MfaRequired`].
    /// On success the session starts: its refresh token goes into the store.
    ///
    /// # Errors
    /// [`ClientError::NoPendingStep`] without that step; the server's 401
    /// `invalid_credentials` (a wrong code; the step stays for another try),
    /// 422 or 429 as [`ClientError::Api`]; [`ClientError::NoSession`] when
    /// a logout ended the session before it was saved.
    pub async fn login_mfa(&self, code: SecretText<CODE_MAX_CHARS>) -> Result<(), ClientError> {
        self.core.login_mfa(code).await
    }

    /// `POST /auth/logout`: ends the session, here at once and then on the
    /// server, best effort. With no session it does nothing.
    ///
    /// # Errors
    /// The server's or the transport's error; the session has ended here
    /// anyway, and the server's lives on until its idle expiry.
    pub async fn logout(&self) -> Result<(), ClientError> {
        self.core.logout().await
    }

    /// Ends the session here without telling the server: the stored refresh
    /// token is removed and the access token and any pending step are
    /// dropped; nothing is sent. The desktop app calls it on a client for a
    /// server profile it just set, so a token stored under another profile
    /// is never resumed (ADR-0017).
    ///
    /// # Errors
    /// [`ClientError::Store`] when the store can't be cleared: unlike
    /// [`Client::logout`], the failure is returned, since a token may remain.
    /// The tokens in memory are dropped anyway.
    pub async fn discard_session(&self) -> Result<(), ClientError> {
        self.core.discard().await
    }

    /// `POST /auth/step-up`: the password again, which allows step-up
    /// actions in this session for 5 minutes.
    ///
    /// # Errors
    /// A wrong password is the server's 403 `forbidden`, which never ends
    /// the session; any other error as for every authenticated call.
    pub async fn step_up(&self, request: &StepUpRequest) -> Result<(), ClientError> {
        self.core.step_up(request).await
    }

    /// `POST /auth/register`: an account from an invite. It answers no
    /// tokens: log in with it next.
    ///
    /// # Errors
    /// The server's 422 (an invite it can't use, a taken name, a weak
    /// password) or 429 as [`ClientError::Api`]; any transport error.
    pub async fn register(
        &self,
        request: &RegisterRequest,
    ) -> Result<RegisterResponse, ClientError> {
        self.core.register(request).await
    }

    /// `GET /me`: the user's profile.
    ///
    /// # Errors
    /// [`ClientError::NoSession`] without a session; any other error as for
    /// every authenticated call.
    pub async fn me(&self) -> Result<MeResponse, ClientError> {
        self.core.me().await
    }

    /// `POST /me/password`: a new password, after [`Client::step_up`]. Every
    /// other session of the user ends.
    ///
    /// # Errors
    /// The server's 403 `step_up_required` or 422 as [`ClientError::Api`];
    /// any other error as for every authenticated call.
    pub async fn change_password(
        &self,
        request: &PasswordChangeRequest,
    ) -> Result<(), ClientError> {
        self.core.change_password(request).await
    }

    /// `GET /me/sessions`: a page of the user's live sessions.
    ///
    /// # Errors
    /// The server's 422 for a `limit` out of range; any other error as for
    /// every authenticated call.
    pub async fn sessions(&self, query: &PageQuery<SessionId>) -> Result<SessionPage, ClientError> {
        self.core.sessions(query).await
    }

    /// `DELETE /me/sessions/{id}`: ends another session of the user.
    ///
    /// # Errors
    /// The server's 409 for the current session (log out instead) or 404
    /// as [`ClientError::Api`]; any other error as for every authenticated
    /// call.
    pub async fn revoke_session(&self, id: SessionId) -> Result<(), ClientError> {
        self.core.revoke_session(id).await
    }

    /// `POST /me/mfa/enroll`: a new TOTP secret, with the pending setup
    /// step's token. It may be repeated; each call replaces the secret.
    ///
    /// # Errors
    /// [`ClientError::NoPendingStep`] without a setup step; the server's
    /// 401 as [`ClientError::Api`], after which the step is gone (it
    /// expired: start again).
    pub async fn mfa_enroll(&self) -> Result<MfaEnrollResponse, ClientError> {
        self.core.mfa_enroll().await
    }

    /// `POST /me/mfa/confirm`: the first code of the new authenticator. It
    /// answers the 10 recovery codes, shown once, and ends the setup step.
    ///
    /// # Errors
    /// [`ClientError::NoPendingStep`] without a setup step; the server's
    /// 422 for a wrong code (the step stays) or 401 (the step is gone) as
    /// [`ClientError::Api`].
    pub async fn mfa_confirm(
        &self,
        code: SecretText<CODE_MAX_CHARS>,
    ) -> Result<MfaConfirmResponse, ClientError> {
        self.core.mfa_confirm(code).await
    }

    /// `POST /me/mfa/reset`: starts replacing the authenticator, after
    /// [`Client::step_up`]. The answer's setup token becomes the pending
    /// step for [`Client::mfa_enroll`] and [`Client::mfa_confirm`]; it lives
    /// for the duration returned. Until confirm, the old authenticator
    /// keeps working.
    ///
    /// # Errors
    /// The server's 403 `step_up_required` as [`ClientError::Api`]; any
    /// other error as for every authenticated call.
    pub async fn mfa_reset(&self) -> Result<Duration, ClientError> {
        self.core.mfa_reset().await
    }

    /// `GET /audit`: one page of the audit log, newest first (Admins and
    /// the Owner): each entry as the log stores it. `query`'s `before` is
    /// the `next` of the page before.
    ///
    /// # Errors
    /// The server's 403 for a Member, 422 (a `limit` outside 1 to 100, a
    /// `before` below 1) or 429 as [`ClientError::Api`]; any other error as
    /// for every authenticated call.
    pub async fn audit(&self, query: &AuditPageQuery) -> Result<AuditPage, ClientError> {
        self.core.audit(query).await
    }

    /// `GET /agents`: a page of the agents, newest first, with their status
    /// and bots count (Admins and the Owner).
    ///
    /// # Errors
    /// The server's 403 for a Member, or 422; any other error as for every
    /// authenticated call.
    pub async fn agents(&self, query: &PageQuery<AgentId>) -> Result<AgentPage, ClientError> {
        self.core.agents(query).await
    }

    /// `POST /agents/{id}/release`: releases a disabled agent's bots, on the
    /// caller's word that its host and its bots are stopped, so they can be
    /// placed elsewhere (Admins and the Owner, after a step-up). The agent
    /// as the list shows it after.
    ///
    /// # Errors
    /// The server's 403 (`step_up_required` without a fresh step-up;
    /// `forbidden` for a Member), 404, 409 (`agent_not_disabled`) or 429 as
    /// [`ClientError::Api`]; any other error as for every authenticated
    /// call.
    pub async fn release_agent(&self, id: AgentId) -> Result<AgentResponse, ClientError> {
        self.core.release_agent(id).await
    }

    /// `GET /users/{id}/grants`: a page of the grants the user holds,
    /// dormant ones included, newest account first: the review at a role
    /// change (the Owner's).
    ///
    /// # Errors
    /// The server's 403 (an Admin, or a Member on themselves), 404 (no such
    /// user, or a Member on another), 422 or 429 as [`ClientError::Api`];
    /// any other error as for every authenticated call.
    pub async fn user_grants(
        &self,
        id: UserId,
        query: &PageQuery<AccountId>,
    ) -> Result<HeldGrantPage, ClientError> {
        self.core.user_grants(id, query).await
    }

    /// `GET /users/{id}/shared-accounts`: a page of the user's accounts that
    /// at least one grant shares, newest first; [`Client::account_grants`]
    /// lists each one's grants (the Owner's).
    ///
    /// # Errors
    /// As [`Client::user_grants`].
    pub async fn shared_accounts(
        &self,
        id: UserId,
        query: &PageQuery<AccountId>,
    ) -> Result<AccountPage, ClientError> {
        self.core.shared_accounts(id, query).await
    }

    /// `GET /users`: a page of the users (Admins and the Owner).
    ///
    /// # Errors
    /// The server's 403 for a Member, or 422; any other error as for every
    /// authenticated call.
    pub async fn users(&self, query: &PageQuery<UserId>) -> Result<UserPage, ClientError> {
        self.core.users(query).await
    }

    /// `PATCH /users/{id}`: one change of a user, a role or `disabled`,
    /// after [`Client::step_up`] within 5 minutes.
    ///
    /// # Errors
    /// The server's 403 (`step_up_required` without a fresh step-up,
    /// `forbidden` for a change that isn't the caller's), 404 or 422 as
    /// [`ClientError::Api`]; any other error as for every authenticated call.
    pub async fn patch_user(
        &self,
        id: UserId,
        request: &UserPatchRequest,
    ) -> Result<UserResponse, ClientError> {
        self.core.patch_user(id, request).await
    }

    /// `POST /invites`: a new invite, whose code is answered this once,
    /// after [`Client::step_up`] within 5 minutes.
    ///
    /// # Errors
    /// The server's 403 (`step_up_required` without a fresh step-up,
    /// `forbidden` for a role that isn't the caller's to invite), 404 or 409
    /// (too many pending invites) as [`ClientError::Api`]; any other error as
    /// for every authenticated call.
    pub async fn create_invite(
        &self,
        request: &CreateInviteRequest,
    ) -> Result<CreatedInviteResponse, ClientError> {
        self.core.create_invite(request).await
    }

    /// `GET /invites`: a page of the invites.
    ///
    /// # Errors
    /// The server's 403 for a Member, or 422; any other error as for every
    /// authenticated call.
    pub async fn invites(&self, query: &PageQuery<InviteId>) -> Result<InvitePage, ClientError> {
        self.core.invites(query).await
    }

    /// `DELETE /invites/{id}`: revokes a pending invite, after
    /// [`Client::step_up`] within 5 minutes.
    ///
    /// # Errors
    /// The server's 409 for a used invite, or 403 (`step_up_required`
    /// without a fresh step-up, `forbidden` for an invite that isn't the
    /// caller's to revoke) or 404, as [`ClientError::Api`]; any other error
    /// as for every authenticated call.
    pub async fn revoke_invite(&self, id: InviteId) -> Result<(), ClientError> {
        self.core.revoke_invite(id).await
    }

    /// `POST /accounts/link`: starts linking a Microsoft account with
    /// Microsoft's device code. The answer is the only place the user code
    /// ever shows; [`VerifiedLink`](crate::VerifiedLink) checks its link
    /// before an app opens it.
    ///
    /// # Errors
    /// The server's 409 (a Member's quota is full), 429 (the user's flow is
    /// finishing, or the daily cap; with its wait) or 503 (every place is
    /// taken, or Microsoft can't be reached; with its wait) as
    /// [`ClientError::Api`]; any other error as for every authenticated
    /// call.
    pub async fn start_link(&self) -> Result<LinkStartedResponse, ClientError> {
        self.core.start_link().await
    }

    /// `GET /accounts/link/{flow_id}`: the state of the caller's link flow.
    /// It never holds the user code.
    ///
    /// # Errors
    /// The server's 404 for a flow that isn't the caller's, was replaced, or
    /// ended more than 15 minutes ago, as [`ClientError::Api`]; any other
    /// error as for every authenticated call.
    pub async fn link_status(&self, flow: LinkFlowId) -> Result<LinkFlowResponse, ClientError> {
        self.core.link_status(flow).await
    }

    /// `POST /accounts/offline`: a new offline account, while the server
    /// allows them (`[accounts] allow_offline`, meant for test servers).
    ///
    /// # Errors
    /// The server's 404 while offline accounts are off, 409 (the name is
    /// taken in any case, or a Member's quota is full) or 422 as
    /// [`ClientError::Api`]; any other error as for every authenticated
    /// call.
    pub async fn create_offline_account(
        &self,
        request: &CreateOfflineAccountRequest,
    ) -> Result<CreatedOfflineAccountResponse, ClientError> {
        self.core.create_offline_account(request).await
    }

    /// `GET /accounts`: a page of the accounts the caller may see, newest
    /// first.
    ///
    /// # Errors
    /// The server's 422 for a limit out of range; any other error as for
    /// every authenticated call.
    pub async fn accounts(&self, query: &PageQuery<AccountId>) -> Result<AccountPage, ClientError> {
        self.core.accounts(query).await
    }

    /// `GET /accounts/awaiting-approval`: a page of the pending links the
    /// caller may approve or refuse; a Member's is empty.
    ///
    /// # Errors
    /// The server's 422 for a limit out of range; any other error as for
    /// every authenticated call.
    pub async fn accounts_awaiting_approval(
        &self,
        query: &PageQuery<AccountId>,
    ) -> Result<AccountPage, ClientError> {
        self.core.accounts_awaiting_approval(query).await
    }

    /// `GET /accounts/{id}`: one account, as the caller sees it.
    ///
    /// # Errors
    /// The server's 404 for an account that doesn't exist or that the caller
    /// may not see, as [`ClientError::Api`]; any other error as for every
    /// authenticated call.
    pub async fn account(&self, id: AccountId) -> Result<AccountResponse, ClientError> {
        self.core.account(id).await
    }

    /// `DELETE /accounts/{id}`: deletes an account with its grants and its
    /// stored sign-in, after [`Client::step_up`] within 5 minutes.
    ///
    /// # Errors
    /// The server's 403 (`step_up_required` without a fresh step-up,
    /// `forbidden` below Manage) or 404, as [`ClientError::Api`]; any other
    /// error as for every authenticated call.
    pub async fn delete_account(&self, id: AccountId) -> Result<(), ClientError> {
        self.core.delete_account(id).await
    }

    /// `POST /accounts/{id}/approve`: approves a pending link, after
    /// [`Client::step_up`] within 5 minutes; bots may use it from then on.
    ///
    /// # Errors
    /// The server's 403 (`step_up_required` without a fresh step-up,
    /// `forbidden` when the caller isn't above the owner), 404, or 409 for
    /// an account that isn't pending, as [`ClientError::Api`]; any other
    /// error as for every authenticated call.
    pub async fn approve_account(&self, id: AccountId) -> Result<AccountResponse, ClientError> {
        self.core.approve_account(id).await
    }

    /// `POST /accounts/{id}/refuse`: refuses a pending link, which deletes
    /// it, after [`Client::step_up`] within 5 minutes.
    ///
    /// # Errors
    /// As [`Client::approve_account`].
    pub async fn refuse_account(&self, id: AccountId) -> Result<(), ClientError> {
        self.core.refuse_account(id).await
    }

    /// `POST /accounts/{id}/relink`: starts linking an account again, with
    /// the same answer and caps as [`Client::start_link`].
    ///
    /// # Errors
    /// The server's 403 below Manage, 404, 409 for a pending or offline
    /// account, or the 429s and 503s of [`Client::start_link`], as
    /// [`ClientError::Api`]; any other error as for every authenticated
    /// call.
    pub async fn relink_account(&self, id: AccountId) -> Result<LinkStartedResponse, ClientError> {
        self.core.relink_account(id).await
    }

    /// `GET /accounts/{id}/grants`: a page of who else has access (Manage).
    ///
    /// # Errors
    /// The server's 403 below Manage, 404 or 422, as [`ClientError::Api`];
    /// any other error as for every authenticated call.
    pub async fn account_grants(
        &self,
        id: AccountId,
        query: &PageQuery<UserId>,
    ) -> Result<GrantPage, ClientError> {
        self.core.account_grants(id, query).await
    }

    /// `POST /accounts/{id}/grants`: gives a user access by their exact
    /// username, after [`Client::step_up`] within 5 minutes.
    ///
    /// # Errors
    /// The server's 403 (`step_up_required` without a fresh step-up,
    /// `forbidden` below Manage), 404, 409 (a grant that exists, or a
    /// pending account), 422 (a name that can't be given a grant) or 429
    /// (the by-name limit), as [`ClientError::Api`]; any other error as for
    /// every authenticated call.
    pub async fn create_grant(
        &self,
        id: AccountId,
        request: &CreateGrantRequest,
    ) -> Result<GrantResponse, ClientError> {
        self.core.create_grant(id, request).await
    }

    /// `PUT /accounts/{id}/grants/{user}`: changes a grant's level, after
    /// [`Client::step_up`] within 5 minutes.
    ///
    /// # Errors
    /// The server's 403 (`step_up_required` without a fresh step-up,
    /// `forbidden` below Manage), 404 (no such grant), or 409 (a raise on a
    /// pending account, or for someone with implicit Manage), as
    /// [`ClientError::Api`]; any other error as for every authenticated
    /// call.
    pub async fn change_grant(
        &self,
        id: AccountId,
        user: UserId,
        request: &ChangeGrantRequest,
    ) -> Result<GrantResponse, ClientError> {
        self.core.change_grant(id, user, request).await
    }

    /// `DELETE /accounts/{id}/grants/{user}`: revokes a grant, after
    /// [`Client::step_up`] within 5 minutes.
    ///
    /// # Errors
    /// The server's 403 (`step_up_required` without a fresh step-up,
    /// `forbidden` below Manage) or 404 (no such grant), as
    /// [`ClientError::Api`]; any other error as for every authenticated
    /// call.
    pub async fn revoke_grant(&self, id: AccountId, user: UserId) -> Result<(), ClientError> {
        self.core.revoke_grant(id, user).await
    }

    /// `DELETE /accounts/{id}/grants/me`: gives up the caller's own grant,
    /// leaving the shared account, after [`Client::step_up`] within 5
    /// minutes.
    ///
    /// # Errors
    /// The server's 403 (`step_up_required` without a fresh step-up,
    /// `forbidden` for the account's owner, the Owner, or an Admin on a
    /// Member's account) or 404 (no such account, or no grant on it), as
    /// [`ClientError::Api`]; any other error as for every authenticated
    /// call.
    pub async fn leave_account(&self, id: AccountId) -> Result<(), ClientError> {
        self.core.leave_account(id).await
    }

    /// `GET /bots`: a page of the bots the caller may see, newest first.
    ///
    /// # Errors
    /// The server's 422 (a bad `before` or `limit`) as [`ClientError::Api`];
    /// any other error as for every authenticated call.
    pub async fn bots(&self, query: &PageQuery<BotId>) -> Result<BotPage, ClientError> {
        self.core.bots(query).await
    }

    /// `GET /bots/{id}`: one bot, as the caller sees it.
    ///
    /// # Errors
    /// The server's 404 (no such bot, or one the caller may not see) as
    /// [`ClientError::Api`]; any other error as for every authenticated
    /// call.
    pub async fn bot(&self, id: BotId) -> Result<BotResponse, ClientError> {
        self.core.bot(id).await
    }

    /// `PATCH /bots/{id}`: sets the bot's server address (Manage), after
    /// [`Client::step_up`] within 5 minutes. A running bot moves to it.
    ///
    /// # Errors
    /// The server's 403 (`step_up_required` without a fresh step-up,
    /// `forbidden` below Manage), 404, 422 (an address that doesn't parse)
    /// or 429 (the bot's own limit), as [`ClientError::Api`]; any other
    /// error as for every authenticated call.
    pub async fn patch_bot(
        &self,
        id: BotId,
        request: &BotPatchRequest,
    ) -> Result<BotResponse, ClientError> {
        self.core.patch_bot(id, request).await
    }

    /// `PUT /bots/{id}/mode`: sets the bot's mode, one the caller may use.
    ///
    /// # Errors
    /// The server's 403 (a mode whose commands only Manage may give), 404 or
    /// 422 (a mode the caller can't use), as [`ClientError::Api`]; any
    /// other error as for every authenticated call.
    pub async fn set_bot_mode(
        &self,
        id: BotId,
        request: &BotModeRequest,
    ) -> Result<BotResponse, ClientError> {
        self.core.set_bot_mode(id, request).await
    }

    /// `POST /bots/{id}/start`: the bot should run (Control). Where it
    /// can't yet, it reads unplaced, with why.
    ///
    /// # Errors
    /// The server's 403, 404 or 429 (the bot's own limit) as
    /// [`ClientError::Api`]; any other error as for every authenticated
    /// call.
    pub async fn start_bot(&self, id: BotId) -> Result<BotResponse, ClientError> {
        self.core.control_bot(id, BotCall::Start).await
    }

    /// `POST /bots/{id}/stop`: the bot should be stopped (Control); a
    /// paused or failed bot stays so.
    ///
    /// # Errors
    /// The server's 403 or 404 as [`ClientError::Api`]; any other error as
    /// for every authenticated call.
    pub async fn stop_bot(&self, id: BotId) -> Result<BotResponse, ClientError> {
        self.core.control_bot(id, BotCall::Stop).await
    }

    /// `POST /bots/{id}/restart`: restarts a running bot's run (Control),
    /// sent to its agent: always [`BotCommandEffect::Sent`].
    ///
    /// # Errors
    /// The server's 403, 404, 409 (`bot_not_running`, `bot_unreachable`) or
    /// 429 as [`ClientError::Api`]; any other error as for every
    /// authenticated call.
    pub async fn restart_bot(&self, id: BotId) -> Result<BotCommandAnswer, ClientError> {
        self.core.command_bot(id, BotCall::Restart).await
    }

    /// `POST /bots/{id}/reset`: lets a failed bot try again (Control): sent
    /// to its agent, or, where it runs nowhere, its stored state cleared, as
    /// the answer's effect says.
    ///
    /// # Errors
    /// The server's 403, 404, 409 (`bot_not_failed`, `bot_unreachable`) or
    /// 429 as [`ClientError::Api`]; any other error as for every
    /// authenticated call.
    pub async fn reset_bot(&self, id: BotId) -> Result<BotCommandAnswer, ClientError> {
        self.core.command_bot(id, BotCall::Reset).await
    }

    /// `POST /bots/{id}/resume`: lets a paused bot join again (Control):
    /// sent to its agent, or, where it runs nowhere, its stored state
    /// cleared, as the answer's effect says. A human playing the account is
    /// kicked.
    ///
    /// # Errors
    /// The server's 403, 404, 409 (`bot_not_paused`, `bot_unreachable`) or
    /// 429 as [`ClientError::Api`]; any other error as for every
    /// authenticated call.
    pub async fn resume_bot(&self, id: BotId) -> Result<BotCommandAnswer, ClientError> {
        self.core.command_bot(id, BotCall::Resume).await
    }

    /// `GET /bots/{id}/chat`: one page of the bot's chat, newest first, as
    /// the caller may see it: a command's arguments only for Manage holders,
    /// who had a user's line sent only for callers who see the account's
    /// owner, and `mine` on the caller's own lines. `query`'s `before` is
    /// the `next` of the page before.
    ///
    /// # Errors
    /// The server's 404 (no such bot, or one the caller may not see), 422
    /// (a `limit` outside 1 to 100, a `before` below 1) or 429 (with its
    /// wait) as [`ClientError::Api`]; [`ClientError::NoSession`] without a
    /// session; and the transport's errors.
    pub async fn chat(&self, id: BotId, query: &ChatPageQuery) -> Result<ChatPage, ClientError> {
        self.core.chat(id, query).await
    }

    /// `POST /bots/{id}/chat`: an online bot sends a line, plain chat or a
    /// command on the server's allowlist (Control). The answer says
    /// whether its agent said it went out (`sent`) or no answer came in time
    /// (`pending`); either way it's in the bot's chat as the caller's.
    ///
    /// # Errors
    /// The server's 403 (`command_needs_step_up` for a command off the
    /// allowlist, which [`Client::send_command`] sends; `forbidden` below
    /// Control), 404, 409 (`bot_not_online`, `chat_unavailable`,
    /// `bot_unreachable`), 422 or 429 (with its wait) as
    /// [`ClientError::Api`]; any other error as for every authenticated
    /// call.
    pub async fn send_chat(
        &self,
        id: BotId,
        request: &ChatSendRequest,
    ) -> Result<ChatSendResponse, ClientError> {
        self.core.send_chat(id, request).await
    }

    /// `POST /bots/{id}/command`: an online bot sends a command, after
    /// [`Client::step_up`] within 5 minutes: Control for one on the
    /// allowlist, Manage for any other. It answers as
    /// [`Client::send_chat`] does.
    ///
    /// # Errors
    /// The server's 403 (`step_up_required` without a fresh step-up,
    /// `forbidden` below the level the command needs), 404, 409, 422 (a line
    /// that isn't a command) or 429 as [`ClientError::Api`]; any other error
    /// as for every authenticated call.
    pub async fn send_command(
        &self,
        id: BotId,
        request: &ChatCommandRequest,
    ) -> Result<ChatSendResponse, ClientError> {
        self.core.send_command(id, request).await
    }

    /// `GET /modes`: a page of the modes the caller may see, newest first,
    /// each as the server shows it to them: who owns it only for Admins and
    /// the Owner, what they may do with it, and a command's arguments only
    /// when they may edit the mode.
    ///
    /// # Errors
    /// The server's 422 (a `limit` outside 1 to 100), as
    /// [`ClientError::Api`]; any other error as for every authenticated call.
    pub async fn modes(&self, query: &PageQuery<ModeId>) -> Result<ModePage, ClientError> {
        self.core.modes(query).await
    }

    /// `POST /modes`: a new mode of the caller's (201), whose steps send no
    /// command off the server's allowlist; [`Client::create_mode_with_commands`]
    /// takes one that does.
    ///
    /// # Errors
    /// The server's 403 (`mode_needs_step_up` for a command off the
    /// allowlist; `forbidden` for a shared mode below Admin), 409 (`conflict`:
    /// the caller owns 50 modes) or 422 (a field that breaks its rule, at its
    /// path), as [`ClientError::Api`]; any other error as for every
    /// authenticated call.
    pub async fn create_mode(
        &self,
        request: &CreateModeRequest,
    ) -> Result<ModeResponse, ClientError> {
        self.core.create_mode(request, false).await
    }

    /// `POST /modes/with-commands`: a new mode of the caller's (201), whatever
    /// commands its steps send, after [`Client::step_up`] within 5 minutes.
    ///
    /// # Errors
    /// As [`Client::create_mode`], and 403 `step_up_required` without a
    /// fresh step-up instead of the pointer.
    pub async fn create_mode_with_commands(
        &self,
        request: &CreateModeRequest,
    ) -> Result<ModeResponse, ClientError> {
        self.core.create_mode(request, true).await
    }

    /// `PUT /modes/{id}`: the mode as it should be, at the `version` the
    /// caller read (200: the mode as it now is, unchanged when the edit shows
    /// the same), whose steps send no command off the server's allowlist;
    /// [`Client::update_mode_with_commands`] takes one that does.
    ///
    /// # Errors
    /// The server's 403 (`mode_needs_step_up` for a command off the
    /// allowlist; `forbidden` without the right to edit the mode, both as it
    /// is and as it will be), 404, 409 (`mode_changed`: the mode changed
    /// since `version`; `mode_in_use_by_others`: a bot uses it that the
    /// caller couldn't set the edited mode on) or 422 (a field that breaks its
    /// rule, at its path), as [`ClientError::Api`]; any other error as for
    /// every authenticated call.
    pub async fn update_mode(
        &self,
        id: ModeId,
        request: &UpdateModeRequest,
    ) -> Result<ModeResponse, ClientError> {
        self.core.update_mode(id, request, false).await
    }

    /// `PUT /modes/{id}/with-commands`: the mode as it should be, whatever
    /// commands its steps send, after [`Client::step_up`] within 5 minutes.
    ///
    /// # Errors
    /// As [`Client::update_mode`], and 403 `step_up_required` without a
    /// fresh step-up instead of the pointer.
    pub async fn update_mode_with_commands(
        &self,
        id: ModeId,
        request: &UpdateModeRequest,
    ) -> Result<ModeResponse, ClientError> {
        self.core.update_mode(id, request, true).await
    }

    /// `DELETE /modes/{id}`: deletes a mode no bot uses, after
    /// [`Client::step_up`] within 5 minutes.
    ///
    /// # Errors
    /// The server's 403 (`step_up_required` without a fresh step-up,
    /// `forbidden` without the right to delete it), 404 or 409
    /// (`mode_in_use`: a bot uses it), as [`ClientError::Api`]; any other
    /// error as for every authenticated call.
    pub async fn delete_mode(&self, id: ModeId) -> Result<(), ClientError> {
        self.core.delete_mode(id).await
    }

    /// `GET /server-settings`: a page of the servers' settings, newest
    /// first; Admins and the Owner only (P11.9; ADR-0020, S75).
    ///
    /// # Errors
    /// The server's 403 (`forbidden`: not an Admin or the Owner) or 422 (a
    /// `limit` outside 1 to 10), as [`ClientError::Api`]; any other error as
    /// for every authenticated call.
    pub async fn server_settings(
        &self,
        query: &ServerSettingsPageQuery,
    ) -> Result<ServerSettingsPage, ClientError> {
        self.core.server_settings(query).await
    }

    /// `POST /server-settings`: a server's new settings (201), after
    /// [`Client::step_up`] within 5 minutes (P11.9; ADR-0020, U54).
    ///
    /// # Errors
    /// The server's 403 (`step_up_required`, or `forbidden`: not an Admin
    /// or the Owner), 409 (`conflict`: 100 servers have settings already)
    /// or 422 (a field that breaks its rule, at its path; a server that has
    /// settings already at `server`), as [`ClientError::Api`]; any other
    /// error as for every authenticated call.
    pub async fn create_server_settings(
        &self,
        request: &CreateServerSettingsRequest,
    ) -> Result<ServerSettingsResponse, ClientError> {
        self.core.create_server_settings(request).await
    }

    /// `PUT /server-settings/{id}`: the settings as they should be, at the
    /// `version` the caller read (200: as they now are, unchanged when the
    /// edit asks for what's stored), after [`Client::step_up`] within 5
    /// minutes (S77).
    ///
    /// # Errors
    /// The server's 403 (`step_up_required`, or `forbidden`), 404, 409
    /// (`server_settings_changed`: they changed since `version`) or 422, as
    /// [`ClientError::Api`]; any other error as for every authenticated call.
    pub async fn update_server_settings(
        &self,
        id: ServerSettingsId,
        request: &UpdateServerSettingsRequest,
    ) -> Result<ServerSettingsResponse, ClientError> {
        self.core.update_server_settings(id, request).await
    }

    /// `DELETE /server-settings/{id}`: deletes a server's settings, after
    /// [`Client::step_up`] within 5 minutes.
    ///
    /// # Errors
    /// The server's 403 (`step_up_required`, or `forbidden`) or 404, as
    /// [`ClientError::Api`]; any other error as for every authenticated call.
    pub async fn delete_server_settings(&self, id: ServerSettingsId) -> Result<(), ClientError> {
        self.core.delete_server_settings(id).await
    }

    /// The live events socket of this client's session (P11.8; ADR-0020,
    /// U25, S53): a [`LiveEvents`] its caller drives with `recv`, which
    /// asks for a new ticket at every connect and reconnects by the
    /// socket's policy. Nothing happens until the first `recv`. Neither the
    /// ticket nor the access token ever leaves the client.
    #[must_use]
    pub fn live_events(&self) -> LiveEvents<S> {
        LiveEvents::new(Arc::clone(&self.core))
    }
}

impl<S> Clone for Client<S> {
    fn clone(&self) -> Self {
        Self {
            core: Arc::clone(&self.core),
        }
    }
}

/// The server's address only.
impl<S> fmt::Debug for Client<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("server", &self.core.server().as_str())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use crate::store::MemoryStore;

    use super::*;

    #[test]
    fn debug_shows_only_the_server() {
        let client = Client::new(
            ClientConfig {
                server: ServerUrl::parse("https://example.com").unwrap(),
                trust: Trust::System,
            },
            MemoryStore::new(),
        )
        .unwrap();

        assert_eq!(
            format!("{client:?}"),
            r#"Client { server: "https://example.com/", .. }"#
        );
        assert_eq!(client.clone().server().as_str(), "https://example.com/");
    }
}
