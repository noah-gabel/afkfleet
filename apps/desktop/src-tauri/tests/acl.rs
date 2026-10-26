//! The desktop app's ACL and IPC contract (P8.3, P8.4, ADR-0017), on Tauri's mock
//! runtime with the real `tauri.conf.json`, capabilities and app manifest:
//! - no command reaches the app, and no core command runs, unless a capability grants it
//! - tauri-plugin-opener's own commands don't exist for the webview: the app calls only
//!   its free `open_url`, from Rust (ADR-0018)
//! - every command the app registers and grants answers exactly as
//!   `tests/ipc-contract.json` says, with the arguments `TauriApiClient` sends
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::sync::{Arc, mpsc};

use afkfleet_desktop::keychain::KeychainFactory;
use afkfleet_desktop::profile::PROFILE_FILE;
use afkfleet_desktop::state::{AppState, LinkOpener, OpenError};
use fleet_api_types::app::{CommandError, LogoutResponse, ServerProfileState};
use fleet_client::VerifiedLink;
use rstest::rstest;
use serde_json::Value;
use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponseBody};
use tauri::test::{INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder};
use tauri::webview::InvokeRequest;
use tauri::{Manager as _, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tempfile::TempDir;

/// How long a test waits for a command's answer.
const WAIT: Duration = Duration::from_secs(10);

/// The contract fixture, shared with the TypeScript tests.
const CONTRACT: &str = include_str!("ipc-contract.json");

/// An app command that only this test registers, so no capability can grant it.
#[tauri::command]
fn probe() -> &'static str {
    "reached"
}

/// The app built from the real context with only `probe` registered, and its main
/// window.
fn probe_window() -> WebviewWindow<MockRuntime> {
    let app = mock_builder()
        .invoke_handler(tauri::generate_handler![probe])
        .build(tauri::generate_context!(test = true))
        .unwrap();
    WebviewWindowBuilder::new(&app, "main", WebviewUrl::default())
        .build()
        .unwrap()
}

/// An opener that fails the test if anything reaches it: no contract entry may open a
/// link, so none ever starts a browser.
struct NeverOpens;

impl LinkOpener for NeverOpens {
    fn open(&self, link: &VerifiedLink) -> Result<(), OpenError> {
        panic!("the ACL test opened {}", link.as_str())
    }
}

/// The app as `run` builds it, with the real commands, over a profile path in `dir`
/// (no file there: no server is set up), keyring-core's in-memory mock store and an
/// opener that must never be called, and its main window.
fn app_window(dir: &TempDir) -> WebviewWindow<MockRuntime> {
    let app = afkfleet_desktop::configure(mock_builder())
        .build(tauri::generate_context!(test = true))
        .unwrap();
    let factory =
        KeychainFactory::new(keyring_core::mock::Store::new().unwrap(), "io.example.test");
    app.manage(AppState::load(
        dir.path().join(PROFILE_FILE),
        factory,
        Arc::new(NeverOpens),
    ));
    WebviewWindowBuilder::new(&app, "main", WebviewUrl::default())
        .build()
        .unwrap()
}

/// Sends `cmd` with `args` from the main window's own page, the local origin, and
/// returns the IPC layer's answer: the JSON of a command's Ok, or its error value.
/// The call runs on a thread, so a command that never answers fails the test.
fn invoke(window: &WebviewWindow<MockRuntime>, cmd: &str, args: Value) -> Result<Value, Value> {
    let request = InvokeRequest {
        cmd: cmd.to_owned(),
        callback: CallbackFn(0),
        error: CallbackFn(1),
        url: window.url().unwrap(),
        body: InvokeBody::Json(args),
        headers: tauri::http::HeaderMap::default(),
        invoke_key: INVOKE_KEY.to_owned(),
    };
    let window = window.clone();
    let (answer, answered) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = answer.send(get_ipc_response(&window, request));
    });
    match answered
        .recv_timeout(WAIT)
        .expect("the command didn't answer in time")
    {
        Ok(InvokeResponseBody::Json(json)) => Ok(serde_json::from_str(&json).unwrap()),
        Ok(InvokeResponseBody::Raw(bytes)) => {
            Ok(Value::String(format!("{} raw bytes", bytes.len())))
        }
        Err(error) => Err(error),
    }
}

/// What kind of rejection `value` is, for a failure's message: Tauri rejects with a
/// plain string, never with a `CommandError`.
fn rejection_kind(value: &Value) -> &'static str {
    let text = value.as_str().unwrap_or_default();
    if text.contains("not allowed") {
        "the ACL refused it"
    } else if text.contains("invalid args") || text.contains("invalid channel value") {
        // A channel argument that's a string, but not `__CHANNEL__:<id>`, is
        // refused with the channel's own message, before the command runs.
        "Tauri couldn't read its arguments"
    } else if text.contains("not found") {
        "no handler is registered for it"
    } else {
        "an unknown rejection"
    }
}

