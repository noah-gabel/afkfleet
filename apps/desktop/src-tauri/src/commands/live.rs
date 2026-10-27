//! The live events (Plan.md P11.8; ADR-0020, U29, S59): one long-running
//! command whose updates come over a `Channel`.

use fleet_api_types::app::{CommandError, LiveUpdateMessage};
use tauri::ipc::Channel;

use super::Managed;

/// Forwards the session's live events to `updates` until the watch is
/// stopped (by a newer one, a server switch, or `updates` refusing an update,
/// after which it never delivers again: S64), or the session ends
/// (`no_session`), or the server's certificate is refused (`certificate`).
#[tauri::command]
pub(crate) async fn watch_events(
    state: Managed<'_>,
    updates: Channel<LiveUpdateMessage>,
) -> Result<(), CommandError> {
    state.watch_events(updates).await
}
