//! A bot's spec, state, sticky state, alert and snapshot.

use core::num::NonZeroU32;

use fleet_core::bot::{
    BotAccount, BotNotification, BotSnapshot, BotSpec, BotState, DesiredRunState, FailReason,
    PauseReason, StickyState,
};
use fleet_core::disconnect::{ConflictKind, ConflictTexts, PermanentKind};
use fleet_core::value::{McUsername, ServerAddress};

use super::{enum_from_wire, id_from_wire, id_to_wire, required, time_from_wire, time_to_wire};
use crate::generated as wire;
use crate::{ConvertError, Problem};

impl From<BotSpec> for wire::BotSpec {
    fn from(spec: BotSpec) -> Self {
        Self {
            id: id_to_wire(spec.id),
            account: Some(wire::BotAccount {
                account: Some(match spec.account {
                    BotAccount::Offline(username) => {
                        wire::bot_account::Account::OfflineUsername(username.into())
                    }
                    BotAccount::Online(account) => {
                        wire::bot_account::Account::OnlineAccountId(id_to_wire(account))
                    }
                }),
            }),
            server: spec.server.to_string(),
            mode: Some(spec.mode.into()),
            desired: match spec.desired {
                DesiredRunState::Running => wire::DesiredRunState::Running,
                DesiredRunState::Stopped => wire::DesiredRunState::Stopped,
            }
            .into(),
            conflict_texts: spec.conflict_texts.iter().map(str::to_owned).collect(),
        }
    }
}

impl TryFrom<wire::BotSpec> for BotSpec {
    type Error = ConvertError;

    fn try_from(spec: wire::BotSpec) -> Result<Self, ConvertError> {
        let id = id_from_wire("BotSpec.id", &spec.id)?;
        let account = match required(
            "BotAccount.account",
            required("BotSpec.account", spec.account)?.account,
        )? {
            wire::bot_account::Account::OfflineUsername(name) => BotAccount::Offline(
                McUsername::try_from(name.as_str())
                    .map_err(|e| ConvertError::new("BotAccount.offline_username", e.into()))?,
            ),
            wire::bot_account::Account::OnlineAccountId(account) => {
                BotAccount::Online(id_from_wire("BotAccount.online_account_id", &account)?)
            }
        };
        let server = server_from_wire(&spec.server)?;
        let mode = required("BotSpec.mode", spec.mode)?.try_into()?;
        let desired = match enum_from_wire("BotSpec.desired", spec.desired)? {
            wire::DesiredRunState::Running => DesiredRunState::Running,
            wire::DesiredRunState::Stopped => DesiredRunState::Stopped,
            wire::DesiredRunState::Unspecified => {
                return Err(ConvertError::new("BotSpec.desired", Problem::Missing));
            }
        };
        // A set's order and duplicates are how it's written down, not what it
        // holds, so they're accepted.
        let conflict_texts = ConflictTexts::try_new(&spec.conflict_texts)
            .map_err(|e| ConvertError::new("BotSpec.conflict_texts", e.into()))?;
        Ok(Self {
            id,
            account,
            server,
            mode,
            desired,
            conflict_texts,
        })
    }
}

/// Reads a server address, which must arrive exactly as the type writes it:
/// lowercase, without anything the constructor would change.
fn server_from_wire(text: &str) -> Result<ServerAddress, ConvertError> {
    const FIELD: &str = "BotSpec.server";
    let server = ServerAddress::try_from(text).map_err(|e| ConvertError::new(FIELD, e.into()))?;
    if server.to_string() == text {
        Ok(server)
    } else {
        Err(ConvertError::new(FIELD, Problem::NotExact))
    }
}

/// Reads an attempt, which counts from 1.
fn attempt_from_wire(field: &'static str, attempt: u32) -> Result<NonZeroU32, ConvertError> {
    NonZeroU32::new(attempt).ok_or(ConvertError::new(field, Problem::Missing))
}

const fn pause_to_wire(reason: PauseReason) -> wire::PauseReason {
    match reason {
        PauseReason::Conflict {
            kind: ConflictKind::DuplicateLogin,
        } => wire::PauseReason::DuplicateLogin,
    }
}

