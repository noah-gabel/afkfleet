//! The agents' use cases (Plan.md P10.4; ADR-0019).
//!
//! - [`connections`]: admitting the certificate a connecting agent presents,
//!   recording its accepted hello, and recording how its stream ended
//!   (P10.4), behind the [`AgentConnections`](crate::ports::agent_connections::AgentConnections)
//!   port the gRPC port calls.

pub mod connections;

pub use connections::AgentConnectionService;
