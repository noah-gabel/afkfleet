//! The scenario's judgments, as pure functions of what it read: Compose's
//! version, RCON's player list, nextest's time limit, the waits between a
//! bot's attempts and the deadlines the retry policy allows, the server's
//! healthy moment, and the log lines nobody expects.

use core::time::Duration;
use std::num::NonZeroU32;

use chrono::{DateTime, Utc};
use figment::Figment;
use figment::providers::{Format, Toml};
use fleet_core::resilience::{CircuitBreaker, CircuitPolicy, RetryPolicy};
use serde::Deserialize;

use crate::docker::Probe;
use crate::logs::{LogLine, State};
use crate::stack_checks::ExpectedWarning;

/// The oldest Compose that knows `!reset`, which compose.isolated.yaml uses.
pub(crate) const MIN_COMPOSE: (u32, u32) = (2, 24);

/// How much shorter than its window a measured wait may be. The log's times
/// are taken when each line is written, not when the backoff timer starts,
/// so a jitter right at the lower bound can measure a few ms short; a
/// reconnect storm still misses the bound by seconds.
pub(crate) const BELOW: Duration = Duration::from_millis(100);
/// How much longer than its window a measured wait may be: timers and
/// logging on a loaded machine.
pub(crate) const ABOVE: Duration = Duration::from_secs(1);

/// The `(major, minor)` version in `docker compose version --short`'s
/// output, such as `5.5.1`, `v2.24.0` or `2.24.0-desktop.1`.
pub(crate) fn compose_version(short: &str) -> Option<(u32, u32)> {
    let version = short.trim().trim_start_matches('v');
    let version = version.split('-').next()?;
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// The players in the output of RCON's `list`, such as `There are 2 of a max
/// of 60 players online: AfkBot4, AfkBot5`.
pub(crate) fn online_players(list: &str) -> Vec<String> {
    let (_, players) = list.split_once(':').unwrap_or_default();
    players
        .split(',')
        .map(str::trim)
        .filter(|player| !player.is_empty())
        .map(str::to_owned)
        .collect()
}

/// How long nextest's `slow` profile lets a test run before it kills it:
/// `slow-timeout`'s period times `terminate-after`.
pub(crate) fn slow_limit(nextest_toml: &str) -> Duration {
    #[derive(Deserialize)]
    struct SlowTimeout {
        period: String,
        #[serde(rename = "terminate-after")]
        terminate_after: u32,
    }

    let timeout: SlowTimeout = Figment::from(Toml::string(nextest_toml))
        .extract_inner("profile.slow.slow-timeout")
        .expect("nextest.toml should set the slow profile's slow-timeout with terminate-after");
    let period = timeout
        .period
        .strip_suffix('s')
        .and_then(|secs| secs.parse().ok())
        .map_or_else(
            || {
                panic!(
                    "the slow-timeout period should be in seconds: {}",
                    timeout.period
                )
            },
            Duration::from_secs,
        );
    period * timeout.terminate_after
}

/// A problem when `elapsed` plus every reserved part exceeds `limit`.
pub(crate) fn budget_problem(
    elapsed: Duration,
    reserved: &[(&str, Duration)],
    limit: Duration,
) -> Option<String> {
    let needed = reserved
        .iter()
        .fold(elapsed, |needed, (_, part)| needed + *part);
    if needed <= limit {
        return None;
    }
    let parts: Vec<String> = reserved
        .iter()
        .map(|(what, part)| format!("{what} ({part:?})"))
        .collect();
    Some(format!(
        "nextest's limit of {limit:?} can't hold the rest of the test: {elapsed:?} have passed, \n         and it still needs {}",
        parts.join(" + ")
    ))
}

/// One wait between two of a bot's attempts: from its `Backoff { attempt }`
/// to the `AwaitingSession` that follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Gap {
    pub(crate) attempt: NonZeroU32,
    pub(crate) from: DateTime<Utc>,
    pub(crate) length: Duration,
}

/// The waits in a bot's states.
pub(crate) fn gaps(states: &[(DateTime<Utc>, State)]) -> Vec<Gap> {
    states
        .windows(2)
        .filter_map(|pair| match pair {
            [
                (from, State::Backoff { attempt }),
                (to, State::AwaitingSession { .. }),
            ] => Some(Gap {
                attempt: NonZeroU32::new(*attempt)?,
                from: *from,
                length: (*to - *from).to_std().ok()?,
            }),
            _ => None,
        })
        .collect()
}

