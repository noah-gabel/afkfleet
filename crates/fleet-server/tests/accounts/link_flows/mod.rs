//! The link flows (Plan.md P9.4, ADR-0018), over a fresh database from
//! `#[sqlx::test]`, the Microsoft fake, the pacing fake and a manual clock:
//!
//! - [`start`]: what a start answers and records, and its failures.
//! - [`caps`]: the daily cap, the server-wide cap and the Member quota.
//! - [`replace`]: one flow per user: a new start replaces a waiting one.
//! - [`poll`]: the poller's waits and Microsoft's answers.
//! - [`finish`]: the Minecraft session after the approval, and its retries.
//! - [`commit`]: the stored account, its audit entries, and the re-checks of
//!   the creator and their session.
//! - [`retention`]: how long a result stays readable.
//! - [`shutdown`]: stopping every flow at once.
//! - [`redaction`]: no code, token, name or UUID in any log or `Debug`.
//! - [`relink`]: linking an account again (P9.6).
//! - [`http`]: the routes, through the real router and serve's wiring.
//!
//! Time isn't paused: sqlx's worker threads would make paused timeouts fire
//! early (CLAUDE.md). The pacing fake moves the manual clock by each wait,
//! and every call is bounded.

mod caps;
mod commit;
mod finish;
mod http;
mod poll;
mod redaction;
mod relink;
mod replace;
mod retention;
mod shutdown;
mod start;

use fleet_core::id::LinkFlowId;
use fleet_server::app::users::ActingUser;
use fleet_server::ports::msauth::{MinecraftSession, PollOutcome};

use crate::support::link::LinkSetup;
use crate::support::msauth::{Script, session, tokens};
use crate::support::service::{DAY, PROFILE, T0, at};

/// Starts a flow for `by` whose poller waits forever, so the clock stays put
/// and no poll is needed. It returns once the poller is in that wait: a flow
/// replaced before its first wait would leave the hold to the next flow.
async fn held_start(setup: &LinkSetup, by: &ActingUser, code: &str) -> LinkFlowId {
    let mut wait = setup.pace.hold_next();
    let flow = setup.started(by, code).await;
    wait.entered().await;
    flow
}

/// Microsoft's approval with the tokens `access-<n>` and `refresh-<n>`, the
/// access token living an hour.
fn approval(n: u8) -> PollOutcome {
    PollOutcome::Authorized(tokens(
        &format!("access-{n}"),
        &format!("refresh-{n}"),
        at(T0 + 3_600_000),
    ))
}

/// A session of [`PROFILE`] named `AfkBot1`.
fn profile_session() -> MinecraftSession {
    session("minecraft-1", PROFILE, "AfkBot1", at(T0 + DAY))
}

/// Scripts the next poll to approve, and the session to succeed.
fn script_link(setup: &LinkSetup, n: u8) {
    setup.fake.script_poll(Script::Answer(Ok(approval(n))));
    setup
        .fake
        .script_session(Script::Answer(Ok(profile_session())));
}
