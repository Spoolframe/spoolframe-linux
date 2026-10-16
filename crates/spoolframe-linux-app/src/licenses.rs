//! The components the app is built from and their licenses, behind
//! Settings › About › Open source licenses (LinuxLicenses and
//! LinuxLicenseText). The list is `data/licenses.json`, written by
//! `scripts/gen-licenses.py` from Cargo.lock and the Flatpak manifest and
//! compiled into the binary's resource (`build.rs`). CI fails when it is
//! out of date. Pure: the page is `licenses_page`.

use std::collections::BTreeMap;

use serde::Deserialize;

/// Where `build.rs` puts the list in the resource.
pub const RESOURCE: &str = "/casa/coldsteel/Spoolframe/licenses.json";

/// The list's three sections, in the board's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    /// The SDK crates and what they pull in.
    Sdk,
    /// This app's own crates and the rest of what it links.
    App,
    /// C libraries the Flatpak builds as its own modules.
    Flatpak,
}

impl Group {
    pub const ALL: [Group; 3] = [Group::Sdk, Group::App, Group::Flatpak];

    pub fn title(self) -> &'static str {
        match self {
            Group::Sdk => "Spoolframe SDK",
            Group::App => "This app",
            Group::Flatpak => "Bundled with the Flatpak",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Group::Sdk => "The connection to your servers, shared by every Spoolframe app.",
            Group::App => {
                "The window, the player, the desktop’s media keys, and the palette, icons and fonts."
            }
            Group::Flatpak => "C libraries the security-key prompt needs.",
        }
    }
}

/// One crate or library, as the data file has it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Component {
    pub name: String,
    pub version: String,
    pub group: Group,
    /// Its SPDX license expression ("MIT OR Apache-2.0").
    pub license: String,
    /// The "Copyright …" lines of its license files, else its authors.
    pub copyright: Vec<String>,
    /// Each license id of the expression, to its text in [`Licenses::text`].
    texts: BTreeMap<String, usize>,
}

impl Component {
    /// What sits under the title: the copyright lines, one a line.
    pub fn copyright_line(&self) -> Option<String> {
        (!self.copyright.is_empty()).then(|| self.copyright.join("\n"))
    }

    /// The license ids in its expression.
    pub fn ids(&self) -> Vec<String> {
        ids(&self.license)
    }
}

#[derive(Deserialize)]
struct File {
    components: Vec<Component>,
    texts: Vec<String>,
}

/// The data file is not what the generator writes.
#[derive(Debug)]
pub enum Error {
    Parse(serde_json::Error),
    /// The component that names a text the list does not have.
    MissingText(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Parse(e) => write!(f, "the license list cannot be read: {e}"),
            Error::MissingText(name) => {
                write!(f, "{name} names a license text the list does not have")
            }
        }
    }
}

impl std::error::Error for Error {}

/// The whole list.
#[derive(Debug)]
pub struct Licenses {
    components: Vec<Component>,
    texts: Vec<String>,
}

impl Licenses {
    /// Reads the data file, checking every text it points at is there.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let file: File = serde_json::from_slice(bytes).map_err(Error::Parse)?;
        for component in &file.components {
            if component.texts.values().any(|&i| i >= file.texts.len()) {
                return Err(Error::MissingText(component.name.clone()));
            }
        }
        let mut components = file.components;
        // Each group's own crates first (the spoolframe-* ones), then by name.
        components.sort_by(|a, b| {
            let theirs = |c: &Component| !c.name.starts_with("spoolframe-");
            (theirs(a), &a.name, &a.version).cmp(&(theirs(b), &b.name, &b.version))
        });
        Ok(Self {
            components,
            texts: file.texts,
        })
    }

    pub fn components(&self) -> &[Component] {
        &self.components
    }

    /// The text of license `id` as `component` ships it.
    pub fn text(&self, component: &Component, id: &str) -> Option<&str> {
        component
            .texts
            .get(id)
            .and_then(|&i| self.texts.get(i))
            .map(String::as_str)
    }

    /// The chips worth showing: All, then each family some component has.
    pub fn chips(&self) -> Vec<Chip> {
        Chip::ALL
            .into_iter()
            .filter(|chip| *chip == Chip::All || self.components.iter().any(|c| chip.matches(c)))
            .collect()
    }

    /// Indices into [`Self::components`] that `query` and `chip` show, by
    /// group, in the board's order. A group nothing matches is left out.
    pub fn shown(&self, query: &str, chip: Chip) -> Vec<(Group, Vec<usize>)> {
        Group::ALL
            .into_iter()
            .filter_map(|group| {
                let shown: Vec<usize> = self
                    .components
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.group == group && matches(c, query, chip))
                    .map(|(i, _)| i)
                    .collect();
                (!shown.is_empty()).then_some((group, shown))
            })
            .collect()
    }

    /// The text shown for `choice` of `component`: its one license, or each
    /// of them under its id when the choice is several at once.
    pub fn shown_text(&self, component: &Component, choice: &[String]) -> String {
        let text = |id: &str| self.text(component, id).unwrap_or_default().trim_end();
        match choice {
            [one] => format!("{}\n", text(one)),
            many => many
                .iter()
                .map(|id| format!("{id}\n\n{}\n", text(id)))
                .collect::<Vec<_>>()
                .join("\n────────\n\n"),
        }
    }
}