/// A problem when `gap` lies outside the policy's window for its attempt,
/// with [`BELOW`] and [`ABOVE`] as slack.
pub(crate) fn gap_problem(policy: &RetryPolicy, gap: &Gap) -> Option<String> {
    let (min, max) = policy.bounds(gap.attempt);
    let fits = gap.length + BELOW >= min && gap.length <= max + ABOVE;
    (!fits).then(|| {
        format!(
            "the wait after attempt {} from {} took {:?}, outside its window of {min:?} to \n             {max:?}",
            gap.attempt, gap.from, gap.length
        )
    })
}

/// Until when a breaker with `circuit` would be open at `at`, had every
/// time in `failures` been a failure. A long gap after that many failures is
/// the breaker's cool-down, not the backoff's.
pub(crate) fn breaker_open_until(
    circuit: CircuitPolicy,
    failures: &[DateTime<Utc>],
    at: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let mut breaker = CircuitBreaker::new(circuit);
    for failure in failures {
        breaker.record_failure(*failure);
    }
    breaker.open_until().filter(|until| at < *until)
}

/// How long a bot may take to come Online once the server is up: its first
/// attempt, one failed attempt with the backoff after it, and the attempt
/// that works, plus a second.
pub(crate) fn first_join_deadline(policy: &RetryPolicy, connect: Duration) -> Duration {
    let (_, backoff) = policy.bounds(NonZeroU32::MIN);
    connect + backoff + connect + Duration::from_secs(1)
}

/// How long a bot whose last backoff was `attempt` may take to come Online
/// again from the server's healthy moment: the rest of that backoff, one more
/// failed attempt with its backoff (Docker checks the server's health only
/// every 5 s, so an attempt in flight then can still fail), and the attempt
/// that works, plus a second.
pub(crate) fn reconnect_deadline(
    policy: &RetryPolicy,
    connect: Duration,
    attempt: NonZeroU32,
) -> Duration {
    let (_, rest) = policy.bounds(attempt);
    let (_, next) = policy.bounds(attempt.saturating_add(1));
    rest + connect + next + connect + Duration::from_secs(1)
}

/// When the server became healthy after starting at `started_at`: the end of
/// the first successful probe that started after it.
pub(crate) fn healthy_since(probes: &[Probe], started_at: DateTime<Utc>) -> Option<DateTime<Utc>> {
    probes
        .iter()
        .filter(|probe| probe.exit_code == 0 && probe.start >= started_at)
        .min_by_key(|probe| probe.start)
        .map(|probe| probe.end)
}

