//! The agents' DTOs (Plan.md P10.3, Appendix B, ADR-0019): creating an
//! enrollment token.
//!
//! A token enrolls one new agent within 15 minutes. It reaches the agent in
//! the bundle, `<token>.<CA fingerprint>`, which is in
//! [`CreatedEnrollmentTokenResponse`] only: the server keeps just the
//! token's hash, so it's never shown again. Creating one is the Owner's,
//! after a step-up.

use fleet_core::enrollment::EnrollmentBundle;
use fleet_core::id::EnrollmentTokenId;
use fleet_core::pki::CertFingerprint;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::{PartialSchema as _, ToSchema};

use crate::ids::id_schema;
use crate::secret::SecretText;
use crate::text::BoundedText;
use crate::time::Timestamp;

/// The characters of an enrollment bundle.
pub const BUNDLE_CHARS: usize = EnrollmentBundle::TEXT_LEN;

/// The characters of a CA fingerprint: SHA-256 in openssl's uppercase
/// colon form.
pub const FINGERPRINT_CHARS: usize = CertFingerprint::TEXT_LEN;

/// `POST /agents/enrollment-tokens`: the new token, in its bundle, shown
/// this once.
#[derive(Debug, Serialize, Deserialize, TS, ToSchema)]
pub struct CreatedEnrollmentTokenResponse {
    /// The token's ID, which its audit entries name.
    #[ts(type = "string")]
    #[schema(schema_with = id_schema)]
    pub id: EnrollmentTokenId,
    /// The bundle (`afk_et_….<CA fingerprint>`) that `afkfleet-agent
    /// enroll` reads from stdin.
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<BUNDLE_CHARS>::schema)]
    pub bundle: SecretText<BUNDLE_CHARS>,
    /// The CA's fingerprint, also in the bundle: the one `pki init`
    /// printed.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<FINGERPRINT_CHARS>::schema)]
    pub ca_fingerprint: BoundedText<FINGERPRINT_CHARS>,
    /// When the token expires, 15 minutes after it was created.
    #[ts(type = "string")]
    #[schema(schema_with = Timestamp::schema)]
    pub expires_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const FINGERPRINT: &str = "AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB";

    fn created_json() -> serde_json::Value {
        json!({
            "id": "018bcfe5-6800-7101-8101-010101010101",
            "bundle": format!("afk_et_BUNDLEBUNDLE.{FINGERPRINT}"),
            "ca_fingerprint": FINGERPRINT,
            "expires_at": "2027-01-15T08:15:00.000Z"
        })
    }

    #[test]
    fn the_limits_are_the_core_types_lengths() {
        assert_eq!(BUNDLE_CHARS, 146);
        assert_eq!(FINGERPRINT_CHARS, FINGERPRINT.len());
    }

    #[test]
    fn a_created_token_round_trips() {
        let created = created_json();

        let back: CreatedEnrollmentTokenResponse = serde_json::from_value(created.clone()).unwrap();

        assert_eq!(serde_json::to_value(back).unwrap(), created);
    }

    #[test]
    fn no_dto_shows_a_secret_in_debug() {
        let created: CreatedEnrollmentTokenResponse =
            serde_json::from_value(created_json()).unwrap();

        let shown = format!("{created:?}");

        assert!(!shown.contains("BUNDLEBUNDLE"), "{shown}");
    }
}
