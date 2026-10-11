//! The login's DTOs (Plan.md P7.6, §7.2, ADR-0016): the password step, the
//! code step, refreshing, and stepping up.
//!
//! Logging in takes two steps, and the first never hands out real tokens:
//! the password step answers an mfa token for the code step, or, for a
//! user without 2FA, a setup token for enrolling first ([`LoginStatus`]).
//! The code step answers the session's tokens ([`TokenResponse`]), as a
//! refresh does. Every secret here is a [`SecretText`].

use fleet_core::auth::SecondFactor;
use fleet_core::value::Username;
use garde::Validate;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::{PartialSchema as _, ToSchema};

use crate::int::integer_schema;
use crate::mfa::{CODE_MAX_CHARS, SETUP_TOKEN_MAX_SECS, TOKEN_MAX_CHARS};
use crate::open::{Open, OpenEnum};
use crate::secret::SecretText;

/// The most characters of a username field: a `Username`'s longest.
pub const USERNAME_MAX_CHARS: usize = 32;
/// The most characters of a password field at login and step-up: generous,
/// 8 × a new password's longest, so a longer one is refused before any
/// hashing.
pub const PASSWORD_MAX_CHARS: usize = 1024;
/// The longest an access token lives, in seconds (`[auth]
/// access_token_ttl_secs`' maximum).
pub const ACCESS_TOKEN_MAX_SECS: u32 = 3600;
/// The longest a refresh token lives, in seconds (`[auth]
/// refresh_token_absolute_days`' maximum of 365 days).
pub const REFRESH_TOKEN_MAX_SECS: u32 = 31_536_000;

/// `POST /auth/login`: the password step.
#[derive(Debug, Deserialize, Validate, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    /// The username, in any case.
    #[garde(custom(valid_username))]
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<USERNAME_MAX_CHARS>::schema)]
    pub username: SecretText<USERNAME_MAX_CHARS>,
    /// The password.
    #[garde(length(chars, max = PASSWORD_MAX_CHARS))]
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<PASSWORD_MAX_CHARS>::schema)]
    pub password: SecretText<PASSWORD_MAX_CHARS>,
}

/// What the password step leads to.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on it keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LoginStatus {
    /// Send a code from the authenticator app, or a recovery code, with
    /// the mfa token to `POST /auth/login/mfa`.
    MfaRequired,
    /// The user has no 2FA yet: enroll and confirm an authenticator with
    /// the setup token, then log in again.
    MfaSetupRequired,
}

impl OpenEnum for LoginStatus {
    const ALL: &'static [Self] = &[Self::MfaRequired, Self::MfaSetupRequired];

    fn as_str(self) -> &'static str {
        match self {
            Self::MfaRequired => "mfa_required",
            Self::MfaSetupRequired => "mfa_setup_required",
        }
    }
}

/// The password step's answer: never real tokens.
#[derive(Debug, Serialize, Deserialize, TS, ToSchema)]
pub struct LoginResponse {
    /// What comes next.
    #[ts(as = "LoginStatus")]
    #[schema(value_type = LoginStatus)]
    pub status: Open<LoginStatus>,
    /// The mfa token (`afk_mt_…`, single use) or the setup token
    /// (`afk_st_…`).
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<TOKEN_MAX_CHARS>::schema)]
    pub token: SecretText<TOKEN_MAX_CHARS>,
    /// How many seconds the token has left: restart the step once it's out.
    #[schema(schema_with = integer_schema::<0, SETUP_TOKEN_MAX_SECS>)]
    pub expires_in: u32,
}

/// `POST /auth/login/mfa`: the code step.
#[derive(Debug, Deserialize, Validate, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LoginMfaRequest {
    /// The password step's mfa token.
    #[garde(length(chars, max = TOKEN_MAX_CHARS))]
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<TOKEN_MAX_CHARS>::schema)]
    pub mfa_token: SecretText<TOKEN_MAX_CHARS>,
    /// A code from the authenticator app (6 digits) or a recovery code (20
    /// characters); spaces and `-` are ignored, and so is case.
    #[garde(length(chars, max = CODE_MAX_CHARS), custom(second_factor))]
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<CODE_MAX_CHARS>::schema)]
    pub code: SecretText<CODE_MAX_CHARS>,
}

