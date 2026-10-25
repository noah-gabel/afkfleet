//! The repository traits the services depend on (Plan.md §4, P6.4,
//! ADR-0015). Services hold them as `Arc<dyn …>`; `infra` implements them,
//! and nothing here names sqlx, so a different database could implement
//! them too (ADR-0005).
//!
//! - [`store`]: the database as a unit of work. [`store::Store`] hands out
//!   read handles and one write transaction at a time, [`store::WriteTx`],
//!   whose commit always records an audit entry.
//! - [`users`]: the users.
//! - [`bots`]: the bots, one per linked Minecraft account, with their
//!   desired state, their agent and their last reported state (P11.1, P10.5).
//! - [`modes`]: the modes, what a bot does while it's online; the built-in
//!   ones seeded with fixed IDs (P11.1).
//! - [`chat`]: the bots' chat log, what they received and what their modes
//!   sent; append-only (P11.1, P10.8).
//! - [`retention`]: what the retention task deletes, in a transaction of
//!   its own, and the reads of where it cuts (P11.6): the only deletes of
//!   the audit log, and of chat but for an account's deletion, which takes
//!   its bot's chat with it.
//! - [`agents`]: the enrolled agents and the certificates the server signed
//!   for them (P10.2, P10.3).
//! - [`agent_connections`]: what an agent's `Connect` stream asks the app:
//!   admitting its certificate, recording its hello, its end and a conflict
//!   over its identity, and checking the live streams again (P10.4, P10.5).
//! - [`live_agents`]: what the app asks of the live streams: ending one
//!   whose permission ended, what's known of one and its bots (P10.5), and
//!   a session request's answer for the stream that asked (P10.6).
//! - [`session_grants`]: what a `Connect` stream asks the app for a session
//!   request: its grant or its denial, sent to that stream (P10.6).
//! - [`ingest`]: what a `Connect` stream hands the server's ingest: the
//!   reports about the bots it holds, and the sticky states its batches
//!   write (P10.8).
//! - [`assignments`]: what the reconciler asks of the store: everything it
//!   plans from, and each change of a bot's agent, audited (P10.5).
//! - [`reconcile`]: the hints that tell the reconciler to look again after a
//!   commit or a stream's change (P10.5).
//! - [`live_hints`]: the hints that tell the live events sockets to read
//!   again after a commit: bots, a mode, a role (P11.5).
//! - [`accounts`]: the linked Minecraft accounts, Microsoft or offline
//!   (P9.1).
//! - [`grants`]: the grants that share an account with other users (P9.1).
//! - [`enrollment_tokens`]: the agents' enrollment tokens, single-use and
//!   expiring (P10.3).
//! - [`invites`]: the invites to register, single-use and expiring (P7.1,
//!   P7.9).
//! - [`sessions`]: the sessions, one per login's token family (P7.1, P7.3).
//! - [`login_attempts`]: the failed logins per username, keyed so the name
//!   itself is never stored (P7.1, P7.10).
//! - [`mfa`]: the users' second factor, a confirmed TOTP authenticator and
//!   its recovery codes (P7.1, P7.7).
//! - [`recovery_codes`]: the recovery codes' hashes, the only form of a
//!   recovery code the database holds (P7.7).
//! - [`tokens`]: the tokens' hashes, the only form of a token the database
//!   holds (P7.3).
//! - [`passwords`]: hashing and checking passwords (P7.2).
//! - [`pki`]: issuing agents' certificates from their CSRs (P10.2), and
//!   reading which agent a certificate names (P10.4).
//! - [`totp`]: TOTP's crypto, a time step's code and the otpauth URI (P7.7).
//! - [`qr`]: drawing the enrollment's QR code (P7.7).
//! - [`vault`]: encrypting stored secrets (P7.4), and reading an envelope's
//!   key (P9.2).
//! - [`envelopes`]: the stored envelopes, for `vault status` and
//!   `vault rotate-key` (P9.2).
//! - [`msauth`]: Microsoft's account flows, the device code, the refresh and
//!   the Minecraft session (P9.3).
//! - [`pace`]: how a background task waits between its calls (P9.4).
//! - [`audit`]: the audit log. It's append-only: the only operations are
//!   recording (inside a write transaction) and listing.
//! - [`page`]: the keyset pages the lists share (P7.6, P7.9).

pub mod accounts;
pub mod agent_connections;
pub mod agents;
pub mod assignments;
pub mod audit;
pub mod bots;
pub mod chat;
pub mod enrollment_tokens;
pub mod envelopes;
pub mod grants;
pub mod ingest;
pub mod invites;
pub mod live_agents;
pub mod live_hints;
pub mod login_attempts;
pub mod mfa;
pub mod modes;
pub mod msauth;
pub mod pace;
pub mod page;
pub mod passwords;
pub mod pki;
pub mod qr;
pub mod reconcile;
pub mod recovery_codes;
pub mod retention;
pub mod session_grants;
pub mod sessions;
pub mod store;
pub mod tokens;
pub mod totp;
pub mod users;
pub mod vault;
