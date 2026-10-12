//! The desktop app's ACL (P8.3, ADR-0017), on Tauri's mock runtime with the real
//! `tauri.conf.json`, capabilities and app manifest: no command reaches the app, and no
//! core command runs, unless a capability grants it.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder};
use tauri::webview::InvokeRequest;
use tauri::{WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// An app command that only this test registers, so no capability can grant it.
#[tauri::command]
fn probe() -> &'static str {
    "reached"
}

/// The app built from the real context, with `probe` registered, and its main window.
fn main_window() -> WebviewWindow<MockRuntime> {
    let app = mock_builder()
        .invoke_handler(tauri::generate_handler![probe])
        .build(tauri::generate_context!(test = true))
        .unwrap();
    WebviewWindowBuilder::new(&app, "main", WebviewUrl::default())
        .build()
        .unwrap()
}

/// Sends `cmd` from the main window's own page, the local origin, and returns the
/// IPC layer's answer.
fn invoke(window: &WebviewWindow<MockRuntime>, cmd: &str) -> Result<String, String> {
    let request = InvokeRequest {
        cmd: cmd.to_owned(),
        callback: CallbackFn(0),
        error: CallbackFn(1),
        url: window.url().unwrap(),
        body: InvokeBody::default(),
        headers: tauri::http::HeaderMap::default(),
        invoke_key: INVOKE_KEY.to_owned(),
    };
    get_ipc_response(window, request)
        .map(|body| format!("{body:?}"))
        .map_err(|error| error.to_string())
}

#[test]
fn an_app_command_no_capability_grants_is_refused_by_the_acl() {
    let window = main_window();

    let answer = invoke(&window, "probe");

    let error = answer.expect_err("the command ran, so app commands aren't behind the ACL");
    assert!(error.contains("not allowed"), "{error}");
}

#[test]
fn a_core_command_is_refused_since_no_core_permission_is_granted() {
    let window = main_window();

    let answer = invoke(&window, "plugin:event|listen");

    let error = answer.expect_err("a core command ran without a grant");
    assert!(error.contains("not allowed"), "{error}");
}