fn pause_from_wire(field: &'static str, number: i32) -> Result<PauseReason, ConvertError> {
    match enum_from_wire(field, number)? {
        wire::PauseReason::DuplicateLogin => Ok(PauseReason::Conflict {
            kind: ConflictKind::DuplicateLogin,
        }),
        wire::PauseReason::Unspecified => Err(ConvertError::new(field, Problem::Missing)),
    }
}

const fn fail_to_wire(reason: FailReason) -> wire::FailReason {
    match reason {
        FailReason::Permanent { kind } => match kind {
            PermanentKind::Banned => wire::FailReason::Banned,
            PermanentKind::NotWhitelisted => wire::FailReason::NotWhitelisted,
            PermanentKind::WrongVersion => wire::FailReason::WrongVersion,
            PermanentKind::AccountBanned => wire::FailReason::AccountBanned,
            PermanentKind::MultiplayerDisabled => wire::FailReason::MultiplayerDisabled,
        },
        FailReason::Auth => wire::FailReason::Auth,
        FailReason::SessionDenied => wire::FailReason::SessionDenied,
        FailReason::CrashLoop => wire::FailReason::CrashLoop,
    }
}

fn fail_from_wire(field: &'static str, number: i32) -> Result<FailReason, ConvertError> {
    let permanent = |kind| Ok(FailReason::Permanent { kind });
    match enum_from_wire(field, number)? {
        wire::FailReason::Banned => permanent(PermanentKind::Banned),
        wire::FailReason::NotWhitelisted => permanent(PermanentKind::NotWhitelisted),
        wire::FailReason::WrongVersion => permanent(PermanentKind::WrongVersion),
        wire::FailReason::AccountBanned => permanent(PermanentKind::AccountBanned),
        wire::FailReason::MultiplayerDisabled => permanent(PermanentKind::MultiplayerDisabled),
        wire::FailReason::Auth => Ok(FailReason::Auth),
        wire::FailReason::SessionDenied => Ok(FailReason::SessionDenied),
        wire::FailReason::CrashLoop => Ok(FailReason::CrashLoop),
        wire::FailReason::Unspecified => Err(ConvertError::new(field, Problem::Missing)),
    }
}

impl TryFrom<BotState> for wire::BotState {
    type Error = ConvertError;

    fn try_from(state: BotState) -> Result<Self, ConvertError> {
        use wire::bot_state::{self, State};
        let state = match state {
            BotState::Stopped => State::Stopped(wire::Unit {}),
            BotState::AwaitingSession { attempt, fresh } => {
                State::AwaitingSession(bot_state::AwaitingSession {
                    attempt: attempt.get(),
                    fresh,
                })
            }
            BotState::Connecting {
                attempt,
                auth_retried,
            } => State::Connecting(bot_state::Connecting {
                attempt: attempt.get(),
                auth_retried,
            }),
            BotState::Online { since, attempt } => State::Online(bot_state::Online {
                since_unix_ms: Some(time_to_wire("BotState.Online.since_unix_ms", since)?),
                attempt: attempt.get(),
            }),
            BotState::Backoff { attempt, fresh } => State::Backoff(bot_state::Backoff {
                attempt: attempt.get(),
                fresh,
            }),
            BotState::Paused { reason } => State::Paused(pause_to_wire(reason).into()),
            BotState::Failed { reason } => State::Failed(fail_to_wire(reason).into()),
            BotState::Stopping { restart } => State::Stopping(bot_state::Stopping { restart }),
        };
        Ok(Self { state: Some(state) })
    }
}

impl TryFrom<wire::BotState> for BotState {
    type Error = ConvertError;