#[test]
fn an_app_command_no_capability_grants_is_refused_by_the_acl() {
    let window = probe_window();

    let answer = invoke(&window, "probe", Value::Object(serde_json::Map::new()));

    let error = answer.expect_err("the command ran, so app commands aren't behind the ACL");
    assert!(error.to_string().contains("not allowed"), "{error}");
}

#[test]
fn a_core_command_is_refused_since_no_core_permission_is_granted() {
    let window = probe_window();

    let answer = invoke(
        &window,
        "plugin:event|listen",
        Value::Object(serde_json::Map::new()),
    );

    let error = answer.expect_err("a core command ran without a grant");
    assert!(error.to_string().contains("not allowed"), "{error}");
}

#[test]
fn the_opener_plugins_command_is_refused() {
    let dir = TempDir::new().unwrap();
    let window = app_window(&dir);

    let answer = invoke(
        &window,
        "plugin:opener|open_url",
        serde_json::json!({ "url": "https://www.microsoft.com/link" }),
    );

    let error = answer.expect_err("the opener plugin's command ran");
    assert_eq!(rejection_kind(&error), "the ACL refused it", "{error}");
}

#[test]
fn every_contract_command_passes_the_acl_and_answers_as_the_contract_says() {
    let dir = TempDir::new().unwrap();
    let window = app_window(&dir);
    let contract: Value = serde_json::from_str(CONTRACT).unwrap();
    let entries = contract["commands"].as_array().unwrap();

    for entry in entries {
        let command = entry["command"].as_str().unwrap();
        let answer = invoke(&window, command, entry["args"].clone());

        match (&entry["expect"]["ok"], &entry["expect"]["error"], answer) {
            (expected, Value::Null, Ok(value)) if !expected.is_null() => {
                assert_ok(command, expected, &value);
            }
            (Value::Null, Value::String(code), Err(value)) => {
                let error: CommandError = serde_json::from_value(value.clone())
                    .unwrap_or_else(|_| panic!("{command}: {} ({value})", rejection_kind(&value)));
                assert_eq!(
                    serde_json::to_value(error.code).unwrap(),
                    Value::String(code.clone()),
                    "{command}: {error:?}"
                );
            }
            (_, _, answer) => panic!("{command}: unexpected answer {answer:?}"),
        }
    }
    assert_eq!(entries.len(), 39);
}

/// An Ok answer must parse as `command`'s own response type and equal the expected
/// value.
fn assert_ok(command: &str, expected: &Value, value: &Value) {
    match command {
        "server_profile" => assert_eq!(
            serde_json::from_value::<ServerProfileState>(value.clone()).unwrap(),
            serde_json::from_value::<ServerProfileState>(expected.clone()).unwrap(),
            "{command}"
        ),
        "logout" => assert_eq!(
            serde_json::from_value::<LogoutResponse>(value.clone()).unwrap(),
            serde_json::from_value::<LogoutResponse>(expected.clone()).unwrap(),
            "{command}"
        ),
        other => panic!("{other}: the contract expects no Ok answer from it"),
    }
}

#[test]
fn a_command_with_an_unknown_argument_shape_is_refused_before_it_runs() {
    let dir = TempDir::new().unwrap();
    let window = app_window(&dir);

    let answer = invoke(
        &window,
        "login",
        serde_json::json!({ "request": { "username": "AfkBot1", "password": "x", "token": "y" } }),
    );

    let error = answer.expect_err("an unknown field reached the command");
    assert!(
        serde_json::from_value::<CommandError>(error.clone()).is_err(),
        "{error}"
    );
    assert_eq!(
        rejection_kind(&error),
        "Tauri couldn't read its arguments",
        "{error}"
    );
}

#[rstest]
#[case::a_number(serde_json::json!(5))]
#[case::a_string(serde_json::json!("x"))]
#[case::a_channels_prefix_without_an_id(serde_json::json!("__CHANNEL__:x"))]
fn a_watch_whose_updates_arent_a_channel_is_refused_before_it_runs(#[case] updates: Value) {
    let dir = TempDir::new().unwrap();
    let window = app_window(&dir);

    let answer = invoke(
        &window,
        "watch_events",
        serde_json::json!({ "updates": updates }),
    );

    let error = answer.expect_err("a watch ran without a channel");
    assert!(
        serde_json::from_value::<CommandError>(error.clone()).is_err(),
        "{error}"
    );
    assert_eq!(
        rejection_kind(&error),
        "Tauri couldn't read its arguments",
        "{error}"
    );
}

/// Tauri hands a channel's updates of 8 KiB and more to the webview through
/// its own fetch command, which the ACL lets through without a core grant:
/// it answers its own refusal here (no channel to read), never the ACL's.
#[test]
fn the_channels_own_fetch_needs_no_core_grant() {
    let dir = TempDir::new().unwrap();
    let window = app_window(&dir);

    let answer = invoke(
        &window,
        "plugin:__TAURI_CHANNEL__|fetch",
        Value::Object(serde_json::Map::new()),
    );

    let error = answer.expect_err("a fetch without a channel answered");
    assert_ne!(rejection_kind(&error), "the ACL refused it", "{error}");
    assert!(error.to_string().contains("channel id"), "{error}");
}
