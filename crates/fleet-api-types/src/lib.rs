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
//!   and for every rule. garde joins this crate with the first request DTO
//!   (P7).
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
//!
//! # Modules
//! - [`error`]: the error envelope that every failed request answers with.
//! - [`int`]: the 64-bit integer type, [`SafeInt`].
//! - [`open`]: tolerant values, [`Open`] and [`OpenEnum`].
//! - [`text`]: cleaned and capped text, [`BoundedText`].
//! - [`typescript`]: the export to TypeScript, behind `just gen` and
//!   `just gen-check`.

pub mod error;
pub mod int;
pub mod open;
pub mod text;
pub mod typescript;

pub use error::{ErrorBody, ErrorCode, ErrorResponse, FieldError};
pub use int::{SafeInt, SafeIntError};
pub use open::{Open, OpenEnum};
pub use text::BoundedText;
