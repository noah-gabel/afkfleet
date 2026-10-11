//! The adapters that implement the ports: the outside world as the services
//! see it (Plan.md §4, ADR-0015).
//!
//! - [`system`]: the real clock and the OS's secure randomness.
//! - [`sqlite`]: the SQLite database, its pools and its migrations.
//! - [`crypto`]: the password service (P7.2), and later the vault and the tokens.
//!
//! P9 adds `msauth`.

pub mod crypto;
pub mod sqlite;
pub mod system;
