//! The live events socket's reconnect policy (ADR-0020, S54): what the
//! socket does after each end and each refused connect, and how long it
//! waits. [`decide`] reads an end by S54's table; [`Backoff`] turns the
//! decision into a wait.
//!
//! - **Prompt** (no wait, no failure counted): the first 4001 since the last
//!   stable socket, the one expected per access token's life; the ticket's
//!   request then refreshes the session.
//! - **Backoff** (jittered steps of 1–2, 2–4, 4–8 and 8–16 s, then 15–30 s):
//!   every other close, a lost connection, silence, no `ready`, a frame too
//!   large, a repeated 4001, an unreachable server, and the answers a server
//!   gives while it's busy or a ticket was bad; a `Retry-After` is honoured,
//!   up to [`RETRY_AFTER_MAX`].
//! - **Slow** (the 15–30 s plateau at once): answers that won't fix
//!   themselves in seconds, such as a server without the route or a 101
//!   that doesn't check out.
//! - **Park:** a refused certificate gets no timer (ADR-0017's banner rule);
//!   the socket ends (R73).
//!
//! The failures start over only after a socket stayed live for
//! [`STABLE_AFTER`], longer than the server's idle close, so a server that
//! sends `ready` and drops the socket can't make the client loop. The jitter
//! is drawn from a seed the process got from the OS, through a splitmix64
//! sequence, as the agent's link backoff draws it (S67).

use core::time::Duration;
use std::hash::{BuildHasher as _, Hasher as _, RandomState};

use tracing::Level;

/// The first backoff step.
pub const RECONNECT_BASE: Duration = Duration::from_secs(1);

/// The longest backoff step.
pub const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// How long a socket must stay live after its `ready` for the failures to
/// start over.
pub const STABLE_AFTER: Duration = Duration::from_secs(60);

/// The most of a server's `Retry-After` the socket waits.
pub const RETRY_AFTER_MAX: Duration = Duration::from_secs(60);

/// How long after its 101 a socket waits for `ready`.
pub const READY_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a live socket goes without any frame before it takes the server
/// for gone.
pub const SILENCE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the socket's own close, and the drain after the server's,
/// may take.
pub const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// What an attempt ended in, as the policy reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum End {
    /// The server closed the socket with this code, or without one.
    Closed(Option<u16>),
    /// The connection went without a close frame.
    Lost,
    /// No frame of any kind for [`SILENCE_TIMEOUT`] after `ready`.
    Silent,
    /// No `ready` within [`READY_TIMEOUT`] of the 101.
    NoReady,
    /// A frame or message over the socket's limit.
    TooLarge,
    /// A known event before `ready`.
    EarlyEvent,
    /// The ticket's request was refused, or got no answer.
    Ticket(Refused),
    /// The upgrade was answered without a 101, or got no answer.
    Upgrade(Refused),
    /// The 101 didn't check out.
    Handshake,
}

/// How a request was refused, or why it got no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refused {
    /// An answer with this status, and the wait it asked for.
    Status {
        status: u16,
        retry_after: Option<Duration>,
    },
    /// The server couldn't be reached.
    Unreachable,
    /// The request took too long.
    TimedOut,
    /// The server's certificate was refused.
    Certificate,
    /// A success the route never answers: the wrong body, too large.
    Malformed,
    /// The client's own failure: its session store, or its refresh's task.
    Local,
}

/// What the socket does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// Reconnect at once, counting no failure.
    Prompt,
    /// Count a failure and wait its step, at least `retry_after`.
    Backoff { retry_after: Option<Duration> },
    /// Count a failure and wait the plateau.
    Slow,
    /// Give up: the socket ends.
    Park,
}

/// What follows `end`, by S54's table; `prompt_left` says whether the
/// expected 4001 since the last stable socket is still to come.
#[must_use]
pub(crate) fn decide(end: End, prompt_left: bool) -> Action {
    const BACKOFF: Action = Action::Backoff { retry_after: None };
    match end {
        End::Closed(Some(4001)) if prompt_left => Action::Prompt,
        End::Closed(_) | End::Lost | End::Silent | End::NoReady | End::TooLarge => BACKOFF,
        End::EarlyEvent | End::Handshake => Action::Slow,
        End::Ticket(refused) => refusal(refused, Request::Ticket),
        End::Upgrade(refused) => refusal(refused, Request::Upgrade),
    }
}

