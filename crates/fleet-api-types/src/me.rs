//! The user's own DTOs (Plan.md P7.6, §7.2, ADR-0016): who they are
//! (`GET /me`) and changing their password (`POST /me/password`).
//!
//! `GET /me` also counts the wrong 2FA codes since the user's previous
//! login: a wrong code means someone had the password.

use fleet_core::id::UserId;
use garde::Validate;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::{PartialSchema as _, ToSchema};

use crate::auth::{NEW_PASSWORD_MAX_CHARS, USERNAME_MAX_CHARS, new_password_rule};
use crate::ids::id_schema;
use crate::int::integer_schema;
use crate::open::Open;
use crate::secret::SecretText;
use crate::text::BoundedText;
use crate::time::Timestamp;
use crate::users::UserRole;

/// The most recovery codes a user holds: a fresh set.
pub const RECOVERY_CODES_MAX: u32 = 10;

/// The wrong 2FA codes typed for the user after their previous successful
/// login: what happened since they last logged in. The wrong codes of the
/// newest login's own step, the user's typos before it succeeded, aren't
/// counted, so a count above 0 means a login step that never succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct FailedCodeAttempts {
    /// How many.
    #[schema(schema_with = integer_schema::<0, { u32::MAX }>)]
    pub count: u32,
    /// When the newest was typed; left out when there's none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = Timestamp::schema, nullable = false)]
    pub last_at: Option<Timestamp>,
}

/// `GET /me`: the user's profile and security facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct MeResponse {
    /// The ID.
    #[ts(type = "string")]
    #[schema(schema_with = id_schema)]
    pub id: UserId,
    /// The username, lowercase.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<USERNAME_MAX_CHARS>::schema)]
    pub username: BoundedText<USERNAME_MAX_CHARS>,
    /// The global role.
    #[ts(as = "UserRole")]
    #[schema(value_type = UserRole)]
    pub role: Open<UserRole>,
    /// When the account was created.
    #[ts(type = "string")]
    #[schema(schema_with = Timestamp::schema)]
    pub created_at: Timestamp,
    /// When the password was last set.
    #[ts(type = "string")]
    #[schema(schema_with = Timestamp::schema)]
    pub password_changed_at: Timestamp,
    /// When the current authenticator was confirmed. A user who logged in
    /// always has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = Timestamp::schema, nullable = false)]
    pub mfa_confirmed_at: Option<Timestamp>,
    /// How many unused recovery codes are left, of 10.
    #[schema(schema_with = integer_schema::<0, RECOVERY_CODES_MAX>)]
    pub recovery_codes_left: u32,
    /// The wrong 2FA codes since the previous login, without the newest
    /// login's own typos.
    pub failed_code_attempts: FailedCodeAttempts,
}

/// `POST /me/password`: a new password, after stepping up. Every other
/// session of the user ends, and so does this session's step-up.
#[derive(Debug, Serialize, Deserialize, Validate, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PasswordChangeRequest {
    /// The new password: 12 to 128 characters, no control characters,
    /// strong enough, and not the current one.
    #[garde(custom(new_password_rule))]
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<NEW_PASSWORD_MAX_CHARS>::schema)]
    pub new_password: SecretText<NEW_PASSWORD_MAX_CHARS>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn change(json: serde_json::Value) -> Result<PasswordChangeRequest, serde_json::Error> {
        serde_json::from_value(json)
    }

    fn problems(request: &PasswordChangeRequest) -> Vec<(String, String)> {
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
    fn a_new_password_in_its_limits_is_valid() {
        let request = change(json!({ "new_password": "correct horse battery staple" })).unwrap();

        assert_eq!(problems(&request), []);
    }

    #[test]
    fn a_short_new_password_breaks_its_rule() {
        let request = change(json!({ "new_password": "short" })).unwrap();

        assert_eq!(
            problems(&request),
            [(
                "new_password".to_owned(),
                "a password has at least 12 characters".to_owned()
            )]
        );
    }

    #[test]
    fn a_change_refuses_unknown_and_missing_fields() {
        assert!(change(json!({ "new_password": "x", "password": "y" })).is_err());
        assert!(change(json!({})).is_err());
    }

    #[test]
    fn a_change_shows_no_secret_in_debug() {
        let request = change(json!({ "new_password": "correct horse battery staple" })).unwrap();

        assert!(!format!("{request:?}").contains("horse"));
    }

    #[test]
    fn the_profile_round_trips_and_leaves_out_what_isnt_there() {
        let full = json!({
            "id": "018bcfe5-6800-7101-8101-010101010101",
            "username": "alice",
            "role": "member",
            "created_at": "2027-01-15T08:00:00.000Z",
            "password_changed_at": "2027-01-15T08:00:00.000Z",
            "mfa_confirmed_at": "2027-01-15T08:01:00.000Z",
            "recovery_codes_left": 9,
            "failed_code_attempts": { "count": 2, "last_at": "2027-01-16T08:00:00.000Z" }
        });
        let bare = json!({
            "id": "018bcfe5-6800-7101-8101-010101010101",
            "username": "alice",
            "role": "owner",
            "created_at": "2027-01-15T08:00:00.000Z",
            "password_changed_at": "2027-01-15T08:00:00.000Z",
            "recovery_codes_left": 0,
            "failed_code_attempts": { "count": 0 }
        });

        for profile in [full, bare] {
            let back: MeResponse = serde_json::from_value(profile.clone()).unwrap();

            assert_eq!(serde_json::to_value(back).unwrap(), profile);
        }
    }
}
