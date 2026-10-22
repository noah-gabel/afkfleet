//! The services: one use case each, with its transaction and its audit entry
//! (Plan.md §4, ADR-0015). They hold the ports as `Arc<dyn …>` and read the
//! time and randomness only through fleet-core's `Clock` and `SecureRandom`.
//!
//! - [`accounts`]: the linked Minecraft accounts' use cases beside the link
//!   flow: creating an offline account (P9.7), listing and reading
//!   accounts, reviewing pending links and deleting accounts (P9.6).
//! - [`agents`]: the agents' connections: admitting a connecting agent's
//!   certificate, recording its hello and its stream's end (P10.4); the
//!   bots' assignments, and releasing a disabled agent's bots (P10.5).
//! - [`audit`]: stamping audit entries, recording the ones that change
//!   nothing, and listing the log.
//! - [`auth`]: sessions and their tokens: issuing, refreshing with reuse
//!   detection (P7.3), and from P7.8 authenticating every request.
//! - [`bots`]: the bots as they read, with what their state reads as now
//!   (P10.5).
//! - [`challenges`]: the login's mfa and setup tokens, held in memory
//!   (P7.6, P7.7).
//! - [`enrollment`]: the agents' enrollment tokens, and enrolling an agent
//!   with one (P10.3).
//! - [`event_tickets`]: the live events' single-use tickets, held in memory
//!   (P11.5).
//! - [`events`]: the internal event bus: what agents report about their
//!   bots, for the live events P11.5 sends on (P10.8).
//! - [`grants`]: sharing a linked account with other users (P9.6).
//! - [`health`]: whether the server is ready to serve requests.
//! - [`login`]: logging in (the password step and the code step) and
//!   stepping up (P7.6).
//! - [`lockout`]: the per-username lockout: when failed logins lock a name,
//!   and its periodic purge (P7.10).
//! - [`mfa`]: the second factor: enrolling and confirming a TOTP
//!   authenticator, replacing it, and checking a code at login (P7.7).
//! - [`invites`]: creating, listing and revoking the invites to register
//!   (P7.9).
//! - [`link_flows`]: linking a Microsoft account through Microsoft's
//!   device-code flow, held in memory and polled in the background (P9.4).
//! - [`session_tokens`]: Minecraft session tokens for linked Microsoft
//!   accounts, one refresh per account at a time (P9.5).
//! - [`users`]: creating the Owner (P7.5), the admins' list and changes
//!   (P7.9), and the re-check every admin's write makes in its transaction.
//! - [`vault`]: what the vault's keys seal, and re-encrypting the secrets
//!   under the active key (P9.2).

pub mod accounts;
pub mod agents;
pub mod audit;
pub mod auth;
pub mod bots;
pub mod challenges;
pub mod enrollment;
pub mod event_tickets;
pub mod events;
pub mod grants;
pub mod health;
pub mod invites;
pub mod link_flows;
pub mod lockout;
pub mod login;
pub mod mfa;
pub mod modes;
pub mod session_tokens;
pub mod users;
pub mod vault;
