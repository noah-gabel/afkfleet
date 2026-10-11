//! The crypto adapters (Plan.md §7.2, §7.5, ADR-0016).
//!
//! - [`passwords`]: [`passwords::Argon2Passwords`], the password service:
//!   Argon2id with zxcvbn's strength check, in blocking tasks behind one
//!   bounded queue.
//! - [`vault`]: [`vault::XChaChaVault`], XChaCha20-Poly1305 over the
//!   configured keys, and [`vault::VaultKeys`], the keys as loaded from their
//!   files.
//!
//! The tokens arrive with P7.3.

pub mod passwords;
pub mod vault;
