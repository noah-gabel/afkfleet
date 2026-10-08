//! [`restore`]: where a restarted actor picks a bot up.

use core::num::NonZeroU32;

use chrono::{DateTime, Utc};

use super::{BotRules, BotState, Effect, Transition};

/// Returns where a bot's restarted actor starts, from the last state its
/// crashed actor published at `now` (Plan.md §6 row 7; ADR-0013).
///
/// A restart never skips the backoff:
/// - `AwaitingSession`, `Connecting`, `Online` and `Backoff` come back as
///   `Backoff` for the same attempt, with `ScheduleRetry`. An `Online` bot
///   that had lasted the stable period comes back as `Backoff{1}`, and its
///   session counts as a success for the circuit breaker, as when it ends any
///   other way.
/// - `Paused`, `Failed` and `Stopped` stay as they are.
/// - `Stopping` becomes `Stopped`: the old actor's session is gone.
///
/// A crash isn't a breaker failure, so nothing here records one. The actor
/// then applies the spec's desired state.
pub fn restore(state: &BotState, now: DateTime<Utc>, rules: &BotRules) -> Transition {
    match *state {
        BotState::Online { since, .. } if rules.retry.is_stable(since, now) => {
            back_off(NonZeroU32::MIN, vec![Effect::RecordSuccess])
        }
        BotState::AwaitingSession { attempt, .. }
        | BotState::Connecting { attempt, .. }
        | BotState::Online { attempt, .. }
        | BotState::Backoff { attempt } => back_off(attempt, Vec::new()),
        BotState::Stopping { .. } => Transition {
            state: BotState::Stopped,
            effects: Vec::new(),
        },
        BotState::Stopped | BotState::Paused { .. } | BotState::Failed { .. } => Transition {
            state: *state,
            effects: Vec::new(),
        },
    }
}

