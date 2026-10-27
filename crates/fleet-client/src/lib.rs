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
//!   returns, logs or formats holds one. The live events socket opens inside
//!   the client too ([`LiveEvents`]): its tickets and the access token it
//!   upgrades with stay there (ADR-0020, U25, S53, R80). The crate has no
//!   features, and `just tauri-check` fails if the desktop app's build ever
//!   resolves one (S57).
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
//!   body at most [`MAX_RESPONSE_BYTES`]. The live events socket's upgrade
//!   gets the request's timeout too, and each of its frames at most
//!   [`MAX_RESPONSE_BYTES`].
//! - **No redirects, no proxies, no automatic retries.** A 429 or 503 comes
//!   back with its `retry_after`. The one thing that tries again by itself
//!   is the live events socket, by its own reconnect policy: jittered waits
//!   of 1 to 30 s, a `Retry-After` up to 60 s, one prompt reconnect for the
//!   4001 an access token's end brings, and a new ticket every time
//!   ([`LiveEvents`]; ADR-0020, S54). A REST call never retries.
//! - **Certificates are always checked:** against the system's roots, or
//!   only against a configured CA ([`Trust`]).
//!
//! # Logging
//! Through `tracing`, never a token, a body or a query: `debug` for a
//! refresh and a retry, `info` when the session ends, `warn` when a new
//! refresh token couldn't be saved (the store is cleared instead), `error`
//! when the store couldn't even be cleared. The live events socket logs each
//! time it goes down with its cause and wait, at the level its reconnect
//! policy gives it: `warn` for every cause that waits the plateau (an answer
//! that won't change in seconds, a bad 101, an event before `ready`), for
//! 4000, 4002, 1008, a code the protocol doesn't name, silence and a missing
//! `ready`; `info` for the expected 4001, a 1001 and the other waits;
//! `debug` for a 4001 again before a socket stayed live and a cause repeated
//! in a row; never its ticket, its URL or a frame. tungstenite logs every
//! frame at `trace`, so every binary's filter caps its targets at `info`
//! (fleet-startup; ADR-0020, S56).

use core::time::Duration;

pub mod client;
pub mod error;
pub mod events;
pub mod link;
pub mod store;
pub mod trust;
pub mod url;

mod session;
mod transport;

pub use client::{BotCommandAnswer, BotCommandEffect, Client, ClientConfig, LoginStep};
pub use error::{ApiFailure, BuildError, ClientError, Unexpected};
pub use events::{Disconnect, LiveEnd, LiveEvents, LiveStop, LiveUpdate, RefetchReason};
pub use link::{VerifiedLink, VerifiedLinkError};
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
