//! The control plane's PKI (Plan.md P10.2; ADR-0019): the CA, the gRPC
//! listener's server certificate, and agents' certificates signed from
//! their CSRs.
//!
//! - [`keys`]: Ed25519 keys made from the `SecureRandom` port, never by a
//!   crypto library's own random generator (S10), and their PEM files.
//! - [`profiles`]: what each certificate says, decided by the server alone
//!   (S11).
//! - [`csr`]: checking an agent's CSR, which yields only its public key.
//! - [`authority`]: the CA, which signs, and [`AgentIssuer`], the CA behind
//!   the enrollment service's port.
//! - [`mod@load`]: reading and checking the PKI at serve's startup.
//! - [`names`]: the agent a connecting agent's certificate names.
//! - [`files`]: writing `pki init`'s files without ever overwriting one.
//!
//! The CA key is its own secret: it lives in its file and in serve's
//! memory, never in the database, a log or an error (security rule 4).

pub mod authority;
pub mod csr;
pub mod files;
pub mod keys;
pub mod load;
pub mod names;
mod pem;
pub mod profiles;

pub use authority::{AgentIssuer, CertificateAuthority, Issued, PkiError, fingerprint};
pub use csr::{CheckedCsr, check_csr};
pub use keys::{Ed25519Key, KeyError};
pub use load::{LoadedPki, PkiLoadError, ca_fingerprint, load, load_ca};
pub use names::X509AgentNames;
pub use profiles::{ServerNames, ServerNamesError, Validity};
