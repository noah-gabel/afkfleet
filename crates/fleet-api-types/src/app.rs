//! The desktop app's IPC types (Plan.md P8.4, ADR-0017): what the app's Rust
//! side and its UI exchange through Tauri commands. None of them crosses the
//! HTTP API, so the `OpenAPI` document leaves them out; `just gen` exports
//! them with the DTOs, and the UI never hand-writes them.
//!
//! - A failed command answers a [`CommandError`]. Its [`CommandErrorCode`]
//!   is a closed set, since the app's Rust side and its UI ship in one exe;
//!   a refusal from the server is `api`, with the server's own open code in
//!   [`ApiErrorDetails`].
//! - No type here holds a token: the client keeps the access and step tokens,
//!   the keychain the refresh token. A login's answer is only the step that
//!   follows ([`LoginStepResponse`]).
//! - The server profile goes in as a [`ServerProfileRequest`] and comes back
//!   as a [`ServerProfileView`]: the origin and the custom CA's fingerprints,
//!   never the CA's text.
//! - The live events reach the UI as [`LiveUpdateMessage`]s on the
//!   `watch_events` command's channel: the server's frames, why to fetch
//!   again, and why the socket went down with its wait; never the ticket or
//!   the token the socket spent.

use garde::Validate;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{ErrorCode, FieldError, MESSAGE_MAX_CHARS, REQUEST_ID_MAX_CHARS};
use crate::events::EventMessage;
use crate::open::Open;
use crate::text::BoundedText;

/// The most bytes of [`ServerProfileRequest::url`]: far more than any origin,
/// it only bounds the work before the URL is parsed.
pub const SERVER_URL_MAX_BYTES: usize = 2048;
/// The most bytes of [`ServerProfileRequest::ca_pem`]: fleet-client's
/// `CustomCa::MAX_BYTES`, which this crate can't name; the desktop crate
/// tests that both are equal.
pub const CA_PEM_MAX_BYTES: usize = 65_536;

/// What went wrong with a command. The app's Rust side and its UI ship
/// together, so the UI knows every code; the server's code, which can grow,
/// is in [`ApiErrorDetails::code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CommandErrorCode {
    /// The server can't be reached.
    Unreachable,
    /// The server's certificate was refused.
    Certificate,
    /// The server didn't answer in time.
    TimedOut,
    /// The server's answer was larger than the client reads.
    TooLarge,
    /// The server's answer isn't one the client understands.
    UnexpectedResponse,
    /// The server refused the request: [`CommandError::api`] says how.
    Api,
    /// There's no session: log in.
    NoSession,
    /// The command needs a pending login or 2FA step, and there's none.
    NoPendingStep,
    /// A session is already held: log out first.
    AlreadyLoggedIn,
    /// The keychain failed.
    Store,
    /// A refresh or a code step ended without an answer.
    Interrupted,
    /// No server is set up yet.
    NoServer,
    /// An input broke a rule: [`CommandError::fields`] lists them.
    InvalidInput,
    /// The server's address can't be used: [`CommandError::fields`] says why.
    InvalidServerUrl,
    /// The custom CA can't be used: [`CommandError::fields`] says why.
    InvalidCa,
    /// The connection to the server can't be set up.
    ClientBuild,
    /// The server profile can't be saved. Only after a server switch has
    /// logged out of the old server: the old profile stays, logged out.
    ProfileSave,
    /// The new server's stored session can't be removed from the keychain.
    /// Only after a server switch has logged out of the old server: the old
    /// profile stays, logged out.
    SessionDiscard,
    /// The server's link to Microsoft's sign-in page breaks the rule the app
    /// checks it with (ADR-0018): the start's answer, its code included, is
    /// dropped.
    InvalidVerificationLink,
    /// There's no checked link to Microsoft's sign-in page to open: no link
    /// was started since the last sign-in.
    NoVerificationLink,
    /// Windows couldn't open Microsoft's sign-in page.
    VerificationLinkOpen,
    /// The app's own call didn't reach the command. Only the UI produces
    /// this code, for a refusal from Tauri itself.
    Ipc,
}