/// "465 components. …", the line under the list's title.
pub fn summary(count: usize) -> String {
    let noun = if count == 1 {
        "component"
    } else {
        "components"
    };
    format!(
        "{count} {noun}. The list is made from Spoolframe’s dependency tree: \
         Cargo.lock and the Flatpak manifest."
    )
}

/// The line beside the license toggle, for `choices` choices.
pub fn choice_note(choices: &[Vec<String>]) -> Option<&'static str> {
    match choices {
        [] => None,
        [one] if one.len() > 1 => Some("All of these apply; each text is below."),
        [_] => None,
        [_, _] => Some("Offered under either; both texts ship with the app."),
        _ => Some("Offered under any one of these; every text ships with the app."),
    }
}

/// The license filter chips, as on the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chip {
    All,
    Mit,
    Apache,
    Mpl,
    Bsd,
    Isc,
    Other,
}

impl Chip {
    pub const ALL: [Chip; 7] = [
        Chip::All,
        Chip::Mit,
        Chip::Apache,
        Chip::Mpl,
        Chip::Bsd,
        Chip::Isc,
        Chip::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Chip::All => "All",
            Chip::Mit => "MIT",
            Chip::Apache => "Apache-2.0",
            Chip::Mpl => "MPL-2.0",
            Chip::Bsd => "BSD",
            Chip::Isc => "ISC",
            Chip::Other => "Other",
        }
    }

    /// Whether `id` is of this chip's family.
    fn family(self, id: &str) -> bool {
        match self {
            Chip::All => true,
            Chip::Mit => id == "MIT" || id == "MIT-0",
            Chip::Apache => id.starts_with("Apache-2.0"),
            Chip::Mpl => id.starts_with("MPL-2.0"),
            Chip::Bsd => id.starts_with("BSD-") || id == "0BSD",
            Chip::Isc => id == "ISC",
            Chip::Other => !Chip::ALL[1..6].iter().any(|chip| chip.family(id)),
        }
    }

    /// A component is under a chip when any license it names is.
    pub fn matches(self, component: &Component) -> bool {
        self == Chip::All || component.ids().iter().any(|id| self.family(id))
    }
}

/// Whether the search field's `query` and the picked `chip` show
/// `component`: the query is looked for in its name and its license.
pub fn matches(component: &Component, query: &str, chip: Chip) -> bool {
    let query = query.trim().to_lowercase();
    chip.matches(component)
        && (query.is_empty()
            || component.name.to_lowercase().contains(&query)
            || component.license.to_lowercase().contains(&query))
}

