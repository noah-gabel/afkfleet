//! The 2FA DTOs (Plan.md P7.7, §7.2, ADR-0016): enrolling a TOTP
//! authenticator, confirming it, and starting its replacement.
//!
//! Enroll and confirm take a setup token, as `Authorization: Bearer
//! afk_st_…`: from the password step of a user without 2FA, or from
//! [`MfaResetResponse`] when a user replaces theirs. Every secret here is a
//! [`SecretText`].

use garde::Validate;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{ArrayBuilder, Schema};
use utoipa::{PartialSchema as _, ToSchema};

use crate::int::integer_schema;
use crate::secret::SecretText;

/// The most characters of [`MfaEnrollResponse::otpauth_uri`]: the URI of the
/// longest username fits with room to spare.
pub const OTPAUTH_URI_MAX_CHARS: usize = 256;
/// The most characters of [`MfaEnrollResponse::qr_svg`]: the data URI of the
/// longest username's QR code fits with room to spare.
pub const QR_SVG_MAX_CHARS: usize = 32_768;
/// The most characters of [`MfaEnrollResponse::secret`]: 20 bytes as base32
/// without padding.
pub const TOTP_SECRET_MAX_CHARS: usize = 32;
/// The most characters of a code field, such as [`MfaConfirmRequest::code`].
pub const CODE_MAX_CHARS: usize = 64;
/// The most characters of a token field, such as
/// [`MfaResetResponse::setup_token`].
pub const TOKEN_MAX_CHARS: usize = 64;
/// The most characters of a recovery code as shown: 20 characters in groups
/// of four.
pub const RECOVERY_CODE_MAX_CHARS: usize = 24;
/// The longest a setup token lives, in seconds (§7.2's 10 minutes).
pub const SETUP_TOKEN_MAX_SECS: u32 = 600;

/// `POST /me/mfa/enroll`: a new TOTP secret for the authenticator app.
/// Each call replaces the pending one; nothing is stored until confirm.
#[derive(Debug, Serialize, Deserialize, TS, ToSchema)]
pub struct MfaEnrollResponse {
    /// The `otpauth://totp/…` URI, holding the secret.
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<OTPAUTH_URI_MAX_CHARS>::schema)]
    pub otpauth_uri: SecretText<OTPAUTH_URI_MAX_CHARS>,
    /// The URI as a QR code: `data:image/svg+xml;base64,…`. Show it only
    /// through an `<img>`, never as HTML.
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<QR_SVG_MAX_CHARS>::schema)]
    pub qr_svg: SecretText<QR_SVG_MAX_CHARS>,
    /// The secret as base32, for typing it into an app by hand.
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<TOTP_SECRET_MAX_CHARS>::schema)]
    pub secret: SecretText<TOTP_SECRET_MAX_CHARS>,
}

/// `POST /me/mfa/confirm`: the first code the authenticator app shows, which
/// proves it holds the secret.
#[derive(Debug, Deserialize, Validate, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MfaConfirmRequest {
    /// The 6-digit code; spaces and `-` are ignored.
    #[garde(length(chars, max = CODE_MAX_CHARS), custom(totp_code))]
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<CODE_MAX_CHARS>::schema)]
    pub code: SecretText<CODE_MAX_CHARS>,
}

/// The confirmed authenticator's recovery codes: shown once, each usable
/// once instead of a code from the app.
#[derive(Debug, Serialize, Deserialize, TS, ToSchema)]
pub struct MfaConfirmResponse {
    /// The 10 codes, as `abcd-efgh-ijkl-mnop-qrst`.
    #[ts(as = "Vec<String>")]
    #[schema(schema_with = recovery_codes_schema)]
    pub recovery_codes: Vec<SecretText<RECOVERY_CODE_MAX_CHARS>>,
}

/// `POST /me/mfa/reset`: a setup token for replacing the authenticator.
/// Until confirm, the current one keeps working.
#[derive(Debug, Serialize, Deserialize, TS, ToSchema)]
pub struct MfaResetResponse {
    /// The setup token for enroll and confirm.
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<TOKEN_MAX_CHARS>::schema)]
    pub setup_token: SecretText<TOKEN_MAX_CHARS>,
    /// How many seconds the token has left.
    #[schema(schema_with = integer_schema::<0, SETUP_TOKEN_MAX_SECS>)]
    pub expires_in: u32,
}

/// The garde rule of a code field that only takes a TOTP code: after
/// dropping spaces and `-`, exactly 6 digits. The message never holds the
/// text. garde passes its context (`()`) by reference; it's generic here
/// because the rule needs none.
fn totp_code<const MAX: usize, C>(code: &SecretText<MAX>, _context: &C) -> garde::Result {
    match fleet_core::auth::SecondFactor::parse(code.expose()) {
        Some(fleet_core::auth::SecondFactor::Totp(_)) => Ok(()),
        _ => Err(garde::Error::new(
            "must be the 6-digit code from the authenticator app",
        )),
    }
}

