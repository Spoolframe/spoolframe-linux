//! Catalog and user text is never read as markup (GTK warned and blanked
//! "Crime & Punishment"). Turning markup off in a builder is not enough:
//! a builder sets the title before `use-markup`, and libadwaita parses it
//! as it is set. Rows take their text after they are built.

const SOURCES: &[(&str, &str)] = &[
    ("app.rs", include_str!("../../src/app.rs")),
    ("pages.rs", include_str!("../../src/pages.rs")),
    ("preferences.rs", include_str!("../../src/preferences.rs")),
    ("profile_page.rs", include_str!("../../src/profile_page.rs")),
    ("search.rs", include_str!("../../src/search.rs")),
    ("start.rs", include_str!("../../src/start.rs")),
    ("views.rs", include_str!("../../src/views.rs")),
    ("nowplaying.rs", include_str!("../../src/nowplaying.rs")),
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
