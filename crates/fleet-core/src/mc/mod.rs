//! The Minecraft ports: how the runtime drives a Minecraft session without
//! knowing azalea (Plan.md P2.10; ADR-0008, ADR-0010).
//!
//! - [`MinecraftConnector::connect`] starts a session with [`ConnectParams`]
//!   and returns a [`SessionHandle`] and the session's [`SessionEvents`].
//!   Failures to even start a session are a [`ConnectError`]; everything
//!   that happens on the network arrives as a [`SessionEvent`].
//! - The [`SessionHandle`] performs [`GameAction`](crate::mode::GameAction)s,
//!   sends chat, respawns, reads the session's [`Liveness`] and tears the
//!   session down. Its calls fail with a [`SessionError`].
//! - [`SessionCredentials`] carry the Minecraft access token as a secret.
//!
//! fleet-mc implements the ports with azalea, on one host thread per session
//! (ADR-0008 §2), and fleet-testkit with a scriptable fake. Every future they
//! return is `Send`, so a bot's actor can run on any tokio worker:
//!
//! ```
//! use fleet_core::mc::{
//!     ConnectParams, MinecraftConnector, SessionEvent, SessionEvents, SessionHandle,
//! };
//! use fleet_core::mode::GameAction;
//!
//! /// Runs one session until it ends.
//! async fn run_once<C: MinecraftConnector>(connector: &C, params: ConnectParams) {
//!     let Ok((session, mut events)) = connector.connect(params).await else {
//!         return;
//!     };
//!     while let Some(event) = events.next().await {
//!         match event {
//!             SessionEvent::Joined => {
//!                 let _ = session.perform(GameAction::SwingArm).await;
//!             }
//!             SessionEvent::Died => {
//!                 let _ = session.respawn().await;
//!             }
//!             SessionEvent::Chat(_) => {}
//!             SessionEvent::Disconnected(_) | SessionEvent::ConnectionFailed(_) => break,
//!         }
//!     }
//!     let _stamps = session.liveness();
//!     session.disconnect().await;
//! }
//!
//! fn assert_send<F: Send>(_: &F) {}
//!
//! fn run_once_is_send<C: MinecraftConnector>(connector: &C, params: ConnectParams) {
//!     assert_send(&run_once(connector, params));
//! }
//! ```

mod connector;
mod credentials;
mod event;
mod liveness;
mod session;

pub use connector::{ConnectError, ConnectParams, MinecraftConnector};
pub use credentials::SessionCredentials;
pub use event::{SessionEvent, SessionEvents};
pub use liveness::Liveness;
pub use session::{SessionError, SessionHandle};
