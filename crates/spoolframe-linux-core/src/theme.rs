//! Themes. "Follow system" uses libadwaita as it is: the desktop's light or
//! dark style, its accent colour and its high-contrast mode. Each Catppuccin
//! theme is a stylesheet that only redefines libadwaita's colour variables,
//! so stock widgets and our own (which use the same variables, never a
//! literal colour) change together.

/// A theme the user can pick in Preferences → Appearance. Mocha until
/// they do: the app is drawn for its own palettes, and the system's plain
/// Adwaita leaves its controls without their styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Theme {
    System,
    #[default]
    Mocha,
    Macchiato,
    Frappe,
    Latte,
}

/// A theme's look in miniature, as `#rrggbb`: its window, sidebar,
/// borders, text and accent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Swatch {
    pub base: &'static str,
    pub mantle: &'static str,
    pub surface: &'static str,
    pub text: &'static str,
    pub accent: &'static str,
}

/// How the theme sets libadwaita's light or dark style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    /// Follow the desktop's setting.
    System,
    ForceDark,
    ForceLight,
}

impl Theme {
    /// In the order Preferences shows them.
    pub const ALL: [Theme; 5] = [
        Theme::System,
        Theme::Mocha,
        Theme::Macchiato,
        Theme::Frappe,
        Theme::Latte,
    ];

    /// The name the user sees.
    pub fn name(self) -> &'static str {
        match self {
            Theme::System => "Follow system",
            Theme::Mocha => "Mocha",
            Theme::Macchiato => "Macchiato",
            Theme::Frappe => "Frappé",
            Theme::Latte => "Latte",
        }
    }

    /// The key the theme is saved under.
    pub fn key(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Mocha => "mocha",
            Theme::Macchiato => "macchiato",
            Theme::Frappe => "frappe",
            Theme::Latte => "latte",
        }
    }

    /// The theme saved under `key`, or `None` for a key no theme has.
    pub fn from_key(key: &str) -> Option<Theme> {
        Theme::ALL.into_iter().find(|t| t.key() == key)
    }

    /// The theme to draw. "Follow system" needs the desktop to state a
    /// light or dark preference, which libadwaita reads through the
    /// settings portal. A desktop without one (a bare Hyprland session, for
    /// example) would leave libadwaita on its light default, so the app's
    /// own look, Mocha, stands in until the desktop provides one.
    pub fn effective(self, system_has_preference: bool) -> Theme {
        match self {
            Theme::System if !system_has_preference => Theme::Mocha,
            theme => theme,
        }
    }

    /// A palette is drawn for one style. Some of libadwaita's shades are
    /// not variables, so the style must be forced to match, or borders and
    /// hover states would come out in the other style's colours.
    pub fn scheme(self) -> Scheme {
        match self.palette() {
            None => Scheme::System,
            Some(p) if p.dark => Scheme::ForceDark,
            Some(_) => Scheme::ForceLight,
        }
    }

    /// The colours Preferences draws this theme's swatch in, or `None`
    /// for "Follow system", which has no colours of its own.
    pub fn swatch(self) -> Option<Swatch> {
        self.palette().map(|p| Swatch {
            base: p.base,
            mantle: p.mantle,
            surface: p.surface,
            text: p.text,
            accent: p.accent,
        })
    }

    /// The stylesheet for this theme, or `None` for "Follow system".
    pub fn css(self) -> Option<String> {
        self.palette().map(|p| p.css())
    }

    fn palette(self) -> Option<Palette> {
        Some(match self {
            Theme::System => return None,
            Theme::Mocha => Palette {
                dark: true,
                base: "#1e1e2e",
                mantle: "#181825",
                crust: "#11111b",
                surface: "#313244",
                text: "#cdd6f4",
                accent: "#94e2d5",
                accent_text: "#94e2d5",
                on_accent: "#11111b",
                red: "#f38ba8",
                green: "#a6e3a1",
                yellow: "#f9e2af",
                blue: "#89b4fa",
                peach: "#fab387",
                pink: "#f5c2e7",
                mauve: "#cba6f7",
                overlay: "#9399b2",
            },
            Theme::Macchiato => Palette {
                dark: true,
                base: "#24273a",
                mantle: "#1e2030",
                crust: "#181926",
                surface: "#363a4f",
                text: "#cad3f5",
                accent: "#8bd5ca",
                accent_text: "#8bd5ca",
                on_accent: "#181926",
                red: "#ed8796",
                green: "#a6da95",
                yellow: "#eed49f",
                blue: "#8aadf4",
                peach: "#f5a97f",
                pink: "#f5bde6",
                mauve: "#c6a0f6",
                overlay: "#939ab7",
            },
            Theme::Frappe => Palette {
                dark: true,
                base: "#303446",
                mantle: "#292c3c",
                crust: "#232634",
                surface: "#414559",
                text: "#c6d0f5",
                accent: "#81c8be",
                accent_text: "#81c8be",
                on_accent: "#232634",
                red: "#e78284",
                green: "#a6d189",
                yellow: "#e5c890",
                blue: "#8caaee",
                peach: "#ef9f76",
                pink: "#f4b8e4",
                mauve: "#ca9ee6",
                overlay: "#949cbb",
            },
            Theme::Latte => Palette {
                dark: false,
                base: "#eff1f5",
                mantle: "#e6e9ef",
                crust: "#dce0e8",
                surface: "#ccd0da",
                text: "#4c4f69",
                accent: "#179299",
                // Latte's teal is too light for text on Latte's base (about
                // 3.3:1); text and icons use a darker teal to reach 4.5:1.
                accent_text: "#0f6f74",
                on_accent: "#ffffff",
                red: "#d20f39",
                green: "#40a02b",
                yellow: "#df8e1d",
                blue: "#1e66f5",
                peach: "#fe640b",
                pink: "#ea76cb",
                mauve: "#8839ef",
                overlay: "#7c7f93",
            },
        })
    }
}