/// An array of recovery codes as shown.
fn recovery_codes_schema() -> RefOr<Schema> {
    ArrayBuilder::new()
        .items(SecretText::<RECOVERY_CODE_MAX_CHARS>::schema())
        .into()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn confirm(json: serde_json::Value) -> Result<MfaConfirmRequest, serde_json::Error> {
        serde_json::from_value(json)
    }

    /// The field paths and messages of a validation's report.
    fn problems(request: &MfaConfirmRequest) -> Vec<(String, String)> {
        request.validate().map_or_else(
            |report| {
                report
                    .iter()
                    .map(|(path, error)| (path.to_string(), error.message().to_owned()))
                    .collect()
            },
            |()| Vec::new(),
        )
    }

    #[test]
    fn a_six_digit_code_is_valid() {
        for code in ["123456", "123 456", "123-456"] {
            let request = confirm(json!({ "code": code })).unwrap();

            assert_eq!(problems(&request), [], "{code}");
        }
    }

    #[test]
    fn a_code_that_isnt_six_digits_breaks_its_rule_without_showing_it() {
        for code in ["12345", "1234567", "abcdef", "abz2-7a7a-hijk-lmno-pqrs", ""] {
            let request = confirm(json!({ "code": code })).unwrap();

            let found = problems(&request);

            assert_eq!(
                found,
                [(
                    "code".to_owned(),
                    "must be the 6-digit code from the authenticator app".to_owned()
                )],
                "{code}"
            );
        }
    }

    #[test]
    fn a_code_longer_than_its_cap_breaks_the_length_rule() {
        let request = confirm(json!({ "code": "1".repeat(CODE_MAX_CHARS + 1) })).unwrap();

        let found = problems(&request);

        assert!(
            found.contains(&("code".to_owned(), "length is greater than 64".to_owned())),
            "{found:?}"
        );
        assert!(found.iter().all(|(_, message)| !message.contains("1111")));
    }

    #[test]
    fn a_confirm_request_refuses_unknown_and_missing_fields() {
        assert!(confirm(json!({ "code": "123456", "extra": 1 })).is_err());
        assert!(confirm(json!({})).is_err());
        assert!(confirm(json!({ "code": 123_456 })).is_err());
    }

    #[test]
    fn no_dto_shows_a_secret_in_debug() {
        let enroll = MfaEnrollResponse {
            otpauth_uri: SecretText::new("otpauth://totp/x?secret=SECRETSECRET".to_owned()),
            qr_svg: SecretText::new("data:image/svg+xml;base64,QRQRQR".to_owned()),
            secret: SecretText::new("SECRETSECRET".to_owned()),
        };
        let request = confirm(json!({ "code": "987654" })).unwrap();
        let codes = MfaConfirmResponse {
            recovery_codes: vec![SecretText::new("abcd-efgh-ijkl-mnop-qrst".to_owned())],
        };
        let reset = MfaResetResponse {
            setup_token: SecretText::new("afk_st_TOKENTOKEN".to_owned()),
            expires_in: 600,
        };

        let shown = format!("{enroll:?} {request:?} {codes:?} {reset:?}");

        for secret in ["SECRET", "QRQR", "987654", "abcd", "TOKEN"] {
            assert!(!shown.contains(secret), "{shown}");
        }
    }

    #[test]
    fn the_responses_round_trip() {
        let enroll = json!({ "otpauth_uri": "otpauth://x", "qr_svg": "data:x", "secret": "ABC" });
        let codes = json!({ "recovery_codes": ["abcd-efgh-ijkl-mnop-qrst"] });
        let reset = json!({ "setup_token": "afk_st_x", "expires_in": 600 });

        let enroll_back: MfaEnrollResponse = serde_json::from_value(enroll.clone()).unwrap();
        let codes_back: MfaConfirmResponse = serde_json::from_value(codes.clone()).unwrap();
        let reset_back: MfaResetResponse = serde_json::from_value(reset.clone()).unwrap();

        assert_eq!(serde_json::to_value(enroll_back).unwrap(), enroll);
        assert_eq!(serde_json::to_value(codes_back).unwrap(), codes);
        assert_eq!(serde_json::to_value(reset_back).unwrap(), reset);
    }

    #[test]
    fn a_response_ignores_a_field_a_newer_server_adds() {
        let reset: MfaResetResponse =
            serde_json::from_value(json!({ "setup_token": "afk_st_x", "expires_in": 1, "new": 2 }))
                .unwrap();

        assert_eq!(reset.expires_in, 1);
    }
}
