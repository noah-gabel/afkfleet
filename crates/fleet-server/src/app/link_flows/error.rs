//! [`LinkStartError`]: why a link flow didn't start.

use core::time::Duration;

use fleet_core::audit::AuditError;
use fleet_core::authz::AuthzError;
use fleet_core::system::MintError;

use super::table::Busy;
use crate::ports::msauth::Failure;
use crate::ports::store::StoreError;

/// Why a link flow didn't start. The first five are refusals the client
/// may retry later; see the [module docs](super).
#[derive(Debug, thiserror::Error)]
pub enum LinkStartError {
    /// The server is stopping.
    #[error("the server is stopping")]
    ShuttingDown,
    /// The user's flow is finishing, or another start of theirs is in
    /// progress.
    #[error("the user's link flow is finishing or starting")]
    InProgress,
    /// Every place for a flow is taken.
    #[error("every link flow's place is taken")]
    AllBusy,
    /// The user started their daily cap of flows; the next one may start
    /// after `retry_after`.
    #[error("the user started their daily cap of link flows")]
    DailyCap {
        /// How long until a counted start ages out.
        retry_after: Duration,
    },
    /// The Member owns their quota of accounts already.
    #[error("the member owns their quota of accounts")]
    OverQuota,
    /// Linking again: the account is gone.
    #[error("the account doesn't exist")]
    AccountGone,
    /// Linking again: `authorize()` refused it on the fresh facts.
    #[error("linking the account again isn't allowed")]
    Denied(#[source] AuthzError),
    /// Linking again: the account is pending, or an offline one.
    #[error("the account can't be linked again")]
    NotRelinkable,
    /// The acting user's session ended, or they were disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// Microsoft didn't hand out a device code.
    #[error("Microsoft didn't start the link flow")]
    Microsoft(#[source] Failure),
    /// The flow's ID couldn't be minted.
    #[error("the link flow's ID can't be minted")]
    Mint(#[source] MintError),
    /// The audit entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<Busy> for LinkStartError {
    fn from(busy: Busy) -> Self {
        match busy {
            Busy::InProgress => Self::InProgress,
            Busy::AllBusy => Self::AllBusy,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_refusals_name_no_value() {
        assert_eq!(
            LinkStartError::DailyCap {
                retry_after: Duration::from_secs(7)
            }
            .to_string(),
            "the user started their daily cap of link flows"
        );
        assert_eq!(
            LinkStartError::from(Busy::InProgress).to_string(),
            "the user's link flow is finishing or starting"
        );
        assert_eq!(
            LinkStartError::from(Busy::AllBusy).to_string(),
            "every link flow's place is taken"
        );
        assert_eq!(
            LinkStartError::Microsoft(Failure::TimedOut).to_string(),
            "Microsoft didn't start the link flow"
        );
    }
}
