//! [`LastState`]: the last state a bot's agent reported, as the server stores
//! it (Plan.md P10.5, P11.1; ADR-0019, S40).
//!
//! The server keeps a bot's last report in `bots.last_state`,
//! `last_state_reason` and `last_state_at`, so a Paused or Failed bot keeps
//! that state on its next agent ([`LastState::sticky`]) and a bot whose agent
//! went quiet still shows what it was doing. Only the kind and, for Paused and
//! Failed, the reason are kept: the counters a [`BotState`] carries change too
//! often to be worth storing.

use chrono::{DateTime, Utc};

use super::{BotState, FailReason, PauseReason, StickyState};
use crate::disconnect::{ConflictKind, PermanentKind};

/// Why stored columns aren't a [`LastState`]. The message never repeats them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LastStateError {
    /// The name isn't one of the [`LastState`] kinds.
    #[error("unknown bot state")]
    UnknownState,
    /// The reason isn't one of the paused or failed reasons, or belongs to
    /// the other one.
    #[error("unknown bot state reason")]
    UnknownReason,
    /// A paused or failed state without its reason.
    #[error("a paused or failed state needs its reason")]
    MissingReason,
    /// A reason on a state that isn't paused or failed.
    #[error("only a paused or failed state has a reason")]
    UnexpectedReason,
}

/// A bot's state as the server stores it: a [`BotState`] without its
/// counters and times, so a Paused or Failed bot keeps that state on its
/// next agent ([`LastState::sticky`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LastState {
    /// [`BotState::Stopped`].
    Stopped,
    /// [`BotState::AwaitingSession`].
    AwaitingSession,
    /// [`BotState::Connecting`].
    Connecting,
    /// [`BotState::Online`].
    Online,
    /// [`BotState::Backoff`].
    Backoff,
    /// [`BotState::Paused`], with its reason.
    Paused(PauseReason),
    /// [`BotState::Failed`], with its reason.
    Failed(FailReason),
    /// [`BotState::Stopping`].
    Stopping,
}

impl LastState {
    /// Every value, for the tests that check them all.
    pub const ALL: [Self; 15] = [
        Self::Stopped,
        Self::AwaitingSession,
        Self::Connecting,
        Self::Online,
        Self::Backoff,
        Self::Paused(PauseReason::Conflict {
            kind: ConflictKind::DuplicateLogin,
        }),
        Self::Failed(FailReason::Permanent {
            kind: PermanentKind::Banned,
        }),
        Self::Failed(FailReason::Permanent {
            kind: PermanentKind::NotWhitelisted,
        }),
        Self::Failed(FailReason::Permanent {
            kind: PermanentKind::WrongVersion,
        }),
        Self::Failed(FailReason::Permanent {
            kind: PermanentKind::AccountBanned,
        }),
        Self::Failed(FailReason::Permanent {
            kind: PermanentKind::MultiplayerDisabled,
        }),
        Self::Failed(FailReason::Auth),
        Self::Failed(FailReason::SessionDenied),
        Self::Failed(FailReason::CrashLoop),
        Self::Stopping,
    ];

