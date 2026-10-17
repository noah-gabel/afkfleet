//! The agent's managed mode (Plan.md P10.6, P10.7; ADR-0019): what the
//! server's control stream drives.
//!
//! - [`credentials`]: the session credentials the server grants, behind
//!   fleet-core's `SessionCredentialProvider` (P10.6), over a
//!   [`SessionLink`](credentials::SessionLink) to the stream.
//!
//! The stream client itself, its reconnects and the events it forwards come
//! with P10.7, which implements the link and wires the provider into the
//! fleet.

pub mod credentials;