impl CommandErrorCode {
    /// Every code, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::Unreachable,
        Self::Certificate,
        Self::TimedOut,
        Self::TooLarge,
        Self::UnexpectedResponse,
        Self::Api,
        Self::NoSession,
        Self::NoPendingStep,
        Self::AlreadyLoggedIn,
        Self::Store,
        Self::Interrupted,
        Self::NoServer,
        Self::InvalidInput,
        Self::InvalidServerUrl,
        Self::InvalidCa,
        Self::ClientBuild,
        Self::ProfileSave,
        Self::SessionDiscard,
        Self::InvalidVerificationLink,
        Self::NoVerificationLink,
        Self::VerificationLinkOpen,
        Self::Ipc,
    ];

    /// The code's fixed sentence: the same for every error with this code,
    /// never runtime data.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Unreachable => "The server can't be reached.",
            Self::Certificate => "The server's certificate isn't trusted.",
            Self::TimedOut => "The server didn't answer in time.",
            Self::TooLarge => "The server's answer is too large.",
            Self::UnexpectedResponse => "The server's answer isn't one this app understands.",
            Self::Api => "The server refused the request.",
            Self::NoSession => "You're not logged in.",
            Self::NoPendingStep => "No login or 2FA step is pending. Start again.",
            Self::AlreadyLoggedIn => "You're already logged in. Log out first.",
            Self::Store => "The Windows Credential Manager failed.",
            Self::Interrupted => "The request ended without an answer. Try again.",
            Self::NoServer => "No server is set up yet.",
            Self::InvalidInput => "Some inputs are invalid.",
            Self::InvalidServerUrl => "The server's address can't be used.",
            Self::InvalidCa => "The CA certificates can't be used.",
            Self::ClientBuild => "The connection to the server can't be set up.",
            Self::ProfileSave => "The server profile can't be saved.",
            Self::SessionDiscard => {
                "The new server's old session can't be removed from the Windows Credential Manager."
            }
            Self::InvalidVerificationLink => "The server's sign-in link isn't Microsoft's page.",
            Self::NoVerificationLink => "There's no Microsoft page to open.",
            Self::VerificationLinkOpen => "Windows couldn't open Microsoft's page.",
            Self::Ipc => "The app couldn't run the request.",
        }
    }
}

/// A failed command. Nothing in it holds a token, a URL or a raw body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct CommandError {
    /// What went wrong.
    pub code: CommandErrorCode,
    /// The code's fixed sentence ([`CommandErrorCode::message`]).
    #[ts(type = "string")]
    pub message: BoundedText<MESSAGE_MAX_CHARS>,
    /// The server's refusal; only for `api`.
    // Boxed, so every `Result<_, CommandError>` stays small.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub api: Option<Box<ApiErrorDetails>>,
    /// The broken rules of `invalid_input`, `invalid_server_url` and
    /// `invalid_ca`, each naming the rule, never the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fields: Option<Vec<FieldError>>,
}

impl CommandError {
    /// An error with `code`'s fixed message.
    #[must_use]
    pub fn new(code: CommandErrorCode) -> Self {
        Self {
            code,
            message: BoundedText::new(code.message()),
            api: None,
            fields: None,
        }
    }

    /// An error with `code`'s fixed message and the rules `fields` broke.
    #[must_use]
    pub fn with_fields(code: CommandErrorCode, fields: Vec<FieldError>) -> Self {
        Self {
            fields: Some(fields),
            ..Self::new(code)
        }
    }

    /// The server's refusal, as `api`.
    #[must_use]
    pub fn api(details: ApiErrorDetails) -> Self {
        Self {
            api: Some(Box::new(details)),
            ..Self::new(CommandErrorCode::Api)
        }
    }
}

/// The server's refusal: its error envelope, with the status and the wait it
/// asks for. Code that branches on [`ApiErrorDetails::code`] keeps a default
/// branch that shows the message and the request ID, since a newer server
/// may send a code this version doesn't know (ADR-0015).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApiErrorDetails {
    /// The HTTP status.
    pub status: u16,
    /// The server's error code.
    #[ts(as = "ErrorCode")]
    pub code: Open<ErrorCode>,
    /// The server's message, cleaned and capped.
    #[ts(type = "string")]
    pub message: BoundedText<MESSAGE_MAX_CHARS>,
    /// The request's ID, for the server's log.
    #[ts(type = "string")]
    pub request_id: BoundedText<REQUEST_ID_MAX_CHARS>,
    /// The fields a 422 refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fields: Option<Vec<FieldError>>,
    /// How many seconds to wait before trying again: a 429's or a 503's
    /// `Retry-After`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub retry_after_secs: Option<u32>,
}

/// Which step follows the password.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum LoginStepKind {
    /// A code from the authenticator app, or a recovery code: `login_mfa`.
    MfaRequired,
    /// The user has no 2FA yet: `mfa_enroll`, `mfa_confirm`, then log in
    /// again.
    SetupRequired,
}