    /// Returns the stable name stored in the `last_state` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::AwaitingSession => "awaiting_session",
            Self::Connecting => "connecting",
            Self::Online => "online",
            Self::Backoff => "backoff",
            Self::Paused(_) => "paused",
            Self::Failed(_) => "failed",
            Self::Stopping => "stopping",
        }
    }

    /// Returns the reason stored in the `last_state_reason` column: only a
    /// paused or failed state has one.
    #[must_use]
    pub const fn reason(self) -> Option<&'static str> {
        match self {
            Self::Paused(reason) => Some(pause_name(reason)),
            Self::Failed(reason) => Some(fail_name(reason)),
            Self::Stopped
            | Self::AwaitingSession
            | Self::Connecting
            | Self::Online
            | Self::Backoff
            | Self::Stopping => None,
        }
    }

    /// Reads a state back from its `last_state` and `last_state_reason`
    /// columns.
    ///
    /// # Errors
    /// - [`LastStateError::UnknownState`] for an unknown state name.
    /// - [`LastStateError::MissingReason`] for `paused` or `failed` without a
    ///   reason.
    /// - [`LastStateError::UnknownReason`] for an unknown reason, or one of
    ///   the other kind's.
    /// - [`LastStateError::UnexpectedReason`] for a reason on any other state.
    pub fn from_columns(state: &str, reason: Option<&str>) -> Result<Self, LastStateError> {
        let without_reason = match state {
            "stopped" => Self::Stopped,
            "awaiting_session" => Self::AwaitingSession,
            "connecting" => Self::Connecting,
            "online" => Self::Online,
            "backoff" => Self::Backoff,
            "stopping" => Self::Stopping,
            "paused" => {
                let reason = reason.ok_or(LastStateError::MissingReason)?;
                return PAUSE_REASONS
                    .into_iter()
                    .find(|known| pause_name(*known) == reason)
                    .map(Self::Paused)
                    .ok_or(LastStateError::UnknownReason);
            }
            "failed" => {
                let reason = reason.ok_or(LastStateError::MissingReason)?;
                return FAIL_REASONS
                    .into_iter()
                    .find(|known| fail_name(*known) == reason)
                    .map(Self::Failed)
                    .ok_or(LastStateError::UnknownReason);
            }
            _ => return Err(LastStateError::UnknownState),
        };
        match reason {
            None => Ok(without_reason),
            Some(_) => Err(LastStateError::UnexpectedReason),
        }
    }

    /// The Paused or Failed state a bot starts in when it's assigned again
    /// (ADR-0013); none for every other state.
    #[must_use]
    pub const fn sticky(self) -> Option<StickyState> {
        match self {
            Self::Paused(reason) => Some(StickyState::Paused(reason)),
            Self::Failed(reason) => Some(StickyState::Failed(reason)),
            Self::Stopped
            | Self::AwaitingSession
            | Self::Connecting
            | Self::Online
            | Self::Backoff
            | Self::Stopping => None,
        }
    }

    /// Whether the bot rests: Stopped, Paused or Failed, a state it leaves
    /// only when the server asks.
    #[must_use]
    pub const fn rests(self) -> bool {
        matches!(self, Self::Stopped | Self::Paused(_) | Self::Failed(_))
    }
}

impl From<BotState> for LastState {
    fn from(state: BotState) -> Self {
        match state {
            BotState::Stopped => Self::Stopped,
            BotState::AwaitingSession { .. } => Self::AwaitingSession,
            BotState::Connecting { .. } => Self::Connecting,
            BotState::Online { .. } => Self::Online,
            BotState::Backoff { .. } => Self::Backoff,
            BotState::Paused { reason } => Self::Paused(reason),
            BotState::Failed { reason } => Self::Failed(reason),
            BotState::Stopping { .. } => Self::Stopping,
        }
    }
}

/// Every pause reason, for reading one back by its name.
const PAUSE_REASONS: [PauseReason; 1] = [PauseReason::Conflict {
    kind: ConflictKind::DuplicateLogin,
}];

/// Every fail reason, for reading one back by its name.
const FAIL_REASONS: [FailReason; 8] = [
    FailReason::Permanent {
        kind: PermanentKind::Banned,
    },
    FailReason::Permanent {
        kind: PermanentKind::NotWhitelisted,
    },
    FailReason::Permanent {
        kind: PermanentKind::WrongVersion,
    },
    FailReason::Permanent {
        kind: PermanentKind::AccountBanned,
    },
    FailReason::Permanent {
        kind: PermanentKind::MultiplayerDisabled,
    },
    FailReason::Auth,
    FailReason::SessionDenied,
    FailReason::CrashLoop,
];

/// A pause reason's stored name.
const fn pause_name(reason: PauseReason) -> &'static str {
    match reason {
        PauseReason::Conflict {
            kind: ConflictKind::DuplicateLogin,
        } => "duplicate_login",
    }
}

/// A fail reason's stored name: a permanent kick's kind, flattened as the
/// protocol does.
const fn fail_name(reason: FailReason) -> &'static str {
    match reason {
        FailReason::Permanent { kind } => match kind {
            PermanentKind::Banned => "banned",
            PermanentKind::NotWhitelisted => "not_whitelisted",
            PermanentKind::WrongVersion => "wrong_version",
            PermanentKind::AccountBanned => "account_banned",
            PermanentKind::MultiplayerDisabled => "multiplayer_disabled",
        },
        FailReason::Auth => "auth",
        FailReason::SessionDenied => "session_denied",
        FailReason::CrashLoop => "crash_loop",
    }
}

