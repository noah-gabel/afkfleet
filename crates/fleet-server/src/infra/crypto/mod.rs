//! The crypto adapters (Plan.md §7.2, §7.5, ADR-0016).
//!
//! - [`attempt_keys`]: [`attempt_keys::HmacAttemptKeys`], the keys of the
//!   `login_attempts` rows: an HMAC of the normalized username, never the
//!   name (P7.10).
//! - [`passwords`]: [`passwords::Argon2Passwords`], the password service:
//!   Argon2id with zxcvbn's strength check, in blocking tasks behind one
//!   bounded queue.
//! - [`recovery_codes`]: [`recovery_codes::Sha256RecoveryCodes`], the
//!   recovery codes' SHA-256 (P7.7).
//! - [`tokens`]: [`tokens::Sha256Tokens`], the session tokens and the login's
//!   challenges: minted from the `SecureRandom` port, stored only as their
//!   SHA-256 (P7.3).
//! - [`totp`]: [`totp::TotpRs`], one TOTP step's code and the otpauth URI,
//!   through totp-rs (P7.7).
//! - [`vault`]: [`vault::XChaChaVault`], XChaCha20-Poly1305 over the
//!   configured keys, and [`vault::VaultKeys`], the keys as loaded from their
//!   files.

pub mod attempt_keys;
pub mod passwords;
pub mod recovery_codes;
pub mod tokens;
pub mod totp;
pub mod vault;
