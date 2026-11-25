//! The `convert_to_script` command's work (Plan.md P15.27; ADR-0046): a
//! step mode's steps, as the mode editor holds them, written as the script
//! that does what they do, with what it does differently.
//!
//! - **The steps are read as the server reads them** (fleet-api-types'
//!   `ModeSteps`): a step that breaks a rule is invalid input at its field,
//!   worded as the server's 422 at save.
//! - **fleet-script's converter writes the script** (`fleet_script::convert`),
//!   and the answer is the scripted mode's body as the editor reads a mode
//!   its viewer may edit: the one who converts may edit it, so each declared
//!   message is whole. A chat step can hold a password, so the call is a
//!   direct one in the UI, and nothing here is stored or logged.
//! - **Why steps can't be a script** is the answer's `problem` at its field:
//!   a chat step's text a mode can't declare at its `message`, more timers
//!   than a script has at `steps`.
//! - **Nothing is compiled or run:** the converter's tests hold that what it
//!   writes compiles, and the server compiles it again at save. Writing it
//!   is a few microseconds of text, so it runs on the command's own task.

use fleet_api_types::FieldError;
use fleet_api_types::app::{
    CommandError, CommandErrorCode, ConversionNote, ConversionNoteKind, ScriptConversion,
    ScriptConvertRequest,
};
use fleet_api_types::script::ModeScript;
use fleet_script::{ConvertError, Note, NoteKind};
use garde::Validate as _;

use crate::error;

/// The steps of `request` as a script; see the module comment.
///
/// # Errors
/// [`CommandErrorCode::InvalidInput`] for steps that break a rule, at its
/// field; [`CommandErrorCode::ScriptCheck`] for a conversion that failed,
/// which no step mode leads to.
pub fn convert(request: &ScriptConvertRequest) -> Result<ScriptConversion, CommandError> {
    request
        .validate()
        .map_err(|report| error::invalid(&report))?;
    // `validate` read the steps through `definition` already, so this
    // passes; a rule it names anyway is invalid input all the same.
    let definition = request
        .steps
        .definition()
        .map_err(|_| CommandError::new(CommandErrorCode::InvalidInput))?;
    match fleet_script::convert(&definition) {
        Ok(conversion) => Ok(ScriptConversion {
            script: Some(ModeScript::of(conversion.script(), true)),
            notes: conversion.notes().iter().filter_map(note).collect(),
            problem: None,
        }),
        Err(ConvertError::Message { step, reason }) => Ok(problem(FieldError::new(
            &format!("steps[{step}].action.message"),
            &format!("can't be a script's declared message: {reason}"),
        ))),
        Err(ConvertError::TooManyTimers { count }) => Ok(problem(FieldError::new(
            "steps",
            &format!(
                "needs {count} timers as a script, one a repeating step; a script has at most {}, and repeating steps next to each other on one schedule without jitter share one",
                fleet_core::script::limits::MAX_TIMERS
            ),
        ))),
        Err(ConvertError::NotSteps | ConvertError::Script(_)) => {
            Err(CommandError::new(CommandErrorCode::ScriptCheck))
        }
    }
}

/// An answer with `field` as its problem.
fn problem(field: FieldError) -> ScriptConversion {
    ScriptConversion {
        script: None,
        notes: Vec::new(),
        problem: Some(field),
    }
}

/// `note` as the UI reads it; none for a step past what a mode holds,
/// which the steps' own rule rules out.
fn note(note: &Note) -> Option<ConversionNote> {
    let step = u8::try_from(note.step).ok()?;
    let millis = |every: core::time::Duration| u32::try_from(every.as_millis()).ok();
    let (kind, other, every_ms) = match note.kind {
        NoteKind::SwingSkipsEntities => (ConversionNoteKind::SwingSkipsEntities, None, None),
        NoteKind::Slower { every } => (ConversionNoteKind::Slower, None, millis(every)),
        NoteKind::HoldRepeats => (ConversionNoteKind::HoldRepeats, None, None),
        NoteKind::GapCapped => (ConversionNoteKind::GapCapped, None, None),
        NoteKind::SharesLimit { other, every } => (
            ConversionNoteKind::SharesLimit,
            Some(u8::try_from(other).ok()?),
            millis(every),
        ),
    };
    Some(ConversionNote {
        step,
        kind,
        other,
        every_ms,
    })
}

#[cfg(test)]
mod tests {
    use fleet_api_types::app::CommandErrorCode;
    use serde_json::{Value, json};

    use super::*;

    fn request(steps: &Value) -> ScriptConvertRequest {
        serde_json::from_value(json!({ "steps": steps })).unwrap()
    }

    fn at_start(action: &Value) -> Value {
        json!({ "action": action, "schedule": { "type": "at_start" }, "probability": 100 })
    }