/// A bot's last state and when the server received it, by its own clock
/// (ADR-0019, S40).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastReport {
    /// The state.
    pub state: LastState,
    /// When the server received the report that carried it.
    pub at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;

    use proptest::prelude::*;
    use rstest::rstest;

    use super::*;

    const DUPLICATE_LOGIN: PauseReason = PauseReason::Conflict {
        kind: ConflictKind::DuplicateLogin,
    };

    const fn permanent(kind: PermanentKind) -> FailReason {
        FailReason::Permanent { kind }
    }

    #[rstest]
    #[case::stopped(LastState::Stopped, "stopped", None)]
    #[case::awaiting_session(LastState::AwaitingSession, "awaiting_session", None)]
    #[case::connecting(LastState::Connecting, "connecting", None)]
    #[case::online(LastState::Online, "online", None)]
    #[case::backoff(LastState::Backoff, "backoff", None)]
    #[case::duplicate_login(LastState::Paused(DUPLICATE_LOGIN), "paused", Some("duplicate_login"))]
    #[case::banned(
        LastState::Failed(permanent(PermanentKind::Banned)),
        "failed",
        Some("banned")
    )]
    #[case::not_whitelisted(
        LastState::Failed(permanent(PermanentKind::NotWhitelisted)),
        "failed",
        Some("not_whitelisted")
    )]
    #[case::wrong_version(
        LastState::Failed(permanent(PermanentKind::WrongVersion)),
        "failed",
        Some("wrong_version")
    )]
    #[case::account_banned(
        LastState::Failed(permanent(PermanentKind::AccountBanned)),
        "failed",
        Some("account_banned")
    )]
    #[case::multiplayer_disabled(
        LastState::Failed(permanent(PermanentKind::MultiplayerDisabled)),
        "failed",
        Some("multiplayer_disabled")
    )]
    #[case::auth(LastState::Failed(FailReason::Auth), "failed", Some("auth"))]
    #[case::session_denied(
        LastState::Failed(FailReason::SessionDenied),
        "failed",
        Some("session_denied")
    )]
    #[case::crash_loop(LastState::Failed(FailReason::CrashLoop), "failed", Some("crash_loop"))]
    #[case::stopping(LastState::Stopping, "stopping", None)]
    fn every_state_has_its_stored_names(
        #[case] state: LastState,
        #[case] name: &str,
        #[case] reason: Option<&str>,
    ) {
        assert_eq!(state.as_str(), name);
        assert_eq!(state.reason(), reason);
        assert_eq!(LastState::from_columns(name, reason), Ok(state));
    }

    #[test]
    fn every_value_reads_back_from_its_columns_and_all_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for state in LastState::ALL {
            assert_eq!(
                LastState::from_columns(state.as_str(), state.reason()),
                Ok(state)
            );
            assert!(seen.insert((state.as_str(), state.reason())), "{state:?}");
        }
    }

    #[rstest]
    #[case::unknown_state("sleeping", None, LastStateError::UnknownState)]
    #[case::wrong_case("Online", None, LastStateError::UnknownState)]
    #[case::empty_state("", None, LastStateError::UnknownState)]
    #[case::paused_without_reason("paused", None, LastStateError::MissingReason)]
    #[case::failed_without_reason("failed", None, LastStateError::MissingReason)]
    #[case::unknown_reason("failed", Some("tired"), LastStateError::UnknownReason)]
    #[case::failed_reason_on_paused("paused", Some("auth"), LastStateError::UnknownReason)]
    #[case::paused_reason_on_failed(
        "failed",
        Some("duplicate_login"),
        LastStateError::UnknownReason
    )]
    #[case::reason_on_online("online", Some("auth"), LastStateError::UnexpectedReason)]
    #[case::empty_reason_on_stopped("stopped", Some(""), LastStateError::UnexpectedReason)]
    fn columns_that_arent_a_state_are_refused(
        #[case] state: &str,
        #[case] reason: Option<&str>,
        #[case] expected: LastStateError,
    ) {
        assert_eq!(LastState::from_columns(state, reason), Err(expected));
    }

    #[test]
    fn the_errors_never_repeat_the_columns() {
        let refused = [
            LastState::from_columns("sleeping-x", None),
            LastState::from_columns("failed", Some("tired-x")),
        ];
        for error in refused.into_iter().map(Result::unwrap_err) {
            assert!(!error.to_string().contains("-x"), "{error}");
        }
    }

    #[test]
    fn only_paused_and_failed_are_sticky_and_they_keep_their_reason() {
        for state in LastState::ALL {
            let expected = match state {
                LastState::Paused(reason) => Some(StickyState::Paused(reason)),
                LastState::Failed(reason) => Some(StickyState::Failed(reason)),
                _ => None,
            };
            assert_eq!(state.sticky(), expected, "{state:?}");
        }
    }

    #[rstest]
    #[case::stopped(LastState::Stopped, true)]
    #[case::paused(LastState::Paused(DUPLICATE_LOGIN), true)]
    #[case::failed(LastState::Failed(FailReason::Auth), true)]
    #[case::awaiting_session(LastState::AwaitingSession, false)]
    #[case::connecting(LastState::Connecting, false)]
    #[case::online(LastState::Online, false)]
    #[case::backoff(LastState::Backoff, false)]
    #[case::stopping(LastState::Stopping, false)]
    fn only_stopped_paused_and_failed_rest(#[case] state: LastState, #[case] rests: bool) {
        assert_eq!(state.rests(), rests);
    }

    fn n(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).unwrap()
    }

    #[rstest]
    #[case::stopped(BotState::Stopped, LastState::Stopped)]
    #[case::awaiting_session(
        BotState::AwaitingSession { attempt: n(3), fresh: true },
        LastState::AwaitingSession
    )]
    #[case::connecting(
        BotState::Connecting { attempt: n(2), auth_retried: true },
        LastState::Connecting
    )]
    #[case::online(
        BotState::Online { since: DateTime::UNIX_EPOCH, attempt: n(1) },
        LastState::Online
    )]
    #[case::backoff(BotState::Backoff { attempt: n(4), fresh: false }, LastState::Backoff)]
    #[case::fresh_backoff(BotState::Backoff { attempt: n(4), fresh: true }, LastState::Backoff)]
    #[case::paused(BotState::Paused { reason: DUPLICATE_LOGIN }, LastState::Paused(DUPLICATE_LOGIN))]
    #[case::failed(
        BotState::Failed { reason: FailReason::CrashLoop },
        LastState::Failed(FailReason::CrashLoop)
    )]
    #[case::stopping(BotState::Stopping { restart: true }, LastState::Stopping)]
    fn a_bot_state_keeps_its_kind_and_reason(#[case] state: BotState, #[case] last: LastState) {
        assert_eq!(LastState::from(state), last);
    }

    fn reason_strategy() -> impl Strategy<Value = FailReason> {
        proptest::sample::select(vec![
            permanent(PermanentKind::Banned),
            permanent(PermanentKind::NotWhitelisted),
            permanent(PermanentKind::WrongVersion),
            permanent(PermanentKind::AccountBanned),
            permanent(PermanentKind::MultiplayerDisabled),
            FailReason::Auth,
            FailReason::SessionDenied,
            FailReason::CrashLoop,
        ])
    }

    fn state_strategy() -> impl Strategy<Value = BotState> {
        let attempt = (1..=u32::MAX).prop_map(n);
        prop_oneof![
            Just(BotState::Stopped),
            (attempt.clone(), any::<bool>())
                .prop_map(|(attempt, fresh)| BotState::AwaitingSession { attempt, fresh }),
            (attempt.clone(), any::<bool>()).prop_map(|(attempt, auth_retried)| {
                BotState::Connecting {
                    attempt,
                    auth_retried,
                }
            }),
            (0..=4_102_444_800_000_i64, attempt.clone()).prop_map(|(ms, attempt)| {
                BotState::Online {
                    since: DateTime::from_timestamp_millis(ms).unwrap(),
                    attempt,
                }
            }),
            (attempt, any::<bool>())
                .prop_map(|(attempt, fresh)| BotState::Backoff { attempt, fresh }),
            Just(BotState::Paused {
                reason: DUPLICATE_LOGIN
            }),
            reason_strategy().prop_map(|reason| BotState::Failed { reason }),
            any::<bool>().prop_map(|restart| BotState::Stopping { restart }),
        ]
    }

    proptest! {
        #[test]
        fn any_bot_state_is_stored_and_read_back_as_its_last_state(state in state_strategy()) {
            let last = LastState::from(state);
            prop_assert_eq!(LastState::from_columns(last.as_str(), last.reason()), Ok(last));
            prop_assert_eq!(
                last.sticky().map(BotState::from),
                matches!(state, BotState::Paused { .. } | BotState::Failed { .. }).then_some(state)
            );
        }
    }
}
