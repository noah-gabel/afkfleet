//! Bringing the running app forward when it's launched again (P8.3).
//!
//! The single-instance plugin ends a second launch at once and calls back into the first
//! instance, which restores, shows and focuses its main window with
//! [`focus_main_window`].

/// What bringing the main window forward needs from it.
pub(crate) trait MainWindow {
    /// Restores the window if it's minimized.
    fn unminimize(&self) -> tauri::Result<()>;
    /// Shows the window if it's hidden.
    fn show(&self) -> tauri::Result<()>;
    /// Gives the window the keyboard focus, bringing it to the front.
    fn set_focus(&self) -> tauri::Result<()>;
}

impl<R: tauri::Runtime> MainWindow for tauri::WebviewWindow<R> {
    fn unminimize(&self) -> tauri::Result<()> {
        tauri::WebviewWindow::unminimize(self)
    }

    fn show(&self) -> tauri::Result<()> {
        tauri::WebviewWindow::show(self)
    }

    fn set_focus(&self) -> tauri::Result<()> {
        tauri::WebviewWindow::set_focus(self)
    }
}

/// Restores, shows and focuses `window`, in that order. Each step is tried even when an
/// earlier one failed, and failures are dropped: the launch that asked for this has
/// already ended, so there's nobody to report them to, and a window brought partly
/// forward is still better than none.
pub(crate) fn focus_main_window(window: &impl MainWindow) {
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::{MainWindow, focus_main_window};

    /// A window that records each call, and fails the ones named in `failing`.
    #[derive(Default)]
    struct RecordingWindow {
        calls: RefCell<Vec<&'static str>>,
        failing: Vec<&'static str>,
    }

    impl RecordingWindow {
        fn record(&self, call: &'static str) -> tauri::Result<()> {
            self.calls.borrow_mut().push(call);
            if self.failing.contains(&call) {
                Err(tauri::Error::WindowNotFound)
            } else {
                Ok(())
            }
        }
    }

    impl MainWindow for RecordingWindow {
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

    #[test]
    fn the_window_is_restored_shown_and_focused_in_that_order() {
        let window = RecordingWindow::default();

        focus_main_window(&window);

        assert_eq!(*window.calls.borrow(), ["unminimize", "show", "set_focus"]);
    }

    #[test]
    fn every_step_is_tried_even_when_an_earlier_one_fails() {
        let window = RecordingWindow {
            failing: vec!["unminimize", "show"],
            ..RecordingWindow::default()
        };

        focus_main_window(&window);

        assert_eq!(*window.calls.borrow(), ["unminimize", "show", "set_focus"]);
    }
}
