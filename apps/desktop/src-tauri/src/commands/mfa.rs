//! Setting up and replacing the authenticator.

use fleet_api_types::SecretText;
use fleet_api_types::app::{CommandError, SetupStepResponse};
use fleet_api_types::mfa::{CODE_MAX_CHARS, MfaConfirmResponse, MfaEnrollResponse};

use super::Managed;

/// A new TOTP secret for the pending setup step.
#[tauri::command]
pub(crate) async fn mfa_enroll(state: Managed<'_>) -> Result<MfaEnrollResponse, CommandError> {
    state.mfa_enroll().await
}

/// The authenticator's first code; it answers the recovery codes, once.
#[tauri::command]
pub(crate) async fn mfa_confirm(
    state: Managed<'_>,
    code: SecretText<CODE_MAX_CHARS>,
) -> Result<MfaConfirmResponse, CommandError> {
    state.mfa_confirm(code).await
}

/// Starts replacing the authenticator, after a step-up.
#[tauri::command]
pub(crate) async fn mfa_reset(state: Managed<'_>) -> Result<SetupStepResponse, CommandError> {
    state.mfa_reset().await
}
