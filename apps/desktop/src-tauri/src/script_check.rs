//! The `check_script` command's work (Plan.md P15.25; ADR-0045): a scripted
//! mode's text checked as the server checks it at save, while it's typed.
//!
//! - **The same check, the same words.** The text goes through fleet-api-types'
//!   `ModeScriptRequest::script` (fleet-core's rules for a script's text) and
//!   fleet-script's own `compile`, the one the server and the agent run; a
//!   problem is the field error the server's 422 would carry
//!   (`script.source`, its message, its line and its column). The server
//!   stays the judge: it checks again at save, with the mode's settings and
//!   declarations, which the editor's form checks with the server's answer.
//! - **Compiled, never run:** no hook is called, so a script whose hooks never
//!   end is checked as quickly as any other.
//! - **Off the UI's thread and the async runtime:** compiling is CPU work, so it
//!   runs in `spawn_blocking`, one check at a time. A check waits at most
//!   [`TURN_WAIT`] for the one before it.
//! - **Untrusted text:** the text can be another Admin's shared script. A check
//!   that fails (a panic in the compiler, a lost task, too long a wait)
//!   answers [`CommandErrorCode::ScriptCheck`], which the editor shows as
//!   "couldn't check": never a crash, and never a verdict on the script. The
//!   request is bounded at [`SCRIPT_CHECK_MAX_BYTES`], four times a script's
//!   own limit.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use fleet_api_types::FieldError;
use fleet_api_types::app::{CommandError, CommandErrorCode, ScriptCheck, ScriptCheckRequest};
use fleet_api_types::script::{ModeScriptRequest, not_a_program};
use fleet_core::script::{LeashRadius, ToolGuard};
use garde::Validate as _;
use tauri::async_runtime::spawn_blocking;
use tokio::sync::Semaphore;

use crate::error;

#[cfg(doc)]
use fleet_api_types::app::SCRIPT_CHECK_MAX_BYTES;

/// How long a check waits for the one before it: far longer than the longest
/// script takes to compile, about a millisecond.
pub const TURN_WAIT: Duration = Duration::from_secs(5);

/// One check at a time, for the whole app.
static TURN: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));

/// The first problem the server's check at save would find in `source`, or
/// none; see the module comment.
#[must_use]
pub fn check(source: &str) -> ScriptCheck {
    let request = ModeScriptRequest {
        source: source.to_owned(),
        leash: LeashRadius::DEFAULT.blocks(),
        tool_guard: ToolGuard::DEFAULT.uses_left(),
        block_guard: Vec::new(),
        messages: Vec::new(),
        patterns: Vec::new(),
    };
    let problem = match request.script() {
        Err(rule) => Some(FieldError::from(rule)),
        Ok(script) => fleet_script::compile(&script)
            .err()
            .map(|error| not_a_program(&error)),
    };
    ScriptCheck { problem }
}

/// The `check_script` command: `request` checked in `spawn_blocking`, one
/// check at a time.
///
/// # Errors
/// [`CommandErrorCode::InvalidInput`] for a text over its bound, and
/// [`CommandErrorCode::ScriptCheck`] for a check that failed or waited too
/// long.
pub async fn check_script(request: ScriptCheckRequest) -> Result<ScriptCheck, CommandError> {
    run(request, &TURN, TURN_WAIT, check).await
}

/// `work` on `request`'s text in `spawn_blocking`, once `turn` is free, which
/// it waits for at most `wait`.
async fn run(
    request: ScriptCheckRequest,
    turn: &Arc<Semaphore>,
    wait: Duration,
    work: impl FnOnce(&str) -> ScriptCheck + Send + 'static,
) -> Result<ScriptCheck, CommandError> {
    request
        .validate()
        .map_err(|report| error::invalid(&report))?;
    let failed = || CommandError::new(CommandErrorCode::ScriptCheck);
    // Not network IO: the wait for the check before it, which compiles at most
    // 64 KiB and takes about a millisecond, bounded all the same.
    let permit = tokio::time::timeout(wait, Arc::clone(turn).acquire_owned())
        .await
        .map_err(|_| failed())?
        .map_err(|_| failed())?;
    // The permit moves into the thread: a check that outlives its caller still
    // holds the turn until it ends. A panic is a JoinError, never a crash: the
    // app unwinds (`[profile.release]`).
    spawn_blocking(move || {
        let _permit = permit;
        work(&request.source)
    })
    .await
    .map_err(|_| failed())
}

#[cfg(test)]
mod tests {
    use fleet_api_types::script::not_a_program;
    use fleet_core::script::{ScriptApiLevel, ScriptDraft};

    use super::*;

    /// A test's bound on waiting for a check: real time, since the check runs
    /// on Tauri's runtime, outside the test's.
    const SHORT: Duration = Duration::from_millis(100);

    fn problem(source: &str) -> FieldError {
        check(source).problem.unwrap()
    }