    fn try_from(state: wire::BotState) -> Result<Self, ConvertError> {
        use wire::bot_state::State;
        Ok(match required("BotState.state", state.state)? {
            State::Stopped(wire::Unit {}) => Self::Stopped,
            State::AwaitingSession(awaiting) => Self::AwaitingSession {
                attempt: attempt_from_wire("BotState.AwaitingSession.attempt", awaiting.attempt)?,
                fresh: awaiting.fresh,
            },
            State::Connecting(connecting) => Self::Connecting {
                attempt: attempt_from_wire("BotState.Connecting.attempt", connecting.attempt)?,
                auth_retried: connecting.auth_retried,
            },
            State::Online(online) => Self::Online {
                since: time_from_wire("BotState.Online.since_unix_ms", online.since_unix_ms)?,
                attempt: attempt_from_wire("BotState.Online.attempt", online.attempt)?,
            },
            State::Backoff(backoff) => Self::Backoff {
                attempt: attempt_from_wire("BotState.Backoff.attempt", backoff.attempt)?,
                fresh: backoff.fresh,
            },
            State::Paused(reason) => Self::Paused {
                reason: pause_from_wire("BotState.paused", reason)?,
            },
            State::Failed(reason) => Self::Failed {
                reason: fail_from_wire("BotState.failed", reason)?,
            },
            State::Stopping(stopping) => Self::Stopping {
                restart: stopping.restart,
            },
        })
    }
}

impl From<StickyState> for wire::StickyState {
    fn from(sticky: StickyState) -> Self {
        use wire::sticky_state::State;
        Self {
            state: Some(match sticky {
                StickyState::Paused(reason) => State::Paused(pause_to_wire(reason).into()),
                StickyState::Failed(reason) => State::Failed(fail_to_wire(reason).into()),
            }),
        }
    }
}

impl TryFrom<wire::StickyState> for StickyState {
    type Error = ConvertError;

    fn try_from(sticky: wire::StickyState) -> Result<Self, ConvertError> {
        use wire::sticky_state::State;
        Ok(match required("StickyState.state", sticky.state)? {
            State::Paused(reason) => Self::Paused(pause_from_wire("StickyState.paused", reason)?),
            State::Failed(reason) => Self::Failed(fail_from_wire("StickyState.failed", reason)?),
        })
    }
}

impl From<BotNotification> for wire::BotNotification {
    fn from(notification: BotNotification) -> Self {
        use wire::bot_notification::Notification;
        Self {
            notification: Some(match notification {
                BotNotification::Paused { reason } => {
                    Notification::Paused(pause_to_wire(reason).into())
                }
                BotNotification::Failed { reason } => {
                    Notification::Failed(fail_to_wire(reason).into())
                }
            }),
        }
    }
}

impl TryFrom<wire::BotNotification> for BotNotification {
    type Error = ConvertError;

    fn try_from(notification: wire::BotNotification) -> Result<Self, ConvertError> {
        use wire::bot_notification::Notification;
        Ok(
            match required("BotNotification.notification", notification.notification)? {
                Notification::Paused(reason) => Self::Paused {
                    reason: pause_from_wire("BotNotification.paused", reason)?,
                },
                Notification::Failed(reason) => Self::Failed {
                    reason: fail_from_wire("BotNotification.failed", reason)?,
                },
            },
        )
    }
}

impl TryFrom<BotSnapshot> for wire::BotSnapshot {
    type Error = ConvertError;

    fn try_from(snapshot: BotSnapshot) -> Result<Self, ConvertError> {
        Ok(Self {
            bot_id: id_to_wire(snapshot.bot_id),
            state: Some(snapshot.state.try_into()?),
            since_unix_ms: Some(time_to_wire("BotSnapshot.since_unix_ms", snapshot.since)?),
            last_disconnect: snapshot.last_disconnect.map(Into::into),
        })
    }
}

impl TryFrom<wire::BotSnapshot> for BotSnapshot {
    type Error = ConvertError;

    fn try_from(snapshot: wire::BotSnapshot) -> Result<Self, ConvertError> {
        Ok(Self {
            bot_id: id_from_wire("BotSnapshot.bot_id", &snapshot.bot_id)?,
            state: required("BotSnapshot.state", snapshot.state)?.try_into()?,
            since: time_from_wire("BotSnapshot.since_unix_ms", snapshot.since_unix_ms)?,
            last_disconnect: snapshot
                .last_disconnect
                .map(TryInto::try_into)
                .transpose()?,
        })
    }
}
