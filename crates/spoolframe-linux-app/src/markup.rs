//! Catalog and user text is never read as markup (GTK warned and blanked
//! "Crime & Punishment"). Turning markup off in a builder is not enough:
//! a builder sets the title before `use-markup`, and libadwaita parses it
//! as it is set. Rows take their text after they are built.
//!
//! Toasts and status pages' descriptions are markup too, and a server's
//! words reach them (errors, names): a hostile server could style or hide
//! text with them (PX-11, finding 19). Toasts go through [`toast`], and a
//! description not written here goes through [`plain`].

use gtk::glib;

/// `text` as markup that shows it as it is.
pub fn plain(text: &str) -> glib::GString {
    glib::markup_escape_text(text)
}

/// A toast that says `text` as it is.
pub fn toast(text: &str) -> adw::Toast {
    adw::Toast::new(&plain(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCES: &[(&str, &str)] = &[
        ("app.rs", include_str!("app.rs")),
        ("confirm.rs", include_str!("confirm.rs")),
        ("library.rs", include_str!("library.rs")),
        ("minibar.rs", include_str!("minibar.rs")),
        ("nowplaying.rs", include_str!("nowplaying.rs")),
        ("pages.rs", include_str!("pages.rs")),
        ("passkey_dialog.rs", include_str!("passkey_dialog.rs")),
        ("passkeys.rs", include_str!("passkeys.rs")),
        ("pin.rs", include_str!("pin.rs")),
        ("player.rs", include_str!("player.rs")),
        ("preferences.rs", include_str!("preferences.rs")),
        ("profile_page.rs", include_str!("profile_page.rs")),
        ("queue.rs", include_str!("queue.rs")),
        ("remote_sign_in.rs", include_str!("remote_sign_in.rs")),
        ("search.rs", include_str!("search.rs")),
        ("start.rs", include_str!("start.rs")),
        ("tracks.rs", include_str!("tracks.rs")),
        ("views.rs", include_str!("views.rs")),
    ];

    /// Rows and toasts whose builder sets a title or subtitle that is not a
    /// string literal, as "file:line".
    fn titled_from_data(name: &str, source: &str) -> Vec<String> {
        let mut found = Vec::new();
        for start in [
            "adw::ActionRow::builder()",
            "adw::SwitchRow::builder()",
            "adw::Toast::builder()",
        ] {
            for (at, _) in source.match_indices(start) {
                let end = source[at..]
                    .find(".build()")
                    .map_or(source.len(), |e| at + e);
                let block = &source[at..end];
                for method in [".title(", ".subtitle("] {
                    if let Some(i) = block.find(method) {
                        let argument = block[i + method.len()..].trim_start();
                        if !argument.starts_with('"') {
                            let line = source[..at].matches('\n').count() + 1;
                            found.push(format!("{name}:{line}"));
                        }
                    }
                }
            }
        }
        found
    }

    /// Markup made from anything but a literal or [`plain`]: a toast made
    /// without [`toast`], or a description, as "file:line".
    fn markup_from_data(name: &str, source: &str) -> Vec<String> {
        let line = |at: usize| format!("{name}:{}", source[..at].matches('\n').count() + 1);
        let mut found: Vec<String> = source
            .match_indices("Toast::new(")
            .map(|(at, _)| line(at))
            .collect();
        for call in ["set_description(Some(", ".description("] {
            for (at, _) in source.match_indices(call) {
                let argument = source[at + call.len()..].trim_start();
                let argument = argument.strip_prefix('&').unwrap_or(argument);
                if !(argument.starts_with('"')
                    || argument.starts_with("plain(")
                    || argument.starts_with("crate::markup::plain("))
                {
                    found.push(line(at));
                }
            }
        }
        found.sort();
        found
    }

    #[test]
    fn markup_takes_server_text_only_escaped() {
        let found: Vec<String> = SOURCES
            .iter()
            .flat_map(|(name, source)| markup_from_data(name, source))
            .collect();
        assert_eq!(found, Vec::<String>::new());
    }

    #[test]
    fn the_check_finds_a_toast_and_a_description_from_data() {
        let source = "toasts.add_toast(adw::Toast::new(&e.to_string()));\n\
                      page.set_description(Some(&why));\n\
                      page.set_description(Some(&crate::markup::plain(&why)));";
        assert_eq!(markup_from_data("x.rs", source), ["x.rs:1", "x.rs:2"]);
    }

    #[test]
    fn plain_text_is_shown_as_it_is() {
        assert_eq!(
            plain("<b>Tom & Jerry</b>"),
            "&lt;b&gt;Tom &amp; Jerry&lt;/b&gt;"
        );
    }

    #[test]
    fn rows_take_text_from_data_after_they_are_built() {
        let found: Vec<String> = SOURCES
            .iter()
            .flat_map(|(name, source)| titled_from_data(name, source))
            .collect();
        assert_eq!(found, Vec::<String>::new());
    }

    #[test]
    fn the_check_finds_a_title_from_data_in_a_builder() {
        let source = "let row = adw::ActionRow::builder()\n    .use_markup(false)\n    .title(&item.title)\n    .build();";
        assert_eq!(titled_from_data("x.rs", source), ["x.rs:1"]);
    }
}
