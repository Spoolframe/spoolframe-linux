//! Opening the server's web pages in the system browser. Only http and
//! https: a hostile server's `web_url` could otherwise name a local file,
//! a share, or a scheme some other app handles (PX-11, finding 18).

use gtk::glib;
use gtk::prelude::*;

/// Whether `url` is a web page, by its scheme.
pub fn is_web(url: &str) -> bool {
    glib::Uri::peek_scheme(url).is_some_and(|scheme| scheme == "http" || scheme == "https")
}

/// Opens `url` in the system browser, over `window`, through the portal
/// in a sandbox. Anything but a web page is refused, and a failure is
/// logged: the screens show the address to open by hand.
pub fn open(url: &str, window: Option<&impl IsA<gtk::Window>>) {
    if !is_web(url) {
        tracing::warn!(url, "not opening a server's address that is no web page");
        return;
    }
    gtk::UriLauncher::new(url).launch(window, None::<&gtk::gio::Cancellable>, |result| {
        if let Err(e) = result {
            tracing::warn!(error = %e, "cannot open the browser");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_and_https_are_web_pages_in_any_case() {
        assert!(
            [
                "https://media.example.org/auth/device",
                "HTTP://10.0.0.2:7440"
            ]
            .iter()
            .all(|url| is_web(url))
        );
    }

    #[test]
    fn other_schemes_are_refused() {
        assert!(
            [
                "file:///etc/passwd",
                "javascript:alert(1)",
                "smb://host/share",
                "steam://run/1",
                "media.example.org",
                "",
            ]
            .iter()
            .all(|url| !is_web(url))
        );
    }
}
