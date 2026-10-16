//! The DTOs of afkfleet's HTTP API (Plan.md P6.9, ADR-0015): what the server
//! sends and receives, shared with `fleet-client` and exported to TypeScript
//! for the app (`just gen` writes `packages/ui/src/generated/`).
//!
//! # Rules for every DTO
//! - **Never hand-write a TypeScript type** that mirrors one of these: change
//!   the Rust type and run `just gen`. CI's `ts-types-fresh` job fails while
//!   the generated files are stale.
//! - **Requests** derive `Deserialize`, garde's `Validate` and
//!   `#[serde(deny_unknown_fields)]`, and each has tests for an unknown field
//!   and for every rule. A rule's message names the rule, never the value.
//!   They derive `Serialize` too, so `fleet-client` sends exactly these
//!   types; an optional field is left out when it holds nothing, never sent
//!   as `null` (ADR-0017). A test round-trips each one.
//! - **Responses** derive `Serialize` and `Deserialize`, without
//!   `deny_unknown_fields`, so `fleet-client` reads them and a field a newer
//!   server adds doesn't break an older app. Each one round-trips in a test.
//! - **64-bit integers** cross the API only as [`SafeInt`]: JavaScript reads a
//!   JSON number exactly only within ±(2^53 − 1), and ts-rs would export a
//!   plain `i64` or `u64` as `bigint`, which `JSON.parse` never produces. A
//!   test fails if any generated file contains `bigint`.
//! - **Values that can grow** in a later version, such as [`ErrorCode`], are
//!   read through [`Open`], so a value only a newer server knows still reads
//!   back. The TypeScript type lists only the values this version knows, so
//!   app code that branches on one always keeps a default branch.
//! - **Text a client reads** is a [`BoundedText`]: cleaned and capped when it's
//!   read, so a buggy or hostile server can't put control characters, a fake
//!   log line or megabytes into a client's output.
//! - **Secrets** (passwords, tokens, codes, TOTP secrets) are a
//!   [`SecretText`]: never in `Debug`, wiped on drop, read only through
//!   `expose()` or `into_secret()`. A test formats every DTO that holds one.
//!
//! # Modules
//! - [`accounts`]: the linked Minecraft accounts: starting a Microsoft link
//!   flow, reading where it stands, creating an offline account, listing,
//!   reviewing and sharing accounts.
//! - [`agents`]: the agents: creating an enrollment token.
//! - [`app`]: the desktop app's IPC types, between its Rust side and its UI;
//!   exported to TypeScript, never on the HTTP wire.
//! - [`auth`]: logging in, refreshing, stepping up and registering.
//! - [`error`]: the error envelope that every failed request answers with.
//! - [`ids`]: fleet-core's typed IDs in DTOs, re-exported, and their schema.
//! - [`int`]: the 64-bit integer type, [`SafeInt`], and the schema of a
//!   bounded 32-bit field.
//! - [`mfa`]: enrolling, confirming and replacing a TOTP authenticator.
//! - [`invites`]: creating and listing the invites to register.
//! - [`me`]: the user's profile and changing their password.
//! - [`open`]: tolerant values, [`Open`] and [`OpenEnum`].
//! - [`openapi`]: the `OpenAPI` schemas, which the server's dev-only document
//!   serves and which a test keeps equal to the TypeScript.
//! - [`page`]: the query of every list, [`PageQuery`].
//! - [`secret`]: secrets in requests and responses, [`SecretText`].
//! - [`sessions`]: the user's live sessions.
//! - [`text`]: cleaned and capped text, [`BoundedText`].
//! - [`time`]: a point in time, [`Timestamp`].
//! - [`typescript`]: the export to TypeScript, behind `just gen` and
//!   `just gen-check`.
//! - [`users`]: the users Admins list and change.

pub mod accounts;
pub mod agents;
pub mod app;
pub mod auth;
pub mod error;
pub mod ids;
pub mod int;
pub mod invites;
pub mod me;
pub mod mfa;
pub mod open;
pub mod openapi;
pub mod page;
pub mod secret;
pub mod sessions;
pub mod text;
pub mod time;
pub mod typescript;
pub mod users;

pub use error::{ErrorBody, ErrorCode, ErrorResponse, FieldError};
pub use ids::{AccountId, EnrollmentTokenId, InviteId, LinkFlowId, SessionId, UserId};
pub use int::{SafeInt, SafeIntError};
pub use open::{Open, OpenEnum};
pub use page::PageQuery;
pub use secret::SecretText;
pub use text::BoundedText;
pub use time::Timestamp;
