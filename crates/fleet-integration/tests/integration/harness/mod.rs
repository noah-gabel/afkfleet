//! What every case is built from (ADR-0019, S72).
//!
//! - [`server`]: the real server: `app_with` over a manual clock and seeded
//!   randomness, then serve's own `open`, `grpc_port` and `serve`, its PKI
//!   written by `pki init` and its database in the test's folder; stopped by
//!   a signal and started again on the same database, PKI and addresses.
//! - [`agent`]: a managed agent, enrolled with the server for real, run
//!   in-process through `fleet_agent::run::run`, its bots on the ledger's
//!   fake connector; stopped by a signal, or crashed.
//! - [`proxy`]: the stalling proxy between each agent and the gRPC port.
//! - [`ledger`]: the bots' sessions across every agent run, which fails a
//!   case that ever runs one account twice.
//! - [`client`]: the Owner, logged in with 2FA, who changes bots only
//!   through the real HTTP API.
//! - [`live`]: the Owner's live events socket, read by a task of its own,
//!   each update logged (P11.7, S58).
//! - [`logs`]: the one log of the server and every agent, which wakes the
//!   waits.
//! - [`world`]: all of them for one case.

pub(crate) mod agent;
pub(crate) mod client;
pub(crate) mod ledger;
pub(crate) mod live;
pub(crate) mod logs;
pub(crate) mod proxy;
pub(crate) mod server;
pub(crate) mod world;
