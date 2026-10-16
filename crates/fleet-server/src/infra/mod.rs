//! The adapters that implement the ports: the outside world as the services
//! see it (Plan.md §4, ADR-0015).
//!
//! - [`system`]: the real clock and the OS's secure randomness.
//! - [`sqlite`]: the SQLite database, its pools and its migrations.
//! - [`crypto`]: the password service (P7.2), the vault (P7.4), the
//!   session tokens (P7.3), TOTP and the recovery codes (P7.7).
//! - [`qr`]: the enrollment's QR code, drawn as an SVG (P7.7).
//! - [`msauth`]: Microsoft's account flows over HTTP, the adapter's own
//!   requests (P9.3).
//! - [`pki`]: the control plane's CA, the server certificate and agents'
//!   certificates (P10.2).

pub mod crypto;
pub mod msauth;
pub mod pki;
pub mod qr;
pub mod sqlite;
pub mod system;
