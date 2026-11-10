//! The agent's managed mode (Plan.md P10.6, P10.7; ADR-0019): what the
//! server's control stream drives.
//!
//! - [`credentials`]: the session credentials the server grants, behind
//!   fleet-core's `SessionCredentialProvider` (P10.6), over a
//!   [`SessionLink`](credentials::SessionLink) to the stream.
//!
//! - [`identity`], [`link`], [`conductor`] and [`outbound`]: the identity
//!   the agent connects with, the stream client and its reconnects, what
//!   the server's instructions do to the fleet, and what the agent sends
//!   (P10.7).
//! - [`renewal`]: the agent renews its own certificate at two thirds of its
//!   lifetime, over the stream's connection (P13.6; ADR-0027).

pub mod conductor;
pub mod credentials;
pub mod identity;
pub mod link;
pub mod outbound;
pub mod renewal;