    fn compiled(source: &str) -> Result<(), FieldError> {
        let script = fleet_core::script::Script::try_from(ScriptDraft {
            // The level the server saves a mode at until it stamps a
            // script's own (Plan.md P16.15).
            api: ScriptApiLevel::V1.get(),
            source: source.to_owned(),
            leash: 8,
            tool_guard: 2,
            block_guard: Vec::new(),
            messages: Vec::new(),
            patterns: Vec::new(),
            signs: Vec::new(),
        })
        .unwrap();
        fleet_script::compile(&script)
            .map(|_| ())
            .map_err(|error| not_a_program(&error))
    }

    fn request(source: &str) -> ScriptCheckRequest {
        ScriptCheckRequest {
            source: source.to_owned(),
        }
    }

    #[test]
    fn every_example_of_the_reference_has_no_problem() {
        for example in fleet_script::EXAMPLES {
            assert_eq!(
                check(example.source),
                ScriptCheck { problem: None },
                "{}",
                example.id
            );
        }
    }

    #[test]
    fn a_syntax_error_is_the_servers_field_error_at_its_line_and_column() {
        let source = "fn on_join(first) {\n    keys.jump.tap(;\n}\n";

        let found = problem(source);

        assert_eq!(Err(found.clone()), compiled(source));
        assert_eq!(found.path.as_str(), "script.source");
        assert_eq!(found.line, Some(2));
        assert!(found.column.is_some());
    }

    #[test]
    fn an_undefined_object_is_found_with_its_name_and_place() {
        let found = problem("fn on_join(first) {\n\tkees.forward.hold();\n}");

        assert!(
            found.message.as_str().contains("kees"),
            "{}",
            found.message.as_str()
        );
        assert_eq!((found.line, found.column), (Some(2), Some(2)));
    }

    #[test]
    fn a_rule_of_the_text_itself_is_found_at_its_place() {
        let found = problem("fn on_join(first) {\n\u{200b}}");

        assert_eq!(found.path.as_str(), "script.source");
        assert_eq!((found.line, found.column), (Some(2), Some(1)));
    }

    #[test]
    fn a_text_past_a_scripts_limit_is_told_so_without_a_place() {
        let found = problem(&format!(
            "fn on_join(first) {{ }}\n{}",
            "/".repeat(16 * 1024)
        ));

        assert_eq!(found.path.as_str(), "script.source");
        assert!(
            found.message.as_str().contains("16"),
            "{}",
            found.message.as_str()
        );
        assert_eq!((found.line, found.column), (None, None));
    }

    #[test]
    fn a_hook_that_never_ends_is_compiled_and_never_run() {
        assert_eq!(
            check("fn on_join(first) {\n    loop { }\n}\n").problem,
            None
        );
    }

    #[tokio::test]
    async fn the_command_answers_the_check() {
        let answer = check_script(request("fn on_join(first) {\n\tkees.jump.tap();\n}"))
            .await
            .unwrap();

        assert_eq!(answer.problem.map(|found| found.line), Some(Some(2)));
    }

    #[tokio::test]
    async fn a_text_past_the_requests_bound_is_invalid_input_and_never_compiled() {
        let turn = Arc::new(Semaphore::new(1));
        let source = "x".repeat(fleet_api_types::app::SCRIPT_CHECK_MAX_BYTES + 1);

        let refused = run(request(&source), &turn, SHORT, |_| {
            panic!("a text past its bound was checked")
        })
        .await
        .unwrap_err();

        assert_eq!(refused.code, CommandErrorCode::InvalidInput);
    }

    #[tokio::test]
    async fn a_check_that_panics_is_one_that_couldnt_check_never_a_crash() {
        let turn = Arc::new(Semaphore::new(1));

        let failed = run(request("fn on_join(first) { }"), &turn, SHORT, |_| {
            panic!("the compiler failed")
        })
        .await
        .unwrap_err();

        assert_eq!(failed.code, CommandErrorCode::ScriptCheck);
        assert_eq!(turn.available_permits(), 1, "the turn was given back");
    }

    #[tokio::test]
    async fn a_check_waits_for_the_one_before_it_and_no_longer_than_its_bound() {
        let turn = Arc::new(Semaphore::new(1));
        let held = Arc::clone(&turn).acquire_owned().await.unwrap();

        let waited = run(request("fn on_join(first) { }"), &turn, SHORT, check)
            .await
            .unwrap_err();
        drop(held);
        let next = run(request("fn on_join(first) { }"), &turn, SHORT, check)
            .await
            .unwrap();

        assert_eq!(waited.code, CommandErrorCode::ScriptCheck);
        assert_eq!(next.problem, None);
    }

    #[tokio::test]
    async fn a_check_runs_off_the_callers_thread() {
        let turn = Arc::new(Semaphore::new(1));
        let caller = std::thread::current().id();

        let elsewhere = run(request(""), &turn, SHORT, move |_| {
            let here = std::thread::current().id();
            ScriptCheck {
                problem: (here == caller).then(|| FieldError::new("thread", "the caller's")),
            }
        })
        .await
        .unwrap();

        assert_eq!(elsewhere.problem, None);
    }
}
