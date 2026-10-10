//! The compose e2e test (Plan.md P5.7; ADR-0014): the agent's image and a
//! real Minecraft server, as `just stack-up` runs them, under the project
//! `afkfleet-e2e` with `deploy/compose.isolated.yaml` on top, so it never
//! touches the dev stack.
//!
//! `slow_compose_scenario` runs three steps against one stack:
//! 1. **Up:** every bot of `deploy/dev/agent.compose.toml` comes Online.
//! 2. **The server restarts:** the agent itself isn't restarted, every wait
//!    between two attempts lies in the retry policy's window for its attempt,
//!    and every bot is Online again by the deadline that policy gives.
//! 3. **The agent stops:** exit code 0, the shutdown report as the last line,
//!    no warning during the shutdown, and the server lets every bot go at
//!    once.
//!
//! Then the whole log must hold no ERROR and only the warnings
//! `deploy/dev/stack-checks.json` expects. Every check also asserts that it
//! saw data, so a parser that reads nothing can't pass it.
//!
//! It needs Docker and the agent's image built from this source: run it
//! with `just test-slow`, which builds the image first and says so through
//! `AFKFLEET_E2E_IMAGE_BUILT`. Pass and fail are judged by the Docker VM's
//! clock only (the agent's log, the containers' start times and health
//! probes); the host's clock only bounds the waits and the time budget, and
//! the waits never sleep: they wake on the agent's next log line, or after
//! [`POLL`]. The helpers' unit tests are fast and run with every `just test`.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions would count as library code.
#![cfg(test)]

mod checks;
mod docker;
#[path = "../common/jail.rs"]
mod jail;
mod logs;
#[path = "../common/stack_checks.rs"]
mod stack_checks;

use core::time::Duration;
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::path::Path;
use std::time::Instant;

use chrono::{DateTime, TimeDelta, Utc};
use fleet_agent::config::{AgentConfig, AgentMode, load};
use fleet_core::resilience::{CircuitPolicy, RetryPolicy};

use crate::checks::{
    MIN_COMPOSE, breaker_open_until, budget_problem, compose_version, first_join_deadline,
    gap_problem, gaps, healthy_since, online_players, reconnect_deadline, slow_limit, unexpected,
};
use crate::docker::{IMAGE, LogFollower, QUICK, Stack, TEARDOWN, compose_ok, docker, logs, rcon};
use crate::logs::{LogLine, State, parse_log, states, usernames, with_message};
use crate::stack_checks::{expected_warnings, millis};

const NEXTEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.config/nextest.toml"
));
/// Set by `just test-slow` once it has built the agent's image.
const BUILT: &str = "AFKFLEET_E2E_IMAGE_BUILT";
/// The longest wait for the agent's next log line before the test looks again.
const POLL: Duration = Duration::from_millis(250);
/// `up --wait`'s limit: the server's first start downloads its jar.
const UP: Duration = Duration::from_secs(300);
/// The server's restart: compose.dev.yaml's 60 s stop grace, then the start.
const RESTART: Duration = Duration::from_secs(120);
/// How soon the server must drop the bots once the agent has stopped. A
/// connection it only noticed by its own timeout would take 30 s.
const RCON_EMPTY: Duration = Duration::from_secs(5);

/// agent.compose.toml, without any environment variable.
fn compose_config() -> AgentConfig {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/dev/agent.compose.toml");
    let mut loaded = None;
    jail::in_jail(|jail| {
        jail.clear_env();
        loaded = Some(load(&path));
    });
    loaded
        .unwrap()
        .unwrap_or_else(|error| panic!("agent.compose.toml should load: {error}"))
}

/// Step 0: the image is this source's, Compose knows `!reset`, and the
/// config loads.
fn preconditions() -> AgentConfig {
    assert_eq!(
        std::env::var(BUILT).as_deref(),
        Ok("1"),
        "run `just test-slow`, which builds the image first"
    );
    assert!(
        docker(&["image", "inspect", IMAGE], QUICK).success,
        "there's no {IMAGE}: run `just test-slow`, which builds the image first"
    );
    let version = docker(&["compose", "version", "--short"], QUICK).stdout;
    assert!(
        compose_version(&version).is_some_and(|version| version >= MIN_COMPOSE),
        "compose.isolated.yaml's `!reset` needs Docker Compose {}.{} or newer, not {version:?}",
        MIN_COMPOSE.0,
        MIN_COMPOSE.1
    );
    compose_config()
}

