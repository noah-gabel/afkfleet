//! The agents' gRPC port (Plan.md §4, P10.3, P10.4; ADR-0019, S13, S14,
//! S22–S30): one listener on `[grpc] bind`, only while `[grpc]` is
//! configured.
//!
//! - [`listener`]: TLS with the server certificate of `pki init`, verifying
//!   any client certificate against the CA while not requiring one, each
//!   connection's age (lifted while an agent's stream lives), HTTP/2
//!   keepalive from `[grpc]`'s heartbeats, and serving until the shutdown.
//! - [`enrollment`]: `AgentEnrollment.Enroll`, behind its gate (the request
//!   ID, the peer's address and its rate limit).
//! - [`control`]: `AgentControl.Connect`, behind a gate that refuses a call
//!   without a client certificate first, then the stream's admission by
//!   that certificate, its hello and its session.
//! - [`registry`]: each agent's one live stream, ended when its permission
//!   ends, and what it may report (P10.5).
//! - [`reconciler`]: which agent runs which bot, kept true (P10.5).
//! - [`status`]: what every answer on the port shares.

pub mod control;
pub mod enrollment;
pub mod listener;
pub mod reconciler;
pub mod registry;
pub mod status;