/// The license ids an expression names, in order, each once. `X WITH Y`
/// is one id.
pub fn ids(expression: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for choice in choices(expression) {
        for id in choice {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// The ways to take a component under its expression: one entry per
/// choice, each the licenses that choice is under at once. `MIT OR
/// Apache-2.0` is two choices of one; `MIT AND ISC` one choice of two. A
/// choice that holds all of another's licenses and more is left out:
/// taking the other is the same and less. An expression that cannot be
/// read is one choice of every id in it.
pub fn choices(expression: &str) -> Vec<Vec<String>> {
    let tokens = tokens(expression);
    let mut parser = Parser {
        tokens: &tokens,
        at: 0,
    };
    let parsed = parser.or();
    let choices = match parsed {
        Some(choices) if parser.at == tokens.len() => choices,
        _ => {
            let mut all: Vec<String> = Vec::new();
            for token in &tokens {
                if let Token::Id(id) = token
                    && !all.contains(id)
                {
                    all.push(id.clone());
                }
            }
            return vec![all];
        }
    };
    // Left out: a choice that is another's licenses and more, or the same
    // as an earlier one.
    let mut kept: Vec<Vec<String>> = Vec::new();
    for (i, choice) in choices.iter().enumerate() {
        let covered = choices.iter().enumerate().any(|(j, other)| {
            let within = other.iter().all(|id| choice.contains(id));
            within && (other.len() < choice.len() || (other.len() == choice.len() && j < i))
        });
        if !covered {
            kept.push(choice.clone());
        }
    }
    kept
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Open,
    Close,
    And,
    Or,
    Id(String),
}

fn tokens(expression: &str) -> Vec<Token> {
    // Cargo's old `MIT/Apache-2.0` is OR.
    let spaced = expression
        .replace('(', " ( ")
        .replace(')', " ) ")
        .replace('/', " OR ");
    let words: Vec<&str> = spaced.split_whitespace().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let token = match words[i] {
            "(" => Token::Open,
            ")" => Token::Close,
            "AND" | "and" => Token::And,
            "OR" | "or" => Token::Or,
            id if words.get(i + 1).is_some_and(|w| *w == "WITH") && i + 2 < words.len() => {
                i += 2;
                Token::Id(format!("{id} WITH {}", words[i]))
            }
            id => Token::Id(id.to_owned()),
        };
        out.push(token);
        i += 1;
    }
    out
}

/// SPDX's grammar, AND binding tighter than OR, read into its choices.
struct Parser<'a> {
    tokens: &'a [Token],
    at: usize,
}

impl Parser<'_> {
    fn or(&mut self) -> Option<Vec<Vec<String>>> {
        let mut choices = self.and()?;
        while self.tokens.get(self.at) == Some(&Token::Or) {
            self.at += 1;
            choices.extend(self.and()?);
        }
        Some(choices)
    }

    fn and(&mut self) -> Option<Vec<Vec<String>>> {
        let mut choices = self.one()?;
        while self.tokens.get(self.at) == Some(&Token::And) {
            self.at += 1;
            let right = self.one()?;
            let mut joined = Vec::new();
            for left in &choices {
                for more in &right {
                    let mut both = left.clone();
                    for id in more {
                        if !both.contains(id) {
                            both.push(id.clone());
                        }
                    }
                    joined.push(both);
                }
            }
            choices = joined;
        }
        Some(choices)
    }

    fn one(&mut self) -> Option<Vec<Vec<String>>> {
        match self.tokens.get(self.at)? {
            Token::Open => {
                self.at += 1;
                let inner = self.or()?;
                (self.tokens.get(self.at) == Some(&Token::Close)).then(|| {
                    self.at += 1;
                    inner
                })
            }
            Token::Id(id) => {
                self.at += 1;
                Some(vec![vec![id.clone()]])
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(choices: &[&[&str]]) -> Vec<Vec<String>> {
        choices
            .iter()
            .map(|c| c.iter().map(|s| (*s).to_owned()).collect())
            .collect()
    }

    const SAMPLE: &str = r#"{"components":[
        {"name":"tokio","version":"1.53.1","group":"sdk","license":"MIT",
         "copyright":["Copyright (c) Tokio Contributors"],"texts":{"MIT":0}},
        {"name":"iroh","version":"1.3.0","group":"sdk","license":"MIT OR Apache-2.0",
         "copyright":[],"texts":{"MIT":0,"Apache-2.0":1}},
        {"name":"spoolframe-types","version":"0.7.0","group":"sdk","license":"MIT OR Apache-2.0",
         "copyright":["Copyright (c) A","Copyright (c) B"],"texts":{"MIT":0,"Apache-2.0":1}},
        {"name":"relm4","version":"0.11.0","group":"app","license":"Apache-2.0 OR MIT",
         "copyright":[],"texts":{"MIT":0,"Apache-2.0":1}},
        {"name":"mpris-server","version":"0.10.0","group":"app","license":"MPL-2.0",
         "copyright":[],"texts":{"MPL-2.0":2}},
        {"name":"libfido2","version":"1.17.0","group":"flatpak","license":"BSD-2-Clause",
         "copyright":[],"texts":{"BSD-2-Clause":3}}
    ],"texts":["MIT text\n","Apache text\n","MPL text\n","BSD text\n"]}"#;

    fn sample() -> Licenses {
        Licenses::parse(SAMPLE.as_bytes()).unwrap()
    }

    fn names(licenses: &Licenses, shown: &[(Group, Vec<usize>)]) -> Vec<(Group, Vec<String>)> {
        shown
            .iter()
            .map(|(group, rows)| {
                let names = rows
                    .iter()
                    .map(|&i| licenses.components()[i].name.clone())
                    .collect();
                (*group, names)
            })
            .collect()
    }

    #[test]
    fn either_of_two_licenses_is_two_choices() {
        assert_eq!(
            choices("MIT OR Apache-2.0"),
            strings(&[&["MIT"], &["Apache-2.0"]])
        );
    }

    #[test]
    fn both_of_two_licenses_is_one_choice_of_both() {
        assert_eq!(choices("MIT AND ISC"), strings(&[&["MIT", "ISC"]]));
    }

    #[test]
    fn cargos_old_slash_is_or() {
        assert_eq!(
            choices("MIT/Apache-2.0"),
            strings(&[&["MIT"], &["Apache-2.0"]])
        );
    }

    #[test]
    fn an_exception_stays_with_its_license() {
        assert_eq!(
            choices("Apache-2.0 WITH LLVM-exception OR MIT"),
            strings(&[&["Apache-2.0 WITH LLVM-exception"], &["MIT"]])
        );
    }

    #[test]
    fn and_binds_tighter_than_or() {
        assert_eq!(
            choices("MIT OR Apache-2.0 AND ISC"),
            strings(&[&["MIT"], &["Apache-2.0", "ISC"]])
        );
    }

    #[test]
    fn a_choice_with_more_than_another_is_left_out() {
        // ring's: ISC alone covers the ISC-or-Apache part.
        assert_eq!(choices("ISC AND (Apache-2.0 OR ISC)"), strings(&[&["ISC"]]));
    }

    #[test]
    fn aws_lc_sys_is_one_choice_of_four() {
        let expression = "ISC AND (Apache-2.0 OR ISC) AND Apache-2.0 AND MIT AND BSD-3-Clause \
            AND (Apache-2.0 OR ISC OR MIT) AND (Apache-2.0 OR ISC OR MIT-0)";
        assert_eq!(
            choices(expression),
            strings(&[&["ISC", "Apache-2.0", "MIT", "BSD-3-Clause"]])
        );
    }

    #[test]
    fn an_unreadable_expression_is_every_id_at_once() {
        assert_eq!(choices("MIT AND (ISC"), strings(&[&["MIT", "ISC"]]));
    }

    #[test]
    fn ids_lists_each_license_once() {
        assert_eq!(
            ids("(MIT OR Apache-2.0) AND Unicode-3.0"),
            vec!["MIT", "Unicode-3.0", "Apache-2.0"]
        );
    }

    #[test]
    fn the_sdk_lists_its_own_crates_first() {
        let licenses = sample();
        let shown = licenses.shown("", Chip::All);
        assert_eq!(
            names(&licenses, &shown),
            vec![
                (
                    Group::Sdk,
                    vec!["spoolframe-types".into(), "iroh".into(), "tokio".into()]
                ),
                (Group::App, vec!["mpris-server".into(), "relm4".into()]),
                (Group::Flatpak, vec!["libfido2".into()]),
            ]
        );
    }

    #[test]
    fn search_finds_by_name_whatever_the_case() {
        let licenses = sample();
        let shown = licenses.shown("IRO", Chip::All);
        assert_eq!(
            names(&licenses, &shown),
            vec![(Group::Sdk, vec!["iroh".into()])]
        );
    }

    #[test]
    fn search_finds_by_license_too() {
        let licenses = sample();
        let shown = licenses.shown("mpl", Chip::All);
        assert_eq!(
            names(&licenses, &shown),
            vec![(Group::App, vec!["mpris-server".into()])]
        );
    }

    #[test]
    fn a_chip_takes_any_license_of_an_or() {
        let licenses = sample();
        let shown = licenses.shown("", Chip::Apache);
        assert_eq!(
            names(&licenses, &shown),
            vec![
                (Group::Sdk, vec!["spoolframe-types".into(), "iroh".into()]),
                (Group::App, vec!["relm4".into()]),
            ]
        );
    }

    #[test]
    fn the_bsd_chip_takes_each_bsd_variant() {
        let licenses = sample();
        let shown = licenses.shown("", Chip::Bsd);
        assert_eq!(
            names(&licenses, &shown),
            vec![(Group::Flatpak, vec!["libfido2".into()])]
        );
    }

    #[test]
    fn other_is_a_license_no_other_chip_names() {
        let licenses =
            Licenses::parse(SAMPLE.replace("\"MPL-2.0\"", "\"Zlib\"").as_bytes()).unwrap();
        let shown = licenses.shown("", Chip::Other);
        assert_eq!(
            names(&licenses, &shown),
            vec![(Group::App, vec!["mpris-server".into()])]
        );
    }

    #[test]
    fn only_chips_some_component_has_are_offered() {
        assert_eq!(
            sample().chips(),
            vec![Chip::All, Chip::Mit, Chip::Apache, Chip::Mpl, Chip::Bsd]
        );
    }

    #[test]
    fn copyright_lines_go_one_a_line() {
        let licenses = sample();
        let types = &licenses.components()[0];
        assert_eq!(
            types.copyright_line().as_deref(),
            Some("Copyright (c) A\nCopyright (c) B")
        );
    }

    #[test]
    fn no_copyright_line_when_none_is_known() {
        let licenses = sample();
        let iroh = licenses
            .components()
            .iter()
            .find(|c| c.name == "iroh")
            .unwrap();
        assert_eq!(iroh.copyright_line(), None);
    }

    #[test]
    fn one_license_shows_its_text() {
        let licenses = sample();
        let iroh = &licenses.components()[1];
        assert_eq!(
            licenses.shown_text(iroh, &["Apache-2.0".to_owned()]),
            "Apache text\n"
        );
    }

    #[test]
    fn licenses_at_once_show_each_text_under_its_id() {
        let licenses = sample();
        let iroh = &licenses.components()[1];
        assert_eq!(
            licenses.shown_text(iroh, &["MIT".to_owned(), "Apache-2.0".to_owned()]),
            "MIT\n\nMIT text\n\n────────\n\nApache-2.0\n\nApache text\n"
        );
    }

    #[test]
    fn a_text_the_list_does_not_have_is_refused() {
        let broken = SAMPLE.replace("\"BSD-2-Clause\":3", "\"BSD-2-Clause\":9");
        assert!(matches!(
            Licenses::parse(broken.as_bytes()),
            Err(Error::MissingText(name)) if name == "libfido2"
        ));
    }

    #[test]
    fn the_note_says_either_for_two_choices() {
        assert_eq!(
            choice_note(&choices("MIT OR Apache-2.0")),
            Some("Offered under either; both texts ship with the app.")
        );
    }

    #[test]
    fn one_license_needs_no_note() {
        assert_eq!(choice_note(&choices("MIT")), None);
    }

    #[test]
    fn the_summary_counts_components() {
        assert!(summary(465).starts_with("465 components."));
    }

    /// The committed list, as the app reads it.
    fn committed() -> Licenses {
        Licenses::parse(include_bytes!("../data/licenses.json")).unwrap()
    }

    #[test]
    fn every_license_of_every_choice_has_its_text() {
        let licenses = committed();
        let missing: Vec<String> = licenses
            .components()
            .iter()
            .flat_map(|c| {
                c.ids()
                    .into_iter()
                    .filter(|id| licenses.text(c, id).is_none())
                    .map(|id| format!("{} {}: {id}", c.name, c.version))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(missing, Vec::<String>::new());
    }

    #[test]
    fn the_committed_list_has_every_group() {
        let licenses = committed();
        let groups: Vec<Group> = licenses
            .shown("", Chip::All)
            .iter()
            .map(|(g, _)| *g)
            .collect();
        assert_eq!(groups, Group::ALL.to_vec());
    }

    #[test]
    fn the_committed_list_has_this_app_under_its_own_license() {
        let licenses = committed();
        let app = licenses
            .components()
            .iter()
            .find(|c| c.name == "spoolframe-linux-app")
            .map(|c| (c.group, c.license.as_str(), c.version.as_str()));
        assert_eq!(
            app,
            Some((Group::App, "GPL-3.0-or-later", env!("CARGO_PKG_VERSION")))
        );
    }

    #[test]
    fn the_committed_list_names_the_palette_the_icons_and_the_fonts() {
        let licenses = committed();
        let found: Vec<(&str, &str)> = licenses
            .components()
            .iter()
            .filter(|c| c.group == Group::App)
            .filter(|c| {
                [
                    "Catppuccin",
                    "Lucide",
                    "Figtree",
                    "Outfit",
                    "JetBrains Mono",
                ]
                .contains(&c.name.as_str())
                    || c.name == "Bricolage Grotesque"
            })
            .map(|c| (c.name.as_str(), c.license.as_str()))
            .collect();
        assert_eq!(
            found,
            vec![
                ("Bricolage Grotesque", "OFL-1.1"),
                ("Catppuccin", "MIT"),
                ("Figtree", "OFL-1.1"),
                ("JetBrains Mono", "OFL-1.1"),
                ("Lucide", "ISC AND MIT"),
                ("Outfit", "OFL-1.1"),
            ]
        );
    }

    #[test]
    fn the_other_chip_shows_the_fonts() {
        let licenses = committed();
        let shown = licenses.shown("figtree", Chip::Other);
        assert_eq!(shown.len(), 1);
    }
}