/// The libadwaita colour variables every palette sets. The app's own
/// stylesheet may use only these, so that a palette never leaves one of our
/// widgets in libadwaita's default colours.
pub const VARIABLES: &[&str] = &[
    "accent-bg-color",
    "accent-fg-color",
    "accent-color",
    "destructive-bg-color",
    "destructive-fg-color",
    "destructive-color",
    "success-bg-color",
    "success-fg-color",
    "success-color",
    "warning-bg-color",
    "warning-fg-color",
    "warning-color",
    "error-bg-color",
    "error-fg-color",
    "error-color",
    "window-bg-color",
    "window-fg-color",
    "view-bg-color",
    "view-fg-color",
    "headerbar-bg-color",
    "headerbar-fg-color",
    "headerbar-border-color",
    "headerbar-backdrop-color",
    "headerbar-shade-color",
    "sidebar-bg-color",
    "sidebar-fg-color",
    "sidebar-backdrop-color",
    "sidebar-border-color",
    "sidebar-shade-color",
    "card-bg-color",
    "card-fg-color",
    "card-shade-color",
    "dialog-bg-color",
    "dialog-fg-color",
    "popover-bg-color",
    "popover-fg-color",
    "popover-shade-color",
    "thumbnail-bg-color",
    "thumbnail-fg-color",
    "shade-color",
    "scrollbar-outline-color",
    // The named accent colours (libadwaita 1.6). Placeholder tiles use
    // them, so items without artwork are told apart in each theme's hues.
    "accent-blue",
    "accent-teal",
    "accent-green",
    "accent-yellow",
    "accent-orange",
    "accent-red",
    "accent-pink",
    "accent-purple",
    "accent-slate",
];

