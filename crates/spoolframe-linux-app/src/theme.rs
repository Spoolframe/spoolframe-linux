//! Applies a [`Theme`] to the running app.

use std::cell::Cell;
use std::rc::Rc;

use adw::StyleManager;
use gtk::gdk::Display;
use spoolframe_linux_core::theme::{Scheme, Theme};

/// Holds the stylesheet of the theme being drawn. It sits one step above
/// the user's own `~/.config/gtk-4.0/gtk.css`: a theme picked inside the
/// app should win over it. When the desktop is followed it is empty, and
/// the user's gtk.css applies as usual.
#[derive(Clone)]
pub struct ThemeApplier {
    provider: gtk::CssProvider,
    /// What the user picked; what is drawn can differ (see
    /// [`Theme::effective`]).
    chosen: Rc<Cell<Theme>>,
}

impl ThemeApplier {
    pub fn new() -> Self {
        let display = Display::default().expect("GTK is initialised before the theme");
        let provider = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER + 1,
        );
        // The app's own stylesheet goes above the user's gtk.css as well,
        // added after the palette so its rules read the palette's
        // variables (style.css explains why).
        let app_css = gtk::CssProvider::new();
        app_css.load_from_string(include_str!("style.css"));
        gtk::style_context_add_provider_for_display(
            &display,
            &app_css,
            gtk::STYLE_PROVIDER_PRIORITY_USER + 1,
        );
        let applier = Self {
            provider,
            chosen: Rc::new(Cell::new(Theme::default())),
        };
        // A settings portal can appear after start-up; follow it when it
        // does, and fall back again if it goes away.
        let watcher = applier.clone();
        StyleManager::default().connect_system_supports_color_schemes_notify(move |_| {
            watcher.apply(watcher.chosen.get());
        });
        applier
    }

    pub fn apply(&self, chosen: Theme) {
        self.chosen.set(chosen);
        let drawn = chosen.effective(desktop_has_preference());
        self.provider
            .load_from_string(drawn.css().as_deref().unwrap_or(""));
        StyleManager::default().set_color_scheme(match drawn.scheme() {
            Scheme::System => adw::ColorScheme::Default,
            Scheme::ForceDark => adw::ColorScheme::ForceDark,
            Scheme::ForceLight => adw::ColorScheme::ForceLight,
        });
    }
}

/// Whether the desktop states a light or dark preference that libadwaita
/// can read.
pub fn desktop_has_preference() -> bool {
    StyleManager::default().system_supports_color_schemes()
}