/// The lines of `log` nobody expects: every ERROR, and every WARN that no
/// entry of `expected` matches by target and message prefix.
pub(crate) fn unexpected<'a>(log: &'a [LogLine], expected: &[ExpectedWarning]) -> Vec<&'a LogLine> {
    log.iter()
        .filter(|line| match line.level.as_str() {
            "ERROR" => true,
            "WARN" => !expected.iter().any(|warning| {
                line.target == warning.target && line.message.starts_with(&warning.message_prefix)
            }),
            _ => false,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use rstest::rstest;

    use super::*;
    use crate::logs::parse_log;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text).unwrap().to_utc()
    }

    fn secs(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    fn n(attempt: u32) -> NonZeroU32 {
        NonZeroU32::new(attempt).unwrap()
    }

    /// Appendix A's defaults, as agent.compose.toml leaves them.
    fn policy() -> RetryPolicy {
        RetryPolicy::try_new(secs(5), secs(300), secs(300)).unwrap()
    }

    fn circuit() -> CircuitPolicy {
        CircuitPolicy::try_new(NonZeroUsize::new(8).unwrap(), secs(600), secs(900)).unwrap()
    }

    #[rstest]
    #[case("5.5.1", Some((5, 5)))]
    #[case("v2.24.0", Some((2, 24)))]
    #[case("2.24.0-desktop.1", Some((2, 24)))]
    #[case("2.23.3\n", Some((2, 23)))]
    #[case("compose", None)]
    #[case("", None)]
    fn the_compose_version_is_read_as_major_and_minor(
        #[case] short: &str,
        #[case] expected: Option<(u32, u32)>,
    ) {
        assert_eq!(compose_version(short), expected);
    }

    #[test]
    fn a_newer_major_version_counts_as_new_enough_whatever_its_minor() {
        let version = compose_version("5.5.1").unwrap();

        assert!(version >= MIN_COMPOSE);
        assert!(compose_version("2.23.9").unwrap() < MIN_COMPOSE);
    }

    #[rstest]
    #[case(
        "There are 2 of a max of 60 players online: AfkBot4, AfkBot5",
        &["AfkBot4", "AfkBot5"]
    )]
    #[case("There are 0 of a max of 60 players online: ", &[])]
    #[case("There are 0 of a max of 60 players online:", &[])]
    fn rcons_list_names_the_players_online(#[case] list: &str, #[case] expected: &[&str]) {
        assert_eq!(online_players(list), expected);
    }

    #[test]
    fn the_slow_limit_is_the_period_times_terminate_after() {
        let toml = "[profile.slow]\nslow-timeout = { period = \"30s\", terminate-after = 4 }\n";

        assert_eq!(slow_limit(toml), secs(120));
    }

    #[test]
    fn the_repositorys_slow_limit_is_ten_minutes() {
        let toml = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../.config/nextest.toml"
        ));

        assert_eq!(slow_limit(toml), secs(600));
    }

    #[test]
    fn a_budget_that_fits_has_no_problem() {
        let reserved = [("the deadline", secs(200)), ("the teardown", secs(60))];

        assert_eq!(budget_problem(secs(340), &reserved, secs(600)), None);
    }

    #[test]
    fn a_budget_that_overflows_names_every_part() {
        let reserved = [("the deadline", secs(200)), ("the teardown", secs(60))];

        let problem = budget_problem(secs(341), &reserved, secs(600)).unwrap();

        assert!(problem.contains("the deadline"), "{problem}");
        assert!(problem.contains("the teardown"), "{problem}");
        assert!(problem.contains("341"), "{problem}");
    }

    #[test]
    fn a_gap_runs_from_a_backoff_to_the_next_session_request() {
        let states = [
            (at("2026-10-10T08:00:00Z"), State::Online),
            (at("2026-10-10T08:00:10Z"), State::Backoff { attempt: 1 }),
            (
                at("2026-10-10T08:00:17.5Z"),
                State::AwaitingSession { attempt: 2 },
            ),
            (
                at("2026-10-10T08:00:17.6Z"),
                State::Connecting { attempt: 2 },
            ),
            (at("2026-10-10T08:00:18Z"), State::Backoff { attempt: 2 }),
            (
                at("2026-10-10T08:00:31Z"),
                State::AwaitingSession { attempt: 3 },
            ),
            (at("2026-10-10T08:00:32Z"), State::Online),
            // A shutdown during a backoff ends it without a gap.
            (at("2026-10-10T08:01:00Z"), State::Backoff { attempt: 1 }),
            (at("2026-10-10T08:01:01Z"), State::Other),
        ];

        assert_eq!(
            gaps(&states),
            vec![
                Gap {
                    attempt: n(1),
                    from: at("2026-10-10T08:00:10Z"),
                    length: Duration::from_millis(7500),
                },
                Gap {
                    attempt: n(2),
                    from: at("2026-10-10T08:00:18Z"),
                    length: secs(13),
                },
            ]
        );
    }

    #[rstest]
    #[case(1, 5_000, true)]
    #[case(1, 10_000, true)]
    #[case(1, 4_950, true)]
    #[case(1, 10_900, true)]
    #[case(1, 4_800, false)]
    #[case(1, 11_200, false)]
    #[case(1, 0, false)]
    #[case(3, 39_000, true)]
    #[case(3, 19_000, false)]
    #[case(9, 299_000, true)]
    #[case(9, 149_000, false)]
    fn a_gap_must_lie_in_its_attempts_window_with_the_slack(
        #[case] attempt: u32,
        #[case] millis: u64,
        #[case] fits: bool,
    ) {
        let gap = Gap {
            attempt: n(attempt),
            from: at("2026-10-10T08:00:00Z"),
            length: Duration::from_millis(millis),
        };

        let problem = gap_problem(&policy(), &gap);

        assert_eq!(problem.is_none(), fits, "{problem:?}");
    }

    #[test]
    fn a_gap_problem_names_the_attempt_and_the_window() {
        let gap = Gap {
            attempt: n(2),
            from: at("2026-10-10T08:00:00Z"),
            length: secs(3),
        };

        let problem = gap_problem(&policy(), &gap).unwrap();

        assert!(problem.contains("attempt 2"), "{problem}");
        assert!(
            problem.contains("10s") && problem.contains("20s"),
            "{problem}"
        );
    }

    #[test]
    fn eight_failures_within_the_window_would_open_the_breaker() {
        let failures: Vec<_> = (0..8)
            .map(|minute| at(&format!("2026-10-10T08:0{minute}:00Z")))
            .collect();

        assert_eq!(
            breaker_open_until(circuit(), &failures, at("2026-10-10T08:08:00Z")),
            Some(at("2026-10-10T08:22:00Z"))
        );
        assert_eq!(
            breaker_open_until(circuit(), &failures[..7], at("2026-10-10T08:08:00Z")),
            None
        );
    }

    #[test]
    fn a_breaker_whose_cool_down_ended_is_no_longer_open() {
        let failures: Vec<_> = (0..8)
            .map(|minute| at(&format!("2026-10-10T08:0{minute}:00Z")))
            .collect();

        assert_eq!(
            breaker_open_until(circuit(), &failures, at("2026-10-10T08:30:00Z")),
            None
        );
    }

    #[test]
    fn the_first_join_may_take_one_failed_attempt_and_its_backoff() {
        // 30 s + 10 s + 30 s + 1 s.
        assert_eq!(first_join_deadline(&policy(), secs(30)), secs(71));
    }

    #[rstest]
    // 10 s + 30 s + 20 s + 30 s + 1 s.
    #[case(1, 91)]
    // 40 s + 30 s + 80 s + 30 s + 1 s.
    #[case(3, 181)]
    // Capped: 300 s + 30 s + 300 s + 30 s + 1 s.
    #[case(6, 661)]
    fn a_reconnect_may_take_the_backoff_one_more_failed_attempt_and_a_join(
        #[case] attempt: u32,
        #[case] secs_expected: u64,
    ) {
        assert_eq!(
            reconnect_deadline(&policy(), secs(30), n(attempt)),
            secs(secs_expected)
        );
    }

    fn probe(start: &str, end: &str, exit_code: i64) -> Probe {
        Probe {
            start: at(start),
            end: at(end),
            exit_code,
        }
    }

    #[test]
    fn the_server_is_healthy_from_the_end_of_its_first_good_probe_after_its_start() {
        let probes = [
            // Before the restart: good, but from the old run.
            probe("2026-10-10T08:00:00Z", "2026-10-10T08:00:00.2Z", 0),
            probe("2026-10-10T08:00:20Z", "2026-10-10T08:00:20.4Z", 1),
            probe("2026-10-10T08:00:25Z", "2026-10-10T08:00:25.3Z", 0),
            probe("2026-10-10T08:00:30Z", "2026-10-10T08:00:30.3Z", 0),
        ];

        assert_eq!(
            healthy_since(&probes, at("2026-10-10T08:00:15Z")),
            Some(at("2026-10-10T08:00:25.3Z"))
        );
    }

    #[test]
    fn a_server_without_a_good_probe_since_its_start_isnt_healthy_yet() {
        let probes = [
            probe("2026-10-10T08:00:00Z", "2026-10-10T08:00:00.2Z", 0),
            probe("2026-10-10T08:00:20Z", "2026-10-10T08:00:20.4Z", 1),
        ];

        assert_eq!(healthy_since(&probes, at("2026-10-10T08:00:15Z")), None);
    }

    fn line(level: &str, target: &str, message: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-10-10T08:00:00Z","level":"{level}","message":"{message}","target":"{target}"}}"#
        )
    }

    fn expected() -> Vec<ExpectedWarning> {
        vec![ExpectedWarning {
            target: "azalea_client::plugins::join".to_owned(),
            message_prefix: "failed to create connection".to_owned(),
            why: "the server is down".to_owned(),
        }]
    }

    #[test]
    fn errors_and_warnings_nobody_expects_are_unexpected() {
        let text = [
            line("INFO", "fleet_agent::run", "the agent is running"),
            line(
                "WARN",
                "azalea_client::plugins::join",
                "failed to create connection: Connection refused (os error 111)",
            ),
            line(
                "WARN",
                "azalea_client::plugins::join",
                "something else entirely",
            ),
            line(
                "WARN",
                "fleet_mc::connector",
                "failed to create connection: copied text, wrong target",
            ),
            line(
                "ERROR",
                "azalea_client::plugins::join",
                "failed to create connection: an ERROR is never expected",
            ),
        ]
        .join("\n");
        let log = parse_log(&text);

        let unexpected: Vec<&str> = unexpected(&log, &expected())
            .into_iter()
            .map(|line| line.message.as_str())
            .collect();

        assert_eq!(
            unexpected,
            [
                "something else entirely",
                "failed to create connection: copied text, wrong target",
                "failed to create connection: an ERROR is never expected",
            ]
        );
    }
}
