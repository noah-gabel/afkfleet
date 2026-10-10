//! The agent's JSON log lines (ADR-0014), parsed once, so the checks read
//! typed values: each line's time, level, target and message, the bot it
//! belongs to, and the bot states the actor logs.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::Value;

/// The actor's line for every state change, with the state in `state`.
pub(crate) const STATE_CHANGED: &str = "the bot's state changed";

/// One line of the agent's JSON log.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LogLine {
    /// The line as the agent wrote it, for failure messages.
    pub(crate) raw: String,
    /// When the agent wrote it, by the Docker VM's clock.
    pub(crate) timestamp: DateTime<Utc>,
    pub(crate) level: String,
    pub(crate) target: String,
    pub(crate) message: String,
    /// The `bot_id` of the line's current span, if it's a bot's.
    pub(crate) span_bot_id: Option<String>,
    /// The whole line, for its other fields.
    pub(crate) fields: Value,
}

impl LogLine {
    /// The number in the field `key`, if the line has one.
    pub(crate) fn number(&self, key: &str) -> Option<u64> {
        self.fields.get(key).and_then(Value::as_u64)
    }

    /// The text in the field `key`, if the line has one.
    pub(crate) fn text(&self, key: &str) -> Option<&str> {
        self.fields.get(key).and_then(Value::as_str)
    }

    /// The bot state of a state-change line.
    pub(crate) fn state(&self) -> Option<State> {
        if self.message != STATE_CHANGED {
            return None;
        }
        self.text("state").map(parse_state)
    }
}

/// The part of a bot's state the checks read, from its `Debug` text
/// (`Backoff { attempt: 2 }`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    AwaitingSession {
        attempt: u32,
    },
    Connecting {
        attempt: u32,
    },
    Online,
    Backoff {
        attempt: u32,
    },
    /// Any other state: stopping, stopped, paused or failed.
    Other,
}

/// Parses a state's `Debug` text.
pub(crate) fn parse_state(debug: &str) -> State {
    let name = debug.split([' ', '{']).next().unwrap_or_default();
    let attempt = || {
        let (_, rest) = debug.split_once("attempt: ")?;
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    match (name, attempt()) {
        ("AwaitingSession", Some(attempt)) => State::AwaitingSession { attempt },
        ("Connecting", Some(attempt)) => State::Connecting { attempt },
        ("Online", _) => State::Online,
        ("Backoff", Some(attempt)) => State::Backoff { attempt },
        _ => State::Other,
    }
}

fn parse_line(raw: &str) -> Option<LogLine> {
    let fields: Value = serde_json::from_str(raw).ok()?;
    let text = |key: &str| fields.get(key).and_then(Value::as_str).map(str::to_owned);
    let timestamp = DateTime::parse_from_rfc3339(&text("timestamp")?)
        .ok()?
        .to_utc();
    Some(LogLine {
        raw: raw.to_owned(),
        timestamp,
        level: text("level")?,
        target: text("target")?,
        message: text("message")?,
        span_bot_id: fields
            .get("span")
            .and_then(|span| span.get("bot_id"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        fields,
    })
}

/// Parses the agent's whole log, as `docker compose logs --no-log-prefix`
/// prints it. Every non-empty line must be one of the agent's JSON lines.
pub(crate) fn parse_log(text: &str) -> Vec<LogLine> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            parse_line(line)
                .unwrap_or_else(|| panic!("{line:?} should be one of the agent's JSON lines"))
        })
        .collect()
}

/// Each bot's username by its ID, from "starting a standalone bot".
pub(crate) fn usernames(log: &[LogLine]) -> BTreeMap<String, String> {
    with_message(log, "starting a standalone bot")
        .into_iter()
        .filter_map(|line| {
            Some((
                line.text("bot_id")?.to_owned(),
                line.text("username")?.to_owned(),
            ))
        })
        .collect()
}

/// Each bot's states in `log`, by username (from `usernames`), with when
/// they began. A bot `usernames` doesn't know is left out.
pub(crate) fn states(
    log: &[LogLine],
    usernames: &BTreeMap<String, String>,
) -> BTreeMap<String, Vec<(DateTime<Utc>, State)>> {
    let mut states: BTreeMap<String, Vec<_>> = BTreeMap::new();
    for line in log {
        let (Some(state), Some(bot_id)) = (line.state(), &line.span_bot_id) else {
            continue;
        };
        if let Some(username) = usernames.get(bot_id) {
            states
                .entry(username.clone())
                .or_default()
                .push((line.timestamp, state));
        }
    }
    states
}