/// A session's tokens: the code step's and a refresh's answer.
#[derive(Debug, Serialize, Deserialize, TS, ToSchema)]
pub struct TokenResponse {
    /// The access token (`afk_at_…`), sent with every request.
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<TOKEN_MAX_CHARS>::schema)]
    pub access_token: SecretText<TOKEN_MAX_CHARS>,
    /// The refresh token (`afk_rt_…`), sent only to refresh.
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<TOKEN_MAX_CHARS>::schema)]
    pub refresh_token: SecretText<TOKEN_MAX_CHARS>,
    /// How many seconds the access token has left.
    #[schema(schema_with = integer_schema::<0, ACCESS_TOKEN_MAX_SECS>)]
    pub access_expires_in: u32,
    /// How many seconds the refresh token has left, unless it's used first.
    #[schema(schema_with = integer_schema::<0, REFRESH_TOKEN_MAX_SECS>)]
    pub refresh_expires_in: u32,
}

/// `POST /auth/refresh`: a new token pair for the refresh token.
#[derive(Debug, Deserialize, Validate, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RefreshRequest {
    /// The current refresh token.
    #[garde(length(chars, max = TOKEN_MAX_CHARS))]
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<TOKEN_MAX_CHARS>::schema)]
    pub refresh_token: SecretText<TOKEN_MAX_CHARS>,
}

/// `POST /auth/step-up`: the password again, which allows step-up actions
/// in this session for 5 minutes.
#[derive(Debug, Deserialize, Validate, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StepUpRequest {
    /// The password.
    #[garde(length(chars, max = PASSWORD_MAX_CHARS))]
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<PASSWORD_MAX_CHARS>::schema)]
    pub password: SecretText<PASSWORD_MAX_CHARS>,
}

/// The garde rule of a username: a valid `Username`. The message names the
/// rule, never the text, which may be a password typed into the wrong
/// field. garde passes its context (`()`) by reference.
fn valid_username<C>(name: &SecretText<USERNAME_MAX_CHARS>, _context: &C) -> garde::Result {
    Username::try_from(name.expose()).map(drop).map_err(|_| {
        garde::Error::new(
            "must be 3 to 32 letters, digits, `_`, `.` or `-`, starting with a letter or digit",
        )
    })
}

/// The garde rule of the code step's code: a TOTP code or a recovery code.
fn second_factor<C>(code: &SecretText<CODE_MAX_CHARS>, _context: &C) -> garde::Result {
    SecondFactor::parse(code.expose()).map(drop).ok_or_else(|| {
        garde::Error::new("must be the 6-digit code from the authenticator app or a recovery code")
    })
}

