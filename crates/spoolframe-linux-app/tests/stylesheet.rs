//! The app's stylesheet takes every colour from libadwaita's variables, so
//! that "Follow system", the user's gtk.css and every Catppuccin theme
//! restyle the app's own widgets too.

use spoolframe_linux_core::theme::VARIABLES;

const STYLESHEET: &str = include_str!("../src/style.css");

/// Colour literals: `#rgb`-style hex, and the CSS colour functions.
fn colour_literals(css: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (i, _) in css.match_indices('#') {
        let hex: String = css[i + 1..]
            .chars()
            .take_while(|c| c.is_ascii_hexdigit())
            .collect();
        if [3, 4, 6, 8].contains(&hex.len()) {
            found.push(format!("#{hex}"));
        }
    }
    for function in ["rgb(", "rgba(", "hsl(", "hsla(", "color-mix("] {
        if css.contains(function) {
            found.push(function.to_owned());
        }
    }
    found
}

/// The names in every `var(--name)`.
fn variables_used(css: &str) -> Vec<&str> {
    css.match_indices("var(--")
        .map(|(i, m)| {
            let rest = &css[i + m.len()..];
            &rest[..rest.find([')', ',']).unwrap_or(rest.len())]
        })
        .collect()
}

#[test]
fn the_stylesheet_has_no_colour_literals() {
    assert_eq!(colour_literals(STYLESHEET), Vec::<String>::new());
}

#[test]
fn the_check_finds_a_colour_literal() {
    let found = colour_literals(".x { color: #94e2d5; background: rgb(0 0 0); }");
    assert_eq!(found, ["#94e2d5", "rgb("]);
}

#[test]
fn the_stylesheet_uses_only_variables_every_theme_sets() {
    let used = variables_used(STYLESHEET);
    assert!(!used.is_empty(), "the stylesheet uses no variables at all");
    for name in used {
        assert!(
            VARIABLES.contains(&name),
            "--{name} is not set by the themes"
        );
    }
}