/// The close codes the protocol names (`SocketClose`), and a normal close.
const KNOWN_CODES: [u16; 7] = [1000, 1001, 1008, 1013, 4000, 4001, 4002];

/// The level the line about `end` logs at, `action` being what follows it
/// and `repeat` whether the last line logged the same end (S54, R86).
#[must_use]
pub(crate) fn level(end: End, action: Action, repeat: bool) -> Level {
    if repeat {
        return Level::DEBUG;
    }
    match action {
        // The expected 4001, once per access token's life.
        Action::Prompt => Level::INFO,
        Action::Slow | Action::Park => Level::WARN,
        Action::Backoff { .. } => match end {
            // A 4001 again before a socket stayed live: the expected one's
            // repeat.
            End::Closed(Some(4001)) => Level::DEBUG,
            End::Closed(Some(4000 | 4002 | 1008)) | End::Silent | End::NoReady => Level::WARN,
            End::Closed(Some(code)) if !KNOWN_CODES.contains(&code) => Level::WARN,
            End::Closed(_)
            | End::Lost
            | End::TooLarge
            | End::EarlyEvent
            | End::Handshake
            | End::Ticket(_)
            | End::Upgrade(_) => Level::INFO,
        },
    }
}

/// Which request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Request {
    Ticket,
    Upgrade,
}

/// What follows `refused`, an answer to `request` or none.
const fn refusal(refused: Refused, request: Request) -> Action {
    match refused {
        Refused::Certificate => Action::Park,
        Refused::Unreachable | Refused::TimedOut | Refused::Local => {
            Action::Backoff { retry_after: None }
        }
        Refused::Status {
            status: 429 | 503,
            retry_after,
        } => Action::Backoff { retry_after },
        // The uniform answer to a bad ticket, and a token the API took
        // for the ticket but the upgrade didn't: a new ticket after the wait.
        Refused::Status {
            status: 401 | 404, ..
        } if matches!(request, Request::Upgrade) => Action::Backoff { retry_after: None },
        Refused::Status {
            status: 408 | 500..=599,
            ..
        } => Action::Backoff { retry_after: None },
        // An answer that doesn't read, a server without the route, a
        // proxy's answer, a refusal that won't change in seconds.
        Refused::Malformed | Refused::Status { .. } => Action::Slow,
    }
}

/// The waits between attempts; see the [module docs](self).
#[derive(Debug, Clone)]
pub(crate) struct Backoff {
    /// Attempts that failed since the count last started over.
    failures: u32,
    /// Whether a 4001 may still reconnect at once.
    prompt_left: bool,
    /// The splitmix64 state.
    seed: u64,
}

impl Backoff {
    /// The waits, their jitter drawn from `seed`.
    #[must_use]
    pub(crate) const fn new(seed: u64) -> Self {
        Self {
            failures: 0,
            prompt_left: true,
            seed,
        }
    }

    /// What follows `end`, as [`Backoff::after`] will decide it, live for
    /// `live` after its `ready` if it got one.
    #[must_use]
    pub(crate) fn action(&self, end: End, live: Option<Duration>) -> Action {
        decide(end, self.prompt_left || Self::stable(live))
    }

    /// Whether a socket live for `live` stayed long enough for the failures
    /// to start over.
    fn stable(live: Option<Duration>) -> bool {
        live.is_some_and(|live| live >= STABLE_AFTER)
    }

    /// The waits, their jitter seeded by the OS.
    #[must_use]
    pub(crate) fn seeded() -> Self {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(0);
        Self::new(hasher.finish())
    }

    /// The wait after an attempt that ended in `end`, live for `live` after
    /// its `ready` if it got one; `None` when the socket gives up.
    pub(crate) fn after(&mut self, end: End, live: Option<Duration>) -> Option<Duration> {
        if Self::stable(live) {
            self.failures = 0;
            self.prompt_left = true;
        }
        match decide(end, self.prompt_left) {
            Action::Prompt => {
                self.prompt_left = false;
                Some(Duration::ZERO)
            }
            Action::Park => None,
            Action::Backoff { retry_after } => {
                self.failures = self.failures.saturating_add(1);
                let step = self.step(self.failures);
                Some(retry_after.map_or(step, |asked| step.max(asked.min(RETRY_AFTER_MAX))))
            }
            Action::Slow => {
                self.failures = self.failures.saturating_add(1);
                Some(self.step(u32::MAX))
            }
        }
    }