#[cfg(test)]
mod tests {
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};

    use super::*;

    fn read<T: DeserializeOwned>(json: Value) -> Result<T, serde_json::Error> {
        serde_json::from_value(json)
    }

    /// The field paths and messages a validation reports.
    fn problems(request: &impl Validate<Context = ()>) -> Vec<(String, String)> {
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

    fn login(username: &str, password: &str) -> LoginRequest {
        read(json!({ "username": username, "password": password })).unwrap()
    }

    const USERNAME_RULE: &str =
        "must be 3 to 32 letters, digits, `_`, `.` or `-`, starting with a letter or digit";

    #[test]
    fn a_valid_login_breaks_no_rule() {
        for name in ["alice", "AfkBot1", "a.b_c-d", &"a".repeat(32)] {
            assert_eq!(problems(&login(name, "")), [], "{name}");
        }
        assert_eq!(problems(&login("alice", &"p".repeat(1024))), []);
    }

    #[test]
    fn a_name_that_isnt_a_username_breaks_its_rule_without_showing_it() {
        for name in [
            "al",
            &"a".repeat(33),
            "-alice",
            "al ice",
            "älice",
            "hunter2 secret",
        ] {
            let found = problems(&login(name, "x"));

            assert_eq!(
                found,
                [("username".to_owned(), USERNAME_RULE.to_owned())],
                "{name}"
            );
        }
    }

    #[test]
    fn a_password_longer_than_1024_characters_breaks_its_rule() {
        let found = problems(&login("alice", &"ä".repeat(1025)));

        assert_eq!(
            found,
            [(
                "password".to_owned(),
                "length is greater than 1024".to_owned()
            )]
        );
    }

    #[test]
    fn a_code_that_is_neither_kind_breaks_its_rule() {
        let request: LoginMfaRequest =
            read(json!({ "mfa_token": "afk_mt_x", "code": "12345" })).unwrap();

        assert_eq!(
            problems(&request),
            [(
                "code".to_owned(),
                "must be the 6-digit code from the authenticator app or a recovery code".to_owned()
            )]
        );
    }

    #[test]
    fn either_kind_of_code_is_valid() {
        for code in ["123 456", "ABCD-efgh-ijkl-mnop-qrst"] {
            let request: LoginMfaRequest =
                read(json!({ "mfa_token": "afk_mt_x", "code": code })).unwrap();

            assert_eq!(problems(&request), [], "{code}");
        }
    }

    #[test]
    fn a_token_longer_than_64_characters_breaks_its_rule() {
        let long = "t".repeat(65);
        let mfa: LoginMfaRequest = read(json!({ "mfa_token": long, "code": "123456" })).unwrap();
        let refresh: RefreshRequest = read(json!({ "refresh_token": long })).unwrap();

        let expected = "length is greater than 64".to_owned();
        assert_eq!(problems(&mfa), [("mfa_token".to_owned(), expected.clone())]);
        assert_eq!(problems(&refresh), [("refresh_token".to_owned(), expected)]);
    }

    #[test]
    fn a_step_up_password_longer_than_1024_characters_breaks_its_rule() {
        let request: StepUpRequest = read(json!({ "password": "p".repeat(1025) })).unwrap();

        assert_eq!(
            problems(&request),
            [(
                "password".to_owned(),
                "length is greater than 1024".to_owned()
            )]
        );
    }

    #[test]
    fn every_request_refuses_unknown_missing_and_mistyped_fields() {
        let full = [
            json!({ "username": "alice", "password": "x" }),
            json!({ "mfa_token": "afk_mt_x", "code": "123456" }),
            json!({ "refresh_token": "afk_rt_x" }),
            json!({ "password": "x" }),
        ];
        for (index, valid) in full.iter().enumerate() {
            let mut extra = valid.clone();
            extra["extra"] = json!(1);
            let refused = |json: Value| match index {
                0 => read::<LoginRequest>(json).is_err(),
                1 => read::<LoginMfaRequest>(json).is_err(),
                2 => read::<RefreshRequest>(json).is_err(),
                _ => read::<StepUpRequest>(json).is_err(),
            };

            assert!(!refused(valid.clone()), "{valid}");
            assert!(refused(extra), "{valid} with an unknown field");
            assert!(refused(json!({})), "{valid} missing fields");
            let key = valid.as_object().unwrap().keys().next().unwrap().clone();
            let mut mistyped = valid.clone();
            mistyped[key.as_str()] = json!(42);
            assert!(refused(mistyped), "{valid} mistyped");
        }
    }

    #[test]
    fn no_dto_shows_a_secret_in_debug() {
        let request = login("hunter2user", "hunter2pass");
        let mfa: LoginMfaRequest =
            read(json!({ "mfa_token": "afk_mt_TOKENA", "code": "987654" })).unwrap();
        let refresh: RefreshRequest = read(json!({ "refresh_token": "afk_rt_TOKENB" })).unwrap();
        let step_up: StepUpRequest = read(json!({ "password": "hunter2pass" })).unwrap();
        let answer = LoginResponse {
            status: Open::from(LoginStatus::MfaRequired),
            token: SecretText::new("afk_mt_TOKENC".to_owned()),
            expires_in: 300,
        };
        let tokens = TokenResponse {
            access_token: SecretText::new("afk_at_TOKEND".to_owned()),
            refresh_token: SecretText::new("afk_rt_TOKENE".to_owned()),
            access_expires_in: 900,
            refresh_expires_in: 604_800,
        };

        let shown = format!("{request:?} {mfa:?} {refresh:?} {step_up:?} {answer:?} {tokens:?}");

        for secret in ["hunter2", "TOKEN", "987654"] {
            assert!(!shown.contains(secret), "{shown}");
        }
    }

    #[test]
    fn the_responses_round_trip() {
        let answer =
            json!({ "status": "mfa_setup_required", "token": "afk_st_x", "expires_in": 600 });
        let tokens = json!({
            "access_token": "afk_at_x",
            "refresh_token": "afk_rt_x",
            "access_expires_in": 900,
            "refresh_expires_in": 604_800,
        });

        let answer_back: LoginResponse = read(answer.clone()).unwrap();
        let tokens_back: TokenResponse = read(tokens.clone()).unwrap();

        assert_eq!(
            answer_back.status.known(),
            Some(LoginStatus::MfaSetupRequired)
        );
        assert_eq!(serde_json::to_value(answer_back).unwrap(), answer);
        assert_eq!(serde_json::to_value(tokens_back).unwrap(), tokens);
    }

    #[test]
    fn a_status_a_newer_server_sends_still_reads() {
        let answer: LoginResponse = read(
            json!({ "status": "password_change_required", "token": "afk_st_x", "expires_in": 1 }),
        )
        .unwrap();

        assert_eq!(answer.status.known(), None);
        assert_eq!(answer.status.as_str(), "password_change_required");
    }

    #[test]
    fn every_status_names_itself_as_serde_does() {
        for status in LoginStatus::ALL {
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                json!(status.as_str())
            );
        }
    }
}
