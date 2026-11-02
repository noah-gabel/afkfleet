//! The native window's colours (Plan.md P11.10; ADR-0020, U85, S93, S94).
//!
//! The window starts hidden, so nothing shows before it has a colour: [`reveal_main_window`], in
//! `setup`, paints it as Windows says, then shows it. The UI's stored choice arrives later, through
//! [`ThemedWindow::follow`] and the `set_window_theme` command: the title bar and the colour at
//! the window's edge follow it. Rust can't read the webview's storage, so a stored choice that
//! differs from Windows shows Windows' colours until the page has started.
//!
//! The colours are the stylesheet's `--background`, which a test keeps equal.

use fleet_api_types::app::WindowTheme;
use tauri::Theme;
use tauri::utils::config::Color;

use crate::focus::{MainWindow, focus_main_window};

/// `--background` in `:root`.
const LIGHT: Color = Color(0xf4, 0xf5, 0xf6, 0xff);
/// `--background` in `.dark`.
const DARK: Color = Color(0x0f, 0x11, 0x13, 0xff);

/// What following a theme needs from a window.
pub(crate) trait ThemedWindow: Sized {
    /// The window's theme now: the one it was given, or Windows'.
    fn theme(&self) -> tauri::Result<Theme>;
    /// Gives the window `theme`, or with `None` lets it follow Windows.
    fn set_theme(&self, theme: Option<Theme>) -> tauri::Result<()>;
    /// Paints the window and its webview.
    fn set_background_color(&self, color: Option<Color>) -> tauri::Result<()>;

    /// Follows `choice`, for the command, which Tauri hands its window by value.
    fn follow(self, choice: WindowTheme) {
        follow_choice(&self, choice);
    }
}

impl<R: tauri::Runtime> ThemedWindow for tauri::WebviewWindow<R> {
    fn theme(&self) -> tauri::Result<Theme> {
        tauri::WebviewWindow::theme(self)
    }

    fn set_theme(&self, theme: Option<Theme>) -> tauri::Result<()> {
        tauri::WebviewWindow::set_theme(self, theme)
    }

    fn set_background_color(&self, color: Option<Color>) -> tauri::Result<()> {
        tauri::WebviewWindow::set_background_color(self, color)
    }
}

/// The colour that goes with `theme`.
#[must_use]
pub(crate) fn color_of(theme: Theme) -> Color {
    if theme == Theme::Dark { DARK } else { LIGHT }
}

/// Paints `window` for `theme`. A failure is dropped: an edge in the wrong colour is no reason to
/// tell anyone.
pub(crate) fn paint(window: &impl ThemedWindow, theme: Theme) {
    let _ = window.set_background_color(Some(color_of(theme)));
}

/// Follows `choice`: the title bar through the window's theme, then the colour. With `windows`
/// the window goes back to following Windows, and says which it is now. Failures are dropped, as
/// the page has already shown the choice.
pub(crate) fn follow_choice(window: &impl ThemedWindow, choice: WindowTheme) {
    let wanted = match choice {
        WindowTheme::Light => Some(Theme::Light),
        WindowTheme::Dark => Some(Theme::Dark),
        WindowTheme::Windows => None,
    };
    let _ = window.set_theme(wanted);
    if let Some(now) = window.theme().ok().or(wanted) {
        paint(window, now);
    }
}

/// That the main window was revealed: the app holds one once its setup ran, which is what a test on
/// Tauri's mock runtime can see, as its windows report no visibility (ADR-0020, S98).
pub(crate) struct Revealed(());