/// One Catppuccin flavour, reduced to the roles libadwaita needs.
struct Palette {
    dark: bool,
    base: &'static str,
    mantle: &'static str,
    crust: &'static str,
    surface: &'static str,
    text: &'static str,
    /// Fills: suggested buttons, progress, selection.
    accent: &'static str,
    /// Accent-coloured text and icons on `base`.
    accent_text: &'static str,
    /// Text on an `accent` fill.
    on_accent: &'static str,
    red: &'static str,
    green: &'static str,
    yellow: &'static str,
    blue: &'static str,
    peach: &'static str,
    pink: &'static str,
    mauve: &'static str,
    overlay: &'static str,
}

impl Palette {
    /// The value of each of [`VARIABLES`], in the same order.
    fn values(&self) -> [&str; VARIABLES.len()] {
        let shade = if self.dark {
            "rgb(0 0 0 / 36%)"
        } else {
            "rgb(0 0 0 / 7%)"
        };
        let outline = if self.dark {
            "rgb(0 0 0 / 50%)"
        } else {
            "rgb(255 255 255 / 50%)"
        };
        [
            self.accent,
            self.on_accent,
            self.accent_text,
            self.red,
            self.on_accent,
            self.red,
            self.green,
            self.on_accent,
            self.green,
            self.yellow,
            self.on_accent,
            self.yellow,
            self.red,
            self.on_accent,
            self.red,
            self.base,
            self.text,
            self.base,
            self.text,
            self.mantle,
            self.text,
            self.crust,
            self.base,
            shade,
            self.mantle,
            self.text,
            self.mantle,
            self.crust,
            shade,
            self.mantle,
            self.text,
            shade,
            self.base,
            self.text,
            self.mantle,
            self.text,
            shade,
            self.surface,
            self.text,
            shade,
            outline,
            self.blue,
            self.accent,
            self.green,
            self.yellow,
            self.peach,
            self.red,
            self.pink,
            self.mauve,
            self.overlay,
        ]
    }

    fn css(&self) -> String {
        let mut css = String::from(":root {\n");
        for (name, value) in VARIABLES.iter().zip(self.values()) {
            css.push_str(&format!("  --{name}: {value};\n"));
        }
        css.push_str("}\n");
        css
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_system_has_no_stylesheet_and_does_not_force_a_style() {
        assert_eq!(Theme::System.css(), None);
        assert_eq!(Theme::System.scheme(), Scheme::System);
    }

    #[test]
    fn follow_system_falls_back_to_mocha_when_the_desktop_has_no_preference() {
        assert_eq!(Theme::System.effective(false), Theme::Mocha);
    }

    #[test]
    fn follow_system_follows_a_desktop_that_has_a_preference() {
        assert_eq!(Theme::System.effective(true), Theme::System);
    }

    #[test]
    fn a_picked_palette_ignores_the_desktop() {
        assert_eq!(Theme::Latte.effective(false), Theme::Latte);
        assert_eq!(Theme::Latte.effective(true), Theme::Latte);
    }

    #[test]
    fn every_palette_sets_every_variable_exactly_once() {
        for theme in Theme::ALL.into_iter().filter(|t| *t != Theme::System) {
            let css = theme.css().unwrap();
            for name in VARIABLES {
                let count = css.matches(&format!("--{name}:")).count();
                assert_eq!(count, 1, "{theme:?} sets --{name} {count} times");
            }
        }
    }

    #[test]
    fn mocha_paints_the_accent_teal() {
        let css = Theme::Mocha.css().unwrap();
        assert!(css.contains("--accent-bg-color: #94e2d5;"), "{css}");
    }

    #[test]
    fn latte_is_light_and_the_dark_flavours_are_dark() {
        assert_eq!(Theme::Latte.scheme(), Scheme::ForceLight);
        for theme in [Theme::Mocha, Theme::Macchiato, Theme::Frappe] {
            assert_eq!(theme.scheme(), Scheme::ForceDark, "{theme:?}");
        }
    }

    #[test]
    fn a_saved_key_gives_back_its_theme() {
        for theme in Theme::ALL {
            assert_eq!(Theme::from_key(theme.key()), Some(theme));
        }
    }

    #[test]
    fn an_unknown_key_is_no_theme() {
        assert_eq!(Theme::from_key("dracula"), None);
    }
}
