//! One method of [`AppState`] per API endpoint (Plan.md P8.4, ADR-0017): what
//! each command does, without Tauri around it, so the integration tests call
//! them against a real server.
//!
//! Each method validates its input at this boundary (garde's rules on the
//! request DTOs, a code's length, a page's range), then calls fleet-client
//! once under the connection's shared guard and maps its error. No call is
//! wrapped in a timeout or a `select!`: the client bounds every request, and
//! a cancelled `mfa_confirm` could lose its one-time recovery codes.
//!
//! A link or re-link start also checks its answer's link to Microsoft's
//! sign-in page with fleet-client's [`VerifiedLink`] before the answer
//! leaves Rust, and holds it for `open_verification_link` (ADR-0018).

use core::time::Duration;

use fleet_api_types::accounts::{
    AccountPage, AccountResponse, ChangeGrantRequest, CreateGrantRequest,
    CreateOfflineAccountRequest, CreatedOfflineAccountResponse, GrantPage, GrantResponse,
    LinkFlowResponse, LinkStartedResponse,
};
use fleet_api_types::app::{
    CommandError, CommandErrorCode, LoginStepKind, LoginStepResponse, LogoutResponse,
    SetupStepResponse,
};
use fleet_api_types::auth::{LoginRequest, RegisterRequest, RegisterResponse, StepUpRequest};
use fleet_api_types::invites::{CreateInviteRequest, CreatedInviteResponse, InvitePage};
use fleet_api_types::me::{MeResponse, PasswordChangeRequest};
use fleet_api_types::mfa::{CODE_MAX_CHARS, MfaConfirmResponse, MfaEnrollResponse};
use fleet_api_types::sessions::SessionPage;
use fleet_api_types::users::{UserPage, UserPatchRequest, UserResponse};
use fleet_api_types::{AccountId, InviteId, LinkFlowId, PageQuery, SecretText, SessionId, UserId};
use fleet_client::{ClientError, LoginStep, VerifiedLink};
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
        // A new sign-in: an earlier session's link flow is over.
        self.hold_link(None);
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
        self.hold_link(None);
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

    /// The `start_link` command: starts linking a Microsoft account. The
    /// answer's link must pass [`VerifiedLink`]'s rule; it's then held for
    /// `open_verification_link`, and the answer, with its user code, goes to
    /// the UI.
    ///
    /// # Errors
    /// `no_server`, `no_session`, `invalid_verification_link` (the answer,
    /// its code included, is dropped and no link is held), or `api` 409,
    /// 429 or 503 (a held link stays: the server keeps the waiting flow).
    pub async fn start_link(&self) -> Result<LinkStartedResponse, CommandError> {
        let conn = self.conn.read().await;
        let result = conn.client()?.start_link().await;
        // Under the shared guard, so a switch can't come between the answer
        // and the held link.
        self.held(result)
    }

    /// The `link_status` command: the state of the caller's link flow.
    ///
    /// # Errors
    /// `no_server`, `no_session`, or `api` 404.
    pub async fn link_status(&self, id: LinkFlowId) -> Result<LinkFlowResponse, CommandError> {
        let conn = self.conn.read().await;
        conn.client()?.link_status(id).await.map_err(error::client)
    }

    /// The `relink_account` command: starts linking an account again, as
    /// `start_link` does.
    ///
    /// # Errors
    /// As `start_link`, and `api` 403, 404 or 409 (a pending or offline
    /// account).
    pub async fn relink_account(&self, id: AccountId) -> Result<LinkStartedResponse, CommandError> {
        let conn = self.conn.read().await;
        let result = conn.client()?.relink_account(id).await;
        self.held(result)
    }

    /// The `create_offline_account` command: a new offline account, while
    /// the server allows them.
    ///
    /// # Errors
    /// `invalid_input`, `no_server`, `no_session`, or `api` 404 (offline
    /// accounts are off), 409 or 422.
    pub async fn create_offline_account(
        &self,
        request: CreateOfflineAccountRequest,
    ) -> Result<CreatedOfflineAccountResponse, CommandError> {
        validate(&request)?;
        let conn = self.conn.read().await;
        conn.client()?
            .create_offline_account(&request)
            .await
            .map_err(error::client)
    }

    /// The `accounts` command: a page of the accounts the caller may see.
    ///
    /// # Errors
    /// `invalid_input` for a limit out of range, `no_server`, `no_session`.
    pub async fn accounts(
        &self,
        before: Option<AccountId>,
        limit: Option<u32>,
    ) -> Result<AccountPage, CommandError> {
        let query = page(before, limit)?;
        let conn = self.conn.read().await;
        conn.client()?.accounts(&query).await.map_err(error::client)
    }

    /// The `accounts_awaiting_approval` command: a page of the pending links
    /// the caller may approve or refuse.
    ///
    /// # Errors
    /// As `accounts`.
    pub async fn accounts_awaiting_approval(
        &self,
        before: Option<AccountId>,
        limit: Option<u32>,
    ) -> Result<AccountPage, CommandError> {
        let query = page(before, limit)?;
        let conn = self.conn.read().await;
        conn.client()?
            .accounts_awaiting_approval(&query)
            .await
            .map_err(error::client)
    }

    /// The `account` command: one account, as the caller sees it.
    ///
    /// # Errors
    /// `no_server`, `no_session`, or `api` 404.
    pub async fn account(&self, id: AccountId) -> Result<AccountResponse, CommandError> {
        let conn = self.conn.read().await;
        conn.client()?.account(id).await.map_err(error::client)
    }

    /// The `delete_account` command: deletes an account, after `step_up`
    /// within 5 minutes.
    ///
    /// # Errors
    /// `no_server`, `no_session`, or `api` 403 (`step_up_required`, or
    /// `forbidden`) or 404.
    pub async fn delete_account(&self, id: AccountId) -> Result<(), CommandError> {
        let conn = self.conn.read().await;
        conn.client()?
            .delete_account(id)
            .await
            .map_err(error::client)
    }

    /// The `approve_account` command: approves a pending link, after
    /// `step_up` within 5 minutes.
    ///
    /// # Errors
    /// `no_server`, `no_session`, or `api` 403 (`step_up_required`, or
    /// `forbidden`), 404 or 409.
    pub async fn approve_account(&self, id: AccountId) -> Result<AccountResponse, CommandError> {
        let conn = self.conn.read().await;
        conn.client()?
            .approve_account(id)
            .await
            .map_err(error::client)
    }

    /// The `refuse_account` command: refuses a pending link, which deletes
    /// it, after `step_up` within 5 minutes.
    ///
    /// # Errors
    /// As `approve_account`.
    pub async fn refuse_account(&self, id: AccountId) -> Result<(), CommandError> {
        let conn = self.conn.read().await;
        conn.client()?
            .refuse_account(id)
            .await
            .map_err(error::client)
    }

    /// The `account_grants` command: a page of who else has access.
    ///
    /// # Errors
    /// `invalid_input` for a limit out of range, `no_server`, `no_session`,
    /// or `api` 403 or 404.
    pub async fn account_grants(
        &self,
        id: AccountId,
        before: Option<UserId>,
        limit: Option<u32>,
    ) -> Result<GrantPage, CommandError> {
        let query = page(before, limit)?;
        let conn = self.conn.read().await;
        conn.client()?
            .account_grants(id, &query)
            .await
            .map_err(error::client)
    }

    /// The `create_grant` command: gives a user access by their exact
    /// username, after `step_up` within 5 minutes.
    ///
    /// # Errors
    /// `invalid_input`, `no_server`, `no_session`, or `api` 403, 404, 409,
    /// 422 or 429.
    pub async fn create_grant(
        &self,
        id: AccountId,
        request: CreateGrantRequest,
    ) -> Result<GrantResponse, CommandError> {
        validate(&request)?;
        let conn = self.conn.read().await;
        conn.client()?
            .create_grant(id, &request)
            .await
            .map_err(error::client)
    }

    /// The `change_grant` command: changes a grant's level, after `step_up`
    /// within 5 minutes.
    ///
    /// # Errors
    /// `no_server`, `no_session`, or `api` 403, 404 or 409.
    pub async fn change_grant(
        &self,
        id: AccountId,
        user: UserId,
        request: ChangeGrantRequest,
    ) -> Result<GrantResponse, CommandError> {
        validate(&request)?;
        let conn = self.conn.read().await;
        conn.client()?
            .change_grant(id, user, &request)
            .await
            .map_err(error::client)
    }

    /// The `revoke_grant` command: revokes a grant, after `step_up` within 5
    /// minutes.
    ///
    /// # Errors
    /// `no_server`, `no_session`, or `api` 403 or 404.
    pub async fn revoke_grant(&self, id: AccountId, user: UserId) -> Result<(), CommandError> {
        let conn = self.conn.read().await;
        conn.client()?
            .revoke_grant(id, user)
            .await
            .map_err(error::client)
    }

    /// A link or re-link start's outcome: a good link is held and the
    /// answer returned; a bad one drops the answer, its code included, and
    /// forgets the held link; a refusal leaves the held link as it was.
    pub(crate) fn held(
        &self,
        result: Result<LinkStartedResponse, ClientError>,
    ) -> Result<LinkStartedResponse, CommandError> {
        let answer = result.map_err(error::client)?;
        if let Ok(link) = VerifiedLink::try_from(answer.verification_uri.as_str()) {
            self.hold_link(Some(link));
            Ok(answer)
        } else {
            // That start replaced any waiting flow, so the held link is stale
            // too; the answer and its code go here.
            self.hold_link(None);
            Err(CommandError::new(CommandErrorCode::InvalidVerificationLink))
        }
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use fleet_api_types::accounts::LinkStartedResponse;
    use fleet_api_types::app::{CommandErrorCode, ServerProfileRequest, ServerProfileResponse};
    use fleet_api_types::auth::LoginRequest;
    use fleet_api_types::{BoundedText, ErrorCode, SecretText};
    use fleet_client::{ApiFailure, ClientError, MemoryStore, ServerUrl, StoreError};
    use tempfile::TempDir;
    use tokio::net::TcpListener;

    use super::Duration;
    use crate::keychain::StoreFactory;
    use crate::profile::PROFILE_FILE;
    use crate::state::{AppState, LinkOpener, OpenError};

    const GOOD: &str = "https://www.microsoft.com/link";

    /// One memory store per call: these tests never log in.
    struct Memory;

    impl StoreFactory for Memory {
        type Store = MemoryStore;

        fn for_origin(&self, _origin: &ServerUrl) -> Result<MemoryStore, StoreError> {
            Ok(MemoryStore::new())
        }
    }

    /// Records the links it opens, or refuses each one.
    #[derive(Default)]
    struct Recording {
        opened: Mutex<Vec<String>>,
        refuse: bool,
    }

    impl Recording {
        fn opened(&self) -> Vec<String> {
            self.opened.lock().unwrap().clone()
        }
    }

    impl LinkOpener for Recording {
        fn open(&self, link: &fleet_client::VerifiedLink) -> Result<(), OpenError> {
            if self.refuse {
                return Err(OpenError);
            }
            self.opened.lock().unwrap().push(link.as_str().to_owned());
            Ok(())
        }
    }

    /// A state without a server, over `opener`, and its folder.
    fn state(opener: &Arc<Recording>) -> (AppState<Memory>, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let opener: Arc<dyn LinkOpener> = Arc::clone(opener) as Arc<dyn LinkOpener>;
        let state = AppState::load(dir.path().join(PROFILE_FILE), Memory, opener);
        (state, dir)
    }

    /// A start's answer with the link `link`.
    fn started(link: &str) -> LinkStartedResponse {
        LinkStartedResponse {
            flow_id: "01900000-0000-7000-8000-00000000000b".parse().unwrap(),
            user_code: SecretText::new("B7X4-KDLQ".to_owned()),
            verification_uri: BoundedText::new(link),
            expires_at: serde_json::from_value(serde_json::json!("2026-10-15T10:15:00.000Z"))
                .unwrap(),
        }
    }

    /// The server's refusal of a start: the user's flow is finishing.
    fn refused() -> ClientError {
        ClientError::Api(ApiFailure {
            status: 429,
            code: ErrorCode::RateLimited.into(),
            message: BoundedText::new(ErrorCode::RateLimited.message()),
            request_id: BoundedText::new("r-1"),
            fields: Vec::new(),
            retry_after: Some(Duration::from_secs(5)),
        })
    }

    /// `state` holding [`GOOD`], as after a good start.
    fn holding_good(state: &AppState<Memory>) {
        state.held(Ok(started(GOOD))).unwrap();
    }

    #[test]
    fn a_good_link_is_held_its_answer_returned_and_opened_on_request() {
        let opener = Arc::new(Recording::default());
        let (state, _dir) = state(&opener);

        let answer = state.held(Ok(started(GOOD))).unwrap();
        state.open_verification_link().unwrap();
        state.open_verification_link().unwrap();

        assert_eq!(answer.user_code.expose(), "B7X4-KDLQ");
        assert_eq!(answer.verification_uri.as_str(), GOOD);
        assert_eq!(opener.opened(), [GOOD, GOOD]);
    }

    #[test]
    fn a_bad_link_drops_the_answer_and_forgets_the_held_link() {
        let opener = Arc::new(Recording::default());
        let (state, _dir) = state(&opener);
        holding_good(&state);

        let refused = state
            .held(Ok(started("https://evil.example.com/link")))
            .unwrap_err();
        let open = state.open_verification_link().unwrap_err();

        assert_eq!(refused.code, CommandErrorCode::InvalidVerificationLink);
        assert_eq!(refused.api, None);
        assert_eq!(refused.fields, None);
        assert_eq!(open.code, CommandErrorCode::NoVerificationLink);
        assert_eq!(opener.opened(), Vec::<String>::new());
    }

    #[test]
    fn a_refused_start_keeps_the_held_link() {
        let opener = Arc::new(Recording::default());
        let (state, _dir) = state(&opener);
        holding_good(&state);

        let error = state.held(Err(refused())).unwrap_err();
        state.open_verification_link().unwrap();

        assert_eq!(error.code, CommandErrorCode::Api);
        assert_eq!(error.api.unwrap().retry_after_secs, Some(5));
        assert_eq!(opener.opened(), [GOOD]);
    }

    #[test]
    fn with_nothing_held_there_is_nothing_to_open() {
        let opener = Arc::new(Recording::default());
        let (state, _dir) = state(&opener);

        let error = state.open_verification_link().unwrap_err();

        assert_eq!(error.code, CommandErrorCode::NoVerificationLink);
        assert_eq!(opener.opened(), Vec::<String>::new());
    }

    #[test]
    fn a_link_the_system_refuses_to_open_is_verification_link_open() {
        let opener = Arc::new(Recording {
            refuse: true,
            ..Recording::default()
        });
        let (state, _dir) = state(&opener);
        holding_good(&state);

        let error = state.open_verification_link().unwrap_err();

        assert_eq!(error.code, CommandErrorCode::VerificationLinkOpen);
        assert_eq!(error.api, None);
    }

    #[tokio::test]
    async fn a_login_and_a_logout_forget_the_held_link() {
        let opener = Arc::new(Recording::default());
        let (state, _dir) = state(&opener);
        let request = LoginRequest {
            username: SecretText::new("afkbot1".to_owned()),
            password: SecretText::new("placeholder password".to_owned()),
        };

        holding_good(&state);
        let login = state.login(request).await.unwrap_err();
        let after_login = state.open_verification_link().unwrap_err();
        holding_good(&state);
        state.logout().await;
        let after_logout = state.open_verification_link().unwrap_err();

        assert_eq!(login.code, CommandErrorCode::NoServer);
        assert_eq!(after_login.code, CommandErrorCode::NoVerificationLink);
        assert_eq!(after_logout.code, CommandErrorCode::NoVerificationLink);
        assert_eq!(opener.opened(), Vec::<String>::new());
    }

    /// Answers one request on `listener` with an empty 200, as the server's
    /// `/health/live` does; that's the only request a first switch sends.
    async fn answer_one_health_check(listener: TcpListener) {
        let (socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|end| end == b"\r\n\r\n") {
            socket.readable().await.unwrap();
            let mut chunk = [0_u8; 1024];
            match socket.try_read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => request.extend_from_slice(chunk.get(..read).unwrap()),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("{error}"),
            }
        }
        assert!(request.starts_with(b"GET /health/live "));
        let mut answer: &[u8] =
            b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
        while !answer.is_empty() {
            socket.writable().await.unwrap();
            match socket.try_write(answer) {
                Ok(written) => answer = answer.get(written..).unwrap(),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("{error}"),
            }
        }
    }

    #[tokio::test]
    async fn a_switch_forgets_the_held_link() {
        let opener = Arc::new(Recording::default());
        let (state, _dir) = state(&opener);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        holding_good(&state);

        let (switched, ()) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(
                state.set_server_profile(ServerProfileRequest { url, ca_pem: None }),
                answer_one_health_check(listener)
            )
        })
        .await
        .unwrap();
        let open = state.open_verification_link().unwrap_err();

        assert!(
            matches!(switched, Ok(ServerProfileResponse::Switched { .. })),
            "{switched:?}"
        );
        assert_eq!(open.code, CommandErrorCode::NoVerificationLink);
        assert_eq!(opener.opened(), Vec::<String>::new());
    }
}
