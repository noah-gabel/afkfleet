//! The real server and the real managed agent together, in-process
//! (Plan.md P10.9; ADR-0019, U19, S72): one test binary, so the harness is
//! shared by every case and by P11.7's full flow.
//!
//! - [`harness`]: the server through serve's own wiring, managed agents
//!   enrolled with it and run over the bots' fake connector, a stalling
//!   proxy in front of each agent, the Owner's client, the bots' ledger and
//!   the one log every wait wakes on.
//! - [`loopback`]: P10.9's cases over mTLS on `127.0.0.1`.
//! - [`load`]: P13.4's load test, 200 bots and 20 Members' sockets,
//!   measured (ADR-0032); only `just load` runs it.
//! - [`crash`]: P13.5's crash cases (ADR-0033): the server or the agent in a
//!   child process of this binary, killed by the operating system at an
//!   exact point of a renewal, a backup, a retention run or an ingest
//!   batch, then started again on the files it left.
//! - [`dod_kit`]: the definition-of-done checks' kit (ADR-0020, S72): the server's and the
//!   agent's configs, loaded with exactly their recipes' exports, fit each
//!   other and their Minecraft container.
//! - [`full_flow`]: P11.7's full flow, a bot started, seen Online, chatting
//!   both ways and stopped on the Owner's live events (ADR-0020, S58).
//! - [`managed_check`]: Phase 10's managed check, a manual test only the
//!   user runs, with their own Microsoft account.
//! - [`slow_on_respawn`]: the slow on-respawn case (ADR-0020, S26): a real
//!   Minecraft server in Docker, the agent's bot on the azalea connector;
//!   `just test-slow` runs it.
//! - [`timing`]: the timing contracts between the agent and the server,
//!   over their constants.
//! - [`smoke`]: P12.7's smoke tests (ADR-0022, S19): the production compose
//!   project from a copy of `deploy/`, on each proxy path, with both images,
//!   and the runbook's operations on it (`smoke::ops`: a restore after a
//!   crash, a new host from the keys' copy; ADR-0022, S43); `just
//!   test-stack` and `just test-slow` run them, and the Stack workflow
//!   nightly. `smoke::chaos` holds P13.5's chaos tests on the same project
//!   (ADR-0033): kills, a partition and a full disk, with a Minecraft
//!   server and real bots; only `just chaos` runs them.
//! - [`sftp`]: the backups' SFTP account (ADR-0022, S15), installed by
//!   `deploy/host/sftp-backups.sh` in a throwaway sshd container.
//!
//! Every loopback case runs on real time on one thread: the sockets, TLS
//! and SQLite need it, and every task of the server and of the agents logs
//! into the test's subscriber. Waits are bounded by
//! [`harness::logs::PATIENCE`]. The smoke and SFTP tests run on real time
//! too, against Docker, each wait bounded and woken by an event.
#![cfg(test)]

mod crash;
mod dod_kit;
mod full_flow;
mod harness;
mod load;
mod loopback;
mod managed_check;
mod sftp;
mod slow_on_respawn;
mod smoke;
mod timing;