/// The `login` command's answer: the step that follows. Its token stays in
/// the app's Rust side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct LoginStepResponse {
    /// What comes next.
    pub step: LoginStepKind,
    /// How many seconds the step has left: restart the login once it's out,
    /// since an expired step answers like a wrong code.
    pub expires_in: u32,
}

/// The `mfa_reset` command's answer: the setup step for `mfa_enroll` and
/// `mfa_confirm` has started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SetupStepResponse {
    /// How many seconds the setup step has left.
    pub expires_in: u32,
}

/// The `logout` command's answer. The session always ends in the app; this
/// says whether the server confirmed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "server", rename_all = "snake_case")]
pub enum LogoutResponse {
    /// The server ended the session too, or there was none.
    Ended,
    /// The server couldn't be told: its session lives on until it expires.
    Unconfirmed {
        /// Why the server couldn't be told.
        error: CommandError,
    },
}

/// The `set_server_profile` command's input: the server's address and,
/// optionally, the CA its certificate comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Validate, TS)]
#[serde(deny_unknown_fields)]
pub struct ServerProfileRequest {
    /// The server's origin, such as `https://example.com`.
    #[garde(length(bytes, max = SERVER_URL_MAX_BYTES))]
    pub url: String,
    /// The custom CA's certificates as PEM; without it, the system's roots
    /// are trusted.
    #[garde(length(bytes, max = CA_PEM_MAX_BYTES))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ca_pem: Option<String>,
}

/// The server profile as the UI shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ServerProfileView {
    /// The server's origin, such as `https://example.com/`.
    pub url: String,
    /// The custom CA, when the profile has one; the system's roots otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub custom_ca: Option<CustomCaView>,
}

/// A profile's custom CA, as the UI shows it: never its text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct CustomCaView {
    /// How many certificates it holds.
    pub certificates: u32,
    /// Each certificate's SHA-256 fingerprint over its DER, in file order,
    /// as uppercase hex pairs joined by colons, as openssl prints them.
    pub fingerprints: Vec<String>,
}

/// The `set_server_profile` command's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum ServerProfileResponse {
    /// The profile was already this one: nothing changed, and the session
    /// is kept.
    #[serde(rename = "none")]
    Unchanged {
        /// The profile.
        profile: ServerProfileView,
    },
    /// The app now uses this profile. Any session with the old server was
    /// logged out first.
    Switched {
        /// The new profile.
        profile: ServerProfileView,
        /// How the old session's logout went.
        logout: LogoutResponse,
    },
}

/// The `server_profile` command's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ServerProfileState {
    /// No server is set up yet.
    #[serde(rename = "none")]
    NotSet,
    /// The app uses this profile.
    Set {
        /// The profile.
        profile: ServerProfileView,
    },
    /// The saved profile can't be used: set the server up again.
    Unreadable {
        /// What's wrong with it.
        reason: ProfileUnreadable,
    },
}

/// What's wrong with a saved server profile. It never repeats the file's
/// content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ProfileUnreadable {
    /// The file can't be read, or it's larger than a profile can be.
    Io,
    /// It isn't a profile's JSON.
    Json,
    /// It's from a version of the app this one doesn't know.
    Version,
    /// Its server address can't be used.
    Url,
    /// Its custom CA can't be used.
    Ca,
    /// The connection can't be set up with it.
    Client,
}

/// One update on the `watch_events` command's channel: what the live events
/// socket says next (Plan.md P11.8; ADR-0020, S59). No token and no ticket
/// is in it: the client spends both inside itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "update", rename_all = "snake_case")]
pub enum LiveUpdateMessage {
    /// Fetch everything shown: after every `ready`, a `resync`, or a frame
    /// that didn't read. It supersedes the events before it.
    Refetch {
        /// Why.
        reason: LiveRefetchReason,
    },
    /// An event of a kind this version knows.
    Event {
        /// The event, as the server built it for this viewer.
        // Boxed, so the other updates stay small.
        event: Box<EventMessage>,
    },
    /// The socket is down; it connects again once `retry_in_secs` passed.
    Disconnected {
        /// Why.
        disconnect: LiveDisconnect,
        /// Seconds until the next connect, rounded up: 0 for the reconnect
        /// an access token's end asks for, which no one needs to see.
        retry_in_secs: u32,
    },
}

