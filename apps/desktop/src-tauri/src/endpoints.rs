//! One method of [`AppState`] per API endpoint (Plan.md P8.4, ADR-0017): what
//! each command does, without Tauri around it, so the integration tests call
//! them against a real server.
//!
//! Each method validates its input at this boundary (garde's rules on the
//! request DTOs, a code's length, a page's range), then calls fleet-client
//! once under the connection's shared guard and maps its error. No call is
//! wrapped in a timeout or a `select!`: the client bounds every request, and
//! a cancelled `mfa_confirm` could lose its one-time recovery codes.

use core::time::Duration;

use fleet_api_types::app::{
    CommandError, LoginStepKind, LoginStepResponse, LogoutResponse, SetupStepResponse,
};
use fleet_api_types::auth::{LoginRequest, RegisterRequest, RegisterResponse, StepUpRequest};
use fleet_api_types::invites::{CreateInviteRequest, CreatedInviteResponse, InvitePage};
use fleet_api_types::me::{MeResponse, PasswordChangeRequest};
use fleet_api_types::mfa::{CODE_MAX_CHARS, MfaConfirmResponse, MfaEnrollResponse};
use fleet_api_types::sessions::SessionPage;
use fleet_api_types::users::{UserPage, UserPatchRequest, UserResponse};
use fleet_api_types::{InviteId, PageQuery, SecretText, SessionId, UserId};
use fleet_client::LoginStep;
use garde::Validate;

use crate::error;
use crate::keychain::StoreFactory;
use crate::state::{AppState, logout_response};

/// A code's message when it's empty or longer than [`CODE_MAX_CHARS`].
const CODE_RULE: &str = "length must be between 1 and 64";

impl<F: StoreFactory> AppState<F> {
    /// The `login` command: the password step.
    ///
    /// # Errors
    /// `invalid_input`, `no_server`, `already_logged_in`, or the server's
    /// refusal (401 `invalid_credentials`, 429) as `api`.
    pub async fn login(&self, request: LoginRequest) -> Result<LoginStepResponse, CommandError> {
        validate(&request)?;
        let conn = self.conn.read().await;
        let step = conn
            .client()?
            .login(&request)
            .await
            .map_err(error::client)?;
        Ok(match step {
            LoginStep::MfaRequired { expires_in } => LoginStepResponse {
                step: LoginStepKind::MfaRequired,
                expires_in: seconds(expires_in),
            },
            LoginStep::SetupRequired { expires_in } => LoginStepResponse {
                step: LoginStepKind::SetupRequired,
                expires_in: seconds(expires_in),
            },
        })
    }

    /// The `login_mfa` command: the code step, which starts the session.
    ///
    /// # Errors
    /// `invalid_input`, `no_server`, `no_pending_step`, or a wrong code as
    /// `api` 401 `invalid_credentials` (the step stays for another try).
    pub async fn login_mfa(&self, code: SecretText<CODE_MAX_CHARS>) -> Result<(), CommandError> {
        check_code(&code)?;
        let conn = self.conn.read().await;
        conn.client()?.login_mfa(code).await.map_err(error::client)
    }

    /// The `logout` command. The session always ends in the app; the answer
    /// says whether the server confirmed it. Without a server there's no
    /// session, which is `ended`.
    pub async fn logout(&self) -> LogoutResponse {
        let conn = self.conn.read().await;
        match conn.client() {
            Ok(client) => logout_response(client.logout().await),
            Err(_) => LogoutResponse::Ended,
        }
    }

    /// The `register` command: an account from an invite. It holds no
    /// session: log in next.
    ///
    /// # Errors
    /// `invalid_input`, `no_server`, or the server's refusal as `api`.
    pub async fn register(
        &self,
        request: RegisterRequest,
    ) -> Result<RegisterResponse, CommandError> {
        validate(&request)?;
        let conn = self.conn.read().await;
        conn.client()?
            .register(&request)
            .await
            .map_err(error::client)
    }

    /// The `step_up` command: the password again, for step-up actions.
    ///
    /// # Errors
    /// `invalid_input`, `no_server`, `no_session`, or a wrong password as
    /// `api` 403 `forbidden`.
    pub async fn step_up(&self, request: StepUpRequest) -> Result<(), CommandError> {
        validate(&request)?;
        let conn = self.conn.read().await;
        conn.client()?
            .step_up(&request)
            .await
            .map_err(error::client)
    }

    /// The `mfa_enroll` command: a new TOTP secret for the pending setup
    /// step.
    ///
    /// # Errors
    /// `no_server`, `no_pending_step`, or the server's 401 as `api` (the step
    /// is gone).
    pub async fn mfa_enroll(&self) -> Result<MfaEnrollResponse, CommandError> {
        let conn = self.conn.read().await;
        conn.client()?.mfa_enroll().await.map_err(error::client)
    }

    /// The `mfa_confirm` command: the authenticator's first code. It answers
    /// the 10 recovery codes, shown once.
    ///
    /// # Errors
    /// `invalid_input`, `no_server`, `no_pending_step`, or a wrong code as
    /// `api` 422.
    pub async fn mfa_confirm(
        &self,
        code: SecretText<CODE_MAX_CHARS>,
    ) -> Result<MfaConfirmResponse, CommandError> {
        check_code(&code)?;
        let conn = self.conn.read().await;
        conn.client()?
            .mfa_confirm(code)
            .await
            .map_err(error::client)
    }

