//! The crypto adapters (Plan.md §7.2, §7.5, ADR-0016).
//!
//! - [`passwords`]: [`passwords::Argon2Passwords`], the password service:
//!   Argon2id with zxcvbn's strength check, in blocking tasks behind one
//!   bounded queue.
//!
//! Later tasks add the vault (P7.4) and the tokens (P7.3).

pub mod passwords;