    /// The wait for failure `n`, counted from 1.
    fn step(&mut self, n: u32) -> Duration {
        // The lower bound doubles from the base to half the cap; the wait
        // falls anywhere from it to twice it, never past the cap.
        let doublings = n.saturating_sub(1).min(16);
        let lower = RECONNECT_BASE
            .saturating_mul(1 << doublings)
            .min(RECONNECT_MAX / 2);
        let upper = lower.saturating_mul(2).min(RECONNECT_MAX);
        let span = u64::try_from(upper.saturating_sub(lower).as_millis()).unwrap_or(0);
        lower.saturating_add(Duration::from_millis(self.draw() % span.saturating_add(1)))
    }

    /// The next draw of the sequence.
    fn draw(&mut self) -> u64 {
        // splitmix64 (Steele, Lea and Flood): a full-period sequence.
        self.seed = self.seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use fleet_api_types::events::EVENT_TICKET_MAX_SECS;
    use fleet_server::http::ws::{CHECK_TIMEOUT, IDLE_TIMEOUT, PING_INTERVAL, SEND_TIMEOUT};
    use rstest::rstest;

    use super::*;
    use crate::REQUEST_TIMEOUT;

    const fn secs(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    fn status(status: u16) -> Refused {
        Refused::Status {
            status,
            retry_after: None,
        }
    }

    const BACKOFF: Action = Action::Backoff { retry_after: None };

    #[rstest]
    #[case::shutdown(End::Closed(Some(1001)), BACKOFF)]
    #[case::busy(End::Closed(Some(1013)), BACKOFF)]
    #[case::no_pong(End::Closed(Some(4000)), BACKOFF)]
    #[case::too_slow(End::Closed(Some(4002)), BACKOFF)]
    #[case::refused_frame(End::Closed(Some(1008)), BACKOFF)]
    #[case::unknown_code(End::Closed(Some(4999)), BACKOFF)]
    #[case::normal_close(End::Closed(Some(1000)), BACKOFF)]
    #[case::no_code(End::Closed(None), BACKOFF)]
    #[case::lost(End::Lost, BACKOFF)]
    #[case::silent(End::Silent, BACKOFF)]
    #[case::no_ready(End::NoReady, BACKOFF)]
    #[case::too_large(End::TooLarge, BACKOFF)]
    #[case::early_event(End::EarlyEvent, Action::Slow)]
    #[case::handshake(End::Handshake, Action::Slow)]
    #[case::ticket_unreachable(End::Ticket(Refused::Unreachable), BACKOFF)]
    #[case::ticket_timed_out(End::Ticket(Refused::TimedOut), BACKOFF)]
    #[case::ticket_local(End::Ticket(Refused::Local), BACKOFF)]
    #[case::ticket_certificate(End::Ticket(Refused::Certificate), Action::Park)]
    #[case::ticket_malformed(End::Ticket(Refused::Malformed), Action::Slow)]
    #[case::ticket_500(End::Ticket(status(500)), BACKOFF)]
    #[case::ticket_502(End::Ticket(status(502)), BACKOFF)]
    #[case::ticket_408(End::Ticket(status(408)), BACKOFF)]
    #[case::ticket_400(End::Ticket(status(400)), Action::Slow)]
    #[case::ticket_403(End::Ticket(status(403)), Action::Slow)]
    #[case::ticket_404(End::Ticket(status(404)), Action::Slow)]
    #[case::ticket_405(End::Ticket(status(405)), Action::Slow)]
    #[case::ticket_422(End::Ticket(status(422)), Action::Slow)]
    #[case::ticket_302(End::Ticket(status(302)), Action::Slow)]
    #[case::upgrade_unreachable(End::Upgrade(Refused::Unreachable), BACKOFF)]
    #[case::upgrade_timed_out(End::Upgrade(Refused::TimedOut), BACKOFF)]
    #[case::upgrade_certificate(End::Upgrade(Refused::Certificate), Action::Park)]
    #[case::upgrade_401(End::Upgrade(status(401)), BACKOFF)]
    #[case::upgrade_404(End::Upgrade(status(404)), BACKOFF)]
    #[case::upgrade_408(End::Upgrade(status(408)), BACKOFF)]
    #[case::upgrade_500(End::Upgrade(status(500)), BACKOFF)]
    #[case::upgrade_200(End::Upgrade(status(200)), Action::Slow)]
    #[case::upgrade_301(End::Upgrade(status(301)), Action::Slow)]
    #[case::upgrade_400(End::Upgrade(status(400)), Action::Slow)]
    #[case::upgrade_403(End::Upgrade(status(403)), Action::Slow)]
    #[case::upgrade_405(End::Upgrade(status(405)), Action::Slow)]
    #[case::upgrade_malformed(End::Upgrade(Refused::Malformed), Action::Slow)]
    fn each_end_has_its_action(#[case] end: End, #[case] action: Action) {
        assert_eq!(decide(end, true), action, "{end:?}");
        assert_eq!(decide(end, false), action, "{end:?}");
    }

    #[rstest]
    #[case::ticket(End::Ticket)]
    #[case::upgrade(End::Upgrade)]
    fn a_busy_or_rate_limited_answer_backs_off_with_its_retry_after(
        #[case] end: fn(Refused) -> End,
    ) {
        for code in [429, 503] {
            let asked = Refused::Status {
                status: code,
                retry_after: Some(secs(45)),
            };

            assert_eq!(
                decide(end(asked), true),
                Action::Backoff {
                    retry_after: Some(secs(45))
                },
                "{code}"
            );
        }
    }

    /// Every end the socket can meet, each request's refusals by status.
    fn every_end() -> Vec<End> {
        let mut ends = vec![
            End::Lost,
            End::Silent,
            End::NoReady,
            End::TooLarge,
            End::EarlyEvent,
            End::Handshake,
            End::Closed(None),
        ];
        ends.extend(
            [1000, 1001, 1008, 1013, 4000, 4001, 4002, 4999].map(|code| End::Closed(Some(code))),
        );
        for request in [End::Ticket, End::Upgrade] {
            ends.extend(
                [
                    Refused::Unreachable,
                    Refused::TimedOut,
                    Refused::Local,
                    Refused::Certificate,
                    Refused::Malformed,
                ]
                .map(request),
            );
            ends.extend(
                [
                    200, 201, 301, 302, 400, 401, 403, 404, 405, 408, 422, 429, 500, 502, 503,
                ]
                .map(|code| request(status(code))),
            );
        }
        ends
    }

    #[rstest]
    #[case::shutdown(End::Closed(Some(1001)), Level::INFO)]
    #[case::busy(End::Closed(Some(1013)), Level::INFO)]
    #[case::normal_close(End::Closed(Some(1000)), Level::INFO)]
    #[case::no_code(End::Closed(None), Level::INFO)]
    #[case::no_pong(End::Closed(Some(4000)), Level::WARN)]
    #[case::too_slow(End::Closed(Some(4002)), Level::WARN)]
    #[case::refused_frame(End::Closed(Some(1008)), Level::WARN)]
    #[case::unknown_code(End::Closed(Some(4999)), Level::WARN)]
    #[case::lost(End::Lost, Level::INFO)]
    #[case::silent(End::Silent, Level::WARN)]
    #[case::no_ready(End::NoReady, Level::WARN)]
    #[case::too_large(End::TooLarge, Level::INFO)]
    #[case::early_event(End::EarlyEvent, Level::WARN)]
    #[case::handshake(End::Handshake, Level::WARN)]
    #[case::ticket_unreachable(End::Ticket(Refused::Unreachable), Level::INFO)]
    #[case::ticket_timed_out(End::Ticket(Refused::TimedOut), Level::INFO)]
    #[case::ticket_local(End::Ticket(Refused::Local), Level::INFO)]
    #[case::ticket_certificate(End::Ticket(Refused::Certificate), Level::WARN)]
    #[case::ticket_malformed(End::Ticket(Refused::Malformed), Level::WARN)]
    #[case::ticket_408(End::Ticket(status(408)), Level::INFO)]
    #[case::ticket_429(End::Ticket(status(429)), Level::INFO)]
    #[case::ticket_500(End::Ticket(status(500)), Level::INFO)]
    #[case::ticket_503(End::Ticket(status(503)), Level::INFO)]
    #[case::ticket_400(End::Ticket(status(400)), Level::WARN)]
    #[case::ticket_403(End::Ticket(status(403)), Level::WARN)]
    #[case::ticket_404(End::Ticket(status(404)), Level::WARN)]
    #[case::ticket_405(End::Ticket(status(405)), Level::WARN)]
    #[case::ticket_422(End::Ticket(status(422)), Level::WARN)]
    #[case::ticket_302(End::Ticket(status(302)), Level::WARN)]
    #[case::upgrade_unreachable(End::Upgrade(Refused::Unreachable), Level::INFO)]
    #[case::upgrade_401(End::Upgrade(status(401)), Level::INFO)]
    #[case::upgrade_404(End::Upgrade(status(404)), Level::INFO)]
    #[case::upgrade_429(End::Upgrade(status(429)), Level::INFO)]
    #[case::upgrade_200(End::Upgrade(status(200)), Level::WARN)]
    #[case::upgrade_301(End::Upgrade(status(301)), Level::WARN)]
    #[case::upgrade_400(End::Upgrade(status(400)), Level::WARN)]
    #[case::upgrade_403(End::Upgrade(status(403)), Level::WARN)]
    #[case::upgrade_405(End::Upgrade(status(405)), Level::WARN)]
    #[case::upgrade_malformed(End::Upgrade(Refused::Malformed), Level::WARN)]
    fn each_end_logs_at_its_level(#[case] end: End, #[case] expected: Level) {
        for prompt_left in [true, false] {
            assert_eq!(
                level(end, decide(end, prompt_left), false),
                expected,
                "{end:?}, prompt left: {prompt_left}"
            );
        }
    }

    #[test]
    fn every_slow_end_logs_at_warn() {
        let mut slow = 0;
        for end in every_end() {
            let action = decide(end, true);
            if action == Action::Slow {
                slow += 1;
                assert_eq!(level(end, action, false), Level::WARN, "{end:?}");
            }
        }
        // The ticket's 200, 201, 301, 302, 400, 401, 403, 404, 405, 422 and
        // malformed answer, the upgrade's 200, 201, 301, 302, 400, 403, 405,
        // 422 and malformed answer, a bad 101 and an early event.
        assert_eq!(slow, 22);
    }

    #[test]
    fn only_the_expected_4001_logs_at_info_and_one_the_policy_backs_off_for_at_debug() {
        let end = End::Closed(Some(4001));

        assert_eq!(level(end, decide(end, true), false), Level::INFO);
        assert_eq!(level(end, decide(end, false), false), Level::DEBUG);
    }

    #[test]
    fn an_end_repeated_in_a_row_logs_at_debug() {
        for end in every_end() {
            for prompt_left in [true, false] {
                assert_eq!(
                    level(end, decide(end, prompt_left), true),
                    Level::DEBUG,
                    "{end:?}"
                );
            }
        }
    }

    #[test]
    fn the_action_is_the_one_after_takes() {
        let end = End::Closed(Some(4001));
        let mut backoff = Backoff::new(9);

        let first = backoff.action(end, Some(secs(5)));
        backoff.after(end, Some(secs(5)));
        let storm = backoff.action(end, Some(secs(5)));
        let after_a_stable_one = backoff.action(end, Some(STABLE_AFTER));

        assert_eq!(first, Action::Prompt);
        assert_eq!(storm, BACKOFF);
        assert_eq!(after_a_stable_one, Action::Prompt);
        assert_eq!(backoff.action(End::Handshake, None), Action::Slow);
    }

    #[test]
    fn the_first_4001_reconnects_at_once_and_another_backs_off() {
        assert_eq!(decide(End::Closed(Some(4001)), true), Action::Prompt);
        assert_eq!(decide(End::Closed(Some(4001)), false), BACKOFF);
    }

    fn within(wait: Option<Duration>, lower: u64, upper: u64) -> bool {
        wait.is_some_and(|wait| secs(lower) <= wait && wait <= secs(upper))
    }

    #[test]
    fn the_waits_double_to_their_plateau() {
        let mut backoff = Backoff::new(1);

        let waits: Vec<Option<Duration>> = (0..7).map(|_| backoff.after(End::Lost, None)).collect();

        for (wait, (lower, upper)) in waits.iter().zip([
            (1, 2),
            (2, 4),
            (4, 8),
            (8, 16),
            (15, 30),
            (15, 30),
            (15, 30),
        ]) {
            assert!(within(*wait, lower, upper), "{waits:?}");
        }
    }

    #[test]
    fn a_slow_answer_waits_the_plateau_from_the_first() {
        let mut backoff = Backoff::new(2);

        let first = backoff.after(End::Handshake, None);
        let next = backoff.after(End::Lost, None);

        assert!(within(first, 15, 30), "{first:?}");
        assert!(within(next, 2, 4), "a slow answer counts: {next:?}");
    }

    #[test]
    fn the_expected_4001_reconnects_at_once_once_per_stable_socket() {
        let mut backoff = Backoff::new(3);

        let first = backoff.after(End::Closed(Some(4001)), Some(secs(5)));
        let storm = backoff.after(End::Closed(Some(4001)), Some(secs(5)));
        let after_a_stable_one = backoff.after(End::Closed(Some(4001)), Some(STABLE_AFTER));

        assert_eq!(first, Some(Duration::ZERO));
        assert!(within(storm, 1, 2), "{storm:?}");
        assert_eq!(after_a_stable_one, Some(Duration::ZERO));
    }

    #[test]
    fn a_stable_socket_starts_the_count_over_and_a_short_one_doesnt() {
        let mut backoff = Backoff::new(4);
        for _ in 0..4 {
            backoff.after(End::Lost, None);
        }

        let short = backoff.after(
            End::Lost,
            Some(STABLE_AFTER.checked_sub(Duration::from_millis(1)).unwrap()),
        );
        let stable = backoff.after(End::Lost, Some(STABLE_AFTER));

        assert!(within(short, 15, 30), "{short:?}");
        assert!(within(stable, 1, 2), "{stable:?}");
    }

    #[test]
    fn a_retry_after_is_waited_up_to_its_cap_and_never_less_than_the_step() {
        let asked = |retry_after: u64| {
            End::Upgrade(Refused::Status {
                status: 429,
                retry_after: Some(secs(retry_after)),
            })
        };
        let mut backoff = Backoff::new(5);

        let too_many = backoff.after(asked(45), None);
        let huge = backoff.after(asked(86_400), None);
        let zero = backoff.after(asked(0), None);

        assert_eq!(too_many, Some(secs(45)));
        assert_eq!(huge, Some(RETRY_AFTER_MAX));
        assert!(within(zero, 4, 8), "the third step: {zero:?}");
    }

    #[test]
    fn a_refused_certificate_gets_no_wait() {
        let mut backoff = Backoff::new(6);

        assert_eq!(backoff.after(End::Ticket(Refused::Certificate), None), None);
    }

    #[test]
    fn the_jitter_follows_its_seed() {
        let waits = |seed: u64| {
            let mut backoff = Backoff::new(seed);
            (0..6)
                .map(|_| backoff.after(End::Lost, None))
                .collect::<Vec<_>>()
        };

        assert_eq!(waits(7), waits(7));
        assert_ne!(waits(7), waits(8));
    }

    /// Over many seeds and every end, no wait exceeds the cap, and only a
    /// prompt reconnects in less than the base.
    #[test]
    fn no_wait_is_past_the_cap_or_shorter_than_the_base_but_a_prompt() {
        let ends = [
            End::Lost,
            End::Closed(Some(4001)),
            End::Handshake,
            End::Upgrade(Refused::Status {
                status: 429,
                retry_after: Some(secs(1_000)),
            }),
        ];
        for seed in 0..200 {
            let mut backoff = Backoff::new(seed);
            for (n, end) in ends.iter().cycle().take(40).enumerate() {
                let live = (n % 7 == 0).then_some(STABLE_AFTER);
                let wait = backoff.after(*end, live).unwrap();

                assert!(wait <= RETRY_AFTER_MAX, "{seed} {n}: {wait:?}");
                assert!(
                    wait >= RECONNECT_BASE || (wait.is_zero() && *end == End::Closed(Some(4001))),
                    "{seed} {n}: {wait:?}"
                );
            }
        }
    }

    #[test]
    fn the_bounds_fit_the_servers() {
        assert!(REQUEST_TIMEOUT < secs(u64::from(EVENT_TICKET_MAX_SECS)));
        assert!(READY_TIMEOUT > CHECK_TIMEOUT + SEND_TIMEOUT);
        assert!(SILENCE_TIMEOUT >= PING_INTERVAL * 2);
        assert!(STABLE_AFTER > IDLE_TIMEOUT && STABLE_AFTER > SILENCE_TIMEOUT);
        assert!(
            RETRY_AFTER_MAX >= IDLE_TIMEOUT,
            "too_many_connections' 45 s"
        );
        assert!(!RECONNECT_BASE.is_zero() && RECONNECT_MAX >= RECONNECT_BASE * 2);
    }

    #[test]
    fn the_os_seeds_two_backoffs_apart() {
        assert_ne!(Backoff::seeded().seed, Backoff::seeded().seed);
    }
}
