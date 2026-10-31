//! The agents Admins and the Owner list, disable and release (Plan.md P11.9;
//! ADR-0020, U77).

use fleet_api_types::agents::{AgentPage, AgentResponse};
use fleet_api_types::app::CommandError;
use fleet_api_types::ids::AgentId;

use super::Managed;

/// A page of the agents.
#[tauri::command]
pub(crate) async fn agents(
    state: Managed<'_>,
    before: Option<AgentId>,
    limit: Option<u32>,
) -> Result<AgentPage, CommandError> {
    state.agents(before, limit).await
}

/// Disables an agent for good, after a step-up.
#[tauri::command]
pub(crate) async fn disable_agent(
    state: Managed<'_>,
    id: AgentId,
) -> Result<AgentResponse, CommandError> {
    state.disable_agent(id).await
}

/// Releases a disabled agent's bots, after a step-up.
#[tauri::command]
pub(crate) async fn release_agent(
    state: Managed<'_>,
    id: AgentId,
) -> Result<AgentResponse, CommandError> {
    state.release_agent(id).await
}
