//! The real server and the real managed agent together, in-process
//! (Plan.md P10.9; ADR-0019, U19, S72): one test binary, so the harness is
//! shared by every case and by P11.7's full flow later.
//!
//! - [`harness`]: the server through serve's own wiring, managed agents
//!   enrolled with it and run over the bots' fake connector, a stalling
//!   proxy in front of each agent, the Owner's client, the bots' ledger and
//!   the one log every wait wakes on.
//! - [`loopback`]: P10.9's cases over mTLS on `127.0.0.1`.
//! - [`managed_check`]: Phase 10's managed check, a manual test only the
//!   user runs, with their own Microsoft account.
//! - [`timing`]: the timing contracts between the agent and the server,
//!   over their constants.
//!
//! Every loopback case runs on real time on one thread: the sockets, TLS
//! and SQLite need it, and every task of the server and of the agents logs
//! into the test's subscriber. Waits are bounded by
//! [`harness::logs::PATIENCE`].
#![cfg(test)]

mod harness;
mod loopback;
mod managed_check;
mod timing;