/// The lines of `log` with the message `message`.
pub(crate) fn with_message<'a>(log: &'a [LogLine], message: &str) -> Vec<&'a LogLine> {
    log.iter().filter(|line| line.message == message).collect()
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    const STARTING: &str = r#"{"timestamp":"2026-10-10T08:00:00.000001Z","level":"INFO","message":"starting a standalone bot","bot_id":"b-4","username":"AfkBot4","server":"minecraft:25565","mode":"afk","target":"fleet_agent::run","span":{"agent":"compose-agent","name":"agent"},"spans":[{"agent":"compose-agent","name":"agent"}]}"#;

    fn state_line(at: &str, bot_id: &str, state: &str) -> String {
        format!(
            r#"{{"timestamp":"{at}","level":"INFO","message":"the bot's state changed","state":"{state}","target":"fleet_runtime::actor::effects","span":{{"bot_id":"{bot_id}","name":"bot"}},"spans":[{{"agent":"compose-agent","name":"agent"}},{{"bot_id":"{bot_id}","name":"bot"}}]}}"#
        )
    }

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text).unwrap().to_utc()
    }

    #[test]
    fn a_line_parses_into_its_time_level_target_message_and_fields() {
        let log = parse_log(STARTING);

        assert_eq!(log.len(), 1);
        let line = &log[0];
        assert_eq!(line.raw, STARTING);
        assert_eq!(line.timestamp, at("2026-10-10T08:00:00.000001Z"));
        assert_eq!(line.level, "INFO");
        assert_eq!(line.target, "fleet_agent::run");
        assert_eq!(line.message, "starting a standalone bot");
        assert_eq!(line.span_bot_id, None);
        assert_eq!(line.text("username"), Some("AfkBot4"));
    }

    #[test]
    fn a_state_line_carries_its_bots_id_from_the_span() {
        let log = parse_log(&state_line(
            "2026-10-10T08:00:01Z",
            "b-4",
            "Backoff { attempt: 2 }",
        ));

        assert_eq!(log[0].span_bot_id.as_deref(), Some("b-4"));
        assert_eq!(log[0].state(), Some(State::Backoff { attempt: 2 }));
    }

    #[test]
    fn empty_lines_are_skipped() {
        let log = parse_log(&format!("\n{STARTING}\n\n"));

        assert_eq!(log.len(), 1);
    }

    #[test]
    #[should_panic(expected = "should be one of the agent's JSON lines")]
    fn a_line_that_isnt_json_fails() {
        let _ = parse_log(&format!("{STARTING}\nWARN[0000] compose said something\n"));
    }

    #[rstest]
    #[case("AwaitingSession { attempt: 2, fresh: false }", State::AwaitingSession { attempt: 2 })]
    #[case("Connecting { attempt: 3, auth_retried: false }", State::Connecting { attempt: 3 })]
    #[case(
        "Online { since: 2026-10-10T08:00:05.120001234Z, attempt: 2 }",
        State::Online
    )]
    #[case("Backoff { attempt: 12 }", State::Backoff { attempt: 12 })]
    #[case("Stopping { restart: false }", State::Other)]
    #[case("Stopped", State::Other)]
    #[case("Paused { reason: Conflict { kind: DuplicateLogin } }", State::Other)]
    fn a_states_debug_text_parses(#[case] debug: &str, #[case] expected: State) {
        assert_eq!(parse_state(debug), expected);
    }

    #[test]
    fn only_a_state_change_has_a_state() {
        let log = parse_log(STARTING);

        assert_eq!(log[0].state(), None);
    }

    #[test]
    fn the_usernames_come_from_the_starting_lines() {
        let log = parse_log(STARTING);

        assert_eq!(
            usernames(&log),
            BTreeMap::from([("b-4".to_owned(), "AfkBot4".to_owned())])
        );
    }

    #[test]
    fn the_states_are_grouped_by_username_in_order() {
        let text = [
            STARTING.to_owned(),
            state_line(
                "2026-10-10T08:00:01Z",
                "b-4",
                "Connecting { attempt: 1, auth_retried: false }",
            ),
            state_line(
                "2026-10-10T08:00:02Z",
                "b-4",
                "Online { since: 2026-10-10T08:00:02Z, attempt: 1 }",
            ),
            // A bot nobody started is left out.
            state_line("2026-10-10T08:00:03Z", "b-9", "Stopped"),
        ]
        .join("\n");

        let log = parse_log(&text);

        let states = states(&log, &usernames(&log));

        assert_eq!(
            states,
            BTreeMap::from([(
                "AfkBot4".to_owned(),
                vec![
                    (at("2026-10-10T08:00:01Z"), State::Connecting { attempt: 1 }),
                    (at("2026-10-10T08:00:02Z"), State::Online),
                ]
            )])
        );
    }
}
