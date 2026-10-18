//! Tests only, nothing else (ADR-0019, U19): the real afkfleet server and
//! the real managed agent together, both in-process.
//!
//! **P10.9's loopback suite** (`tests/integration/loopback/`): the server
//! serves its HTTP API and its gRPC control plane on `127.0.0.1`; managed
//! agents enroll with it and hold their mTLS streams through stalling
//! proxies, their bots on fleet-testkit's fake connector. The cases are a
//! connection cut mid-stream, an agent restart, a server restart with two
//! agents back in either order, a connection gone silent, and a server
//! outage with sessions to grant.
//!
//! **Phase 10's managed check** (`tests/integration/managed_check/`): a
//! manual test only the user runs, with their real Microsoft account against
//! a local online-mode Minecraft server (`just test-managed-account`).
//!
//! P11.7's full-flow test joins the same test binary. Nothing depends on
//! this crate, and it has no API: this library is only its documentation.
