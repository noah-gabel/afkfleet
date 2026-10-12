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

use fleet_api_types::auth::{LoginRequest, RegisterRequest, RegisterResponse, StepUpRequest};
use fleet_api_types::invites::{CreateInviteRequest, CreatedInviteResponse, InvitePage};
use fleet_api_types::me::{MeResponse, PasswordChangeRequest};
use fleet_api_types::mfa::{CODE_MAX_CHARS, MfaConfirmResponse, MfaEnrollResponse};
use fleet_api_types::sessions::SessionPage;
use fleet_api_types::users::{UserPage, UserPatchRequest, UserResponse};
use fleet_api_types::{InviteId, PageQuery, SecretText, SessionId, UserId};

use crate::error::{BuildError, ClientError};
use crate::session::Core;
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

    /// `GET /users`: a page of the users (Admins and the Owner).
    ///
    /// # Errors
    /// The server's 403 for a Member, or 422; any other error as for every
    /// authenticated call.
    pub async fn users(&self, query: &PageQuery<UserId>) -> Result<UserPage, ClientError> {
        self.core.users(query).await
    }

    /// `PATCH /users/{id}`: one change of a user, a role or `disabled`.
    ///
    /// # Errors
    /// The server's 403, 404 or 422 as [`ClientError::Api`]; any other error
    /// as for every authenticated call.
    pub async fn patch_user(
        &self,
        id: UserId,
        request: &UserPatchRequest,
    ) -> Result<UserResponse, ClientError> {
        self.core.patch_user(id, request).await
    }

    /// `POST /invites`: a new invite, whose code is answered this once.
    ///
    /// # Errors
    /// The server's 403, 404 or 409 (too many pending invites) as
    /// [`ClientError::Api`]; any other error as for every authenticated
    /// call.
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

    /// `DELETE /invites/{id}`: revokes a pending invite.
    ///
    /// # Errors
    /// The server's 409 for a used invite, or 403 or 404, as
    /// [`ClientError::Api`]; any other error as for every authenticated
    /// call.
    pub async fn revoke_invite(&self, id: InviteId) -> Result<(), ClientError> {
        self.core.revoke_invite(id).await
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
