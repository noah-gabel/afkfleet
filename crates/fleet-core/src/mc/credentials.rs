//! [`SessionCredentials`]: what a bot needs to log in to a Minecraft server.

use chrono::{DateTime, Utc};
use secrecy::SecretString;
use uuid::Uuid;

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

    #[test]
    fn clone_keeps_the_access_token() {
        let SessionCredentials::Online { access_token, .. } = online().clone() else {
            panic!("expected online credentials");
        };
        assert_eq!(access_token.expose_secret(), TOKEN);
    }
}
