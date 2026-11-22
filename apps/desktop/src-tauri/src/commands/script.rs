//! The mode editor's script commands (Plan.md P15.25, P15.27; ADR-0045, ADR-0046): the check of a
//! scripted mode's text, and a step mode's conversion to a script. With the window's theme, the
//! commands that aren't about the server. The check compiles the text with the engine the server
//! and the agent run, never runs it, and stores nothing ([`crate::script_check`]); the conversion
//! writes a script from steps and stores nothing either ([`crate::script_convert`]).

use fleet_api_types::app::{
    CommandError, ScriptCheck, ScriptCheckRequest, ScriptConversion, ScriptConvertRequest,
};

/// `check_script`: the first problem the server's check at save would find in a script's text.
#[tauri::command]
pub(crate) async fn check_script(request: ScriptCheckRequest) -> Result<ScriptCheck, CommandError> {
    crate::script_check::check_script(request).await
}

/// `convert_to_script`: a step mode's steps as the script that does what they do, with what it does
/// differently. Async, so it runs off the window's thread like every other command.
#[tauri::command]
pub(crate) async fn convert_to_script(
    request: ScriptConvertRequest,
) -> Result<ScriptConversion, CommandError> {
    crate::script_convert::convert(&request)
}
