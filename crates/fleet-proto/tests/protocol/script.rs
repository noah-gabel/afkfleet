//! A scripted mode, the hello's script API level, a spec's chat formats, and
//! a script's status and log on the wire (Plan.md P15.20, P15.34; ADR-0036,
//! D13; ADR-0037, C2).

use fleet_core::bot::BotSpec;
use fleet_core::chat::{ChatFormats, ChatFormatsError};
use fleet_core::mode::ModeDefinition;
use fleet_core::script::menu::SignDraft;
use fleet_core::script::{
    LeashRadiusError, LogKind, MessageDraft, ProblemKind, ReportText, Script, ScriptApiLevelError,
    ScriptDraft, ScriptError, ScriptLogLine, ScriptPlace, ScriptProblem, ScriptState, ValueDraft,
    ValueKind,
};
use fleet_proto::control::{AgentMessage, BotEventKind};
use fleet_proto::generated as wire;
use fleet_proto::{ConvertError, Problem};
use prost::Message as _;

const BOT: &str = "01890a5d-ac96-774b-bcce-b302099a8057";
const AT: i64 = 1_790_000_000_000;

/// A scripted mode with something of every kind: a block guard, a message
/// with a value.
fn draft() -> ScriptDraft {
    ScriptDraft {
        api: 1,
        source: "fn on_join(first) {\n    chat.send(\"pay\", #{ amount: 5 });\n}\n".to_owned(),
        leash: 12,
        tool_guard: 3,
        block_guard: vec!["minecraft:cobblestone".to_owned()],
        messages: vec![MessageDraft {
            key: "pay".to_owned(),
            text: "/pay AfkMain {amount}".to_owned(),
            values: vec![ValueDraft {
                name: "amount".to_owned(),
                kind: ValueKind::Integer { min: 1, max: 1000 },
            }],
        }],
        patterns: Vec::new(),
        signs: Vec::new(),
    }
}

fn scripted() -> ModeDefinition {
    ModeDefinition::scripted(Script::try_from(draft()).unwrap())
}

fn wire_scripted() -> wire::ModeDefinition {
    scripted().into()
}

fn read(mode: wire::ModeDefinition) -> Result<ModeDefinition, ConvertError> {
    ModeDefinition::try_from(mode)
}

fn error(field: &'static str, problem: Problem) -> ConvertError {
    ConvertError::new(field, problem)
}

#[test]
fn a_scripted_mode_crosses_as_itself_with_its_script_and_no_steps() {
    let written = wire_scripted();

    assert_eq!(written.steps, []);
    let script = written.script.as_ref().unwrap();
    assert_eq!(script.block_guard, ["minecraft:cobblestone"]);
    assert_eq!((script.api, script.leash, script.tool_guard), (1, 12, 3));
    assert_eq!(read(written), Ok(scripted()));
}

#[test]
fn a_mode_with_both_steps_and_a_script_is_refused() {
    let mut both = wire_scripted();
    both.steps = wire::ModeDefinition::from(ModeDefinition::farm()).steps;

    assert_eq!(
        read(both),
        Err(error("ModeDefinition.steps", Problem::TwoBodies))
    );
}

#[test]
fn a_script_fleet_core_refuses_is_refused_with_its_reason() {
    let with = |change: fn(&mut wire::Script)| {
        let mut mode = wire_scripted();
        change(mode.script.as_mut().unwrap());
        read(mode)
    };

    assert_eq!(
        with(|script| script.leash = 0),
        Err(error(
            "ModeDefinition.script",
            Problem::Script(ScriptError::Leash(LeashRadiusError { blocks: 0 }))
        ))
    );
    assert_eq!(
        with(|script| script.api = 3),
        Err(error(
            "ModeDefinition.script",
            Problem::Script(ScriptError::Api(ScriptApiLevelError { level: 3 }))
        ))
    );
    assert_eq!(
        with(|script| script.leash = 300),
        Err(error("Script.leash", Problem::OutOfRange))
    );
    assert_eq!(
        with(|script| script.messages[0].values[0].kind = None),
        Err(error("DeclaredValue.kind", Problem::Missing).at(0))
    );
}

