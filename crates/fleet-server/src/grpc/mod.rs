//! The agents' gRPC port (Plan.md §4, P10.3; ADR-0019, S13, S14): one
//! listener on `[grpc] bind`, only while `[grpc]` is configured.
//!
//! - [`listener`]: TLS with the server certificate of `pki init`, verifying
//!   any client certificate against the CA while not requiring one, the
//!   connection limits, and serving until the shutdown.
//! - [`enrollment`]: `AgentEnrollment.Enroll`, behind its gate (the request
//!   ID, the peer's address and its rate limit), the one service of Phase
//!   10's group B.
//!
//! Group C adds `AgentControl` behind a gate of its own that refuses a call
//! without a verified client certificate.

pub mod enrollment;
pub mod listener;
