//! The enrollment's validated messages (Plan.md P10.3; ADR-0019): what an
//! agent sends to enroll and what the server answers, and the limits both
//! ends share.
//!
//! - **The token** moves into a `SecretString` at once and isn't judged
//!   here: the server hashes it and looks it up, and every token problem gets
//!   one answer there (S14). The request's decoding limit bounds it.
//! - **The name** is the agent's config name, an [`AgentName`]; the server
//!   decides what it may be called.
//! - **The CSR** is 1 to [`MAX_CSR_BYTES`] of DER; the server's PKI checks
//!   it.
//! - **The limits** are constants both ends use: the request and answer
//!   sizes, and the server's deadline below the agent's, so a token is never
//!   used after the agent gave up waiting.

use core::fmt;
use core::mem;
use core::time::Duration;

use chrono::{DateTime, Utc};
use fleet_core::id::AgentId;
use fleet_core::value::AgentName;
use secrecy::{ExposeSecret as _, SecretString};
use zeroize::Zeroize as _;

use crate::convert::{id_from_wire, id_to_wire, time_from_wire, time_to_wire};
use crate::generated as wire;
use crate::{ConvertError, Problem};

/// The largest enrollment request either end handles, in bytes.
pub const MAX_REQUEST_BYTES: usize = 8 * 1024;

/// The largest enrollment answer either end handles, in bytes.
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024;

/// The largest CSR a request may carry, in bytes.
pub const MAX_CSR_BYTES: usize = 4096;

/// The largest certificate an answer may carry, in bytes.
pub const MAX_CERTIFICATE_BYTES: usize = 8 * 1024;

/// How long the server works on one enrollment before it answers that it
/// took too long.
pub const SERVER_DEADLINE: Duration = Duration::from_secs(15);

/// How long the agent waits for the answer: longer than the server's
/// deadline.
pub const CLIENT_DEADLINE: Duration = Duration::from_secs(20);

/// What an agent sends to enroll.
pub struct EnrollRequest {
    /// The single-use enrollment token.
    pub token: SecretString,
    /// The name its config gives it.
    pub name: AgentName,
    /// Its CSR, DER-encoded: 1 to [`MAX_CSR_BYTES`].
    pub csr_der: Vec<u8>,
}

impl fmt::Debug for EnrollRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnrollRequest")
            .field("name", &self.name)
            .field("csr_bytes", &self.csr_der.len())
            .finish_non_exhaustive()
    }
}

impl TryFrom<wire::EnrollRequest> for EnrollRequest {
    type Error = ConvertError;

    fn try_from(mut request: wire::EnrollRequest) -> Result<Self, ConvertError> {
        // The token moves first, so it's wiped whatever else is refused.
        let token = secret_from_wire(&mut request.token);
        let name = AgentName::try_from(request.name.as_str())
            .map_err(|error| ConvertError::new("EnrollRequest.name", Problem::AgentName(error)))?;
        let csr_der =
            bytes_from_wire("EnrollRequest.csr_der", &mut request.csr_der, MAX_CSR_BYTES)?;
        Ok(Self {
            token,
            name,
            csr_der,
        })
    }
}

impl From<EnrollRequest> for wire::EnrollRequest {
    fn from(request: EnrollRequest) -> Self {
        Self {
            token: request.token.expose_secret().to_owned(),
            name: request.name.into(),
            csr_der: request.csr_der,
        }
    }
}

/// Moves a secret out of a generated message into a `SecretString` of
/// exactly its length, and wipes the original.
fn secret_from_wire(raw: &mut String) -> SecretString {
    let mut raw = mem::take(raw);
    let secret = SecretString::from(raw.as_str());
    raw.zeroize();
    secret
}

/// Takes bytes that must be 1 to `max` long.
fn bytes_from_wire(
    field: &'static str,
    raw: &mut Vec<u8>,
    max: usize,
) -> Result<Vec<u8>, ConvertError> {
    match raw.len() {
        0 => Err(ConvertError::new(field, Problem::Missing)),
        len if len > max => Err(ConvertError::new(field, Problem::OutOfRange)),
        _ => Ok(mem::take(raw)),
    }
}

/// Takes text that must be 1 to `max` bytes long.
fn text_from_wire(
    field: &'static str,
    raw: &mut String,
    max: usize,
) -> Result<String, ConvertError> {
    match raw.len() {
        0 => Err(ConvertError::new(field, Problem::Missing)),
        len if len > max => Err(ConvertError::new(field, Problem::OutOfRange)),
        _ => Ok(mem::take(raw)),
    }
}

/// What the server answers an agent it enrolled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollResponse {
    /// The new agent's ID.
    pub agent_id: AgentId,
    /// Its certificate, as one PEM `CERTIFICATE` section: 1 to
    /// [`MAX_CERTIFICATE_BYTES`] bytes. The agent checks the section itself.
    pub certificate_pem: String,
    /// When the certificate stops being valid.
    pub not_after: DateTime<Utc>,
}

impl TryFrom<wire::EnrollResponse> for EnrollResponse {
    type Error = ConvertError;

    fn try_from(mut response: wire::EnrollResponse) -> Result<Self, ConvertError> {
        Ok(Self {
            agent_id: id_from_wire("EnrollResponse.agent_id", &response.agent_id)?,
            certificate_pem: text_from_wire(
                "EnrollResponse.certificate_pem",
                &mut response.certificate_pem,
                MAX_CERTIFICATE_BYTES,
            )?,
            not_after: time_from_wire(
                "EnrollResponse.not_after_unix_ms",
                response.not_after_unix_ms,
            )?,
        })
    }
}

impl TryFrom<EnrollResponse> for wire::EnrollResponse {
    type Error = ConvertError;

    fn try_from(response: EnrollResponse) -> Result<Self, ConvertError> {
        Ok(Self {
            agent_id: id_to_wire(response.agent_id),
            certificate_pem: response.certificate_pem,
            not_after_unix_ms: Some(time_to_wire(
                "EnrollResponse.not_after_unix_ms",
                response.not_after,
            )?),
        })
    }
}
