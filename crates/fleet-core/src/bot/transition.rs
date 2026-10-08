//! [`transition`]: the bot's lifecycle rules.

use core::num::NonZeroU32;

use chrono::{DateTime, Utc};

use super::{BotEvent, BotNotification, BotRules, BotState, Effect, FailReason, PauseReason};
use crate::disconnect::{DisconnectClass, DisconnectReason};
use crate::resilience::RetryPolicy;

/// The result of [`transition`]: the next state, and what the actor must do to
/// get there.
#[must_use = "a transition only takes effect when the actor applies it"]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    /// The state after the event.
    pub state: BotState,
    /// What the actor executes, in this order.
    pub effects: Vec<Effect>,
}

/// Applies `event` to `state` at `now`.
///
/// `rules.retry` decides when a bot has been online long enough to count as
/// stable: its attempt counter then starts over, and the circuit breaker
/// records a success. `rules.conflict_texts` decides which plain-text kicks
/// count as a duplicate login. An event that doesn't fit the state changes
/// nothing.
pub fn transition(
    state: &BotState,
    event: BotEvent,
    now: DateTime<Utc>,
    rules: &BotRules,
) -> Transition {
    let state = *state;
    let policy = &rules.retry;
    match event {
        BotEvent::Start => start(state),
        BotEvent::Stop => stop(state, now, policy),
        BotEvent::Reset => match state {
            BotState::Failed { .. } => begin_run(),
            _ => unchanged(state),
        },
        BotEvent::Resume => match state {
            BotState::Paused { .. } => begin_run(),
            _ => unchanged(state),
        },
        BotEvent::SessionReady => session_ready(state),
        BotEvent::SessionUnavailable { retryable } => session_unavailable(state, retryable),
        BotEvent::Joined => joined(state, now),
        BotEvent::ConnectFailed(failure) => session_ended(
            state,
            DisconnectReason::ConnectFailed { failure }.classify(),
            now,
            policy,
        ),
        BotEvent::Disconnected(reason) => {
            session_ended(state, rules.conflict_texts.classify(&reason), now, policy)
        }
        BotEvent::RetryDue => retry_due(state),
        BotEvent::WatchdogTimeout => session_ended(
            state,
            DisconnectReason::WatchdogTimeout.classify(),
            now,
            policy,
        ),
        BotEvent::Died => died(state),
        BotEvent::SessionClosed => session_closed(state, now, policy),
        BotEvent::CrashLoop => fail(FailReason::CrashLoop, leave(state, now, policy)),
    }
}

/// `Start`: from `Stopped` a new run begins; during `Stopping` the bot starts
/// again once the teardown has finished. `Paused` and `Failed` ignore it.
fn start(state: BotState) -> Transition {
    match state {
        BotState::Stopped => begin_run(),
        BotState::Stopping { restart: false } => Transition {
            state: BotState::Stopping { restart: true },
            effects: vec![Effect::ResetBreaker],
        },
        _ => unchanged(state),
    }
}

/// `Stop`: a session is torn down through `Stopping`; without one the bot stops
/// at once. `Paused` and `Failed` ignore it: they're already offline, and only
/// `Resume` and `Reset` leave them.
fn stop(state: BotState, now: DateTime<Utc>, policy: &RetryPolicy) -> Transition {
    match state {
        BotState::AwaitingSession { .. } | BotState::Backoff { .. } => Transition {
            state: BotState::Stopped,
            effects: Vec::new(),
        },
        BotState::Connecting { .. } | BotState::Online { .. } => Transition {
            state: BotState::Stopping { restart: false },
            effects: leave(state, now, policy),
        },
        BotState::Stopping { .. } => Transition {
            state: BotState::Stopping { restart: false },
            effects: Vec::new(),
        },
        BotState::Stopped | BotState::Paused { .. } | BotState::Failed { .. } => unchanged(state),
    }
}

/// `SessionReady`: connect with the credentials.
fn session_ready(state: BotState) -> Transition {
    match state {
        BotState::AwaitingSession { attempt, fresh } => Transition {
            state: BotState::Connecting {
                attempt,
                auth_retried: fresh,
            },
            effects: vec![Effect::Connect],
        },
        _ => unchanged(state),
    }
}

/// `SessionUnavailable`: back off, or fail if asking again won't help.
fn session_unavailable(state: BotState, retryable: bool) -> Transition {
    match state {
        BotState::AwaitingSession { attempt, .. } if retryable => back_off(attempt, Vec::new()),
        BotState::AwaitingSession { .. } => fail(FailReason::SessionDenied, Vec::new()),
        _ => unchanged(state),
    }
}

/// `Joined`: the bot is online, and its mode starts.
fn joined(state: BotState, now: DateTime<Utc>) -> Transition {
    match state {
        BotState::Connecting { attempt, .. } => Transition {
            state: BotState::Online {
                since: now,
                attempt,
            },
            effects: vec![Effect::StartMode],
        },
        _ => unchanged(state),
    }
}

/// `Died`: respawn and stay online.
fn died(state: BotState) -> Transition {
    match state {
        BotState::Online { .. } => Transition {
            state,
            effects: vec![Effect::Respawn],
        },
        _ => unchanged(state),
    }
}

/// `RetryDue`: the next attempt asks for a session.
fn retry_due(state: BotState) -> Transition {
    match state {
        BotState::Backoff { attempt } => {
            request_session(attempt.saturating_add(1), false, Vec::new())
        }
        _ => unchanged(state),
    }
}

/// `SessionClosed`: while `Stopping`, the teardown has finished. While
/// `Connecting` or `Online`, the session ended without saying why, which counts
/// as a crash.
fn session_closed(state: BotState, now: DateTime<Utc>, policy: &RetryPolicy) -> Transition {
    match state {
        BotState::Stopping { restart: true } => request_session(NonZeroU32::MIN, false, Vec::new()),
        BotState::Stopping { restart: false } => Transition {
            state: BotState::Stopped,
            effects: Vec::new(),
        },
        _ => session_ended(
            state,
            DisconnectReason::SessionCrashed.classify(),
            now,
            policy,
        ),
    }
}