/// A level-2 script with a sign whose line starts with its value.
fn signed() -> ModeDefinition {
    ModeDefinition::scripted(
        Script::try_from(ScriptDraft {
            api: 2,
            signs: vec![SignDraft {
                key: "price".to_owned(),
                lines: vec!["{price}".to_owned(), String::new(), "afk".to_owned()],
                values: vec![ValueDraft {
                    name: "price".to_owned(),
                    kind: ValueKind::Integer {
                        min: 1,
                        max: 1_000_000,
                    },
                }],
            }],
            ..draft()
        })
        .unwrap(),
    )
}

#[test]
fn a_level_2_scripts_declared_signs_cross_as_themselves() {
    let written = wire::ModeDefinition::from(signed());

    let script = written.script.as_ref().unwrap();
    assert_eq!(script.api, 2);
    assert_eq!(script.signs.len(), 1);
    assert_eq!(script.signs[0].key, "price");
    assert_eq!(script.signs[0].lines, ["{price}", "", "afk"]);
    assert_eq!(read(written), Ok(signed()));
}

#[test]
fn a_level_1_script_with_signs_on_the_wire_is_refused_as_fleet_core_refuses_it() {
    let mut mode = wire::ModeDefinition::from(signed());
    mode.script.as_mut().unwrap().api = 1;

    assert_eq!(
        read(mode),
        Err(error(
            "ModeDefinition.script",
            Problem::Script(ScriptError::SignsNeedLevel2)
        ))
    );
}

#[test]
fn a_signs_missing_value_kind_is_refused_at_its_place() {
    let mut mode = wire::ModeDefinition::from(signed());
    mode.script.as_mut().unwrap().signs[0].values[0].kind = None;

    assert_eq!(
        read(mode),
        Err(error("DeclaredValue.kind", Problem::Missing).at(0))
    );
}

/// What the server validated is what the agent runs: a block written without
/// its namespace is the same block to fleet-core, but not the same text.
#[test]
fn a_script_not_in_its_exact_form_is_refused() {
    let mut mode = wire_scripted();
    mode.script.as_mut().unwrap().block_guard = vec!["cobblestone".to_owned()];

    assert_eq!(
        read(mode),
        Err(error("ModeDefinition.script", Problem::NotExact))
    );
}

/// `ModeDefinition` as a reader from before scripts declares it: only its
/// steps.
#[derive(Clone, PartialEq, prost::Message)]
struct OldModeDefinition {
    #[prost(message, repeated, tag = "1")]
    steps: Vec<wire::Step>,
}

/// Why the server never sends a scripted mode to an agent from before
/// scripts (ADR-0036, D13): it would read a mode without steps, a valid mode
/// that does nothing, and run it.
#[test]
fn a_reader_from_before_scripts_sees_a_mode_without_steps() {
    let old = OldModeDefinition::decode(wire_scripted().encode_to_vec().as_slice()).unwrap();

    assert_eq!(old.steps, []);
}

#[test]
fn a_hello_without_a_script_api_level_is_an_agent_from_before_scripts() {
    let hello = wire::Hello {
        name: "agent-1".to_owned(),
        agent_version: "0.1.0".to_owned(),
        protocol_version: fleet_proto::PROTOCOL_VERSION,
        capacity: 5,
        running: Vec::new(),
        script_api: 0,
    };
    let encoded = hello.encode_to_vec();

    let read = fleet_proto::control::read_hello(wire::AgentMessage {
        msg: Some(wire::agent_message::Msg::Hello(
            wire::Hello::decode(encoded.as_slice()).unwrap(),
        )),
    })
    .unwrap();

    assert_eq!(read.script_api, 0);
    // A level this version doesn't know is kept: the agent runs every lower
    // one too.
    let newer = fleet_proto::control::read_hello(wire::AgentMessage {
        msg: Some(wire::agent_message::Msg::Hello(wire::Hello {
            script_api: 7,
            ..wire::Hello::decode(encoded.as_slice()).unwrap()
        })),
    })
    .unwrap();
    assert_eq!(newer.script_api, 7);
}

