//! Why a session ended. A kick's key and message came from a Minecraft
//! server, so reading them cleans them again (ADR-0019).

use fleet_core::disconnect::{
    AccountRestriction, ConnectFailure, DisconnectReason, SessionServerFailure,
};

use super::{enum_from_wire, required};
use crate::generated as wire;
use crate::{ConvertError, Problem};

impl From<DisconnectReason> for wire::DisconnectReason {
    fn from(reason: DisconnectReason) -> Self {
        use wire::disconnect_reason::Reason;
        let unit = wire::Unit {};
        let reason = match reason {
            DisconnectReason::Kicked(kick) => Reason::Kicked(wire::Kick {
                key: kick.key().map(|key| key.as_str().to_owned()),
                message: kick.message().to_owned(),
            }),
            DisconnectReason::ConnectionClosed => Reason::ConnectionClosed(unit),
            DisconnectReason::ConnectFailed { failure } => {
                Reason::ConnectFailed(connect_failure_to_wire(failure).into())
            }
            DisconnectReason::AuthRejected => Reason::AuthRejected(unit),
            DisconnectReason::AccountRestricted { restriction } => {
                Reason::AccountRestricted(restriction_to_wire(restriction).into())
            }
            DisconnectReason::SessionServerFailed { failure } => {
                Reason::SessionServerFailed(session_server_failure_to_wire(failure).into())
            }
            DisconnectReason::SessionCrashed => Reason::SessionCrashed(unit),
            DisconnectReason::WatchdogTimeout => Reason::WatchdogTimeout(unit),
            DisconnectReason::LivenessTimeout => Reason::LivenessTimeout(unit),
            DisconnectReason::RespawnFailed => Reason::RespawnFailed(unit),
        };
        Self {
            reason: Some(reason),
        }
    }
}

impl TryFrom<wire::DisconnectReason> for DisconnectReason {
    type Error = ConvertError;

    fn try_from(reason: wire::DisconnectReason) -> Result<Self, ConvertError> {
        use wire::disconnect_reason::Reason;
        Ok(match required("DisconnectReason.reason", reason.reason)? {
            // Text from a Minecraft server: an invalid key counts as none, and
            // the message is cleaned and capped again, as when the agent read it.
            Reason::Kicked(kick) => Self::kicked(kick.key.as_deref(), &kick.message),
            Reason::ConnectionClosed(wire::Unit {}) => Self::ConnectionClosed,
            Reason::ConnectFailed(failure) => Self::ConnectFailed {
                failure: connect_failure_from_wire(failure)?,
            },
            Reason::AuthRejected(wire::Unit {}) => Self::AuthRejected,
            Reason::AccountRestricted(restriction) => Self::AccountRestricted {
                restriction: restriction_from_wire(restriction)?,
            },
            Reason::SessionServerFailed(failure) => Self::SessionServerFailed {
                failure: session_server_failure_from_wire(failure)?,
            },
            Reason::SessionCrashed(wire::Unit {}) => Self::SessionCrashed,
            Reason::WatchdogTimeout(wire::Unit {}) => Self::WatchdogTimeout,
            Reason::LivenessTimeout(wire::Unit {}) => Self::LivenessTimeout,
            Reason::RespawnFailed(wire::Unit {}) => Self::RespawnFailed,
        })
    }
}

const fn connect_failure_to_wire(failure: ConnectFailure) -> wire::ConnectFailure {
    match failure {
        ConnectFailure::Refused => wire::ConnectFailure::Refused,
        ConnectFailure::TimedOut => wire::ConnectFailure::TimedOut,
        ConnectFailure::Unresolvable => wire::ConnectFailure::Unresolvable,
        ConnectFailure::HostUnavailable => wire::ConnectFailure::HostUnavailable,
        ConnectFailure::Other => wire::ConnectFailure::Other,
        ConnectFailure::Blocked => wire::ConnectFailure::Blocked,
    }
}

fn connect_failure_from_wire(number: i32) -> Result<ConnectFailure, ConvertError> {
    const FIELD: &str = "DisconnectReason.connect_failed";
    Ok(match enum_from_wire(FIELD, number)? {
        wire::ConnectFailure::Refused => ConnectFailure::Refused,
        wire::ConnectFailure::TimedOut => ConnectFailure::TimedOut,
        wire::ConnectFailure::Unresolvable => ConnectFailure::Unresolvable,
        wire::ConnectFailure::HostUnavailable => ConnectFailure::HostUnavailable,
        wire::ConnectFailure::Other => ConnectFailure::Other,
        wire::ConnectFailure::Blocked => ConnectFailure::Blocked,
        wire::ConnectFailure::Unspecified => {
            return Err(ConvertError::new(FIELD, Problem::Missing));
        }
    })
}

const fn restriction_to_wire(restriction: AccountRestriction) -> wire::AccountRestriction {
    match restriction {
        AccountRestriction::Banned => wire::AccountRestriction::Banned,
        AccountRestriction::MultiplayerDisabled => wire::AccountRestriction::MultiplayerDisabled,
    }
}

fn restriction_from_wire(number: i32) -> Result<AccountRestriction, ConvertError> {
    const FIELD: &str = "DisconnectReason.account_restricted";
    Ok(match enum_from_wire(FIELD, number)? {
        wire::AccountRestriction::Banned => AccountRestriction::Banned,
        wire::AccountRestriction::MultiplayerDisabled => AccountRestriction::MultiplayerDisabled,
        wire::AccountRestriction::Unspecified => {
            return Err(ConvertError::new(FIELD, Problem::Missing));
        }
    })
}

const fn session_server_failure_to_wire(
    failure: SessionServerFailure,
) -> wire::SessionServerFailure {
    match failure {
        SessionServerFailure::Unreachable => wire::SessionServerFailure::Unreachable,
        SessionServerFailure::RateLimited => wire::SessionServerFailure::RateLimited,
        SessionServerFailure::TimedOut => wire::SessionServerFailure::TimedOut,
        SessionServerFailure::Unexpected => wire::SessionServerFailure::Unexpected,
    }
}

fn session_server_failure_from_wire(number: i32) -> Result<SessionServerFailure, ConvertError> {
    const FIELD: &str = "DisconnectReason.session_server_failed";
    Ok(match enum_from_wire(FIELD, number)? {
        wire::SessionServerFailure::Unreachable => SessionServerFailure::Unreachable,
        wire::SessionServerFailure::RateLimited => SessionServerFailure::RateLimited,
        wire::SessionServerFailure::TimedOut => SessionServerFailure::TimedOut,
        wire::SessionServerFailure::Unexpected => SessionServerFailure::Unexpected,
        wire::SessionServerFailure::Unspecified => {
            return Err(ConvertError::new(FIELD, Problem::Missing));
        }
    })
}
