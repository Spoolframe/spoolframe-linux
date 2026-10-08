//! Opening the server's web pages in the system browser. Only http and
//! https: a hostile server's `web_url` could otherwise name a local file,
//! a share, or a scheme some other app handles (PX-11, finding 18).

use gtk::prelude::*;
use gtk::{gio, glib};

/// Whether `url` is a web page, by its scheme.
pub fn is_web(url: &str) -> bool {
    glib::Uri::peek_scheme(url).is_some_and(|scheme| scheme == "http" || scheme == "https")
}

/// Opens `url` in the system browser through the desktop portal's
/// OpenURI, which starts it outside the app: the app may start no program
/// itself (C-15, see [`crate::sandbox`]), and GTK's launcher would, out of
/// a Flatpak. Anything but a web page is refused, and a failure (no
/// portal) is logged: the screens show the address to open by hand.
pub fn open(url: &str, _window: Option<&impl IsA<gtk::Window>>) {
    if !is_web(url) {
        tracing::warn!(url, "not opening a server's address that is no web page");
        return;
    }
    let arguments = glib::Variant::tuple_from_iter([
        // No parent window: the portal places the browser itself.
        "".to_variant(),
        url.to_variant(),
        glib::VariantDict::new(None).end(),
    ]);
    gio::bus_get(
        gio::BusType::Session,
        None::<&gio::Cancellable>,
        move |bus| match bus {
            Ok(bus) => bus.call(
                Some("org.freedesktop.portal.Desktop"),
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.OpenURI",
                "OpenURI",
                Some(&arguments),
                Some(glib::VariantTy::new("(o)").expect("a valid type")),
                gio::DBusCallFlags::NONE,
                -1,
                None::<&gio::Cancellable>,
                |result| {
                    if let Err(e) = result {
                        tracing::warn!(error = %e, "the desktop portal cannot open the browser");
                    }
                },
            ),
            Err(e) => tracing::warn!(error = %e, "no session bus to open the browser over"),
        },
    );
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