#[test]
fn a_specs_chat_formats_cross_as_written_and_invalid_ones_are_refused() {
    let formats = ChatFormats::try_new(&["[{any}] {sender}: ", "<{sender}> "]).unwrap();
    let spec = BotSpec {
        chat_formats: formats.clone(),
        mode: scripted(),
        ..spec()
    };
    let written = wire::BotSpec::from(spec.clone());
    assert_eq!(written.chat_formats, ["[{any}] {sender}: ", "<{sender}> "]);
    assert_eq!(BotSpec::try_from(written.clone()), Ok(spec));

    let mut invalid = written;
    invalid.chat_formats = vec!["no sender here".to_owned()];
    assert!(matches!(
        BotSpec::try_from(invalid),
        Err(ConvertError {
            field: "BotSpec.chat_formats",
            problem: Problem::ChatFormats(ChatFormatsError::MissingPlaceholder { .. }),
            ..
        })
    ));
}

fn spec() -> BotSpec {
    BotSpec {
        id: BOT.parse().unwrap(),
        account: fleet_core::bot::BotAccount::Offline("AfkBot1".parse().unwrap()),
        server: "localhost:25565".parse().unwrap(),
        mode: ModeDefinition::afk(),
        desired: fleet_core::bot::DesiredRunState::Running,
        conflict_texts: fleet_core::disconnect::ConflictTexts::default(),
        chat_rate: fleet_core::chat::ChatRate::default(),
        chat_formats: ChatFormats::default(),
    }
}

/// A bot's event of `kind` from an agent, read by the server.
fn event(kind: wire::bot_event::Kind) -> Result<AgentMessage, ConvertError> {
    let bot = BOT.parse::<uuid::Uuid>().unwrap().as_bytes().to_vec();
    AgentMessage::try_from(wire::AgentMessage {
        msg: Some(wire::agent_message::Msg::Event(wire::BotEvent {
            bot_id: bot,
            at_unix_ms: Some(AT),
            kind: Some(kind),
        })),
    })
}

fn problem(change: fn(&mut wire::ScriptProblem)) -> wire::bot_event::Kind {
    let mut problem = wire::ScriptProblem {
        kind: wire::ScriptProblemKind::Error.into(),
        hook: wire::ScriptHook::Timer.into(),
        line: Some(3),
        column: Some(9),
        message: "`kees` isn't defined".to_owned(),
    };
    change(&mut problem);
    wire::bot_event::Kind::ScriptStatus(wire::ScriptStatus {
        status: Some(wire::script_status::Status::Stopped(problem)),
    })
}

fn reported(message: AgentMessage) -> BotEventKind {
    match message {
        AgentMessage::Event(event) => event.kind,
        other => panic!("not an event: {other:?}"),
    }
}

#[test]
fn a_scripts_status_crosses_with_its_hook_place_and_message() {
    let read = reported(event(problem(|_| {})).unwrap());

    assert_eq!(
        read,
        BotEventKind::ScriptStatus(ScriptState::Stopped(ScriptProblem {
            kind: ProblemKind::Error,
            hook: Some(fleet_core::script::Hook::Timer),
            place: ScriptPlace::new(3, 9),
            message: ReportText::clean("`kees` isn't defined"),
        }))
    );
}

#[test]
fn every_hook_crosses_in_a_status_level_2s_too() {
    for hook in fleet_core::script::Hook::ALL {
        let state = ScriptState::Stopped(ScriptProblem {
            kind: ProblemKind::Error,
            hook: Some(hook),
            place: None,
            message: ReportText::clean("stopped"),
        });
        let message = AgentMessage::Event(fleet_proto::control::BotEvent {
            bot_id: BOT.parse().unwrap(),
            at: chrono::DateTime::from_timestamp_millis(AT).unwrap(),
            kind: BotEventKind::ScriptStatus(state.clone()),
        });

        let written = wire::AgentMessage::try_from(message).unwrap();
        let Some(wire::agent_message::Msg::Event(wire::BotEvent {
            kind: Some(wire::bot_event::Kind::ScriptStatus(status)),
            ..
        })) = written.msg.clone()
        else {
            panic!("not a status")
        };
        let Some(wire::script_status::Status::Stopped(problem)) = status.status else {
            panic!("not stopped")
        };

        assert_ne!(
            problem.hook,
            i32::from(wire::ScriptHook::Unspecified),
            "{hook:?}"
        );
        assert_eq!(
            reported(AgentMessage::try_from(written).unwrap()),
            BotEventKind::ScriptStatus(state),
            "{hook:?}"
        );
    }
}

