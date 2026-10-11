//! The crypto adapters (Plan.md §7.2, §7.5, ADR-0016).
//!
//! - [`attempt_keys`]: [`attempt_keys::HmacAttemptKeys`], the keys of the
//!   `login_attempts` rows: an HMAC of the normalized username, never the
//!   name (P7.10).
//! - [`passwords`]: [`passwords::Argon2Passwords`], the password service:
//!   Argon2id with zxcvbn's strength check, in blocking tasks behind one
//!   bounded queue.
//! - [`tokens`]: [`tokens::Sha256Tokens`], the session tokens: minted from
//!   the `SecureRandom` port, stored only as their SHA-256 (P7.3).
//! - [`vault`]: [`vault::XChaChaVault`], XChaCha20-Poly1305 over the
//!   configured keys, and [`vault::VaultKeys`], the keys as loaded from their
//!   files.

pub mod attempt_keys;
pub mod passwords;
pub mod tokens;
pub mod vault;
