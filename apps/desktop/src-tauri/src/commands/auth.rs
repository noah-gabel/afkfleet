//! Logging in and out, registering, and stepping up.

use fleet_api_types::SecretText;
use fleet_api_types::app::{CommandError, LoginStepResponse, LogoutResponse};
use fleet_api_types::auth::{LoginRequest, RegisterRequest, RegisterResponse, StepUpRequest};
use fleet_api_types::mfa::CODE_MAX_CHARS;

use super::Managed;

/// The password step.
#[tauri::command]
pub(crate) async fn login(
    state: Managed<'_>,
    request: LoginRequest,
) -> Result<LoginStepResponse, CommandError> {
    state.login(request).await
}

/// The code step, which starts the session.
#[tauri::command]
pub(crate) async fn login_mfa(
    state: Managed<'_>,
    code: SecretText<CODE_MAX_CHARS>,
) -> Result<(), CommandError> {
    state.login_mfa(code).await
}

/// Logs out; it always ends the session in the app.
#[tauri::command]
pub(crate) async fn logout(state: Managed<'_>) -> Result<LogoutResponse, CommandError> {
    Ok(state.logout().await)
}

/// An account from an invite.
#[tauri::command]
pub(crate) async fn register(
    state: Managed<'_>,
    request: RegisterRequest,
) -> Result<RegisterResponse, CommandError> {
    state.register(request).await
}

/// The password again, for step-up actions.
#[tauri::command]
pub(crate) async fn step_up(
    state: Managed<'_>,
    request: StepUpRequest,
) -> Result<(), CommandError> {
    state.step_up(request).await
}