/// Why everything is to be fetched again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum LiveRefetchReason {
    /// The socket is live: it says `ready` after every connect.
    Ready,
    /// The server says some events didn't reach this socket.
    Resync,
    /// A frame didn't read, so one may have been missed.
    Unreadable,
}

/// Why the live events socket went down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "why", rename_all = "snake_case")]
pub enum LiveDisconnect {
    /// The server closed it, with this code, or without one.
    Closed {
        /// The close code.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        code: Option<u16>,
    },
    /// The connection went without a close, or broke the protocol.
    Lost,
    /// No frame came in time: the server is taken for gone.
    Silent,
    /// A connect failed: its ticket's request, or its upgrade.
    NotOpened {
        /// What failed, as any command's error says it.
        error: CommandError,
    },
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::*;
    use crate::events::EventMessageKind;

    /// Each code's position, through an exhaustive match: a new variant
    /// doesn't compile until it's added here, and then the test below fails
    /// until it's in `ALL` too.
    fn index(code: CommandErrorCode) -> usize {
        match code {
            CommandErrorCode::Unreachable => 0,
            CommandErrorCode::Certificate => 1,
            CommandErrorCode::TimedOut => 2,
            CommandErrorCode::TooLarge => 3,
            CommandErrorCode::UnexpectedResponse => 4,
            CommandErrorCode::Api => 5,
            CommandErrorCode::NoSession => 6,
            CommandErrorCode::NoPendingStep => 7,
            CommandErrorCode::AlreadyLoggedIn => 8,
            CommandErrorCode::Store => 9,
            CommandErrorCode::Interrupted => 10,
            CommandErrorCode::NoServer => 11,
            CommandErrorCode::InvalidInput => 12,
            CommandErrorCode::InvalidServerUrl => 13,
            CommandErrorCode::InvalidCa => 14,
            CommandErrorCode::ClientBuild => 15,
            CommandErrorCode::ProfileSave => 16,
            CommandErrorCode::SessionDiscard => 17,
            CommandErrorCode::InvalidVerificationLink => 18,
            CommandErrorCode::NoVerificationLink => 19,
            CommandErrorCode::VerificationLinkOpen => 20,
            CommandErrorCode::Ipc => 21,
        }
    }

    const CODES: usize = 22;

    fn details() -> ApiErrorDetails {
        ApiErrorDetails {
            status: 429,
            code: ErrorCode::RateLimited.into(),
            message: BoundedText::new(ErrorCode::RateLimited.message()),
            request_id: BoundedText::new("01900000-0000-7000-8000-000000000009"),
            fields: None,
            retry_after_secs: Some(30),
        }
    }

    fn view() -> ServerProfileView {
        ServerProfileView {
            url: "https://example.com/".to_owned(),
            custom_ca: None,
        }
    }

    #[test]
    fn all_lists_every_code_exactly_once() {
        let mut seen = [0_u8; CODES];
        for code in CommandErrorCode::ALL {
            seen[index(*code)] += 1;
        }

        assert_eq!(CommandErrorCode::ALL.len(), CODES);
        assert_eq!(seen, [1; CODES]);
    }

    #[test]
    fn every_code_has_its_own_fixed_sentence() {
        let messages: BTreeSet<&str> = CommandErrorCode::ALL
            .iter()
            .map(|code| code.message())
            .collect();

        assert_eq!(messages.len(), CODES, "{messages:?}");
        assert!(messages.iter().all(|message| message.ends_with('.')));
    }

    #[test]
    fn the_wire_names_are_snake_case() {
        let names: Vec<_> = [
            CommandErrorCode::TimedOut,
            CommandErrorCode::UnexpectedResponse,
            CommandErrorCode::InvalidServerUrl,
            CommandErrorCode::InvalidVerificationLink,
            CommandErrorCode::NoVerificationLink,
            CommandErrorCode::VerificationLinkOpen,
            CommandErrorCode::Ipc,
        ]
        .iter()
        .map(|code| serde_json::to_value(code).unwrap())
        .collect();

        assert_eq!(
            names,
            [
                json!("timed_out"),
                json!("unexpected_response"),
                json!("invalid_server_url"),
                json!("invalid_verification_link"),
                json!("no_verification_link"),
                json!("verification_link_open"),
                json!("ipc")
            ]
        );
    }

