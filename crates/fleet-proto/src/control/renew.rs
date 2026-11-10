//! A certificate renewal's validated answer and the limits both ends share
//! (Plan.md P13.6; ADR-0027).
//!
//! The request is empty: the certificate the call presents names the agent
//! and holds the key the new one is for, and the TLS handshake proved the
//! caller has that key. The answer carries the new certificate as PEM, as an
//! enrollment's does, which the agent checks before it stores it.

use core::time::Duration;

use chrono::{DateTime, Utc};

use crate::ConvertError;
use crate::convert::{time_from_wire, time_to_wire};
use crate::enrollment::{MAX_CERTIFICATE_BYTES, text_from_wire};
use crate::generated as wire;

/// How long the server signs one renewal before it answers that it took too
/// long; its write after that runs to its end.
pub const RENEW_SERVER_DEADLINE: Duration = Duration::from_secs(15);

/// How long the agent waits for the answer: longer than the server's
/// deadline.
pub const RENEW_CLIENT_DEADLINE: Duration = Duration::from_secs(20);

/// What the server answers an agent whose certificate it renewed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenewResponse {
    /// The new certificate, as one PEM `CERTIFICATE` section: 1 to
    /// [`MAX_CERTIFICATE_BYTES`] bytes. The agent checks the section itself.
    pub certificate_pem: String,
    /// When it stops being valid.
    pub not_after: DateTime<Utc>,
}

impl TryFrom<wire::RenewResponse> for RenewResponse {
    type Error = ConvertError;

    fn try_from(mut response: wire::RenewResponse) -> Result<Self, ConvertError> {
        Ok(Self {
            certificate_pem: text_from_wire(
                "RenewResponse.certificate_pem",
                &mut response.certificate_pem,
                MAX_CERTIFICATE_BYTES,
            )?,
            not_after: time_from_wire(
                "RenewResponse.not_after_unix_ms",
                response.not_after_unix_ms,
            )?,
        })
    }
}

impl TryFrom<RenewResponse> for wire::RenewResponse {
    type Error = ConvertError;

    fn try_from(response: RenewResponse) -> Result<Self, ConvertError> {
        Ok(Self {
            certificate_pem: response.certificate_pem,
            not_after_unix_ms: Some(time_to_wire(
                "RenewResponse.not_after_unix_ms",
                response.not_after,
            )?),
        })
    }
}
