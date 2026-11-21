//! The editor's check of a scripted mode's text (Plan.md P15.25; ADR-0045): the one command
//! besides the window's theme that isn't about the server. It compiles the text with the
//! engine the server and the agent run, never runs it, and stores nothing ([`crate::script_check`]).

use fleet_api_types::app::{CommandError, ScriptCheck, ScriptCheckRequest};

/// `check_script`: the first problem the server's check at save would find in a script's text.
#[tauri::command]
pub(crate) async fn check_script(request: ScriptCheckRequest) -> Result<ScriptCheck, CommandError> {
    crate::script_check::check_script(request).await
}