    #[test]
    fn an_error_without_details_has_only_its_code_and_message() {
        let error = CommandError::new(CommandErrorCode::NoServer);

        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({ "code": "no_server", "message": CommandErrorCode::NoServer.message() })
        );
    }

    #[test]
    fn a_refusal_carries_the_servers_details_and_leaves_out_what_it_lacks() {
        let error = CommandError::api(details());

        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({
                "code": "api",
                "message": CommandErrorCode::Api.message(),
                "api": {
                    "status": 429,
                    "code": "rate_limited",
                    "message": ErrorCode::RateLimited.message(),
                    "request_id": "01900000-0000-7000-8000-000000000009",
                    "retry_after_secs": 30
                }
            })
        );
    }

    #[test]
    fn an_unknown_server_code_reads_back_as_it_came() {
        let json = json!({
            "code": "api",
            "message": CommandErrorCode::Api.message(),
            "api": {
                "status": 418,
                "code": "from_a_newer_server",
                "message": "Something new.",
                "request_id": "r1"
            }
        });

        let error: CommandError = serde_json::from_value(json.clone()).unwrap();

        assert_eq!(
            error.api.as_ref().unwrap().code.as_str(),
            "from_a_newer_server"
        );
        assert_eq!(serde_json::to_value(&error).unwrap(), json);
    }

    #[test]
    fn broken_rules_are_listed_as_fields() {
        let error = CommandError::with_fields(
            CommandErrorCode::InvalidCa,
            vec![FieldError::new("ca_pem", "holds no certificate")],
        );

        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({
                "code": "invalid_ca",
                "message": CommandErrorCode::InvalidCa.message(),
                "fields": [{ "path": "ca_pem", "message": "holds no certificate" }]
            })
        );
    }

    #[test]
    fn the_login_step_names_the_step_and_its_lifetime() {
        let answers = [
            LoginStepResponse {
                step: LoginStepKind::MfaRequired,
                expires_in: 300,
            },
            LoginStepResponse {
                step: LoginStepKind::SetupRequired,
                expires_in: 600,
            },
        ]
        .map(|answer| serde_json::to_value(answer).unwrap());

        assert_eq!(
            answers,
            [
                json!({ "step": "mfa_required", "expires_in": 300 }),
                json!({ "step": "setup_required", "expires_in": 600 })
            ]
        );
        assert_eq!(
            serde_json::to_value(SetupStepResponse { expires_in: 600 }).unwrap(),
            json!({ "expires_in": 600 })
        );
    }

    #[test]
    fn a_logout_says_whether_the_server_confirmed_it() {
        let ended = serde_json::to_value(LogoutResponse::Ended).unwrap();
        let unconfirmed = serde_json::to_value(LogoutResponse::Unconfirmed {
            error: CommandError::new(CommandErrorCode::Unreachable),
        })
        .unwrap();

        assert_eq!(ended, json!({ "server": "ended" }));
        assert_eq!(
            unconfirmed,
            json!({
                "server": "unconfirmed",
                "error": { "code": "unreachable", "message": CommandErrorCode::Unreachable.message() }
            })
        );
        assert_eq!(
            serde_json::from_value::<LogoutResponse>(unconfirmed).unwrap(),
            LogoutResponse::Unconfirmed {
                error: CommandError::new(CommandErrorCode::Unreachable)
            }
        );
    }

    #[test]
    fn a_profile_change_says_whether_anything_changed() {
        let unchanged = ServerProfileResponse::Unchanged { profile: view() };
        let switched = ServerProfileResponse::Switched {
            profile: view(),
            logout: LogoutResponse::Ended,
        };

        assert_eq!(
            serde_json::to_value(&unchanged).unwrap(),
            json!({ "change": "none", "profile": { "url": "https://example.com/" } })
        );
        assert_eq!(
            serde_json::to_value(&switched).unwrap(),
            json!({
                "change": "switched",
                "profile": { "url": "https://example.com/" },
                "logout": { "server": "ended" }
            })
        );
    }

    #[test]
    fn the_profile_state_is_none_set_or_unreadable_with_a_reason() {
        let with_ca = ServerProfileView {
            custom_ca: Some(CustomCaView {
                certificates: 1,
                fingerprints: vec!["AB:CD".to_owned()],
            }),
            ..view()
        };
        let states = [
            ServerProfileState::NotSet,
            ServerProfileState::Set { profile: with_ca },
            ServerProfileState::Unreadable {
                reason: ProfileUnreadable::Client,
            },
        ]
        .map(|state| serde_json::to_value(state).unwrap());

        assert_eq!(
            states,
            [
                json!({ "state": "none" }),
                json!({
                    "state": "set",
                    "profile": {
                        "url": "https://example.com/",
                        "custom_ca": { "certificates": 1, "fingerprints": ["AB:CD"] }
                    }
                }),
                json!({ "state": "unreadable", "reason": "client" })
            ]
        );
    }

    #[test]
    fn a_profile_request_without_a_ca_leaves_it_out() {
        let request = ServerProfileRequest {
            url: "https://example.com".to_owned(),
            ca_pem: None,
        };

        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({ "url": "https://example.com" })
        );
        assert_eq!(
            serde_json::from_value::<ServerProfileRequest>(json!({ "url": "https://example.com" }))
                .unwrap(),
            request
        );
        assert!(request.validate().is_ok());
    }

    #[test]
    fn a_profile_request_with_an_unknown_field_is_refused() {
        let result = serde_json::from_value::<ServerProfileRequest>(
            json!({ "url": "https://example.com", "insecure": true }),
        );

        assert!(result.is_err());
    }

    #[test]
    fn the_url_and_the_ca_are_capped_in_bytes() {
        let at_cap = ServerProfileRequest {
            url: "a".repeat(SERVER_URL_MAX_BYTES),
            ca_pem: Some("b".repeat(CA_PEM_MAX_BYTES)),
        };
        let over = ServerProfileRequest {
            url: "é".repeat(SERVER_URL_MAX_BYTES / 2 + 1),
            ca_pem: Some("b".repeat(CA_PEM_MAX_BYTES + 1)),
        };

        let report = over.validate().unwrap_err();
        let paths: BTreeSet<String> = report.iter().map(|(path, _)| path.to_string()).collect();

        assert!(at_cap.validate().is_ok());
        assert_eq!(
            paths,
            BTreeSet::from(["ca_pem".to_owned(), "url".to_owned()])
        );
    }

    const BOT: &str = "01900000-0000-7000-8000-000000000001";

    #[test]
    fn each_live_update_is_tagged_by_what_it_is_and_reads_back() {
        let removed = EventMessage::bot_removed(serde_json::from_value(json!(BOT)).unwrap());
        let updates = [
            (
                LiveUpdateMessage::Refetch {
                    reason: LiveRefetchReason::Ready,
                },
                json!({ "update": "refetch", "reason": "ready" }),
            ),
            (
                LiveUpdateMessage::Event {
                    event: Box::new(removed),
                },
                json!({ "update": "event", "event": { "type": "bot_removed", "bot_id": BOT } }),
            ),
            (
                LiveUpdateMessage::Disconnected {
                    disconnect: LiveDisconnect::Closed { code: Some(1001) },
                    retry_in_secs: 2,
                },
                json!({
                    "update": "disconnected",
                    "disconnect": { "why": "closed", "code": 1001 },
                    "retry_in_secs": 2
                }),
            ),
        ];

        for (update, expected) in updates {
            assert_eq!(serde_json::to_value(&update).unwrap(), expected);
            let back: LiveUpdateMessage = serde_json::from_value(expected).unwrap();
            assert_eq!(back, update);
        }
        assert_eq!(
            [
                LiveRefetchReason::Ready,
                LiveRefetchReason::Resync,
                LiveRefetchReason::Unreadable
            ]
            .map(|reason| serde_json::to_value(reason).unwrap()),
            [json!("ready"), json!("resync"), json!("unreadable")]
        );
        assert_eq!(
            serde_json::from_value::<EventMessage>(json!({ "type": "bot_removed", "bot_id": BOT }))
                .unwrap()
                .kind
                .known(),
            Some(EventMessageKind::BotRemoved)
        );
    }

    #[test]
    fn each_disconnect_says_why_and_a_failed_connect_carries_its_error() {
        let disconnects = [
            (
                LiveDisconnect::Closed { code: None },
                json!({ "why": "closed" }),
            ),
            (LiveDisconnect::Lost, json!({ "why": "lost" })),
            (LiveDisconnect::Silent, json!({ "why": "silent" })),
            (
                LiveDisconnect::NotOpened {
                    error: CommandError::new(CommandErrorCode::Unreachable),
                },
                json!({
                    "why": "not_opened",
                    "error": {
                        "code": "unreachable",
                        "message": CommandErrorCode::Unreachable.message()
                    }
                }),
            ),
        ];

        for (disconnect, expected) in disconnects {
            assert_eq!(serde_json::to_value(&disconnect).unwrap(), expected);
            let back: LiveDisconnect = serde_json::from_value(expected).unwrap();
            assert_eq!(back, disconnect);
        }
    }
}
