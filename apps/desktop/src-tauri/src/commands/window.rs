//! The window's own theme (Plan.md P11.10; ADR-0020, U85, S94): the one command that isn't about
//! the server. It takes a closed enum, acts only on the window that called it, stores nothing and
//! answers nothing.

use fleet_api_types::app::WindowTheme;

use crate::window_theme::ThemedWindow as _;

/// `set_window_theme`: the native title bar and the window's colour follow the user's choice.
#[tauri::command]
pub(crate) fn set_window_theme<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    theme: WindowTheme,
) {
    window.follow(theme);
}
