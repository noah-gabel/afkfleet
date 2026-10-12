//! The navigation guard (P8.3, ADR-0017): the app's webview only ever shows the app.
//!
//! The CSP limits what a page may load; this limits where the webview itself may go. An
//! internal plugin's `on_navigation` hook asks [`is_allowed`] about every navigation and
//! cancels the rest, so neither a link nor a script can take the window to another site.

use tauri::plugin::{Builder, TauriPlugin};
use tauri::{Manager as _, Runtime, Url};

/// The guard plugin's name.
const NAME: &str = "navigation-guard";

/// The scheme and host the built app is served from on Windows: Tauri's custom protocol
/// as `http://tauri.localhost` (`useHttpsScheme` is off), on HTTP's default port.
const APP_SCHEME: &str = "http";
const APP_HOST: &str = "tauri.localhost";
const APP_PORT: u16 = 80;

/// The plugin whose `on_navigation` hook cancels every navigation [`is_allowed`] refuses.
/// Only a dev build (`cfg(dev)`, which `tauri dev` and `cargo run` are) also allows
/// `build.devUrl`'s origin, Vite's dev server.
pub(crate) fn guard<R: Runtime>() -> Result<TauriPlugin<R>, tauri::Error> {
    Builder::new(NAME)
        .on_navigation(|webview, url| {
            let dev_url = if cfg!(dev) {
                webview.config().build.dev_url.as_ref()
            } else {
                None
            };
            is_allowed(url, dev_url)
        })
        .try_build()
        .map_err(|error| tauri::Error::PluginInitialization(NAME.to_owned(), error.to_string()))
}

/// Whether the webview may navigate to `url`: only to the app's own origin, with any path
/// or query, or to `dev_url`'s origin when there is one. Origins compare scheme, host and
/// port exactly, so `https://tauri.localhost` or `http://tauri.localhost.example.com` are
/// other sites, and `file:`, `data:`, `javascript:` and `about:` URLs have no origin that
/// could match.
#[must_use]
pub(crate) fn is_allowed(url: &Url, dev_url: Option<&Url>) -> bool {
    let is_app = url.scheme() == APP_SCHEME
        && url.host_str() == Some(APP_HOST)
        && url.port_or_known_default() == Some(APP_PORT);
    is_app || dev_url.is_some_and(|dev| url.origin() == dev.origin())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use tauri::Url;

    use super::is_allowed;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    fn dev_url() -> Url {
        url("http://localhost:1420")
    }

    #[rstest]
    #[case::the_root("http://tauri.localhost/")]
    #[case::without_a_slash("http://tauri.localhost")]
    #[case::a_path("http://tauri.localhost/assets/index.html")]
    #[case::a_query_and_a_fragment("http://tauri.localhost/login?next=%2Fbots#top")]
    #[case::the_explicit_default_port("http://tauri.localhost:80/")]
    fn the_app_origin_is_allowed_with_any_path_or_query(#[case] target: &str) {
        assert!(is_allowed(&url(target), None));
        assert!(is_allowed(&url(target), Some(&dev_url())));
    }

    #[rstest]
    #[case::the_root("http://localhost:1420/")]
    #[case::a_path_and_a_query("http://localhost:1420/src/main.tsx?t=1")]
    fn the_dev_url_origin_is_allowed_only_with_a_dev_url(#[case] target: &str) {
        assert!(is_allowed(&url(target), Some(&dev_url())));
        assert!(!is_allowed(&url(target), None));
    }

    #[rstest]
    #[case::another_site("https://example.com/")]
    #[case::another_port_on_the_dev_host("http://localhost:1421/")]
    #[case::the_dev_host_without_its_port("http://localhost/")]
    #[case::a_host_that_only_starts_like_the_app("http://tauri.localhost.evil.com/")]
    #[case::the_app_host_over_https("https://tauri.localhost/")]
    #[case::the_app_host_on_another_port("http://tauri.localhost:8080/")]
    #[case::a_file("file:///C:/Windows/win.ini")]
    #[case::a_data_url("data:text/html,<h1>hi</h1>")]
    #[case::javascript("javascript:alert(1)")]
    #[case::about_blank("about:blank")]
    fn everything_else_is_refused(#[case] target: &str) {
        assert!(!is_allowed(&url(target), Some(&dev_url())));
        assert!(!is_allowed(&url(target), None));
    }
}