/// The current session ended for a reason of class `class`.
fn session_ended(
    state: BotState,
    class: DisconnectClass,
    now: DateTime<Utc>,
    policy: &RetryPolicy,
) -> Transition {
    match state {
        BotState::Connecting {
            attempt,
            auth_retried,
        } => connecting_ended(attempt, auth_retried, class),
        BotState::Online { since, attempt } => {
            let effects = leave(state, now, policy);
            match class {
                // The session was accepted when the bot joined, so a rejected
                // one counts as transient. A fresh-token retry without a
                // backoff would let a server drive reconnects.
                DisconnectClass::Transient | DisconnectClass::AuthInvalid => {
                    if policy.is_stable(since, now) {
                        back_off(NonZeroU32::MIN, effects)
                    } else {
                        failed_attempt(attempt, effects)
                    }
                }
                DisconnectClass::Permanent { kind } => {
                    fail(FailReason::Permanent { kind }, effects)
                }
                DisconnectClass::Conflict { kind } => {
                    pause(PauseReason::Conflict { kind }, effects)
                }
            }
        }
        _ => unchanged(state),
    }
}

/// Connecting ended for a reason of class `class`.
fn connecting_ended(attempt: NonZeroU32, auth_retried: bool, class: DisconnectClass) -> Transition {
    let effects = vec![Effect::Disconnect];
    match class {
        DisconnectClass::Transient => failed_attempt(attempt, effects),
        DisconnectClass::Permanent { kind } => fail(FailReason::Permanent { kind }, effects),
        DisconnectClass::Conflict { kind } => pause(PauseReason::Conflict { kind }, effects),
        // The first rejected session is retried at once with a fresh one; the
        // second fails the bot (ADR-0010).
        DisconnectClass::AuthInvalid if auth_retried => fail(FailReason::Auth, effects),
        DisconnectClass::AuthInvalid => request_session(attempt, true, effects),
    }
}

/// The effects of leaving `state`: stop the mode, tear the session down, and
/// tell the circuit breaker about a session that lasted the stable period.
fn leave(state: BotState, now: DateTime<Utc>, policy: &RetryPolicy) -> Vec<Effect> {
    match state {
        BotState::Connecting { .. } => vec![Effect::Disconnect],
        BotState::Online { since, .. } if policy.is_stable(since, now) => {
            vec![Effect::StopMode, Effect::Disconnect, Effect::RecordSuccess]
        }
        BotState::Online { .. } => vec![Effect::StopMode, Effect::Disconnect],
        _ => Vec::new(),
    }
}

/// A deliberate start: a new run with a closed circuit breaker.
fn begin_run() -> Transition {
    request_session(NonZeroU32::MIN, false, vec![Effect::ResetBreaker])
}

/// Asks for a session for `attempt`, after `effects`.
fn request_session(attempt: NonZeroU32, fresh: bool, mut effects: Vec<Effect>) -> Transition {
    effects.push(Effect::RequestSession { fresh });
    Transition {
        state: BotState::AwaitingSession { attempt, fresh },
        effects,
    }
}

/// Attempt `attempt` failed: report it to the circuit breaker and back off,
/// after `effects`.
fn failed_attempt(attempt: NonZeroU32, mut effects: Vec<Effect>) -> Transition {
    effects.push(Effect::RecordFailure);
    back_off(attempt, effects)
}

/// Backs off after attempt `attempt`, after `effects`.
fn back_off(attempt: NonZeroU32, mut effects: Vec<Effect>) -> Transition {
    effects.push(Effect::ScheduleRetry { attempt });
    Transition {
        state: BotState::Backoff { attempt },
        effects,
    }
}

/// Fails the bot for `reason` and alerts a human, after `effects`.
fn fail(reason: FailReason, mut effects: Vec<Effect>) -> Transition {
    effects.push(Effect::Notify(BotNotification::Failed { reason }));
    Transition {
        state: BotState::Failed { reason },
        effects,
    }
}

/// Pauses the bot for `reason` and alerts a human, after `effects`.
fn pause(reason: PauseReason, mut effects: Vec<Effect>) -> Transition {
    effects.push(Effect::Notify(BotNotification::Paused { reason }));
    Transition {
        state: BotState::Paused { reason },
        effects,
    }
}