/// Backs off after attempt `attempt`, after `effects`.
fn back_off(attempt: NonZeroU32, mut effects: Vec<Effect>) -> Transition {
    effects.push(Effect::ScheduleRetry { attempt });
    Transition {
        state: BotState::Backoff { attempt },
        effects,
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;
    use core::time::Duration;

    use proptest::prelude::*;
    use rstest::rstest;

    use super::*;
    use crate::bot::{Effect, FailReason, PauseReason};
    use crate::disconnect::{ConflictKind, ConflictTexts, PermanentKind};
    use crate::resilience::RetryPolicy;

    /// `now` in the example tests.
    const NOW: i64 = 10_000;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn n(attempt: u32) -> NonZeroU32 {
        NonZeroU32::new(attempt).unwrap()
    }

    /// The defaults of Appendix A: 5 s base, 300 s maximum, stable after 300 s.
    fn rules() -> BotRules {
        BotRules {
            retry: RetryPolicy::try_new(
                Duration::from_secs(5),
                Duration::from_secs(300),
                Duration::from_secs(300),
            )
            .unwrap(),
            conflict_texts: ConflictTexts::default(),
        }
    }

    fn backoff(attempt: u32) -> BotState {
        BotState::Backoff {
            attempt: n(attempt),
        }
    }

    fn retry(attempt: u32) -> Effect {
        Effect::ScheduleRetry {
            attempt: n(attempt),
        }
    }

    fn to(state: BotState, effects: &[Effect]) -> Transition {
        Transition {
            state,
            effects: effects.to_vec(),
        }
    }

    const PAUSED: BotState = BotState::Paused {
        reason: PauseReason::Conflict {
            kind: ConflictKind::DuplicateLogin,
        },
    };

    #[rstest]
    #[case::awaiting_session(
        BotState::AwaitingSession { attempt: n(3), fresh: false },
        to(backoff(3), &[retry(3)])
    )]
    #[case::awaiting_a_fresh_session(
        BotState::AwaitingSession { attempt: n(3), fresh: true },
        to(backoff(3), &[retry(3)])
    )]
    #[case::connecting(
        BotState::Connecting { attempt: n(4), auth_retried: false },
        to(backoff(4), &[retry(4)])
    )]
    #[case::connecting_with_a_fresh_session(
        BotState::Connecting { attempt: n(4), auth_retried: true },
        to(backoff(4), &[retry(4)])
    )]
    #[case::online_before_the_stable_period(
        BotState::Online { since: at(NOW - 299), attempt: n(5) },
        to(backoff(5), &[retry(5)])
    )]
    #[case::online_for_exactly_the_stable_period(
        BotState::Online { since: at(NOW - 300), attempt: n(5) },
        to(backoff(1), &[Effect::RecordSuccess, retry(1)])
    )]
    #[case::online_when_the_clock_went_backwards(
        BotState::Online { since: at(NOW + 10), attempt: n(5) },
        to(backoff(5), &[retry(5)])
    )]
    #[case::backoff(backoff(6), to(backoff(6), &[retry(6)]))]
    #[case::paused(PAUSED, to(PAUSED, &[]))]
    #[case::failed(
        BotState::Failed { reason: FailReason::Permanent { kind: PermanentKind::Banned } },
        to(BotState::Failed { reason: FailReason::Permanent { kind: PermanentKind::Banned } }, &[])
    )]
    #[case::failed_by_a_crash_loop(
        BotState::Failed { reason: FailReason::CrashLoop },
        to(BotState::Failed { reason: FailReason::CrashLoop }, &[])
    )]
    #[case::stopped(BotState::Stopped, to(BotState::Stopped, &[]))]
    #[case::stopping(BotState::Stopping { restart: false }, to(BotState::Stopped, &[]))]
    #[case::stopping_for_a_restart(
        BotState::Stopping { restart: true },
        to(BotState::Stopped, &[])
    )]
    fn a_restart_picks_the_bot_up_without_skipping_its_backoff(
        #[case] state: BotState,
        #[case] expected: Transition,
    ) {
        assert_eq!(restore(&state, at(NOW), &rules()), expected);
    }

    fn attempt_strategy() -> impl Strategy<Value = NonZeroU32> {
        (1..=u32::MAX).prop_map(n)
    }

    fn time_strategy() -> impl Strategy<Value = DateTime<Utc>> {
        (0..=100_000_i64).prop_map(at)
    }

    /// Paused, and Failed for every reason.
    fn sticky_strategy() -> impl Strategy<Value = BotState> {
        let permanent = |kind| FailReason::Permanent { kind };
        prop_oneof![
            Just(PAUSED),
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
            .prop_map(|reason| BotState::Failed { reason }),
        ]
    }

    fn state_strategy() -> impl Strategy<Value = BotState> {
        prop_oneof![
            Just(BotState::Stopped),
            (attempt_strategy(), any::<bool>())
                .prop_map(|(attempt, fresh)| BotState::AwaitingSession { attempt, fresh }),
            (attempt_strategy(), any::<bool>()).prop_map(|(attempt, auth_retried)| {
                BotState::Connecting {
                    attempt,
                    auth_retried,
                }
            }),
            (time_strategy(), attempt_strategy())
                .prop_map(|(since, attempt)| BotState::Online { since, attempt }),
            attempt_strategy().prop_map(|attempt| BotState::Backoff { attempt }),
            sticky_strategy(),
            any::<bool>().prop_map(|restart| BotState::Stopping { restart }),
        ]
    }

    proptest! {
        #[test]
        fn a_restart_never_connects_records_a_failure_or_alerts(
            state in state_strategy(),
            now in time_strategy(),
        ) {
            let restored = restore(&state, now, &rules());

            for effect in &restored.effects {
                prop_assert!(
                    matches!(effect, Effect::ScheduleRetry { .. } | Effect::RecordSuccess),
                    "unexpected effect {:?}",
                    effect
                );
            }
            prop_assert!(
                matches!(
                    restored.state,
                    BotState::Backoff { .. }
                        | BotState::Paused { .. }
                        | BotState::Failed { .. }
                        | BotState::Stopped
                ),
                "restored into {:?}",
                restored.state
            );
        }

        #[test]
        fn a_retry_is_scheduled_exactly_when_the_bot_backs_off(
            state in state_strategy(),
            now in time_strategy(),
        ) {
            let restored = restore(&state, now, &rules());

            let retries: Vec<_> = restored
                .effects
                .iter()
                .filter(|effect| matches!(effect, Effect::ScheduleRetry { .. }))
                .copied()
                .collect();
            match restored.state {
                BotState::Backoff { attempt } => {
                    prop_assert_eq!(retries, vec![Effect::ScheduleRetry { attempt }]);
                    prop_assert_eq!(restored.effects.last(), Some(&Effect::ScheduleRetry { attempt }));
                }
                _ => prop_assert!(restored.effects.is_empty()),
            }
        }

        // Generated directly, not filtered out of every state: a filter that
        // rejects most cases aborts the run at CI's 1000 cases.
        #[test]
        fn sticky_states_stay_as_they_are(state in sticky_strategy(), now in time_strategy()) {
            prop_assert_eq!(restore(&state, now, &rules()), to(state, &[]));
        }

        #[test]
        fn restoring_a_restored_state_keeps_it(state in state_strategy(), now in time_strategy()) {
            let once = restore(&state, now, &rules());
            let twice = restore(&once.state, now, &rules());

            prop_assert_eq!(twice.state, once.state);
        }
    }
}
