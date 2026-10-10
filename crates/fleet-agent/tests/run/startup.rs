//! The run's start: the bots' IDs, the startup lines, and every startup
//! error.

use std::collections::BTreeSet;

use fleet_agent::config::{AgentMode, ControlPlaneConfig};
use fleet_agent::run::{Exit, Outcome};
use fleet_core::id::BotId;
use fleet_core::mode::ModePreset;
use fleet_runtime::ShutdownReport;
use rstest::rstest;
use serde_json::Value;

use crate::harness::{AGENT, Agent, afk, config, unread_file};

const STARTING: &str = "starting a standalone bot";
const RUNNING: &str = "the agent is running";
const CANT_START: &str = "the agent can't start";
const REFUSED: &str = "the fleet refused a standalone bot; shutting down";

#[tokio::test(start_paused = true)]
async fn each_standalone_bot_gets_a_fresh_v7_id_logged_with_its_username_server_and_mode() {
    let agent = Agent::start(config(&[
        ("AfkBot1", ModePreset::Afk),
        ("AfkBot2", ModePreset::Farm),
    ]))
    .await;

    let lines = agent.lines(STARTING);
    assert_eq!(lines.len(), 2, "{}", agent.capture.text());
    let logged: Vec<(String, String, String, String)> = lines
        .iter()
        .map(|line| {
            assert_eq!(line["level"], "INFO");
            (
                line["bot_id"].as_str().unwrap().to_owned(),
                line["username"].as_str().unwrap().to_owned(),
                line["server"].as_str().unwrap().to_owned(),
                line["mode"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(logged[0].1, "AfkBot1");
    assert_eq!(logged[0].3, "afk");
    assert_eq!(logged[1].1, "AfkBot2");
    assert_eq!(logged[1].3, "farm");
    assert!(logged.iter().all(|(_, _, server, _)| server == "localhost"));
    // Only v7 IDs parse.
    let ids: BTreeSet<BotId> = logged.iter().map(|(id, ..)| id.parse().unwrap()).collect();
    assert_eq!(ids.len(), 2);
    let connected: BTreeSet<BotId> = agent.fake.connects().iter().map(|c| c.bot_id).collect();
    assert_eq!(connected, ids);
}

#[tokio::test(start_paused = true)]
async fn two_starts_give_the_same_bot_different_ids() {
    let first = Agent::start(afk(&["AfkBot1"])).await;
    let second = Agent::start(afk(&["AfkBot1"])).await;

    let first_id = first.lines(STARTING)[0]["bot_id"].clone();
    let second_id = second.lines(STARTING)[0]["bot_id"].clone();

    assert_ne!(first_id, second_id);
}

#[tokio::test(start_paused = true)]
async fn the_agent_logs_that_it_is_running_with_the_bot_count_after_every_bot_is_applied() {
    let agent = Agent::start(afk(&["AfkBot1", "AfkBot2"])).await;

    let messages: Vec<String> = agent
        .capture
        .json_lines()
        .unwrap()
        .iter()
        .filter_map(|line| line["message"].as_str().map(str::to_owned))
        .filter(|message| message == STARTING || message == RUNNING)
        .collect();
    assert_eq!(messages, [STARTING, STARTING, RUNNING]);
    let running = &agent.lines(RUNNING)[0];
    assert_eq!(running["level"], "INFO");
    assert_eq!(running["bots"], 2);
    assert!(!agent.has_ended());
}

#[tokio::test(start_paused = true)]
async fn the_agents_and_its_bots_lines_are_in_the_agent_span() {
    let agent = Agent::start(afk(&["AfkBot1"])).await;
    agent.join(1).await;

    let running = &agent.lines(RUNNING)[0];
    assert_eq!(running["span"]["name"], "agent");
    assert_eq!(running["span"]["agent"], AGENT);
    let state_changes = agent.lines("the bot's state changed");
    assert!(!state_changes.is_empty(), "{}", agent.capture.text());
    for line in state_changes {
        let spans = line["spans"].as_array().unwrap();
        assert_eq!(spans[0]["name"], "agent", "{line}");
        assert_eq!(spans[0]["agent"], AGENT, "{line}");
        assert_eq!(line["span"]["name"], "bot", "{line}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_control_plane_config_is_refused_until_phase_10() {
    let mut managed = afk(&["AfkBot1"]);
    managed.mode = AgentMode::ControlPlane(ControlPlaneConfig {
        url: "https://fleet.example.com:7443".to_owned(),
        ca_cert_file: unread_file("ca.crt"),
        cert_file: unread_file("agent.crt"),
        key_file: unread_file("agent.key"),
    });
    let agent = Agent::start(managed).await;
    let capture = agent.capture.clone();
    let fake = agent.fake.clone();

    let outcome = agent.outcome().await;

    assert_eq!(outcome, Outcome::startup_failed());
    assert_eq!(fake.connects().len(), 0);
    let errors = crate::harness::lines_with(&capture, CANT_START);
    assert_eq!(errors.len(), 1, "{}", capture.text());
    assert_eq!(errors[0]["level"], "ERROR");
    assert!(
        errors[0]["error"].as_str().unwrap().contains("Phase 10"),
        "{}",
        errors[0]
    );
}

#[tokio::test(start_paused = true)]
async fn a_fleet_that_cant_be_set_up_fails_the_start() {
    let mut invalid = afk(&["AfkBot1"]);
    invalid.runtime.chat_interval = core::time::Duration::ZERO;
    let agent = Agent::start(invalid).await;
    let capture = agent.capture.clone();
    let fake = agent.fake.clone();

    let outcome = agent.outcome().await;

    assert_eq!(outcome, Outcome::startup_failed());
    assert_eq!(fake.connects().len(), 0);
    let errors = crate::harness::lines_with(&capture, CANT_START);
    assert_eq!(errors.len(), 1, "{}", capture.text());
    assert_eq!(
        errors[0]["error"],
        "the fleet can't be set up: the chat rate limit is invalid: the chat interval is zero"
    );
}

/// Two bots, the second of which the fleet refuses: the same account, or
/// one bot more than `max_bots`. The config's own check is bypassed.
#[rstest]
#[case::account_in_use("afkbot1", None, "another bot already uses this account")]
#[case::at_capacity("AfkBot2", Some(1), "the fleet holds as many bots as it may")]
#[tokio::test(start_paused = true)]
async fn a_bot_the_fleet_refuses_at_startup_shuts_the_fleet_down_and_fails_the_start(
    #[case] second: &str,
    #[case] max_bots: Option<usize>,
    #[case] error: &str,
) {
    let mut refused = afk(&["AfkBot1", second]);
    if let Some(max) = max_bots {
        refused.runtime.max_bots = core::num::NonZeroUsize::new(max).unwrap();
    }
    let agent = Agent::start(refused).await;
    let capture = agent.capture.clone();

    let outcome = agent.outcome().await;

    assert_eq!(
        outcome,
        Outcome {
            exit: Exit::StartupFailed,
            report: Some(ShutdownReport {
                stopped: 1,
                aborted: 0,
                crashed: 0
            })
        }
    );
    let lines = crate::harness::lines_with(&capture, REFUSED);
    assert_eq!(lines.len(), 1, "{}", capture.text());
    assert_eq!(lines[0]["level"], "ERROR");
    assert_eq!(lines[0]["username"], second);
    assert_eq!(lines[0]["error"], error);
    assert!(lines[0]["bot_id"].is_string());
    assert_eq!(
        crate::harness::lines_with(&capture, RUNNING),
        Vec::<Value>::new()
    );
}