/// Paints `window` as Windows says, then shows it. The window must show whatever failed before
/// it, or the app would run with nothing on the screen.
pub(crate) fn reveal_main_window(window: &(impl ThemedWindow + MainWindow)) -> Revealed {
    if let Ok(now) = window.theme() {
        paint(window, now);
    }
    focus_main_window(window);
    Revealed(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::{
        DARK, LIGHT, MainWindow, Theme, ThemedWindow, WindowTheme, color_of, follow_choice, paint,
        reveal_main_window,
    };
    use tauri::utils::config::Color;

    /// A window that records each call, and fails the ones named in `failing`.
    struct RecordingWindow {
        now: Theme,
        calls: RefCell<Vec<String>>,
        failing: Vec<&'static str>,
    }

    impl RecordingWindow {
        fn showing(now: Theme) -> Self {
            Self {
                now,
                calls: RefCell::new(Vec::new()),
                failing: Vec::new(),
            }
        }

        fn record(&self, call: &str) -> tauri::Result<()> {
            self.calls.borrow_mut().push(call.to_owned());
            if self.failing.iter().any(|name| call.starts_with(name)) {
                Err(tauri::Error::WindowNotFound)
            } else {
                Ok(())
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl ThemedWindow for &RecordingWindow {
        fn theme(&self) -> tauri::Result<Theme> {
            self.record("theme")?;
            Ok(self.now)
        }

        fn set_theme(&self, theme: Option<Theme>) -> tauri::Result<()> {
            self.record(&format!("set_theme {theme:?}"))
        }

        fn set_background_color(&self, color: Option<Color>) -> tauri::Result<()> {
            let Some(Color(red, ..)) = color else {
                return self.record("paint none");
            };
            self.record(&format!("paint {red:#04x}"))
        }
    }

    impl MainWindow for &RecordingWindow {
        fn unminimize(&self) -> tauri::Result<()> {
            self.record("unminimize")
        }

        fn show(&self) -> tauri::Result<()> {
            self.record("show")
        }

        fn set_focus(&self) -> tauri::Result<()> {
            self.record("set_focus")
        }
    }

    /// The `--background` the stylesheet's block starting at `block` holds, as its hex digits.
    fn background_in(css: &str, block: &str) -> String {
        let (_, from_block) = css.split_once(block).unwrap();
        let (_, from_token) = from_block.split_once("--background:").unwrap();
        let (value, _) = from_token.split_once(';').unwrap();
        value.trim().trim_start_matches('#').to_owned()
    }

    /// `color` as the stylesheet writes it.
    fn hex(Color(red, green, blue, _): Color) -> String {
        format!("{red:02x}{green:02x}{blue:02x}")
    }

    #[test]
    fn the_colours_are_the_stylesheets_background_in_every_block() {
        let css = include_str!("../../../../packages/ui/src/styles.css");

        assert_eq!(background_in(css, "\n:root {"), hex(LIGHT));
        assert_eq!(background_in(css, "\n.dark {"), hex(DARK));
        assert_eq!(background_in(css, ":root:not(.light) {"), hex(DARK));
    }

    #[test]
    fn the_main_window_starts_hidden_so_nothing_shows_before_its_colour() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();

        assert_eq!(config["app"]["windows"][0]["visible"], false);
    }

    #[test]
    fn each_theme_has_its_colour() {
        assert_eq!(hex(color_of(Theme::Light)), hex(LIGHT));
        assert_eq!(hex(color_of(Theme::Dark)), hex(DARK));
    }

    #[test]
    fn painting_sets_the_themes_colour() {
        let window = RecordingWindow::showing(Theme::Light);

        paint(&&window, Theme::Dark);

        assert_eq!(window.calls(), ["paint 0x0f"]);
    }

    #[test]
    fn a_choice_gives_the_window_its_theme_then_its_colour() {
        let light = RecordingWindow::showing(Theme::Light);
        let dark = RecordingWindow::showing(Theme::Dark);

        follow_choice(&&light, WindowTheme::Light);
        follow_choice(&&dark, WindowTheme::Dark);

        assert_eq!(
            light.calls(),
            ["set_theme Some(Light)", "theme", "paint 0xf4"]
        );
        assert_eq!(
            dark.calls(),
            ["set_theme Some(Dark)", "theme", "paint 0x0f"]
        );
    }

    #[test]
    fn windows_goes_back_to_following_windows_and_paints_what_it_says_now() {
        let window = RecordingWindow::showing(Theme::Dark);

        follow_choice(&&window, WindowTheme::Windows);

        assert_eq!(window.calls(), ["set_theme None", "theme", "paint 0x0f"]);
    }

    #[test]
    fn a_choice_that_fails_is_dropped_and_still_paints() {
        let window = RecordingWindow {
            failing: vec!["set_theme"],
            ..RecordingWindow::showing(Theme::Light)
        };

        follow_choice(&&window, WindowTheme::Dark);

        assert_eq!(
            window.calls(),
            ["set_theme Some(Dark)", "theme", "paint 0xf4"]
        );
    }

    #[test]
    fn a_theme_that_cant_be_read_after_a_choice_paints_the_choice() {
        let window = RecordingWindow {
            failing: vec!["theme"],
            ..RecordingWindow::showing(Theme::Light)
        };

        follow_choice(&&window, WindowTheme::Dark);

        assert_eq!(
            window.calls(),
            ["set_theme Some(Dark)", "theme", "paint 0x0f"]
        );
    }

    #[test]
    fn revealing_paints_as_windows_says_before_it_shows() {
        let window = RecordingWindow::showing(Theme::Dark);

        reveal_main_window(&&window);

        assert_eq!(
            window.calls(),
            ["theme", "paint 0x0f", "unminimize", "show", "set_focus"]
        );
    }

    #[test]
    fn revealing_shows_the_window_though_its_theme_cant_be_read() {
        let window = RecordingWindow {
            failing: vec!["theme"],
            ..RecordingWindow::showing(Theme::Dark)
        };

        reveal_main_window(&&window);

        assert_eq!(window.calls(), ["theme", "unminimize", "show", "set_focus"]);
    }

    #[test]
    fn revealing_shows_the_window_though_its_colour_fails() {
        let window = RecordingWindow {
            failing: vec!["paint"],
            ..RecordingWindow::showing(Theme::Light)
        };

        reveal_main_window(&&window);

        assert_eq!(
            window.calls(),
            ["theme", "paint 0xf4", "unminimize", "show", "set_focus"]
        );
    }
}
