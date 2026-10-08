//! Catalog and user text is never read as markup (GTK warned and blanked
//! "Crime & Punishment"). Turning markup off in a builder is not enough:
//! a builder sets the title before `use-markup`, and libadwaita parses it
//! as it is set. Rows take their text after they are built.
//!
//! A server's words (errors, names) reach many places GTK reads markup:
//! a hostile server could style or hide text with them (PX-11, findings 19
//! and G-15). So rows, banners and toasts are built with markup off, and
//! what is markup on purpose (a label's markup, a tooltip's, a status
//! page's or group's description) is a literal of ours, [`plain`] text,
//! or [`Markup`] made by [`markup!`]. The checks below hold every source
//! file of the app to that.

use gtk::glib;

/// `text` as markup that shows it as it is.
pub fn plain(text: &str) -> glib::GString {
    glib::markup_escape_text(text)
}

/// A toast that says `text` as it is.
pub fn toast(text: &str) -> adw::Toast {
    // Markup off before the title: a builder sets the title first.
    let toast = adw::Toast::builder().use_markup(false).build();
    toast.set_title(text);
    toast
}

/// Markup of ours with values escaped into it. Only [`markup!`] makes one,
/// so it can be passed around and still be known safe to parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Markup(String);

impl Markup {
    /// For [`markup!`] only; the checks refuse it anywhere else.
    #[doc(hidden)]
    pub fn escaped_by_the_macro(text: String) -> Self {
        Self(text)
    }

    /// The markup, for a setter that takes `&str`.
    pub fn escaped(&self) -> &str {
        &self.0
    }
}

impl From<Markup> for glib::GString {
    fn from(markup: Markup) -> Self {
        markup.0.into()
    }
}