    fn every(action: &Value, interval_ms: u32) -> Value {
        json!({ "action": action, "schedule": { "type": "every", "interval_ms": interval_ms, "jitter_ms": 0 }, "probability": 100 })
    }

    fn the_script(conversion: &ScriptConversion) -> &ModeScript {
        conversion.script.as_ref().expect("a script")
    }

    #[test]
    fn steps_come_back_as_their_script_with_what_it_does_differently() {
        let conversion = convert(&request(&json!([
            at_start(&json!({ "type": "select_hotbar_slot", "slot": 0 })),
            every(&json!({ "type": "swing_arm" }), 250),
        ])))
        .unwrap();

        let script = the_script(&conversion);
        assert!(
            script
                .source
                .as_str()
                .contains("hotbar.select(1); // when it joins, step 1")
        );
        assert!(
            script
                .source
                .as_str()
                .contains("swing(); // while it's online, step 1")
        );
        assert_eq!(conversion.problem, None);
        assert_eq!(
            conversion.notes,
            [
                ConversionNote {
                    step: 1,
                    kind: ConversionNoteKind::SwingSkipsEntities,
                    other: None,
                    every_ms: None,
                },
                ConversionNote {
                    step: 1,
                    kind: ConversionNoteKind::Slower,
                    other: None,
                    every_ms: Some(500),
                },
            ]
        );
    }

    #[test]
    fn steps_that_share_a_limit_name_each_other() {
        let conversion = convert(&request(&json!([
            every(&json!({ "type": "use_item" }), 1_000),
            every(&json!({ "type": "use_item" }), 1_500),
        ])))
        .unwrap();

        assert_eq!(
            conversion.notes,
            [
                ConversionNote {
                    step: 0,
                    kind: ConversionNoteKind::SharesLimit,
                    other: Some(1),
                    every_ms: Some(200),
                },
                ConversionNote {
                    step: 1,
                    kind: ConversionNoteKind::SharesLimit,
                    other: Some(0),
                    every_ms: Some(200),
                },
            ]
        );
    }

    /// The one who converts may edit the mode: a chat step's command comes
    /// back whole as a declared message, never in the script's text.
    #[test]
    fn a_chat_step_is_a_whole_declared_message_and_never_in_the_text() {
        let conversion = convert(&request(&json!([at_start(
            &json!({ "type": "send_chat", "message": "/login hunter2" })
        )])))
        .unwrap();

        let script = the_script(&conversion);
        assert!(!script.source.as_str().contains("hunter2"));
        assert_eq!(script.messages.len(), 1);
        assert_eq!(script.messages[0].key.as_str(), "join_1");
        assert_eq!(script.messages[0].text.as_str(), "/login hunter2");
        assert!(!script.messages[0].arguments_hidden);
    }

    #[test]
    fn steps_that_break_a_rule_are_invalid_input_at_the_field() {
        let mut step = at_start(&json!({ "type": "jump" }));
        step["probability"] = json!(0);

        let error = convert(&request(&json!([step]))).unwrap_err();

        assert_eq!(error.code, CommandErrorCode::InvalidInput);
        let fields = error.fields.unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].path.as_str(), "steps[0].probability");
    }

    #[test]
    fn a_chat_step_a_mode_cant_declare_is_the_problem_at_its_message() {
        let conversion = convert(&request(&json!([
            at_start(&json!({ "type": "jump" })),
            at_start(&json!({ "type": "send_chat", "message": "/sa{y hi" })),
        ])))
        .unwrap();

        let problem = conversion.problem.unwrap();
        assert_eq!(problem.path.as_str(), "steps[1].action.message");
        assert_eq!(
            problem.message.as_str(),
            "can't be a script's declared message: the command's name holds a brace, at position 3"
        );
        assert_eq!(conversion.script, None);
        assert_eq!(conversion.notes, []);
    }

    /// The e2e fake answers its story's steps with this command's own
    /// answer, so the story shows what the app shows.
    #[test]
    fn the_e2e_fakes_conversion_is_the_real_one() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../e2e/fake/converted.json")).unwrap();
        let request: ScriptConvertRequest =
            serde_json::from_value(fixture["request"].clone()).unwrap();
        let answer: ScriptConversion = serde_json::from_value(fixture["answer"].clone()).unwrap();

        assert_eq!(convert(&request).unwrap(), answer);
    }

    #[test]
    fn more_timers_than_a_script_has_is_the_problem_at_the_steps() {
        let steps: Vec<Value> = (0..17)
            .map(|n| every(&json!({ "type": "jump" }), 1_000 + n * 50))
            .collect();

        let conversion = convert(&request(&Value::Array(steps))).unwrap();

        let problem = conversion.problem.unwrap();
        assert_eq!(problem.path.as_str(), "steps");
        assert_eq!(
            problem.message.as_str(),
            "needs 17 timers as a script, one a repeating step; a script has at most 16, and repeating steps next to each other on one schedule without jitter share one"
        );
    }
}
