//! A scriptable fake for the Minecraft ports (`fleet_core::mc`; Plan.md P3.1,
//! ADR-0011).
//!
//! - [`FakeConnector`] implements [`MinecraftConnector`]. It returns scripted
//!   connect results and records every [`ConnectParams`] it's given.
//! - Each session it starts has a [`FakeSession`] (the [`SessionHandle`] the
//!   code under test holds), its [`FakeEvents`], and a [`SessionController`]
//!   for the test.
//! - Through the controller, a test emits events, freezes the liveness stamps,
//!   makes the session hang or its actions fail, and reads a log of what the
//!   code under test did.
//!
//! The fake keeps the port contract of ADR-0010, so code tested against it
//! can rely on the same guarantees as against fleet-mc:
//! - after a terminal event (`Disconnected` or `ConnectionFailed`), or once
//!   the session is torn down, [`SessionEvents::next`] returns `None`;
//! - `Died` comes at most once until [`SessionHandle::respawn`];
//! - when the bounded event queue is full, only `Chat` is dropped, and it's
//!   counted.
//!
//! Liveness stamps follow tokio's clock (`tokio::time::Instant::now().into_std()`),
//! so with `#[tokio::test(start_paused = true)]` a test moves them with
//! `tokio::time::advance` and stops them with
//! [`freeze_ticks`](SessionController::freeze_ticks) or
//! [`freeze_packets`](SessionController::freeze_packets).
//!
//! ```
//! use fleet_core::mc::{MinecraftConnector, SessionEvent, SessionEvents, SessionHandle};
//! use fleet_core::mode::GameAction;
//! use fleet_testkit::mc::{FakeConnector, Performed};
//! # use core::time::Duration;
//! # use fleet_core::mc::{ConnectParams, SessionCredentials};
//! # fn params() -> ConnectParams {
//! #     ConnectParams {
//! #         bot_id: "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap(),
//! #         server: "localhost".try_into().unwrap(),
//! #         credentials: SessionCredentials::Offline { username: "AfkBot1".try_into().unwrap() },
//! #         connect_timeout: Duration::from_secs(30),
//! #     }
//! # }
//! # tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap().block_on(async {
//!
//! let connector = FakeConnector::new();
//! let (session, mut events) = connector.connect(params()).await.unwrap();
//! let controller = connector.session(0).await;
//!
//! controller.emit(SessionEvent::Joined);
//! assert_eq!(events.next().await, Some(SessionEvent::Joined));
//!
//! session.perform(GameAction::Jump).await.unwrap();
//! assert_eq!(controller.log(), [Performed::Action(GameAction::Jump)]);
//! # });
//! ```
//!
//! [`MinecraftConnector`]: fleet_core::mc::MinecraftConnector
//! [`ConnectParams`]: fleet_core::mc::ConnectParams
//! [`SessionHandle`]: fleet_core::mc::SessionHandle
//! [`SessionHandle::respawn`]: fleet_core::mc::SessionHandle::respawn
//! [`SessionEvents::next`]: fleet_core::mc::SessionEvents::next

mod connector;
mod session;

pub use connector::FakeConnector;
pub use session::{EmitOutcome, FakeEvents, FakeSession, Performed, SessionController};