/// `format!` for markup: each value is escaped with [`plain`]. The format
/// cannot name a variable (`{name}`): it goes through `concat!`, which
/// makes that a compile error, as a captured variable would not be escaped.
macro_rules! markup {
    ($format:literal $(, $value:expr)* $(,)?) => {
        $crate::markup::Markup::escaped_by_the_macro(format!(
            concat!($format)
            $(, $crate::markup::plain(&$value.to_string()))*
        ))
    };
}
pub(crate) use markup;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Each source file of the app by its path under src/, but this one:
    /// it holds the helpers and the checks' own examples.
    fn sources() -> Vec<(String, String)> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found = Vec::new();
        let mut dirs = vec![root.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let name = path.strip_prefix(&root).unwrap().display().to_string();
                    if name != "markup.rs" {
                        let source = std::fs::read_to_string(&path).unwrap();
                        found.push((name, source));
                    }
                }
            }
        }
        found.sort();
        found
    }

    /// `source` with its comment lines blanked, so words in them are not
    /// read as code; lines and offsets stay where they were.
    fn code(source: &str) -> String {
        source
            .split_inclusive('\n')
            .map(|line| {
                if line.trim_start().starts_with("//") {
                    line.chars()
                        .map(|c| if c == '\n' { c } else { ' ' })
                        .collect()
                } else {
                    line.to_owned()
                }
            })
            .collect()
    }

    /// The length of the string literal `text` starts with.
    fn literal_len(text: &str) -> Option<usize> {
        let mut chars = text.char_indices().skip(1);
        while let Some((i, c)) = chars.next() {
            match c {
                '\\' => {
                    chars.next();
                }
                '"' => return Some(i + 1),
                _ => {}
            }
        }
        None
    }

    /// What a call's parentheses hold, `text` starting just inside them.
    fn argument(text: &str) -> &str {
        let mut depth = 0;
        let mut i = 0;
        while i < text.len() {
            match text.as_bytes()[i] {
                b'"' => {
                    i += literal_len(&text[i..]).unwrap_or(text.len() - i);
                    continue;
                }
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' if depth == 0 => return &text[..i],
                b')' | b']' | b'}' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        text
    }

    /// Whether `text` is one call of `call`, nothing after it.
    fn whole_call(text: &str, call: &str) -> bool {
        text.strip_prefix(call)
            .is_some_and(|inside| argument(inside).len() + 1 == inside.len())
    }

    /// Calls that escape what they are given.
    const ESCAPES: &[&str] = &[
        "plain(",
        "crate::markup::plain(",
        "markup!(",
        "crate::markup::markup!(",
        "markup_escape_text(",
        "glib::markup_escape_text(",
        "gtk::glib::markup_escape_text(",
    ];

    /// Whether a markup argument is a literal, escaped, or [`Markup`].
    fn escaped(argument: &str) -> bool {
        // rustfmt ends an argument on lines of its own with a comma.
        let argument = argument.trim();
        let argument = argument.strip_suffix(',').unwrap_or(argument).trim_end();
        let argument = argument.strip_prefix('&').unwrap_or(argument).trim_start();
        if whole_call(argument, "Some(") {
            return escaped(&argument["Some(".len()..argument.len() - 1]);
        }
        if argument.starts_with('"') {
            return literal_len(argument) == Some(argument.len());
        }
        // Only [`Markup`] has `escaped()`: a name, or one call that made it,
        // but not text joined to it.
        if let Some(markup) = argument.strip_suffix(".escaped()").map(str::trim_end) {
            let path = |text: &str| {
                text.chars()
                    .all(|c| c.is_alphanumeric() || "_.:!".contains(c))
            };
            return path(markup)
                || markup.split_once('(').is_some_and(|(name, _)| {
                    path(name) && whole_call(markup, &format!("{name}("))
                });
        }
        ESCAPES.iter().any(|call| whole_call(argument, call))
    }

    /// The name a construction at `at` is bound to: `let name =`,
    /// `self.name =` or a field's `name:`, on its own line.
    fn binding(code: &str, at: usize) -> Option<String> {
        let start = code[..at].rfind('\n').map_or(0, |n| n + 1);
        // Without the type's path: `let row = adw::` before `ActionRow`.
        let before = code[start..at]
            .trim_end_matches(|c: char| c.is_alphanumeric() || c == '_' || c == ':')
            .trim();
        let before = before
            .strip_suffix('=')
            .or_else(|| before.strip_suffix(':'))?
            .trim_end();
        let before = before.strip_prefix("let ").map_or(before, |b| {
            let b = b.strip_prefix("mut ").unwrap_or(b);
            b.split(':').next().unwrap_or(b).trim_end()
        });
        let name: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        (!name.is_empty()).then_some(name)
    }

    /// The last name before the method call whose dot is at `at`.
    fn receiver(code: &str, at: usize) -> String {
        let name: Vec<char> = code[..at]
            .trim_end()
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        name.into_iter().rev().collect()
    }

    /// The builder that starts at `at`, up to its `.build()`.
    fn block(code: &str, at: usize) -> &str {
        let end = code[at..].find(".build()").map_or(code.len(), |e| at + e);
        &code[at..end]
    }

    /// Always markup: what they are given must be escaped.
    const SINKS: &[&str] = &[
        ".set_markup(",
        ".markup(",
        ".set_markup_with_mnemonic(",
        ".tooltip_markup(",
        ".set_tooltip_markup(",
        // Status pages', groups' and pages' descriptions.
        ".description(",
        ".set_description(",
    ];

    /// Never written outside this file: toasts are made by [`toast`],
    /// [`Markup`] by [`markup!`], and markup is not turned on after the fact
    /// (a label that wants it says `set_markup`).
    const REFUSED: &[&str] = &[
        "Toast::new(",
        "Markup::escaped_by_the_macro(",
        "set_use_markup(true)",
        "heading_use_markup(true)",
        "body_use_markup(true)",
    ];

    /// Widgets whose title (and a row's subtitle) is markup unless turned
    /// off; each is built with it off.
    const MARKUP_ON: &[&str] = &[
        "ActionRow",
        "SwitchRow",
        "ExpanderRow",
        "EntryRow",
        "ComboRow",
        "SpinRow",
        "PasswordEntryRow",
        "ButtonRow",
        "Toast",
        "Banner",
    ];

    /// Widgets whose title is markup with no way to turn it off.
    const ALWAYS_MARKUP: &[&str] = &["PreferencesGroup", "PreferencesPage"];

    /// Each place `source` gives GTK markup that is not a literal, escaped,
    /// or [`Markup`], or leaves markup on, as "file:line what".
    fn unescaped(name: &str, source: &str) -> Vec<String> {
        let code = code(source);
        let line = |at: usize| code[..at].matches('\n').count() + 1;
        let mut found = Vec::new();
        let mut flag = |at: usize, what: &str| found.push(format!("{name}:{} {what}", line(at)));

        for sink in SINKS {
            for (at, _) in code.match_indices(sink) {
                if !escaped(argument(&code[at + sink.len()..])) {
                    flag(at, sink);
                }
            }
        }
        for refused in REFUSED {
            for (at, _) in code.match_indices(refused) {
                flag(at, refused);
            }
        }
        for kind in MARKUP_ON {
            let built = format!("{kind}::builder()");
            for (at, _) in code.match_indices(&built) {
                if !block(&code, at).contains(".use_markup(false)") {
                    flag(at, "markup left on");
                }
            }
            let new = format!("{kind}::new(");
            for (at, _) in code.match_indices(&new) {
                let off = binding(&code, at)
                    .is_some_and(|name| code.contains(&format!("{name}.set_use_markup(false)")));
                if !off && *kind != "Toast" {
                    flag(at, "markup left on");
                }
            }
        }

        // Names of widgets that read markup: their setters are sinks too.
        let mut markup_names = Vec::new();
        for kind in ALWAYS_MARKUP {
            for start in [format!("{kind}::builder()"), format!("{kind}::new(")] {
                for (at, _) in code.match_indices(&start) {
                    markup_names.extend(binding(&code, at));
                    let built = block(&code, at);
                    if start.ends_with("builder()")
                        && let Some(i) = built.find(".title(")
                        && !escaped(argument(&built[i + ".title(".len()..]))
                    {
                        flag(at, ".title(");
                    }
                }
            }
        }
        for (at, _) in code.match_indices("Label::builder()") {
            let built = block(&code, at);
            if built.contains(".use_markup(true)") {
                markup_names.extend(binding(&code, at));
                if let Some(i) = built.find(".label(")
                    && !escaped(argument(&built[i + ".label(".len()..]))
                {
                    flag(at + i, ".label(");
                }
            }
        }
        for setter in [".set_markup(", ".set_markup_with_mnemonic("] {
            for (at, _) in code.match_indices(setter) {
                markup_names.push(receiver(&code, at));
            }
        }
        for setter in [".set_label(", ".set_title(", ".set_subtitle("] {
            for (at, _) in code.match_indices(setter) {
                if markup_names.contains(&receiver(&code, at))
                    && !escaped(argument(&code[at + setter.len()..]))
                {
                    flag(at, setter);
                }
            }
        }
        found.sort();
        found.dedup();
        found
    }

    #[test]
    fn markup_takes_server_text_only_escaped() {
        let found: Vec<String> = sources()
            .iter()
            .flat_map(|(name, source)| unescaped(name, source))
            .collect();
        assert_eq!(found, Vec::<String>::new());
    }

    #[test]
    fn the_app_has_no_builder_files() {
        // The checks read Rust only: a .ui or .blp file could turn markup
        // on where they do not look.
        let mut found = Vec::new();
        let mut dirs = vec![Path::new(env!("CARGO_MANIFEST_DIR")).to_owned()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "ui" || e == "blp") {
                    found.push(path);
                }
            }
        }
        assert_eq!(found, Vec::<std::path::PathBuf>::new());
    }

    #[test]
    fn the_check_finds_markup_from_data_in_every_sink() {
        let source = "toasts.add_toast(adw::Toast::new(&e.to_string()));\n\
                      page.set_description(Some(&why));\n\
                      label.set_markup(&format!(\"<b>{}</b>\", name));\n\
                      button.set_tooltip_markup(Some(&name));\n\
                      let b = gtk::Button::builder().tooltip_markup(name).build();\n\
                      let s = adw::StatusPage::builder().description(error).build();\n\
                      let l = gtk::Label::builder().use_markup(true).label(&name).build();\n\
                      l.set_label(&name);\n\
                      label.set_label(&title);\n\
                      label.set_use_markup(true);\n\
                      let d = adw::AlertDialog::builder().body_use_markup(true).build();\n\
                      let g = adw::PreferencesGroup::builder().title(&server.name).build();\n\
                      g.set_title(&server.name);\n\
                      let m = Markup::escaped_by_the_macro(name);";
        assert_eq!(
            unescaped("x.rs", source),
            [
                "x.rs:1 Toast::new(",
                "x.rs:10 set_use_markup(true)",
                "x.rs:11 body_use_markup(true)",
                "x.rs:12 .title(",
                "x.rs:13 .set_title(",
                "x.rs:14 Markup::escaped_by_the_macro(",
                "x.rs:2 .set_description(",
                "x.rs:3 .set_markup(",
                "x.rs:4 .set_tooltip_markup(",
                "x.rs:5 .tooltip_markup(",
                "x.rs:6 .description(",
                "x.rs:7 .label(",
                "x.rs:8 .set_label(",
                "x.rs:9 .set_label(",
            ]
        );
    }

    #[test]
    fn the_check_finds_markup_left_on() {
        let source = "let row = adw::ActionRow::builder().title(\"PIN\").build();\n\
                      let combo = adw::ComboRow::builder().title(\"Mode\").build();\n\
                      let toast = adw::Toast::builder().button_label(\"Undo\").build();\n\
                      let banner = adw::Banner::new(\"\");\n\
                      let off = adw::Banner::new(\"\");\n\
                      off.set_use_markup(false);\n\
                      let row = adw::SwitchRow::builder().use_markup(false).build();";
        assert_eq!(
            unescaped("x.rs", source),
            [
                "x.rs:1 markup left on",
                "x.rs:2 markup left on",
                "x.rs:3 markup left on",
                "x.rs:4 markup left on",
            ]
        );
    }

    #[test]
    fn the_check_takes_literals_and_escaped_text() {
        let source = "page.set_description(Some(&crate::markup::plain(&why)));\n\
                      page.set_description(Some(\"It does match in other places:\"));\n\
                      label.set_markup(&glib::markup_escape_text(&name));\n\
                      label.set_markup(hint.escaped());\n\
                      label.set_markup(lead(\"Open\", &url, \"\").escaped());\n\
                      label.set_markup(\n    lead(\"Open\", &url, \"\")\n    .escaped(),\n);\n\
                      label.set_markup(markup!(\"<b>{}</b>\", name).escaped());\n\
                      let l = gtk::Label::builder().use_markup(true).label(who.escaped()).build();\n\
                      l.set_label(markup!(\"<tt>{}</tt>\", code).escaped());\n\
                      let s = adw::StatusPage::builder().title(&name).description(\"Hi\").build();\n\
                      s.set_title(&name);\n\
                      // label.set_markup(&name) in a comment is not code\n\
                      other.set_label(&name);";
        assert_eq!(unescaped("x.rs", source), Vec::<String>::new());
    }

    #[test]
    fn the_check_refuses_text_joined_to_markup() {
        let source = "label.set_markup(&(name + hint.escaped()));\n\
                      label.set_markup(&format!(\"{}\", hint.escaped()));\n\
                      label.set_markup(\"<b>\".to_owned() + &name);\n\
                      label.set_markup(name.clone() + hint.escaped());";
        assert_eq!(
            unescaped("x.rs", source),
            [
                "x.rs:1 .set_markup(",
                "x.rs:2 .set_markup(",
                "x.rs:3 .set_markup(",
                "x.rs:4 .set_markup(",
            ]
        );
    }

    #[test]
    fn markup_escapes_each_value() {
        assert_eq!(
            markup!("<b>{}</b> · {}", "Tom & Jerry", 3).escaped(),
            "<b>Tom &amp; Jerry</b> · 3"
        );
    }

    #[test]
    fn plain_text_is_shown_as_it_is() {
        assert_eq!(
            plain("<b>Tom & Jerry</b>"),
            "&lt;b&gt;Tom &amp; Jerry&lt;/b&gt;"
        );
    }

    /// Rows and toasts whose builder sets a title or subtitle that is not a
    /// string literal, as "file:line".
    fn titled_from_data(name: &str, source: &str) -> Vec<String> {
        let mut found = Vec::new();
        for kind in MARKUP_ON {
            let start = format!("{kind}::builder()");
            for (at, _) in source.match_indices(&start) {
                let block = block(source, at);
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
        let found: Vec<String> = sources()
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