    /// The `mfa_reset` command: starts replacing the authenticator, after
    /// `step_up`.
    ///
    /// # Errors
    /// `no_server`, `no_session`, or `api` 403 `step_up_required`.
    pub async fn mfa_reset(&self) -> Result<SetupStepResponse, CommandError> {
        let conn = self.conn.read().await;
        let expires_in = conn.client()?.mfa_reset().await.map_err(error::client)?;
        Ok(SetupStepResponse {
            expires_in: seconds(expires_in),
        })
    }

    /// The `me` command: the user's profile.
    ///
    /// # Errors
    /// `no_server`, `no_session`, or any other call error.
    pub async fn me(&self) -> Result<MeResponse, CommandError> {
        let conn = self.conn.read().await;
        conn.client()?.me().await.map_err(error::client)
    }

    /// The `change_password` command, after `step_up`.
    ///
    /// # Errors
    /// `invalid_input`, `no_server`, `no_session`, or `api` 403 or 422.
    pub async fn change_password(
        &self,
        request: PasswordChangeRequest,
    ) -> Result<(), CommandError> {
        validate(&request)?;
        let conn = self.conn.read().await;
        conn.client()?
            .change_password(&request)
            .await
            .map_err(error::client)
    }

    /// The `sessions` command: a page of the user's live sessions.
    ///
    /// # Errors
    /// `invalid_input` for a `limit` out of range, `no_server`, `no_session`.
    pub async fn sessions(
        &self,
        before: Option<SessionId>,
        limit: Option<u32>,
    ) -> Result<SessionPage, CommandError> {
        let query = page(before, limit)?;
        let conn = self.conn.read().await;
        conn.client()?.sessions(&query).await.map_err(error::client)
    }

    /// The `revoke_session` command: ends another session of the user.
    ///
    /// # Errors
    /// `no_server`, `no_session`, or `api` 409 for the current session, 404.
    pub async fn revoke_session(&self, id: SessionId) -> Result<(), CommandError> {
        let conn = self.conn.read().await;
        conn.client()?
            .revoke_session(id)
            .await
            .map_err(error::client)
    }

    /// The `users` command: a page of the users (Admins and the Owner).
    ///
    /// # Errors
    /// `invalid_input`, `no_server`, `no_session`, or `api` 403 for a Member.
    pub async fn users(
        &self,
        before: Option<UserId>,
        limit: Option<u32>,
    ) -> Result<UserPage, CommandError> {
        let query = page(before, limit)?;
        let conn = self.conn.read().await;
        conn.client()?.users(&query).await.map_err(error::client)
    }

    /// The `patch_user` command: one change of a user, after `step_up`
    /// within 5 minutes.
    ///
    /// # Errors
    /// `invalid_input` unless exactly one change, `no_server`, `no_session`,
    /// or `api` 403 (`step_up_required`, or `forbidden`), 404 or 422.
    pub async fn patch_user(
        &self,
        id: UserId,
        request: UserPatchRequest,
    ) -> Result<UserResponse, CommandError> {
        validate(&request)?;
        let conn = self.conn.read().await;
        conn.client()?
            .patch_user(id, &request)
            .await
            .map_err(error::client)
    }

    /// The `invites` command: a page of the invites.
    ///
    /// # Errors
    /// `invalid_input`, `no_server`, `no_session`, or `api` 403 for a Member.
    pub async fn invites(
        &self,
        before: Option<InviteId>,
        limit: Option<u32>,
    ) -> Result<InvitePage, CommandError> {
        let query = page(before, limit)?;
        let conn = self.conn.read().await;
        conn.client()?.invites(&query).await.map_err(error::client)
    }

    /// The `create_invite` command: a new invite, whose code is answered
    /// this once, after `step_up` within 5 minutes.
    ///
    /// # Errors
    /// `no_server`, `no_session`, or `api` 403 (`step_up_required`, or
    /// `forbidden`), 404 or 409.
    pub async fn create_invite(
        &self,
        request: CreateInviteRequest,
    ) -> Result<CreatedInviteResponse, CommandError> {
        validate(&request)?;
        let conn = self.conn.read().await;
        conn.client()?
            .create_invite(&request)
            .await
            .map_err(error::client)
    }

    /// The `revoke_invite` command: revokes a pending invite, after
    /// `step_up` within 5 minutes.
    ///
    /// # Errors
    /// `no_server`, `no_session`, or `api` 403 (`step_up_required`, or
    /// `forbidden`), 404 or 409.
    pub async fn revoke_invite(&self, id: InviteId) -> Result<(), CommandError> {
        let conn = self.conn.read().await;
        conn.client()?
            .revoke_invite(id)
            .await
            .map_err(error::client)
    }
}

/// `request` against its garde rules: `invalid_input` with every broken one.
fn validate<T: Validate<Context = ()>>(request: &T) -> Result<(), CommandError> {
    request.validate().map_err(|report| error::invalid(&report))
}

/// A code's length: 1 to [`CODE_MAX_CHARS`] characters. The server checks
/// its shape.
fn check_code(code: &SecretText<CODE_MAX_CHARS>) -> Result<(), CommandError> {
    let chars = code.expose().chars().count();
    if chars == 0 || chars > CODE_MAX_CHARS {
        return Err(error::invalid_field("code", CODE_RULE));
    }
    Ok(())
}

/// A list's page query, checked against its rules.
fn page<I>(before: Option<I>, limit: Option<u32>) -> Result<PageQuery<I>, CommandError> {
    let query = PageQuery { before, limit };
    validate(&query)?;
    Ok(query)
}

/// A lifetime in whole seconds, capped at `u32::MAX`.
fn seconds(lifetime: Duration) -> u32 {
    u32::try_from(lifetime.as_secs()).unwrap_or(u32::MAX)
}
