//! [`SessionTokenError`]: why no Minecraft session was handed out, and
//! [`WaitBoundError`].

use chrono::{DateTime, Utc};
use fleet_core::account::AccountStatus;
use fleet_core::audit::AuditError;

use crate::ports::msauth::{Failure, TokenError};
use crate::ports::store::StoreError;
use crate::ports::vault::VaultError;

/// Why [`SessionTokenService::session`](super::SessionTokenService::session)
/// handed out nothing. No variant carries a token, a name or a UUID.
#[derive(Debug, thiserror::Error)]
pub enum SessionTokenError {
    /// No account has this ID.
    #[error("the account doesn't exist")]
    NotFound,
    /// An offline account has no session token: its agent serves it alone.
    #[error("an offline account has no session token")]
    Offline,
    /// The account isn't active: it waits for approval, or must be linked
    /// again. Also the answer when this request just changed its status.
    #[error("the account is {status}, not active")]
    NotActive {
        /// Its status.
        status: AccountStatus,
    },
    /// The account's owner is disabled.
    #[error("the account's owner is disabled")]
    OwnerDisabled,
    /// Microsoft isn't asked for this account before `until`: it failed
    /// recently, or Microsoft rate limited the server.
    #[error("Microsoft isn't asked again before {until}")]
    BackingOff {
        /// When it may be asked again.
        until: DateTime<Utc>,
    },
    /// The call to Microsoft failed.
    #[error(transparent)]
    Microsoft(Failure),
    /// Another request for this account still runs past the wait bound.
    #[error("another request for this account is still running")]
    InFlight,
    /// The session took longer than the wait bound; it still finishes, and
    /// the next request finds it.
    #[error("the session wasn't ready within the wait bound")]
    TimedOut,
    /// The account's refresh token changed while this request refreshed it:
    /// it was linked again.
    #[error("the account changed while its token was refreshed")]
    ChangedMeanwhile,
    /// The server is shutting down.
    #[error("the server is shutting down")]
    ShuttingDown,
    /// The session's task ended without an answer.
    #[error("the session's task ended without an answer")]
    Interrupted,
    /// The stored refresh token's envelope doesn't open.
    #[error("the account's stored refresh token can't be opened")]
    Envelope(#[source] VaultError),
    /// The stored refresh token opened, but isn't a valid token.
    #[error("the account's stored refresh token isn't a valid token")]
    StoredToken(#[source] TokenError),
    /// The new refresh token couldn't be sealed; it's kept in memory.
    #[error("the new refresh token can't be sealed")]
    Seal(#[source] VaultError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// An audit entry couldn't be built.
    #[error(transparent)]
    Audit(#[from] AuditError),
}

impl SessionTokenError {
    /// Whether asking again later may help: the session grants (P10.6) map
    /// it to `CredentialError::retryable`, except [`Self::OwnerDisabled`],
    /// which they answer retryable (ADR-0019, U14).
    #[must_use]
    pub const fn retryable(&self) -> bool {
        match self {
            Self::BackingOff { .. }
            | Self::Microsoft(_)
            | Self::InFlight
            | Self::TimedOut
            | Self::ChangedMeanwhile
            | Self::ShuttingDown
            | Self::Interrupted
            | Self::Seal(_)
            | Self::Store(StoreError::Busy | StoreError::Backend(_)) => true,
            Self::NotFound
            | Self::Offline
            | Self::NotActive { .. }
            | Self::OwnerDisabled
            | Self::Envelope(_)
            | Self::StoredToken(_)
            | Self::Store(StoreError::Corrupt { .. } | StoreError::Unstorable { .. })
            | Self::Audit(_) => false,
        }
    }
}

impl SessionTokenError {
    /// Whether it refuses the account itself: it's missing, offline, not
    /// active or its owner is disabled.
    pub(super) const fn refuses_the_account(&self) -> bool {
        matches!(
            self,
            Self::NotFound | Self::Offline | Self::NotActive { .. } | Self::OwnerDisabled
        )
    }
}

/// Why a wait bound was refused: it may only shorten the wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a wait bound is longer than zero and at most 2 minutes")]
pub struct WaitBoundError;

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::ports::msauth::{Endpoint, Rule};

    fn at() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap()
    }

    #[rstest]
    #[case::backing_off(SessionTokenError::BackingOff { until: at() })]
    #[case::rate_limited(SessionTokenError::Microsoft(Failure::RateLimited))]
    #[case::unreachable(SessionTokenError::Microsoft(Failure::Unreachable))]
    #[case::unexpected(SessionTokenError::Microsoft(Failure::Unexpected {
        endpoint: Endpoint::Refresh,
        rule: Rule::NotJson,
    }))]
    #[case::in_flight(SessionTokenError::InFlight)]
    #[case::timed_out(SessionTokenError::TimedOut)]
    #[case::changed_meanwhile(SessionTokenError::ChangedMeanwhile)]
    #[case::shutting_down(SessionTokenError::ShuttingDown)]
    #[case::interrupted(SessionTokenError::Interrupted)]
    #[case::seal(SessionTokenError::Seal(VaultError::TooLarge))]
    #[case::busy(SessionTokenError::Store(StoreError::Busy))]
    #[case::backend(SessionTokenError::Store(StoreError::Backend("x".into())))]
    fn a_passing_failure_is_retryable(#[case] error: SessionTokenError) {
        assert!(error.retryable(), "{error:?}");
    }

    #[rstest]
    #[case::not_found(SessionTokenError::NotFound)]
    #[case::offline(SessionTokenError::Offline)]
    #[case::pending(SessionTokenError::NotActive { status: AccountStatus::Pending })]
    #[case::reauth(SessionTokenError::NotActive { status: AccountStatus::ReauthRequired })]
    #[case::owner_disabled(SessionTokenError::OwnerDisabled)]
    #[case::envelope(SessionTokenError::Envelope(VaultError::Malformed))]
    #[case::stored_token(SessionTokenError::StoredToken(TokenError::Length))]
    #[case::corrupt(SessionTokenError::Store(StoreError::Corrupt {
        table: "mc_accounts",
        column: "status",
        rowid: 1,
    }))]
    #[case::unstorable(SessionTokenError::Store(StoreError::Unstorable {
        table: "mc_accounts",
        column: "mc_name",
    }))]
    #[case::audit(SessionTokenError::Audit(AuditError::ActionShape))]
    fn a_lasting_failure_isnt_retryable(#[case] error: SessionTokenError) {
        assert!(!error.retryable(), "{error:?}");
    }

    #[test]
    fn the_messages_name_no_secret() {
        assert_eq!(
            SessionTokenError::NotActive {
                status: AccountStatus::ReauthRequired
            }
            .to_string(),
            "the account is reauth_required, not active"
        );
        assert_eq!(
            WaitBoundError.to_string(),
            "a wait bound is longer than zero and at most 2 minutes"
        );
    }
}
