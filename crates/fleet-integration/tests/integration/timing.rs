//! The timing contracts between the agent and the server, checked over their
//! constants (ADR-0020, S1; PR #59's review).

use core::time::Duration;

use fleet_runtime::RuntimeConfig;
use fleet_server::app::session_tokens::WAIT_BOUND;

/// What a grant adds around the session-token service's wait: its reads and
/// its fenced write, each bounded by a database acquire of 5 s, and the
/// stream's hop.
const GRANT_SLACK: Duration = Duration::from_secs(20);

/// U24 retries a refused login's session fresh, and a fresh request takes
/// only a session minted after it arrived. An agent that gave up before the
/// server's own bound would ask again, never take the session the server
/// had just minted, and spend its bot's fresh limit while Microsoft is slow,
/// until the bot failed. So the agent waits longer than the server may take.
#[test]
fn the_agent_waits_for_a_session_longer_than_the_server_may_take() {
    let agent = RuntimeConfig::default().session_request_timeout;

    assert!(
        agent >= WAIT_BOUND + GRANT_SLACK,
        "the agent waits {agent:?}; the server may take {WAIT_BOUND:?} and {GRANT_SLACK:?} more"
    );
}