/// Nothing happens: the event doesn't fit `state`.
fn unchanged(state: BotState) -> Transition {
    Transition {
        state,
        effects: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;
    use core::time::Duration;

    use proptest::prelude::*;
    use rstest::rstest;

    use super::*;
    use crate::bot::{BotNotification, FailReason, PauseReason};
    use crate::disconnect::{
        AccountRestriction, ConflictKind, ConflictTexts, ConnectFailure, DisconnectClass,
        DisconnectReason, PermanentKind, SessionServerFailure,
    };
    use crate::time;
    use Effect::{
        Connect, Disconnect, RecordFailure, RecordSuccess, ResetBreaker, Respawn, StartMode,
        StopMode,
    };

    /// `now` in the example tests.
    const NOW: i64 = 10_000;

    const CONNECT_FAILURES: [ConnectFailure; 5] = [
        ConnectFailure::Refused,
        ConnectFailure::TimedOut,
        ConnectFailure::Unresolvable,
        ConnectFailure::HostUnavailable,
        ConnectFailure::Other,
    ];

    const SESSION_SERVER_FAILURES: [SessionServerFailure; 4] = [
        SessionServerFailure::Unreachable,
        SessionServerFailure::RateLimited,
        SessionServerFailure::TimedOut,
        SessionServerFailure::Unexpected,
    ];

    const DUPLICATE_LOGIN: PauseReason = PauseReason::Conflict {
        kind: ConflictKind::DuplicateLogin,
    };

    const fn secs(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn n(attempt: u32) -> NonZeroU32 {
        NonZeroU32::new(attempt).unwrap()
    }

    /// The defaults of Appendix A: 5 s base, 300 s maximum, stable after 300 s.
    fn policy() -> RetryPolicy {
        RetryPolicy::try_new(secs(5), secs(300), secs(300)).unwrap()
    }

    /// The default policy and no conflict texts.
    fn rules() -> BotRules {
        BotRules {
            retry: policy(),
            conflict_texts: ConflictTexts::default(),
        }
    }

    /// The default policy and the given conflict texts.
    fn rules_with(conflict_texts: &[&str]) -> BotRules {
        BotRules {
            retry: policy(),
            conflict_texts: ConflictTexts::try_new(conflict_texts).unwrap(),
        }
    }

    fn apply(state: BotState, event: BotEvent) -> Transition {
        transition(&state, event, at(NOW), &rules())
    }

    fn to(state: BotState, effects: &[Effect]) -> Transition {
        Transition {
            state,
            effects: effects.to_vec(),
        }
    }

    /// Applies `events` from `state`, one second apart, and returns the final
    /// state and every effect.
    fn run(
        mut state: BotState,
        events: impl IntoIterator<Item = BotEvent>,
    ) -> (BotState, Vec<Effect>) {
        let mut effects = Vec::new();
        for (second, event) in (0..).zip(events) {
            let next = transition(&state, event, at(NOW + second), &rules());
            effects.extend(next.effects);
            state = next.state;
        }
        (state, effects)
    }

    fn has_session(state: BotState) -> bool {
        matches!(state, BotState::Connecting { .. } | BotState::Online { .. })
    }

    // States.

    fn awaiting(attempt: u32, fresh: bool) -> BotState {
        BotState::AwaitingSession {
            attempt: n(attempt),
            fresh,
        }
    }

    fn connecting(attempt: u32, auth_retried: bool) -> BotState {
        BotState::Connecting {
            attempt: n(attempt),
            auth_retried,
        }
    }

    /// Online for 10 s at `NOW`: not stable yet.
    fn online(attempt: u32) -> BotState {
        BotState::Online {
            since: at(NOW - 10),
            attempt: n(attempt),
        }
    }

    /// Online for exactly the stable period at `NOW`.
    fn stable_online(attempt: u32) -> BotState {
        BotState::Online {
            since: at(NOW - 300),
            attempt: n(attempt),
        }
    }

    fn backoff(attempt: u32) -> BotState {
        BotState::Backoff {
            attempt: n(attempt),
        }
    }

    fn paused() -> BotState {
        BotState::Paused {
            reason: DUPLICATE_LOGIN,
        }
    }

    fn failed(reason: FailReason) -> BotState {
        BotState::Failed { reason }
    }

    fn stopping(restart: bool) -> BotState {
        BotState::Stopping { restart }
    }

    fn permanent(kind: PermanentKind) -> FailReason {
        FailReason::Permanent { kind }
    }

    // Effects.

    fn request(fresh: bool) -> Effect {
        Effect::RequestSession { fresh }
    }

    fn retry(attempt: u32) -> Effect {
        Effect::ScheduleRetry {
            attempt: n(attempt),
        }
    }

    fn fail(reason: FailReason) -> Effect {
        Effect::Notify(BotNotification::Failed { reason })
    }

    fn pause() -> Effect {
        Effect::Notify(BotNotification::Paused {
            reason: DUPLICATE_LOGIN,
        })
    }

    // Disconnect reasons.

    fn kick(key: &str) -> DisconnectReason {
        DisconnectReason::kicked(Some(key), "bye")
    }

    fn transient() -> DisconnectReason {
        DisconnectReason::ConnectionClosed
    }

    fn banned() -> DisconnectReason {
        kick("multiplayer.disconnect.banned")
    }

    fn not_whitelisted() -> DisconnectReason {
        kick("multiplayer.disconnect.not_whitelisted")
    }

    fn wrong_version() -> DisconnectReason {
        kick("multiplayer.disconnect.incompatible")
    }

    fn duplicate_login() -> DisconnectReason {
        kick("multiplayer.disconnect.duplicate_login")
    }

    fn unverified() -> DisconnectReason {
        kick("multiplayer.disconnect.unverified_username")
    }

    fn account_banned() -> DisconnectReason {
        DisconnectReason::AccountRestricted {
            restriction: AccountRestriction::Banned,
        }
    }

    fn multiplayer_disabled() -> DisconnectReason {
        DisconnectReason::AccountRestricted {
            restriction: AccountRestriction::MultiplayerDisabled,
        }
    }

    fn session_server_failed(failure: SessionServerFailure) -> DisconnectReason {
        DisconnectReason::SessionServerFailed { failure }
    }

    /// What a `BungeeCord` proxy sends when the same account logs in again:
    /// plain text, no translation key.
    fn plain_text_kick() -> DisconnectReason {
        DisconnectReason::kicked(None, "You are already connected to this proxy!")
    }

    /// One reason of every kind the classifier knows.
    fn reasons() -> Vec<DisconnectReason> {
        let mut reasons = vec![
            DisconnectReason::ConnectionClosed,
            DisconnectReason::AuthRejected,
            DisconnectReason::SessionCrashed,
            DisconnectReason::WatchdogTimeout,
            DisconnectReason::LivenessTimeout,
            banned(),
            not_whitelisted(),
            wrong_version(),
            duplicate_login(),
            unverified(),
            plain_text_kick(),
            kick("multiplayer.disconnect.server_shutdown"),
            account_banned(),
            multiplayer_disabled(),
        ];
        reasons.extend(CONNECT_FAILURES.map(|failure| DisconnectReason::ConnectFailed { failure }));
        reasons.extend(SESSION_SERVER_FAILURES.map(session_server_failed));
        reasons
    }

    /// One event of every variant, and `Disconnected` once per class.
    fn all_events() -> Vec<BotEvent> {
        vec![
            BotEvent::Start,
            BotEvent::Stop,
            BotEvent::Reset,
            BotEvent::Resume,
            BotEvent::SessionReady,
            BotEvent::SessionUnavailable { retryable: true },
            BotEvent::SessionUnavailable { retryable: false },
            BotEvent::Joined,
            BotEvent::ConnectFailed(ConnectFailure::Refused),
            BotEvent::Disconnected(transient()),
            BotEvent::Disconnected(banned()),
            BotEvent::Disconnected(duplicate_login()),
            BotEvent::Disconnected(DisconnectReason::AuthRejected),
            BotEvent::RetryDue,
            BotEvent::WatchdogTimeout,
            BotEvent::Died,
            BotEvent::SessionClosed,
            BotEvent::CrashLoop,
        ]
    }

    /// Numbers the variants. It stops compiling when `BotEvent` gains one, so
    /// `all_events` can't fall behind.
    fn variant(event: &BotEvent) -> usize {
        match event {
            BotEvent::Start => 0,
            BotEvent::Stop => 1,
            BotEvent::Reset => 2,
            BotEvent::Resume => 3,
            BotEvent::SessionReady => 4,
            BotEvent::SessionUnavailable { .. } => 5,
            BotEvent::Joined => 6,
            BotEvent::ConnectFailed(_) => 7,
            BotEvent::Disconnected(_) => 8,
            BotEvent::RetryDue => 9,
            BotEvent::WatchdogTimeout => 10,
            BotEvent::Died => 11,
            BotEvent::SessionClosed => 12,
            BotEvent::CrashLoop => 13,
        }
    }

    #[test]
    fn all_events_cover_every_variant() {
        let mut covered: Vec<usize> = all_events().iter().map(variant).collect();
        covered.dedup();
        assert_eq!(covered, (0..=13).collect::<Vec<_>>());
    }

    #[rstest]
    #[case::start(
        BotState::Stopped,
        BotEvent::Start,
        to(awaiting(1, false), &[ResetBreaker, request(false)])
    )]
    #[case::reset_after_auth_failed(
        failed(FailReason::Auth),
        BotEvent::Reset,
        to(awaiting(1, false), &[ResetBreaker, request(false)])
    )]
    #[case::reset_after_a_ban(
        failed(permanent(PermanentKind::Banned)),
        BotEvent::Reset,
        to(awaiting(1, false), &[ResetBreaker, request(false)])
    )]
    #[case::resume(
        paused(),
        BotEvent::Resume,
        to(awaiting(1, false), &[ResetBreaker, request(false)])
    )]
    #[case::start_while_stopping(
        stopping(false),
        BotEvent::Start,
        to(stopping(true), &[ResetBreaker])
    )]
    #[case::stop_while_awaiting_a_session(
        awaiting(2, true),
        BotEvent::Stop,
        to(BotState::Stopped, &[])
    )]
    #[case::stop_during_backoff(backoff(3), BotEvent::Stop, to(BotState::Stopped, &[]))]
    #[case::stop_while_connecting(
        connecting(2, false),
        BotEvent::Stop,
        to(stopping(false), &[Disconnect])
    )]
    #[case::stop_while_online(
        online(2),
        BotEvent::Stop,
        to(stopping(false), &[StopMode, Disconnect])
    )]
    #[case::stop_after_the_stable_period(
        stable_online(2),
        BotEvent::Stop,
        to(stopping(false), &[StopMode, Disconnect, RecordSuccess])
    )]
    #[case::stop_cancels_a_restart(stopping(true), BotEvent::Stop, to(stopping(false), &[]))]
    #[case::teardown_finished(
        stopping(false),
        BotEvent::SessionClosed,
        to(BotState::Stopped, &[])
    )]
    #[case::teardown_finished_before_a_restart(
        stopping(true),
        BotEvent::SessionClosed,
        to(awaiting(1, false), &[request(false)])
    )]
    fn starting_and_stopping(
        #[case] state: BotState,
        #[case] event: BotEvent,
        #[case] expected: Transition,
    ) {
        assert_eq!(apply(state, event), expected);
    }

    #[rstest]
    #[case::session_ready(
        awaiting(3, false),
        BotEvent::SessionReady,
        to(connecting(3, false), &[Connect])
    )]
    #[case::fresh_session_ready(
        awaiting(3, true),
        BotEvent::SessionReady,
        to(connecting(3, true), &[Connect])
    )]
    #[case::session_unavailable(
        awaiting(3, false),
        BotEvent::SessionUnavailable { retryable: true },
        to(backoff(3), &[retry(3)])
    )]
    #[case::fresh_session_unavailable(
        awaiting(3, true),
        BotEvent::SessionUnavailable { retryable: true },
        to(backoff(3), &[retry(3)])
    )]
    #[case::session_denied(
        awaiting(3, false),
        BotEvent::SessionUnavailable { retryable: false },
        to(failed(FailReason::SessionDenied), &[fail(FailReason::SessionDenied)])
    )]
    #[case::joined(
        connecting(3, true),
        BotEvent::Joined,
        to(BotState::Online { since: at(NOW), attempt: n(3) }, &[StartMode])
    )]
    #[case::died(online(3), BotEvent::Died, to(online(3), &[Respawn]))]
    #[case::retry_due(backoff(3), BotEvent::RetryDue, to(awaiting(4, false), &[request(false)]))]
    #[case::retry_due_saturates(
        backoff(u32::MAX),
        BotEvent::RetryDue,
        to(awaiting(u32::MAX, false), &[request(false)])
    )]
    fn sessions_and_retries(
        #[case] state: BotState,
        #[case] event: BotEvent,
        #[case] expected: Transition,
    ) {
        assert_eq!(apply(state, event), expected);
    }

    #[rstest]
    #[case::stopped(BotState::Stopped, &[fail(FailReason::CrashLoop)])]
    #[case::awaiting_session(awaiting(2, true), &[fail(FailReason::CrashLoop)])]
    #[case::connecting(connecting(2, false), &[Disconnect, fail(FailReason::CrashLoop)])]
    #[case::online(online(2), &[StopMode, Disconnect, fail(FailReason::CrashLoop)])]
    #[case::stable_online(
        stable_online(2),
        &[StopMode, Disconnect, RecordSuccess, fail(FailReason::CrashLoop)]
    )]
    #[case::backoff(backoff(2), &[fail(FailReason::CrashLoop)])]
    #[case::paused(paused(), &[fail(FailReason::CrashLoop)])]
    #[case::failed_after_a_ban(
        failed(permanent(PermanentKind::Banned)),
        &[fail(FailReason::CrashLoop)]
    )]
    #[case::failed_by_a_crash_loop(failed(FailReason::CrashLoop), &[fail(FailReason::CrashLoop)])]
    #[case::stopping(stopping(false), &[fail(FailReason::CrashLoop)])]
    #[case::restarting(stopping(true), &[fail(FailReason::CrashLoop)])]
    fn crash_loop_fails_the_bot_from_any_state(
        #[case] state: BotState,
        #[case] effects: &[Effect],
    ) {
        assert_eq!(
            apply(state, BotEvent::CrashLoop),
            to(failed(FailReason::CrashLoop), effects)
        );
    }

    #[rstest]
    #[case::transient(
        connecting(2, false),
        BotEvent::Disconnected(transient()),
        to(backoff(2), &[Disconnect, RecordFailure, retry(2)])
    )]
    #[case::transient_with_a_fresh_session(
        connecting(2, true),
        BotEvent::Disconnected(transient()),
        to(backoff(2), &[Disconnect, RecordFailure, retry(2)])
    )]
    #[case::connect_failed(
        connecting(2, false),
        BotEvent::ConnectFailed(ConnectFailure::Refused),
        to(backoff(2), &[Disconnect, RecordFailure, retry(2)])
    )]
    #[case::watchdog_timeout(
        connecting(2, false),
        BotEvent::WatchdogTimeout,
        to(backoff(2), &[Disconnect, RecordFailure, retry(2)])
    )]
    #[case::session_closed(
        connecting(2, false),
        BotEvent::SessionClosed,
        to(backoff(2), &[Disconnect, RecordFailure, retry(2)])
    )]
    #[case::banned(
        connecting(2, false),
        BotEvent::Disconnected(banned()),
        to(
            failed(permanent(PermanentKind::Banned)),
            &[Disconnect, fail(permanent(PermanentKind::Banned))]
        )
    )]
    #[case::not_whitelisted(
        connecting(2, false),
        BotEvent::Disconnected(not_whitelisted()),
        to(
            failed(permanent(PermanentKind::NotWhitelisted)),
            &[Disconnect, fail(permanent(PermanentKind::NotWhitelisted))]
        )
    )]
    #[case::wrong_version(
        connecting(2, false),
        BotEvent::Disconnected(wrong_version()),
        to(
            failed(permanent(PermanentKind::WrongVersion)),
            &[Disconnect, fail(permanent(PermanentKind::WrongVersion))]
        )
    )]
    #[case::account_banned(
        connecting(2, false),
        BotEvent::Disconnected(account_banned()),
        to(
            failed(permanent(PermanentKind::AccountBanned)),
            &[Disconnect, fail(permanent(PermanentKind::AccountBanned))]
        )
    )]
    #[case::multiplayer_disabled(
        connecting(2, false),
        BotEvent::Disconnected(multiplayer_disabled()),
        to(
            failed(permanent(PermanentKind::MultiplayerDisabled)),
            &[Disconnect, fail(permanent(PermanentKind::MultiplayerDisabled))]
        )
    )]
    // Unlike a rejected token, an outage gets a backoff, not a fresh session.
    #[case::session_server_failed(
        connecting(2, false),
        BotEvent::Disconnected(session_server_failed(SessionServerFailure::Unreachable)),
        to(backoff(2), &[Disconnect, RecordFailure, retry(2)])
    )]
    #[case::session_server_failed_with_a_fresh_session(
        connecting(2, true),
        BotEvent::Disconnected(session_server_failed(SessionServerFailure::TimedOut)),
        to(backoff(2), &[Disconnect, RecordFailure, retry(2)])
    )]
    #[case::duplicate_login(
        connecting(2, false),
        BotEvent::Disconnected(duplicate_login()),
        to(paused(), &[Disconnect, pause()])
    )]
    #[case::auth_rejected(
        connecting(2, false),
        BotEvent::Disconnected(DisconnectReason::AuthRejected),
        to(awaiting(2, true), &[Disconnect, request(true)])
    )]
    #[case::unverified_username(
        connecting(2, false),
        BotEvent::Disconnected(unverified()),
        to(awaiting(2, true), &[Disconnect, request(true)])
    )]
    #[case::auth_rejected_again(
        connecting(2, true),
        BotEvent::Disconnected(DisconnectReason::AuthRejected),
        to(failed(FailReason::Auth), &[Disconnect, fail(FailReason::Auth)])
    )]
    #[case::unverified_username_again(
        connecting(2, true),
        BotEvent::Disconnected(unverified()),
        to(failed(FailReason::Auth), &[Disconnect, fail(FailReason::Auth)])
    )]
    fn session_ends_while_connecting(
        #[case] state: BotState,
        #[case] event: BotEvent,
        #[case] expected: Transition,
    ) {
        assert_eq!(apply(state, event), expected);
    }

    #[rstest]
    #[case::transient(
        online(2),
        BotEvent::Disconnected(transient()),
        to(backoff(2), &[StopMode, Disconnect, RecordFailure, retry(2)])
    )]
    #[case::transient_after_the_stable_period(
        stable_online(2),
        BotEvent::Disconnected(transient()),
        to(backoff(1), &[StopMode, Disconnect, RecordSuccess, retry(1)])
    )]
    #[case::liveness_timeout(
        online(2),
        BotEvent::Disconnected(DisconnectReason::LivenessTimeout),
        to(backoff(2), &[StopMode, Disconnect, RecordFailure, retry(2)])
    )]
    #[case::watchdog_timeout(
        online(2),
        BotEvent::WatchdogTimeout,
        to(backoff(2), &[StopMode, Disconnect, RecordFailure, retry(2)])
    )]
    #[case::session_closed(
        online(2),
        BotEvent::SessionClosed,
        to(backoff(2), &[StopMode, Disconnect, RecordFailure, retry(2)])
    )]
    #[case::connect_failed(
        online(2),
        BotEvent::ConnectFailed(ConnectFailure::TimedOut),
        to(backoff(2), &[StopMode, Disconnect, RecordFailure, retry(2)])
    )]
    // A duplicate login behind a proxy: transient until P4.1 adds conflict texts.
    #[case::plain_text_kick(
        online(2),
        BotEvent::Disconnected(plain_text_kick()),
        to(backoff(2), &[StopMode, Disconnect, RecordFailure, retry(2)])
    )]
    // The session was accepted at join, so no fresh-token shortcut: a server
    // can't make the bot reconnect without a backoff.
    #[case::auth_rejected(
        online(2),
        BotEvent::Disconnected(DisconnectReason::AuthRejected),
        to(backoff(2), &[StopMode, Disconnect, RecordFailure, retry(2)])
    )]
    #[case::auth_rejected_after_the_stable_period(
        stable_online(2),
        BotEvent::Disconnected(DisconnectReason::AuthRejected),
        to(backoff(1), &[StopMode, Disconnect, RecordSuccess, retry(1)])
    )]
    #[case::banned(
        online(2),
        BotEvent::Disconnected(banned()),
        to(
            failed(permanent(PermanentKind::Banned)),
            &[StopMode, Disconnect, fail(permanent(PermanentKind::Banned))]
        )
    )]
    #[case::banned_after_the_stable_period(
        stable_online(2),
        BotEvent::Disconnected(banned()),
        to(
            failed(permanent(PermanentKind::Banned)),
            &[StopMode, Disconnect, RecordSuccess, fail(permanent(PermanentKind::Banned))]
        )
    )]
    #[case::account_banned(
        online(2),
        BotEvent::Disconnected(account_banned()),
        to(
            failed(permanent(PermanentKind::AccountBanned)),
            &[StopMode, Disconnect, fail(permanent(PermanentKind::AccountBanned))]
        )
    )]
    #[case::multiplayer_disabled_after_the_stable_period(
        stable_online(2),
        BotEvent::Disconnected(multiplayer_disabled()),
        to(
            failed(permanent(PermanentKind::MultiplayerDisabled)),
            &[
                StopMode,
                Disconnect,
                RecordSuccess,
                fail(permanent(PermanentKind::MultiplayerDisabled)),
            ]
        )
    )]
    #[case::session_server_failed(
        online(2),
        BotEvent::Disconnected(session_server_failed(SessionServerFailure::RateLimited)),
        to(backoff(2), &[StopMode, Disconnect, RecordFailure, retry(2)])
    )]
    #[case::session_server_failed_after_the_stable_period(
        stable_online(2),
        BotEvent::Disconnected(session_server_failed(SessionServerFailure::Unexpected)),
        to(backoff(1), &[StopMode, Disconnect, RecordSuccess, retry(1)])
    )]
    #[case::duplicate_login(
        online(2),
        BotEvent::Disconnected(duplicate_login()),
        to(paused(), &[StopMode, Disconnect, pause()])
    )]
    #[case::duplicate_login_after_the_stable_period(
        stable_online(2),
        BotEvent::Disconnected(duplicate_login()),
        to(paused(), &[StopMode, Disconnect, RecordSuccess, pause()])
    )]
    fn session_ends_while_online(
        #[case] state: BotState,
        #[case] event: BotEvent,
        #[case] expected: Transition,
    ) {
        assert_eq!(apply(state, event), expected);
    }

    /// A proxy's plain-text duplicate-login kick (a placeholder text).
    const PROXY_TEXT: &str = "You are already connected to this proxy!";

    #[rstest]
    #[case::connecting(connecting(2, false), to(paused(), &[Disconnect, pause()]))]
    #[case::online(online(2), to(paused(), &[StopMode, Disconnect, pause()]))]
    #[case::stable_online(
        stable_online(2),
        to(paused(), &[StopMode, Disconnect, RecordSuccess, pause()])
    )]
    fn a_listed_plain_text_kick_pauses_the_bot(
        #[case] state: BotState,
        #[case] expected: Transition,
    ) {
        let event = BotEvent::Disconnected(DisconnectReason::kicked(None, PROXY_TEXT));

        assert_eq!(
            transition(&state, event, at(NOW), &rules_with(&[PROXY_TEXT])),
            expected
        );
    }

    #[test]
    fn an_unlisted_plain_text_kick_stays_transient() {
        let event = BotEvent::Disconnected(DisconnectReason::kicked(None, "Server restarting"));

        assert_eq!(
            transition(&online(2), event, at(NOW), &rules_with(&[PROXY_TEXT])),
            to(backoff(2), &[StopMode, Disconnect, RecordFailure, retry(2)])
        );
    }

    #[test]
    fn a_permanent_key_wins_over_a_listed_text() {
        let event = BotEvent::Disconnected(DisconnectReason::kicked(
            Some("multiplayer.disconnect.banned"),
            PROXY_TEXT,
        ));

        let banned = permanent(PermanentKind::Banned);
        assert_eq!(
            transition(&online(2), event, at(NOW), &rules_with(&[PROXY_TEXT])),
            to(failed(banned), &[StopMode, Disconnect, fail(banned)])
        );
    }

    #[rstest]
    fn session_end_events_match_their_disconnect_reasons(
        #[values(connecting(2, false), connecting(2, true), online(2), stable_online(2))]
        state: BotState,
    ) {
        assert_eq!(
            apply(state, BotEvent::WatchdogTimeout),
            apply(
                state,
                BotEvent::Disconnected(DisconnectReason::WatchdogTimeout)
            )
        );
        assert_eq!(
            apply(state, BotEvent::SessionClosed),
            apply(
                state,
                BotEvent::Disconnected(DisconnectReason::SessionCrashed)
            )
        );
        for failure in CONNECT_FAILURES {
            assert_eq!(
                apply(state, BotEvent::ConnectFailed(failure)),
                apply(
                    state,
                    BotEvent::Disconnected(DisconnectReason::ConnectFailed { failure })
                )
            );
        }
    }

    #[rstest]
    #[case::not_yet(299, to(backoff(4), &[StopMode, Disconnect, RecordFailure, retry(4)]))]
    #[case::exactly_at_the_period(
        300,
        to(backoff(1), &[StopMode, Disconnect, RecordSuccess, retry(1)])
    )]
    #[case::clock_went_backwards(
        -10,
        to(backoff(4), &[StopMode, Disconnect, RecordFailure, retry(4)])
    )]
    fn attempt_counter_starts_over_after_the_stable_period(
        #[case] online_for: i64,
        #[case] expected: Transition,
    ) {
        let state = BotState::Online {
            since: at(1_000),
            attempt: n(4),
        };
        let event = BotEvent::Disconnected(transient());

        assert_eq!(
            transition(&state, event, at(1_000 + online_for), &rules()),
            expected
        );
    }

    #[test]
    fn the_first_retry_waits_the_base_delay() {
        let (state, effects) = run(
            BotState::Stopped,
            [
                BotEvent::Start,
                BotEvent::SessionReady,
                BotEvent::ConnectFailed(ConnectFailure::Refused),
            ],
        );

        assert_eq!(state, backoff(1));
        assert_eq!(effects.last(), Some(&retry(1)));
        assert_eq!(policy().bounds(n(1)), (secs(5), secs(10)));
    }

    #[test]
    fn an_offline_account_on_an_online_mode_server_fails_after_one_fresh_session() {
        let (state, effects) = run(
            BotState::Stopped,
            [
                BotEvent::Start,
                BotEvent::SessionReady,
                BotEvent::Disconnected(unverified()),
                BotEvent::SessionReady,
                BotEvent::Disconnected(unverified()),
                BotEvent::SessionReady,
                BotEvent::RetryDue,
                BotEvent::Start,
            ],
        );

        assert_eq!(state, failed(FailReason::Auth));
        assert_eq!(
            effects.iter().filter(|&&effect| effect == Connect).count(),
            2
        );
        assert_eq!(effects.last(), Some(&fail(FailReason::Auth)));
    }

    #[test]
    fn a_failed_fresh_session_request_starts_the_auth_retry_over() {
        let (state, effects) = run(
            connecting(1, false),
            [
                BotEvent::Disconnected(DisconnectReason::AuthRejected),
                BotEvent::SessionUnavailable { retryable: true },
                BotEvent::RetryDue,
            ],
        );

        assert_eq!(state, awaiting(2, false));
        assert_eq!(effects.last(), Some(&request(false)));
    }

    #[rstest]
    #[case::stopped(BotState::Stopped, |e: &BotEvent| {
        matches!(e, BotEvent::Start | BotEvent::CrashLoop)
    })]
    #[case::awaiting_session(awaiting(2, false), |e: &BotEvent| {
        matches!(
            e,
            BotEvent::Stop
                | BotEvent::SessionReady
                | BotEvent::SessionUnavailable { .. }
                | BotEvent::CrashLoop
        )
    })]
    #[case::connecting(connecting(2, false), |e: &BotEvent| {
        matches!(
            e,
            BotEvent::Stop
                | BotEvent::Joined
                | BotEvent::ConnectFailed(_)
                | BotEvent::Disconnected(_)
                | BotEvent::WatchdogTimeout
                | BotEvent::SessionClosed
                | BotEvent::CrashLoop
        )
    })]
    #[case::online(online(2), |e: &BotEvent| {
        matches!(
            e,
            BotEvent::Stop
                | BotEvent::Died
                | BotEvent::ConnectFailed(_)
                | BotEvent::Disconnected(_)
                | BotEvent::WatchdogTimeout
                | BotEvent::SessionClosed
                | BotEvent::CrashLoop
        )
    })]
    #[case::backoff(backoff(2), |e: &BotEvent| {
        matches!(e, BotEvent::Stop | BotEvent::RetryDue | BotEvent::CrashLoop)
    })]
    #[case::paused(paused(), |e: &BotEvent| {
        matches!(e, BotEvent::Resume | BotEvent::CrashLoop)
    })]
    #[case::failed(failed(FailReason::Auth), |e: &BotEvent| {
        matches!(e, BotEvent::Reset | BotEvent::CrashLoop)
    })]
    #[case::stopping(stopping(false), |e: &BotEvent| {
        matches!(e, BotEvent::Start | BotEvent::SessionClosed | BotEvent::CrashLoop)
    })]
    #[case::restarting(stopping(true), |e: &BotEvent| {
        matches!(e, BotEvent::Stop | BotEvent::SessionClosed | BotEvent::CrashLoop)
    })]
    fn other_events_change_nothing(
        #[case] state: BotState,
        #[case] handled: fn(&BotEvent) -> bool,
    ) {
        for event in all_events().into_iter().filter(|event| !handled(event)) {
            assert_eq!(apply(state, event.clone()), to(state, &[]), "{event:?}");
        }
    }

    // Property tests.

    fn attempt_strategy() -> impl Strategy<Value = NonZeroU32> {
        (1..=u32::MAX).prop_map(n)
    }

    fn time_strategy() -> impl Strategy<Value = DateTime<Utc>> {
        (0..=100_000_i64).prop_map(at)
    }

    fn fail_reason_strategy() -> impl Strategy<Value = FailReason> {
        prop_oneof![
            Just(permanent(PermanentKind::Banned)),
            Just(permanent(PermanentKind::NotWhitelisted)),
            Just(permanent(PermanentKind::WrongVersion)),
            Just(permanent(PermanentKind::AccountBanned)),
            Just(permanent(PermanentKind::MultiplayerDisabled)),
            Just(FailReason::Auth),
            Just(FailReason::SessionDenied),
            Just(FailReason::CrashLoop),
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
            Just(paused()),
            fail_reason_strategy().prop_map(failed),
            any::<bool>().prop_map(stopping),
        ]
    }

    fn session_state_strategy() -> impl Strategy<Value = BotState> {
        state_strategy().prop_filter("a state with a session", |&state| has_session(state))
    }

    fn event_strategy() -> impl Strategy<Value = BotEvent> {
        prop_oneof![
            Just(BotEvent::Start),
            Just(BotEvent::Stop),
            Just(BotEvent::Reset),
            Just(BotEvent::Resume),
            Just(BotEvent::SessionReady),
            any::<bool>().prop_map(|retryable| BotEvent::SessionUnavailable { retryable }),
            Just(BotEvent::Joined),
            proptest::sample::select(CONNECT_FAILURES.to_vec()).prop_map(BotEvent::ConnectFailed),
            proptest::sample::select(reasons()).prop_map(BotEvent::Disconnected),
            Just(BotEvent::RetryDue),
            Just(BotEvent::WatchdogTimeout),
            Just(BotEvent::Died),
            Just(BotEvent::SessionClosed),
            Just(BotEvent::CrashLoop),
        ]
    }

    /// Events, each after a pause of up to 400 s, so sessions sometimes
    /// outlast the stable period.
    fn steps_strategy() -> impl Strategy<Value = Vec<(BotEvent, u64)>> {
        proptest::collection::vec((event_strategy(), 0..=400_u64), 0..100)
    }

    fn count(effects: &[Effect], effect: Effect) -> usize {
        effects.iter().filter(|&&e| e == effect).count()
    }

    fn notification(state: BotState) -> Option<BotNotification> {
        match state {
            BotState::Paused { reason } => Some(BotNotification::Paused { reason }),
            BotState::Failed { reason } => Some(BotNotification::Failed { reason }),
            _ => None,
        }
    }

    proptest! {
        #[test]
        fn stop_then_session_closed_ends_in_stopped(
            state in state_strategy(),
            now in time_strategy(),
        ) {
            let stop = transition(&state, BotEvent::Stop, now, &rules());
            let closed = transition(&stop.state, BotEvent::SessionClosed, now, &rules());

            if matches!(state, BotState::Paused { .. } | BotState::Failed { .. }) {
                prop_assert_eq!(stop, to(state, &[]));
                prop_assert_eq!(closed, to(state, &[]));
            } else {
                prop_assert_eq!(closed.state, BotState::Stopped);
                let disconnects =
                    count(&stop.effects, Disconnect) + count(&closed.effects, Disconnect);
                prop_assert_eq!(disconnects, usize::from(has_session(state)));
            }
        }

        #[test]
        fn failed_and_paused_never_connect_before_reset_or_resume(
            start in prop_oneof![Just(paused()), fail_reason_strategy().prop_map(failed)],
            steps in steps_strategy(),
        ) {
            let mut state = start;
            let mut now = at(0);
            for (event, pause) in steps {
                if matches!(event, BotEvent::Reset | BotEvent::Resume) {
                    break;
                }
                now = time::add(now, secs(pause));
                let next = transition(&state, event, now, &rules());
                prop_assert!(!next.effects.contains(&Connect));
                prop_assert!(
                    matches!(next.state, BotState::Paused { .. } | BotState::Failed { .. }),
                    "left Paused or Failed: {:?}",
                    next.state
                );
                state = next.state;
            }
        }

        #[test]
        fn never_connects_while_connecting_or_online(
            state in session_state_strategy(),
            event in event_strategy(),
            now in time_strategy(),
        ) {
            let next = transition(&state, event, now, &rules());
            prop_assert!(!next.effects.contains(&Connect));
        }

        #[test]
        fn connects_only_when_a_session_arrives(
            state in state_strategy(),
            event in event_strategy(),
            now in time_strategy(),
        ) {
            let next = transition(&state, event, now, &rules());
            if next.effects.contains(&Connect) {
                prop_assert!(
                    matches!(state, BotState::AwaitingSession { .. }),
                    "connected from {:?}",
                    state
                );
                prop_assert!(
                    matches!(next.state, BotState::Connecting { .. }),
                    "connected into {:?}",
                    next.state
                );
            }
        }

        #[test]
        fn attempt_counter_resets_after_the_stable_period(
            since in 0..=100_000_i64,
            attempt in attempt_strategy(),
            online_for in -1_000..=1_000_i64,
            stable_after in 1..=600_u64,
            reason in proptest::sample::select(reasons()),
        ) {
            prop_assume!(matches!(
                reason.classify(),
                DisconnectClass::Transient | DisconnectClass::AuthInvalid
            ));
            let rules = BotRules {
                retry: RetryPolicy::try_new(secs(5), secs(300), secs(stable_after)).unwrap(),
                conflict_texts: ConflictTexts::default(),
            };
            let state = BotState::Online { since: at(since), attempt };

            let next = transition(
                &state,
                BotEvent::Disconnected(reason),
                at(since + online_for),
                &rules,
            );

            let stable = u64::try_from(online_for).is_ok_and(|online_for| online_for >= stable_after);
            let expected = if stable { NonZeroU32::MIN } else { attempt };
            prop_assert_eq!(next.state, BotState::Backoff { attempt: expected });
        }

        #[test]
        fn sessions_and_modes_are_balanced(steps in steps_strategy()) {
            let mut state = BotState::Stopped;
            let mut now = at(0);
            let (mut sessions, mut modes) = (0_i64, 0_i64);
            for (event, pause) in steps {
                now = time::add(now, secs(pause));
                let next = transition(&state, event, now, &rules());
                for effect in &next.effects {
                    match effect {
                        Connect => sessions += 1,
                        Disconnect => sessions -= 1,
                        StartMode => modes += 1,
                        StopMode => modes -= 1,
                        _ => {}
                    }
                }
                state = next.state;
                prop_assert_eq!(sessions, i64::from(has_session(state)));
                prop_assert_eq!(modes, i64::from(matches!(state, BotState::Online { .. })));
            }
        }

        #[test]
        fn connects_are_bounded_by_retries_and_deliberate_starts(
            start in state_strategy(),
            steps in steps_strategy(),
        ) {
            let mut state = start;
            let mut now = at(0);
            let (mut connects, mut triggers) = (0_usize, 0_usize);
            for (event, pause) in steps {
                if matches!(
                    event,
                    BotEvent::Start | BotEvent::Reset | BotEvent::Resume | BotEvent::RetryDue
                ) {
                    triggers += 1;
                }
                now = time::add(now, secs(pause));
                let next = transition(&state, event, now, &rules());
                connects += count(&next.effects, Connect);
                state = next.state;
                prop_assert!(connects <= 2 * (1 + triggers));
            }
        }

        #[test]
        fn notifies_exactly_when_entering_paused_or_failed(
            state in state_strategy(),
            event in event_strategy(),
            now in time_strategy(),
        ) {
            let crash_loop = event == BotEvent::CrashLoop;
            let next = transition(&state, event, now, &rules());

            let notifications: Vec<_> = next
                .effects
                .iter()
                .filter_map(|effect| match effect {
                    Effect::Notify(notification) => Some(*notification),
                    _ => None,
                })
                .collect();
            let entered = next.state != state || crash_loop;
            let expected: Vec<_> =
                notification(next.state).filter(|_| entered).into_iter().collect();
            prop_assert_eq!(notifications, expected);
        }

        #[test]
        fn retries_follow_backoff_and_report_to_the_breaker(
            state in state_strategy(),
            event in event_strategy(),
            now in time_strategy(),
        ) {
            let next = transition(&state, event, now, &rules());

            let retries: Vec<_> = next
                .effects
                .iter()
                .filter(|effect| matches!(effect, Effect::ScheduleRetry { .. }))
                .copied()
                .collect();
            match next.state {
                BotState::Backoff { attempt } if next.state != state => {
                    prop_assert_eq!(retries, vec![Effect::ScheduleRetry { attempt }]);
                    let reports =
                        count(&next.effects, RecordFailure) + count(&next.effects, RecordSuccess);
                    prop_assert_eq!(reports, usize::from(has_session(state)));
                }
                _ => prop_assert!(retries.is_empty()),
            }

            let stable_exit = match state {
                BotState::Online { since, .. } => {
                    !matches!(next.state, BotState::Online { .. })
                        && policy().is_stable(since, now)
                }
                _ => false,
            };
            prop_assert_eq!(count(&next.effects, RecordSuccess), usize::from(stable_exit));
            prop_assert!(count(&next.effects, RecordFailure) <= 1);
            if let Some(position) = next.effects.iter().position(|&effect| effect == RecordFailure) {
                prop_assert!(
                    matches!(next.effects.get(position + 1), Some(Effect::ScheduleRetry { .. })),
                    "RecordFailure without a retry: {:?}",
                    next.effects
                );
            }
        }

        #[test]
        fn only_deliberate_starts_reset_the_breaker(
            state in state_strategy(),
            event in event_strategy(),
            now in time_strategy(),
        ) {
            let deliberate =
                matches!(event, BotEvent::Start | BotEvent::Reset | BotEvent::Resume);
            let next = transition(&state, event, now, &rules());

            let resets = count(&next.effects, ResetBreaker);
            prop_assert_eq!(resets, usize::from(deliberate && !next.effects.is_empty()));
        }
    }
}