/// The agent's log as it is now.
fn agent_log() -> Vec<LogLine> {
    parse_log(&logs("agent"))
}

/// The bots of `bots` that `log` shows Online at some point.
fn online_in(
    log: &[LogLine],
    names: &BTreeMap<String, String>,
    bots: &BTreeSet<String>,
) -> BTreeSet<String> {
    states(log, names)
        .into_iter()
        .filter(|(bot, states)| {
            bots.contains(bot) && states.iter().any(|(_, state)| *state == State::Online)
        })
        .map(|(bot, _)| bot)
        .collect()
}

/// When `bot` first comes Online in `log`.
fn first_online(log: &[LogLine], names: &BTreeMap<String, String>, bot: &str) -> DateTime<Utc> {
    states(log, names)
        .remove(bot)
        .and_then(|states| {
            states
                .into_iter()
                .find(|(_, state)| *state == State::Online)
                .map(|(at, _)| at)
        })
        .unwrap_or_else(|| panic!("{bot} should come Online"))
}

fn later(at: DateTime<Utc>, by: Duration) -> DateTime<Utc> {
    at + TimeDelta::from_std(by).unwrap()
}

fn lines(lines: &[&LogLine]) -> String {
    lines
        .iter()
        .map(|line| line.raw.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// What every step reads.
struct Scenario {
    started: Instant,
    /// nextest's limit for a slow test.
    limit: Duration,
    follower: LogFollower,
    bots: BTreeSet<String>,
    policy: RetryPolicy,
    circuit: CircuitPolicy,
    connect: Duration,
    /// The agent's `stop_grace_period`.
    grace: Duration,
}

impl Scenario {
    fn new(started: Instant, config: &AgentConfig) -> Self {
        let AgentMode::Standalone(bots) = &config.mode else {
            panic!("agent.compose.toml should be standalone");
        };
        Self {
            started,
            limit: slow_limit(NEXTEST),
            follower: LogFollower::start("agent"),
            bots: bots.iter().map(|bot| bot.username.to_string()).collect(),
            policy: config.retry,
            circuit: config.circuit,
            connect: config.runtime.connect_timeout,
            grace: millis("stop_grace_period_ms"),
        }
    }

    /// Fails unless what's left of nextest's limit holds `deadline`, the
    /// agent's stop and the teardown, so nextest can't kill the test
    /// mid-cleanup and leave its containers behind.
    fn assert_budget(&self, deadline: (&str, Duration)) {
        let reserved = [
            deadline,
            ("the agent's stop", self.grace),
            ("the teardown", TEARDOWN),
        ];
        if let Some(problem) = budget_problem(self.started.elapsed(), &reserved, self.limit) {
            panic!("{problem}");
        }
    }

    /// What's left of nextest's limit once the stop and the teardown are
    /// reserved.
    fn remaining(&self) -> Duration {
        self.limit
            .saturating_sub(self.started.elapsed() + self.grace + TEARDOWN)
    }

    /// Calls `check` until it succeeds, waking on the agent's log; fails
    /// with its last answer once `within` has passed.
    fn wait_for<T>(
        &self,
        what: &str,
        within: Duration,
        mut check: impl FnMut() -> Result<T, String>,
    ) -> T {
        let end = Instant::now() + within;
        loop {
            match check() {
                Ok(value) => return value,
                Err(last) => assert!(
                    Instant::now() < end,
                    "{what} within {within:?}; last seen: {last}"
                ),
            }
            self.follower
                .wait(POLL.min(end.saturating_duration_since(Instant::now())));
        }
    }

    /// Waits until every bot is Online in the log from line `from` on, and in
    /// RCON's list; returns the log.
    fn wait_until_online(&self, what: &str, within: Duration, from: usize) -> Vec<LogLine> {
        self.wait_for(what, within, || {
            let log = agent_log();
            let online = online_in(&log[from..], &usernames(&log), &self.bots);
            let players = online_players(&rcon("list"));
            let listed = self.bots.iter().all(|bot| players.contains(bot));
            if online == self.bots && listed {
                Ok(log)
            } else {
                Err(format!(
                    "Online in the log: {online:?}; on the server: {players:?}"
                ))
            }
        })
    }

    /// Step 1: every bot comes Online within the first join's deadline from
    /// "the agent is running". Returns each bot's username by its ID.
    fn every_bot_comes_online(&self) -> BTreeMap<String, String> {
        let deadline = first_join_deadline(&self.policy, self.connect);
        self.assert_budget(("the first join's deadline", deadline));
        let log = self.wait_until_online("every bot Online", deadline, 0);

        let names = usernames(&log);
        assert_eq!(
            names.values().cloned().collect::<BTreeSet<_>>(),
            self.bots,
            "the log should start every configured bot"
        );
        let running = with_message(&log, "the agent is running");
        assert_eq!(running.len(), 1, "{}", lines(&running));
        let running_at = running[0].timestamp;
        for bot in &self.bots {
            let online = first_online(&log, &names, bot);
            assert!(
                online <= later(running_at, deadline),
                "{bot} came Online at {online}, after the deadline {deadline:?} from {running_at}"
            );
        }
        names
    }

    /// Step 2: the server restarts; the agent isn't restarted, and the bots
    /// reconnect within the retry policy.
    fn the_bots_reconnect_after_a_server_restart(&self, names: &BTreeMap<String, String>) {
        let agent_before = docker::state("agent");
        let server_before = docker::state("minecraft");
        let before_restart = agent_log().len();

        compose_ok(&["restart", "--no-deps", "minecraft"], RESTART);
        // Docker keeps only the last five probes (25 s at the 5 s interval),
        // so the healthy moment is read as soon as it's there.
        let healthy = self.wait_for("the server healthy again", self.remaining(), || {
            let server = docker::state("minecraft");
            if server.started_at <= server_before.started_at {
                return Err(format!("still the start at {}", server.started_at));
            }
            healthy_since(&server.probes, server.started_at).ok_or_else(|| {
                format!(
                    "no good probe since the start at {}: {:?}",
                    server.started_at, server.probes
                )
            })
        });
        let deadlines = self.reconnect_deadlines(&agent_log()[before_restart..], names, healthy);
        let slowest = deadlines.values().copied().max().unwrap();
        self.assert_budget(("the slowest bot's reconnect deadline", slowest));
        let log = self.wait_until_online("every bot Online again", slowest, before_restart);

        for (bot, deadline) in &deadlines {
            let online = first_online(&log[before_restart..], names, bot);
            assert!(
                online <= later(healthy, *deadline),
                "{bot} came Online again at {online}, after the deadline {deadline:?} from the \
                 server's healthy moment at {healthy}"
            );
        }
        self.assert_waits_within_the_policy(&log, before_restart, names);
        let agent = docker::state("agent");
        assert_eq!(
            (agent.started_at, agent.restart_count),
            (agent_before.started_at, agent_before.restart_count),
            "the agent itself shouldn't restart"
        );
        let running = with_message(&log, "the agent is running");
        assert_eq!(running.len(), 1, "{}", lines(&running));
    }

    /// Each bot's reconnect deadline from `healthy`, by the attempt of its
    /// last backoff before then.
    fn reconnect_deadlines(
        &self,
        after_restart: &[LogLine],
        names: &BTreeMap<String, String>,
        healthy: DateTime<Utc>,
    ) -> BTreeMap<String, Duration> {
        let after = states(after_restart, names);
        self.bots
            .iter()
            .map(|bot| {
                let states = after
                    .get(bot)
                    .unwrap_or_else(|| panic!("{bot} should change its state after the restart"));
                let attempt = states
                    .iter()
                    .rev()
                    .filter(|(at, _)| *at < healthy)
                    .find_map(|(_, state)| match state {
                        State::Backoff { attempt } => NonZeroU32::new(*attempt),
                        _ => None,
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "{bot} should back off before the server is healthy at {healthy}: \
                             {states:?}"
                        )
                    });
                let deadline = reconnect_deadline(&self.policy, self.connect, attempt);
                (bot.clone(), deadline)
            })
            .collect()
    }

    /// Every wait between two attempts in `log` lies in the policy's window,
    /// and every bot waited at least once after the restart.
    fn assert_waits_within_the_policy(
        &self,
        log: &[LogLine],
        before_restart: usize,
        names: &BTreeMap<String, String>,
    ) {
        let all = states(log, names);
        let after = states(&log[before_restart..], names);
        let mut problems = Vec::new();
        for bot in &self.bots {
            assert!(
                !gaps(&after[bot]).is_empty(),
                "{bot} should wait between attempts after the restart: {:?}",
                after[bot]
            );
            let states = &all[bot];
            let failures: Vec<DateTime<Utc>> = states
                .iter()
                .filter(|(_, state)| matches!(state, State::Backoff { .. }))
                .map(|(at, _)| *at)
                .collect();
            for gap in gaps(states) {
                if let Some(problem) = gap_problem(&self.policy, &gap) {
                    let earlier: Vec<_> = failures
                        .iter()
                        .copied()
                        .filter(|at| *at <= gap.from)
                        .collect();
                    let breaker = breaker_open_until(self.circuit, &earlier, gap.from)
                        .map(|until| format!("; the breaker may have been open until {until}"))
                        .unwrap_or_default();
                    problems.push(format!("{bot}: {problem}{breaker}"));
                }
            }
        }
        assert!(
            problems.is_empty(),
            "waits outside the retry policy:\n{}",
            problems.join("\n")
        );
    }

    /// Step 3: the agent stops cleanly. Returns its whole log.
    fn the_agent_stops_cleanly(&self) -> Vec<LogLine> {
        compose_ok(&["stop", "agent"], self.grace + QUICK);
        let agent = docker::state("agent");
        assert!(!agent.running, "the agent should have stopped");
        assert_eq!(agent.exit_code, 0, "the agent's exit code");

        let log = agent_log();
        let last = log.last().expect("the agent should have logged");
        assert_eq!(last.message, "the agent stopped", "{}", last.raw);
        let report = [
            ("exit_code", 0),
            ("stopped", u64::try_from(self.bots.len()).unwrap()),
            ("aborted", 0),
            ("crashed", 0),
        ];
        for (key, value) in report {
            assert_eq!(last.number(key), Some(value), "{key}: {}", last.raw);
        }
        let shutting_down = log
            .iter()
            .position(|line| line.message == "shutting down")
            .expect("the agent should log \"shutting down\"");
        let noisy: Vec<&LogLine> = log[shutting_down..]
            .iter()
            .filter(|line| line.level == "WARN" || line.level == "ERROR")
            .collect();
        assert!(
            noisy.is_empty(),
            "warnings or errors during the shutdown:\n{}",
            lines(&noisy)
        );
        self.wait_for("the server to drop every bot", RCON_EMPTY, || {
            let players = online_players(&rcon("list"));
            if players.iter().any(|player| self.bots.contains(player)) {
                Err(format!("still on the server: {players:?}"))
            } else {
                Ok(())
            }
        });
        log
    }
}

/// The whole log holds no ERROR, and only the warnings stack-checks.json
/// expects.
fn assert_only_expected_lines(log: &[LogLine]) {
    let expected = expected_warnings();
    let unexpected = unexpected(log, &expected);
    let known: Vec<String> = expected
        .iter()
        .map(|warning| {
            format!(
                "{} \"{}…\" ({})",
                warning.target, warning.message_prefix, warning.why
            )
        })
        .collect();
    assert!(
        unexpected.is_empty(),
        "lines nobody expects:\n{}\n\nthe expected warnings:\n{}",
        lines(&unexpected),
        known.join("\n")
    );
}

#[test]
fn slow_compose_scenario() {
    let started = Instant::now();
    let config = preconditions();
    let stack = Stack::clean();
    compose_ok(
        &[
            "up",
            "--detach",
            "--wait",
            "--wait-timeout",
            "300",
            "--no-build",
        ],
        UP + QUICK,
    );
    let scenario = Scenario::new(started, &config);

    let names = scenario.every_bot_comes_online();
    scenario.the_bots_reconnect_after_a_server_restart(&names);
    let log = scenario.the_agent_stops_cleanly();
    assert_only_expected_lines(&log);

    drop(scenario);
    drop(stack);
}
