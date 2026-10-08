//! [`SessionCredentials`]: what a bot needs to log in to a Minecraft server,
//! and [`SessionCredentialProvider`], the port that hands them out.

use core::future::Future;

use chrono::{DateTime, Utc};
use secrecy::SecretString;
use uuid::Uuid;

use crate::bot::BotAccount;
use crate::id::BotId;
use crate::value::McUsername;

/// The credentials a session logs in with (ADR-0008 §9, ADR-0010).
///
/// An agent never holds a Microsoft token: the server sends it a short-lived
/// Minecraft session for each bot assigned to it (security rule 6). The
/// access token is a [`SecretString`], so it never shows up in `Debug`
/// output. There's no serde: the wire format comes with the proto
/// conversions (P10).
#[derive(Debug, Clone)]
pub enum SessionCredentials {
    /// An offline-mode account, for development and tests. Only servers in
    /// offline mode accept it.
    Offline {
        /// The name the bot logs in with.
        username: McUsername,
    },
    /// A Minecraft session issued by the server for an online-mode account.
    Online {
        /// The account's name.
        username: McUsername,
        /// The account's UUID.
        uuid: Uuid,
        /// The Minecraft access token.
        access_token: SecretString,
        /// When the server says the token expires.
        expires_at: DateTime<Utc>,
    },
}

impl SessionCredentials {
    /// Returns the name the bot logs in with.
    #[must_use]
    pub const fn username(&self) -> &McUsername {
        match self {
            Self::Offline { username } | Self::Online { username, .. } => username,
        }
    }
}

/// A bot's request for session credentials, for one connection attempt
/// (Plan.md P4.3; ADR-0013).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRequest {
    /// The bot that asks.
    pub bot_id: BotId,
    /// The account it plays as.
    pub account: BotAccount,
    /// Whether the last session was rejected, so the provider must bypass any
    /// token cache and get a fresh one (ADR-0010).
    pub fresh: bool,
}

/// Why no session credentials could be had.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{}", credential_error_message(*.retryable))]
pub struct CredentialError {
    /// Whether asking again later may help. The actor reports the error as
    /// [`BotEvent::SessionUnavailable`](crate::bot::BotEvent::SessionUnavailable)
    /// with this flag: a retryable error backs off, any other fails the bot.
    pub retryable: bool,
}

/// The fixed message of a [`CredentialError`].
const fn credential_error_message(retryable: bool) -> &'static str {
    if retryable {
        "no session is available right now"
    } else {
        "no session can be issued for this account"
    }
}

/// Hands out the session credentials a bot connects with (Plan.md P4.3;
/// ADR-0010, ADR-0013).
///
/// The bot's actor asks once per connection attempt and gets exactly one
/// answer; it bounds the wait with its own timeout, which counts as a
/// retryable error. The standalone agent serves offline accounts itself
/// (fleet-runtime's `OfflineCredentials`); a managed agent asks the server,
/// which issues a short-lived Minecraft session for an online account
/// (P10.6). fleet-testkit has a scriptable fake.
pub trait SessionCredentialProvider: Send + Sync + 'static {
    /// Returns the credentials for `request`.
    ///
    /// With `request.fresh`, the provider bypasses any token cache, because
    /// the last session was rejected.
    ///
    /// # Errors
    /// A [`CredentialError`] if no credentials could be had; its
    /// `retryable` flag says whether asking again later may help.
    fn session(
        &self,
        request: SessionRequest,
    ) -> impl Future<Output = Result<SessionCredentials, CredentialError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use secrecy::ExposeSecret;

    /// A placeholder, not a real token.
    const TOKEN: &str = "not-a-real-token";

    fn online() -> SessionCredentials {
        SessionCredentials::Online {
            username: "AfkBot1".parse().unwrap(),
            uuid: Uuid::from_u128(0x0123_4567_89ab_4def_8123_4567_89ab_cdef),
            access_token: SecretString::from(TOKEN),
            expires_at: DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
        }
    }

    fn offline() -> SessionCredentials {
        SessionCredentials::Offline {
            username: "AfkBot1".parse().unwrap(),
        }
    }

    #[test]
    fn debug_output_never_contains_the_access_token() {
        let debug = format!("{:?}", online());
        let pretty = format!("{:#?}", online());

        assert!(!debug.contains(TOKEN), "{debug}");
        assert!(!pretty.contains(TOKEN), "{pretty}");
        assert!(debug.contains("[REDACTED]"), "{debug}");
        assert!(debug.contains("AfkBot1"), "{debug}");
    }

    #[rstest]
    #[case::offline(offline())]
    #[case::online(online())]
    fn username_is_returned_for_both_kinds(#[case] credentials: SessionCredentials) {
        assert_eq!(credentials.username().as_str(), "AfkBot1");
    }

    #[rstest]
    #[case::retryable(true, "no session is available right now")]
    #[case::not_retryable(false, "no session can be issued for this account")]
    fn credential_errors_have_fixed_messages(#[case] retryable: bool, #[case] message: &str) {
        assert_eq!(CredentialError { retryable }.to_string(), message);
    }

    #[test]
    fn clone_keeps_the_access_token() {
        let SessionCredentials::Online { access_token, .. } = online().clone() else {
            panic!("expected online credentials");
        };
        assert_eq!(access_token.expose_secret(), TOKEN);
    }
}