#[test]
fn a_click_line_crosses_as_a_kind_of_its_own() {
    let read = reported(
        event(wire::bot_event::Kind::ScriptLog(wire::ScriptLogLine {
            kind: wire::ScriptLogKind::Click.into(),
            text: "a click on slot 13 may have worked: the script checks the next screen (unclear)"
                .to_owned(),
        }))
        .unwrap(),
    );

    assert_eq!(
        read,
        BotEventKind::ScriptLog(ScriptLogLine {
            kind: LogKind::Click,
            text: ReportText::clean(
                "a click on slot 13 may have worked: the script checks the next screen (unclear)"
            ),
        })
    );
}

/// An agent's text is untrusted: whatever it sends, the server reads one
/// clean line of at most 200 characters, and never refuses the event for
/// its text.
#[test]
fn an_agents_text_is_cleaned_and_cut_never_refused() {
    let dirty = format!("§cline\nbreak\u{202e}{}", "x".repeat(100_000));
    let status = reported(
        event(problem(|problem| {
            problem.message = format!("§cline\nbreak\u{202e}{}", "x".repeat(100_000));
        }))
        .unwrap(),
    );
    let log = reported(
        event(wire::bot_event::Kind::ScriptLog(wire::ScriptLogLine {
            kind: wire::ScriptLogKind::Line.into(),
            text: dirty,
        }))
        .unwrap(),
    );

    let BotEventKind::ScriptStatus(ScriptState::Stopped(problem)) = status else {
        panic!("{status:?}");
    };
    assert!(problem.message.as_str().starts_with("line | break"));
    assert_eq!(
        problem.message.as_str().chars().count(),
        ReportText::MAX_CHARS
    );
    let BotEventKind::ScriptLog(ScriptLogLine { kind, text }) = log else {
        panic!("{log:?}");
    };
    assert_eq!(kind, LogKind::Line);
    assert_eq!(text, problem.message);
}

#[test]
fn a_refusal_names_no_hook() {
    let read = reported(
        event(problem(|problem| {
            problem.kind = wire::ScriptProblemKind::Compile.into();
            problem.hook = wire::ScriptHook::Unspecified.into();
        }))
        .unwrap(),
    );

    let BotEventKind::ScriptStatus(ScriptState::Stopped(problem)) = read else {
        panic!("{read:?}");
    };
    assert_eq!((problem.kind, problem.hook), (ProblemKind::Compile, None));
}

/// A change to a problem on the wire, and the error it reads as.
type ProblemCase = (fn(&mut wire::ScriptProblem), ConvertError);

#[test]
fn a_status_or_line_the_reader_cant_read_is_refused() {
    let place = "ScriptProblem.line";
    let cases: [ProblemCase; 5] = [
        (
            |problem| problem.column = None,
            error(place, Problem::Missing),
        ),
        (
            |problem| problem.line = Some(0),
            error(place, Problem::Missing),
        ),
        (
            |problem| problem.kind = wire::ScriptProblemKind::Unspecified.into(),
            error("ScriptProblem.kind", Problem::Missing),
        ),
        (
            |problem| problem.kind = 99,
            error("ScriptProblem.kind", Problem::Unknown),
        ),
        (
            |problem| problem.hook = 99,
            error("ScriptProblem.hook", Problem::Unknown),
        ),
    ];
    for (change, expected) in cases {
        assert_eq!(event(problem(change)), Err(expected));
    }
    assert_eq!(
        event(wire::bot_event::Kind::ScriptStatus(wire::ScriptStatus {
            status: None
        })),
        Err(error("ScriptStatus.status", Problem::Missing))
    );
    assert_eq!(
        event(wire::bot_event::Kind::ScriptLog(wire::ScriptLogLine {
            kind: wire::ScriptLogKind::Unspecified.into(),
            text: String::new(),
        })),
        Err(error("ScriptLogLine.kind", Problem::Missing))
    );
}
