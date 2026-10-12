//! The typed client of afkfleet's HTTP API (Plan.md P8.1, ADR-0007,
//! ADR-0017): one method per endpoint, the session's tokens kept out of
//! every caller's hands, and the server's rules for refreshing them.
//!
//! The desktop app's Rust side (P8.4) talks to the server only through it,
//! and so do full-stack tests. It sends and reads exactly
//! [`fleet_api_types`]' DTOs.
//!
//! # Rules
//! - **No token ever leaves the client.** The access token lives only in its
//!   memory, the refresh token only in a [`SessionStore`] (the OS keychain in
//!   the app), and the password step's mfa or setup token only in its
//!   memory. Methods take codes, never tokens, and nothing the client
//!   returns, logs or formats holds one.
//! - **One refresh at a time, and only on a 401 `unauthorized` from an
//!   authenticated route** (ADR-0016). A request whose token a refresh
//!   already replaced is retried once with the current one instead. An
//!   authenticated request without an access token, as after a start,
//!   refreshes first. Never on a timer, never for `invalid_credentials`
//!   (login) or a 403 (step-up).
//! - **No grace window.** A refresh whose answer got lost leaves a replaced
//!   token behind; sending it again is reuse, which ends the session.
//! - **The session ends locally** on a refused refresh, on a 401 after the
//!   retry with the newest token, on an empty store, and at every logout;
//!   never on a transient failure. An end never races a save: see
//!   `session.rs`.
//! - **Bounded:** a connection gets [`CONNECT_TIMEOUT`], a whole request
//!   [`REQUEST_TIMEOUT`], each store call [`STORE_TIMEOUT`], and a response
//!   body at most [`MAX_RESPONSE_BYTES`].
//! - **No redirects, no proxies, no automatic retries.** A 429 or 503 comes
//!   back with its `retry_after`.
//! - **Certificates are always checked:** against the system's roots, or
//!   only against a configured CA ([`Trust`]).
//!
//! # Logging
//! Through `tracing`, never a token, a body or a query: `debug` for a
//! refresh and a retry, `info` when the session ends, `warn` when a new
//! refresh token couldn't be saved (the store is cleared instead), `error`
//! when the store couldn't even be cleared.

use core::time::Duration;

pub mod client;
pub mod error;
pub mod store;
pub mod trust;
pub mod url;

mod session;
mod transport;

pub use client::{Client, ClientConfig, LoginStep};
pub use error::{ApiFailure, BuildError, ClientError, Unexpected};
pub use store::{MemoryStore, SessionStore, StoreError, StoreOp};
pub use trust::{CaError, CustomCa, Trust};
pub use url::{ServerUrl, ServerUrlError};

/// How long connecting may take: DNS, TCP and the TLS handshake.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a whole request may take, its response body included. It's
/// longer than the server's `[http] request_timeout_secs` (at most 19 s),
/// so the server's own answer always arrives first, and a refresh never
/// commits on the server after the client gave up on it (ADR-0017).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// How long one call to the [`SessionStore`] may take.
pub const STORE_TIMEOUT: Duration = Duration::from_secs(5);

/// The largest response body the client reads: 1 MiB. Past it, the answer
/// is [`ClientError::TooLarge`] and the connection is dropped (ADR-0015).
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
