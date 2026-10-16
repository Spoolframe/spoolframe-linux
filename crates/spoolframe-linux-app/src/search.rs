//! Search (P3-09), laid out as the design's "Search (P3-09)" Linux
//! boards. One box searches everything or a scope (chips with counts);
//! results come as you type, the top match large, then a row per group
//! (shows, movies, people, artists, albums, tracks, episodes), each with
//! See all. A year at the end of the query becomes a removable token, and
//! when a scope finds nothing the screen says where the query does match.
//! Recent searches are kept per profile, on this computer.
//!
//! Typing waits about 250 ms; a newer query aborts the one in flight, so
//! an older answer never replaces a newer one, and the earlier results
//! stay, dimmed, until the newer arrive.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use serde::{Deserialize, Serialize};
use spoolframe_linux_core::backend::{
    ArtworkSize, BackendError, Person, PersonRole, Profile, SearchCounts, SearchGroup, SearchKind,
    SearchRequest, SearchResults, SearchScope, Session, TopResult,
};
use spoolframe_linux_core::format;
use spoolframe_linux_core::storage::DataStore;
use spoolframe_types::{ArtworkKind, Item};

use crate::views::{self, Cards, Facts};

/// How long typing pauses before it searches.
const TYPING_PAUSE: Duration = Duration::from_millis(250);

/// The most items a group shows in the results; See all asks for more.
const GROUP_LIMIT: u32 = 8;

/// What See all lists.
const SEE_ALL_LIMIT: u32 = 100;

/// Recent searches kept.
const RECENT: usize = 6;

/// The scopes, in chip order, with their names.
const SCOPES: [(SearchScope, &str); 5] = [
    (SearchScope::All, "All"),
    (SearchScope::Movies, "Movies"),
    (SearchScope::Shows, "Shows"),
    (SearchScope::Episodes, "Episodes"),
    (SearchScope::Music, "Music"),
];

/// What the screen needs from the app.
pub struct Needs {
    pub session: Arc<dyn Session>,
    pub cards: Rc<Cards>,
    /// Plays a movie, an episode or a track from the top result.
    pub play: Rc<dyn Fn(Item)>,
    /// Lists a person's items.
    pub open_person: Rc<dyn Fn(Person)>,
    /// Plays items shuffled: a person's, from their top result.
    pub shuffle: Rc<dyn Fn(Vec<Item>)>,
    /// The session ended (signed out elsewhere, or its password changed).
    pub signed_out: Rc<dyn Fn()>,
    /// The server's name, for "Nothing anywhere in … matches".
    pub server: String,
    /// The profile's display name and where its searches are kept.
    pub profile: String,
    pub recent: Option<RecentSearches>,
}

pub struct SearchPage {
    pub widget: gtk::Box,
    /// The field, All results and the year: the header bar's, while the
    /// page shows.
    pub head: gtk::Box,
    needs: Needs,
    /// The app's cards, remembering the search when one opens.
    cards: Rc<Cards>,
    entry: gtk::SearchEntry,
    back: gtk::Button,
    year: gtk::Button,
    year_label: gtk::Label,
    /// Turns in the field while a search is out.
    searching: gtk::Spinner,
    chips_row: gtk::Box,
    chips: Vec<(SearchScope, gtk::ToggleButton, gtk::Label)>,
    note: gtk::Label,
    see_all_head: gtk::Box,
    see_all_title: gtk::Label,
    see_all_note: gtk::Label,
    body: gtk::Stack,
    recent_list: gtk::ListBox,
    recent_clear: gtk::Button,
    recent_note: gtk::Label,
    recent_empty: gtk::Label,
    none: adw::StatusPage,
    elsewhere: gtk::Box,
    results: gtk::Box,
    state: RefCell<State>,
    /// The search in flight, aborted by a newer one.
    load: RefCell<Option<glib::JoinHandle<()>>>,
}

struct State {
    /// The year held as a token in the field: the query ends with it,
    /// the field shows only the words before it.
    year: Option<u16>,
    /// The field's text being set by the page, not typed: replacing it
    /// passes through an empty field, which is no reason to drop the year.
    setting_text: bool,
    scope: SearchScope,
    /// See all: one group.
    kind: Option<SearchKind>,
    query: String,
    /// The library search was opened from, for the scope's note.
    opened_from: Option<String>,
    /// The last answer, for the counts and the empty state.
    last: Option<SearchResults>,
    recent: Vec<Recent>,
    /// Chips being set by the screen, not the user.
    setting_chips: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            scope: SearchScope::All,
            kind: None,
            query: String::new(),
            opened_from: None,
            last: None,
            recent: Vec::new(),
            setting_chips: false,
            year: None,
            setting_text: false,
        }
    }
}

impl SearchPage {
    pub fn new(needs: Needs) -> Rc<Self> {
        let entry = gtk::SearchEntry::builder()
            .placeholder_text("Search everything")
            // About the board's 560 px when there is room, and narrower,
            // down to 16 characters, when the window is: a fixed width
            // kept the window from getting narrow.
            .width_chars(16)
            .max_width_chars(64)
            .css_classes(["search-field"])
            .search_delay(TYPING_PAUSE.as_millis() as u32)
            .build();
        let back = gtk::Button::builder()
            .icon_name(crate::icons::BACK)
            .label("All results")
            .css_classes(["flat"])
            .visible(false)
            .build();
        let year_label = gtk::Label::new(None);
        let year_body = gtk::Box::builder().spacing(4).build();
        year_body.append(&gtk::Image::from_icon_name(crate::icons::CALENDAR));
        year_body.append(&year_label);
        year_body.append(&gtk::Image::from_icon_name(crate::icons::CLOSE));
        let year = gtk::Button::builder()
            .child(&year_body)
            .valign(gtk::Align::Center)
            .css_classes(["search-year"])
            .tooltip_text("Remove year filter")
            .visible(false)
            .build();
        // In the header bar, as designed: All results, the field, the year.
        let head = gtk::Box::builder().spacing(10).build();
        // The field's frame holds the entry and the year token, as the
        // board draws it: the token sits inside the field.
        let searching = gtk::Spinner::builder()
            .width_request(16)
            .height_request(16)
            .visible(false)
            .valign(gtk::Align::Center)
            .build();
        entry.set_hexpand(true);
        let field = gtk::Box::builder()
            .spacing(6)
            .valign(gtk::Align::Center)
            .css_classes(["search-field-box"])
            .build();
        field.append(&entry);
        field.append(&searching);
        field.append(&year);
        head.append(&back);
        head.append(&field);

        let chips_row = gtk::Box::builder().spacing(8).build();
        let mut chips = Vec::new();
        let mut group: Option<gtk::ToggleButton> = None;
        for (scope, name) in SCOPES {
            let count = gtk::Label::builder()
                .css_classes(["search-chip-count"])
                .visible(false)
                .build();
            let label = gtk::Box::builder().spacing(6).build();
            label.append(&gtk::Label::new(Some(name)));
            label.append(&count);
            let chip = gtk::ToggleButton::builder()
                .child(&label)
                .css_classes(["search-chip"])
                .build();
            if let Some(first) = &group {
                chip.set_group(Some(first));
            } else {
                group = Some(chip.clone());
            }
            chips_row.append(&chip);
            chips.push((scope, chip, count));
        }
        let note = gtk::Label::builder()
            .css_classes(["dim-label", "caption"])
            .hexpand(true)
            .xalign(1.0)
            .build();
        chips_row.append(&note);

        let see_all_title = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["title-2", "see-all-title"])
            .build();
        let see_all_note = gtk::Label::builder()
            .valign(gtk::Align::Baseline)
            .css_classes(["dim-label", "small-text"])
            .build();
        let see_all_head = gtk::Box::builder().spacing(10).visible(false).build();
        see_all_head.append(&see_all_title);
        see_all_head.append(&see_all_note);

        // The page's states.
        let recent_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["plain-list"])
            .build();
        let recent_clear = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name(crate::icons::CLEAR_RECENT)
                    .label("Clear")
                    .build(),
            )
            .css_classes(["small-button"])
            .halign(gtk::Align::End)
            .hexpand(true)
            .build();
        let recent_head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        recent_head.append(
            &gtk::Label::builder()
                .label("Recent searches")
                .xalign(0.0)
                .css_classes(["title-3"])
                .build(),
        );
        recent_head.append(&recent_clear);
        let recent_note = gtk::Label::builder()
            .label(format!(
                "Kept on this computer for {}’s profile only.",
                needs.profile
            ))
            .xalign(0.0)
            .css_classes(["dim-label", "caption"])
            .build();
        let recent_empty = gtk::Label::builder()
            .label("No recent searches. What you search for on this computer shows up here.")
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();
        let recent = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .build();
        for widget in [
            recent_head.upcast_ref::<gtk::Widget>(),
            recent_list.upcast_ref(),
            recent_note.upcast_ref(),
            recent_empty.upcast_ref(),
        ] {
            recent.append(widget);
        }

        let loading = skeleton();

        let elsewhere = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(14)
            .build();
        let none = adw::StatusPage::builder()
            .icon_name(crate::icons::SEARCH)
            .css_classes(["small-icon", "search-none"])
            .child(&elsewhere)
            .build();

        let results = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(22)
            .build();

        let body = gtk::Stack::builder()
            .vhomogeneous(false)
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        body.add_named(
            &adw::Clamp::builder()
                .maximum_size(640)
                .halign(gtk::Align::Start)
                .child(&recent)
                .build(),
            Some("recent"),
        );
        body.add_named(&loading, Some("loading"));
        body.add_named(&none, Some("none"));
        body.add_named(&results, Some("results"));

        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .build();
        widget.append(&chips_row);
        widget.append(&see_all_head);
        widget.append(&body);

        let state = State {
            recent: needs.recent.as_ref().map(|r| r.load()).unwrap_or_default(),
            ..State::default()
        };
        let page = Rc::new_cyclic(|weak: &std::rc::Weak<Self>| {
            // Opening something from the results remembers the search.
            let cards = {
                let (weak, open) = (weak.clone(), needs.cards.open.clone());
                Rc::new(Cards {
                    open: Rc::new(move |item| {
                        if let Some(page) = weak.upgrade() {
                            page.remember();
                        }
                        open(item);
                    }),
                    art: needs.cards.art.clone(),
                    set_watched: None,
                    hide: None,
                    enqueue: needs.cards.enqueue.clone(),
                    add_to_playlist: needs.cards.add_to_playlist.clone(),
                    open_person: needs.cards.open_person.clone(),
                })
            };
            Self {
                widget,
                head,
                needs,
                cards,
                entry,
                back,
                year,
                year_label,
                searching,
                chips_row,
                chips,
                note,
                see_all_head,
                see_all_title,
                see_all_note,
                body,
                recent_list,
                recent_clear: recent_clear.clone(),
                recent_note,
                recent_empty,
                none,
                elsewhere,
                results,
                state: RefCell::new(state),
                load: RefCell::new(None),
            }
        });
        page.connect(&recent_clear);
        page.show_recent();
        page
    }

    fn connect(self: &Rc<Self>, recent_clear: &gtk::Button) {
        let weak = Rc::downgrade(self);
        self.entry.connect_search_changed(move |entry| {
            if let Some(page) = weak.upgrade() {
                page.typed(entry.text().trim());
            }
        });
        let weak = Rc::downgrade(self);
        self.entry.connect_activate(move |_| {
            if let Some(page) = weak.upgrade() {
                page.remember();
            }
        });
        for (scope, chip, _) in &self.chips {
            let (weak, scope) = (Rc::downgrade(self), *scope);
            chip.connect_toggled(move |chip| {
                if let Some(page) = weak.upgrade()
                    && chip.is_active()
                    && !page.state.borrow().setting_chips
                {
                    page.pick_scope(scope);
                }
            });
        }
        let weak = Rc::downgrade(self);
        self.year.connect_clicked(move |_| {
            if let Some(page) = weak.upgrade() {
                page.drop_year();
            }
        });
        let weak = Rc::downgrade(self);
        self.back.connect_clicked(move |_| {
            if let Some(page) = weak.upgrade() {
                page.state.borrow_mut().kind = None;
                page.run();
            }
        });
        let weak = Rc::downgrade(self);
        recent_clear.connect_clicked(move |_| {
            if let Some(page) = weak.upgrade() {
                page.set_recent(Vec::new());
            }
        });
    }

    /// Shows the page in `scope`: the default from where search was
    /// opened (a library's kind), with its name for the note.
    pub fn open(self: &Rc<Self>, scope: SearchScope, opened_from: Option<String>) {
        {
            let mut state = self.state.borrow_mut();
            state.scope = scope;
            state.kind = None;
            state.opened_from = opened_from;
        }
        self.show_chips();
        if self.state.borrow().query.is_empty() {
            self.show_recent();
        } else {
            self.run();
        }
        self.entry.grab_focus();
    }

    fn typed(self: &Rc<Self>, text: &str) {
        {
            let mut state = self.state.borrow_mut();
            if state.setting_text {
                return;
            }
            // An empty field lets go of the year too.
            if text.is_empty() {
                state.year = None;
            }
            let query = match state.year {
                Some(year) => format!("{text} {year}"),
                None => text.to_owned(),
            };
            if state.query == query {
                return;
            }
            state.query = query;
            state.kind = None;
        }
        self.show_year();
        self.run();
    }

    /// The token shows while a year is held.
    fn show_year(&self) {
        let year = self.state.borrow().year;
        if let Some(year) = year {
            self.year_label.set_label(&year.to_string());
        }
        self.year.set_visible(year.is_some());
    }

    fn pick_scope(self: &Rc<Self>, scope: SearchScope) {
        {
            let mut state = self.state.borrow_mut();
            state.scope = scope;
            state.kind = None;
            state.opened_from = None;
        }
        self.show_chips();
        self.run();
    }

    /// Searches every year: the query without its year.
    fn drop_year(self: &Rc<Self>) {
        let words = self.entry.text().trim().to_owned();
        {
            let mut state = self.state.borrow_mut();
            state.year = None;
            state.query = words;
            state.kind = None;
        }
        self.show_year();
        self.run();
    }

    /// Asks the server for the query as it stands; the earlier results
    /// stay, dimmed, until the answer comes.
    fn run(self: &Rc<Self>) {
        if let Some(load) = self.load.take() {
            load.abort();
        }
        let (request, seeing_all) = {
            let state = self.state.borrow();
            if state.query.is_empty() {
                drop(state);
                return self.show_recent();
            }
            let request = SearchRequest {
                query: state.query.clone(),
                limit: if state.kind.is_some() {
                    SEE_ALL_LIMIT
                } else {
                    GROUP_LIMIT
                },
                scope: state.scope,
                kind: state.kind,
                library: None,
            };
            (request, state.kind.is_some())
        };
        self.show_see_all_head(seeing_all, None);
        self.searching.set_visible(true);
        self.searching.set_spinning(true);
        if self.body.visible_child_name().as_deref() == Some("results") {
            self.results.set_opacity(0.45);
        } else {
            self.body.set_visible_child_name("loading");
        }
        for (_, _, count) in &self.chips {
            count.set_visible(false);
        }
        let session = self.needs.session.clone();
        let asked = relm4::spawn(async move { session.search(&request).await });
        let weak = Rc::downgrade(self);
        self.load.replace(Some(glib::spawn_future_local(async move {
            let Ok(answer) = asked.await else { return };
            let Some(page) = weak.upgrade() else { return };
            page.load.take();
            page.searching.set_visible(false);
            page.searching.set_spinning(false);
            match answer {
                Ok(results) => page.show(results),
                // Not a failed search: the app goes back to "Who's watching".
                Err(BackendError::SignInRequired) => (page.needs.signed_out)(),
                Err(e) => page.failed(&e),
            }
        })));
    }

    fn show(self: &Rc<Self>, results: SearchResults) {
        self.results.set_opacity(1.0);
        let (scope, kind, query) = {
            let state = self.state.borrow();
            (state.scope, state.kind, state.query.clone())
        };
        for (chip_scope, _, count) in &self.chips {
            match results.counts.of(*chip_scope) {
                Some(n) => {
                    count.set_label(&n.to_string());
                    count.set_visible(true);
                }
                None => count.set_visible(false),
            }
        }
        // A year the server read off the end of the query becomes the
        // token: the field keeps the words, the query keeps the year.
        let held = self.state.borrow().year;
        if held.is_none()
            && let Some(year) = results.year.filter(|y| query.ends_with(&y.to_string()))
        {
            self.state.borrow_mut().year = Some(year);
            let words = without_year(&query, year);
            self.state.borrow_mut().setting_text = true;
            self.entry.set_text(&words);
            self.state.borrow_mut().setting_text = false;
            self.entry.set_position(-1);
        }
        self.show_year();
        let empty = results.top.is_none() && results.groups.iter().all(group_is_empty);
        if empty && kind.is_none() {
            self.show_none(scope, &query, &results.counts);
        } else {
            self.fill(&results, scope, kind, &query);
            self.body.set_visible_child_name("results");
        }
        self.state.borrow_mut().last = Some(results);
    }

    fn fill(
        self: &Rc<Self>,
        results: &SearchResults,
        scope: SearchScope,
        kind: Option<SearchKind>,
        query: &str,
    ) {
        clear(&self.results);
        if let Some(kind) = kind {
            let count = results.groups.first().map_or(0, |g| {
                g.total
                    .map_or(g.items.len() + g.people.len(), |t| t as usize)
            });
            self.show_see_all_head(true, Some((kind, query, count, scope)));
        }
        if let Some(year) = results.year
            && kind.is_none()
        {
            self.results
                .append(&self.year_note(year, &results.without_year));
        }
        match &results.top {
            Some(TopResult::Item(top)) => self.results.append(&self.top(top)),
            Some(TopResult::Person(person)) => self.results.append(&self.top_person(person)),
            None => {}
        }
        for group in &results.groups {
            self.results.append(&self.group(group, kind.is_some()));
        }
    }

    /// "Only 2021 · also matching without the year: …", and searching
    /// without it.
    fn year_note(self: &Rc<Self>, year: u16, without: &[Item]) -> gtk::Widget {
        let others = if without.is_empty() {
            "nothing else".to_owned()
        } else {
            without
                .iter()
                .map(|item| {
                    let facts = Facts::of(item);
                    match &facts.detail {
                        Some(year) if year.len() == 4 => format!("{} ({year})", facts.title),
                        _ => facts.title,
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let note = gtk::Box::builder()
            .spacing(10)
            .css_classes(["search-year-note"])
            .build();
        note.append(&gtk::Image::from_icon_name(crate::icons::CALENDAR));
        note.append(
            &gtk::Label::builder()
                .label(format!(
                    "Only {year} · also matching without the year: {others}"
                ))
                .wrap(true)
                .xalign(0.0)
                .build(),
        );
        let button = gtk::Button::builder()
            .label(format!("Search without {year}"))
            .css_classes(["flat", "text-link"])
            .build();
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(page) = weak.upgrade() {
                page.drop_year();
            }
        });
        note.append(&button);
        note.upcast()
    }

    /// The top result, large: what it is, and what to do with it.
    fn top(self: &Rc<Self>, item: &Item) -> gtk::Widget {
        let facts = Facts::of(item);
        let (shape, size, kinds) = art_of(item, true);
        let tile = views::tile(item.id(), &facts.title, shape, size);
        self.cards
            .art(&tile, facts.artwork, kinds, ArtworkSize::Small);
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .valign(gtk::Align::Center)
            .build();
        text.append(&caption(
            &format!("Top result · {}", kind_name(item)).to_uppercase(),
            &["search-kicker"],
        ));
        text.append(&caption(&facts.title, &["search-top-title"]));
        if let Some(sub) = top_detail(item, &facts) {
            text.append(&caption(&sub, &["dim-label"]));
        }
        let actions = gtk::Box::builder().spacing(8).margin_top(6).build();
        if matches!(item, Item::Movie(_) | Item::Episode(_) | Item::Track(_)) {
            let play = gtk::Button::builder()
                .child(
                    &adw::ButtonContent::builder()
                        .icon_name(crate::icons::PLAY)
                        .label("Play")
                        .build(),
                )
                .css_classes(["small-button", "suggested-action"])
                .build();
            let (weak, item) = (Rc::downgrade(self), item.clone());
            play.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.remember();
                    (page.needs.play)(item.clone());
                }
            });
            actions.append(&play);
        }
        // A show offers its episode up next, once its seasons are in:
        // "Resume S2 · E4", or "Play S1 · E1".
        if let Item::Show(show) = item {
            let play = gtk::Button::builder()
                .visible(false)
                .css_classes(["small-button", "suggested-action"])
                .build();
            actions.append(&play);
            let (session, show_id) = (self.needs.session.clone(), show.id.clone());
            let asked = relm4::spawn(async move {
                let mut seasons = Vec::new();
                for child in session.item(&show_id).await?.1 {
                    if let Item::Season(season) = child {
                        let episodes = session.item(&season.id).await?.1;
                        seasons.push((season, episodes));
                    }
                }
                Ok::<_, BackendError>(seasons)
            });
            let (weak, play_) = (Rc::downgrade(self), play.downgrade());
            glib::spawn_future_local(async move {
                let Ok(Ok(seasons)) = asked.await else { return };
                let next = crate::pages::up_next(&seasons)
                    .or_else(|| seasons.iter().flat_map(|(_, e)| e.first()).next())
                    .cloned();
                let (Some(next), Some(play)) = (next, play_.upgrade()) else {
                    return;
                };
                let word = if Facts::of(&next).position().is_some() {
                    "Resume"
                } else {
                    "Play"
                };
                play.set_child(Some(
                    &adw::ButtonContent::builder()
                        .icon_name(crate::icons::PLAY)
                        .label(format!("{word} {}", crate::pages::episode_code(&next)))
                        .build(),
                ));
                play.set_visible(true);
                play.connect_clicked(move |_| {
                    if let Some(page) = weak.upgrade() {
                        page.remember();
                        (page.needs.play)(next.clone());
                    }
                });
            });
        }
        let open = gtk::Button::builder()
            .label(open_label(item))
            .css_classes(["small-button"])
            .build();
        let (open_item, item) = (self.cards.open.clone(), item.clone());
        open.connect_clicked(move |_| open_item(item.clone()));
        actions.append(&open);
        text.append(&actions);
        let top = gtk::Box::builder()
            .spacing(20)
            .css_classes(["search-top"])
            .build();
        top.append(&tile);
        top.append(&text);
        top.upcast()
    }

    /// A person as the top result: their initials in a large circle, what
    /// they did, and their items to shuffle or list.
    fn top_person(self: &Rc<Self>, person: &Person) -> gtk::Widget {
        let circle = gtk::Label::builder()
            .label(views::initials(&person.name))
            .width_request(TOP_PERSON)
            .height_request(TOP_PERSON)
            .valign(gtk::Align::Center)
            .css_classes(["search-person", "search-top-person"])
            .build();
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .valign(gtk::Align::Center)
            .build();
        text.append(&caption("TOP RESULT · PERSON", &["search-kicker"]));
        text.append(&caption(&person.name, &["search-top-title"]));
        text.append(&caption(&person_detail(person), &["dim-label"]));
        let actions = gtk::Box::builder().spacing(8).margin_top(6).build();
        let shuffle = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name(crate::icons::SHUFFLE)
                    .label("Shuffle their items")
                    .build(),
            )
            .css_classes(["small-button", "suggested-action"])
            .build();
        {
            let (weak, id) = (Rc::downgrade(self), person.id.clone());
            shuffle.connect_clicked(move |button| {
                let Some(page) = weak.upgrade() else { return };
                page.remember();
                button.set_sensitive(false);
                let session = page.needs.session.clone();
                let id = id.clone();
                let asked =
                    relm4::spawn(async move { session.person_items(&id, 0, PERSON_SHUFFLE).await });
                let (play, button) = (page.needs.shuffle.clone(), button.downgrade());
                glib::spawn_future_local(async move {
                    if let Ok(Ok((items, _))) = asked.await {
                        let items = playable(items);
                        if !items.is_empty() {
                            play(items);
                        }
                    }
                    if let Some(button) = button.upgrade() {
                        button.set_sensitive(true);
                    }
                });
            });
        }
        actions.append(&shuffle);
        let open = gtk::Button::builder()
            .label("Show their items")
            .css_classes(["small-button"])
            .build();
        {
            let (weak, person) = (Rc::downgrade(self), person.clone());
            open.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.remember();
                    (page.needs.open_person)(person.clone());
                }
            });
        }
        actions.append(&open);
        text.append(&actions);
        let top = gtk::Box::builder()
            .spacing(20)
            .css_classes(["search-top"])
            .build();
        top.append(&circle);
        top.append(&text);
        top.upcast()
    }

    /// A group: its name, a note, See all, and its items in the shape the
    /// design gives their kind.
    fn group(self: &Rc<Self>, group: &SearchGroup, seeing_all: bool) -> gtk::Widget {
        // As many as the board shows of each kind; See all has the rest.
        let everything = self.state.borrow().scope == SearchScope::All;
        let limit = if seeing_all {
            usize::MAX
        } else {
            kind_limit(group.kind, everything)
        };
        let items: Vec<Item> = group.items.iter().take(limit).cloned().collect();
        let people: Vec<Person> = group.people.iter().take(limit).cloned().collect();
        let cut = items.len() < group.items.len() || people.len() < group.people.len();
        // In line with cards in a row, which sit inside its side room.
        let in_row =
            !seeing_all && !matches!(group.kind, SearchKind::Tracks | SearchKind::Episodes);
        let head = gtk::Box::builder()
            .spacing(10)
            .margin_start(if in_row { views::LIFT_SIDE } else { 0 })
            .build();
        head.append(&caption(kind_label(group.kind), &["title-3"]));
        if group.hidden > 0 {
            head.append(&caption(
                &hidden_note(group.hidden),
                &["dim-label", "small-text"],
            ));
        }
        if (group.more || cut) && !seeing_all {
            let see_all = gtk::Button::builder()
                .label(see_all_label(group.total))
                .css_classes(["flat", "text-link", "see-all"])
                .halign(gtk::Align::End)
                .hexpand(true)
                .build();
            let (weak, kind) = (Rc::downgrade(self), group.kind);
            see_all.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.state.borrow_mut().kind = Some(kind);
                    page.run();
                }
            });
            head.append(&see_all);
        }
        let section = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .build();
        section.append(&head);
        let body: gtk::Widget = match group.kind {
            SearchKind::People => self.cards_of(
                people.iter().map(|p| self.person_card(p)).collect(),
                seeing_all,
            ),
            SearchKind::Shows | SearchKind::Movies | SearchKind::Artists | SearchKind::Albums => {
                self.cards_of(
                    items.iter().map(|i| self.small_card(i)).collect(),
                    seeing_all,
                )
            }
            // Rows in columns: three for episodes among everything, two
            // otherwise.
            SearchKind::Tracks | SearchKind::Episodes => self.rows_of(
                &items,
                if group.kind == SearchKind::Episodes && !seeing_all {
                    3
                } else {
                    2
                },
            ),
        };
        if group.items.is_empty() && group.people.is_empty() {
            section.append(&caption("Nothing here.", &["dim-label"]));
        } else {
            section.append(&body);
        }
        section.upcast()
    }

    /// Cards in a row that scrolls, or wrapping for See all.
    fn cards_of(&self, cards: Vec<gtk::Widget>, seeing_all: bool) -> gtk::Widget {
        if seeing_all {
            let flow = gtk::FlowBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .column_spacing(14)
                .row_spacing(14)
                .max_children_per_line(12)
                .halign(gtk::Align::Start)
                .build();
            for card in cards {
                flow.append(&card);
            }
            return flow.upcast();
        }
        let row = gtk::Box::builder().spacing(14).build();
        views::lift_room(&row);
        for card in cards {
            row.append(&card);
        }
        gtk::ScrolledWindow::builder()
            .child(&row)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .build()
            .upcast()
    }

    /// An album (square) or an artist (round), with its name below.
    fn small_card(&self, item: &Item) -> gtk::Widget {
        let facts = Facts::of(item);
        let (shape, size, kinds) = art_of(item, false);
        let tile = views::tile(item.id(), &facts.title, shape, size);
        self.cards
            .art(&tile, facts.artwork, kinds, ArtworkSize::Small);
        let below = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .build();
        below.append(&views::caption_label(&facts.title, &["heading"]));
        let sub = match item {
            Item::Artist(_) => Some("Artist".to_owned()),
            _ => facts.detail.clone(),
        };
        if let Some(sub) = sub {
            below.append(&views::caption_label(&sub, &["dim-label", "caption"]));
        }
        views::card(item, &tile, &below, size.0, &self.cards.open)
    }

    /// A person: their initials in a circle (people have no artwork), what
    /// they did, and how many items.
    fn person_card(self: &Rc<Self>, person: &Person) -> gtk::Widget {
        let circle = gtk::Label::builder()
            .label(views::initials(&person.name))
            .width_request(120)
            .height_request(120)
            .css_classes(["search-person"])
            .build();
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        body.append(&circle);
        body.append(&views::caption_label(&person.name, &["heading"]));
        body.append(&views::caption_label(
            &person_detail(person),
            &["dim-label", "caption"],
        ));
        let button = gtk::Button::builder()
            .child(&body)
            .width_request(120)
            .valign(gtk::Align::Start)
            .css_classes(["card-button", "flat", "lift"])
            .build();
        let (weak, person) = (Rc::downgrade(self), person.clone());
        button.connect_clicked(move |_| {
            if let Some(page) = weak.upgrade() {
                page.remember();
                (page.needs.open_person)(person.clone());
            }
        });
        button.upcast()
    }

    /// Tracks and episodes, as rows: what, where from, how long.
    fn rows_of(&self, items: &[Item], columns: usize) -> gtk::Widget {
        let lists: Vec<gtk::ListBox> = (0..columns)
            .map(|_| {
                gtk::ListBox::builder()
                    .selection_mode(gtk::SelectionMode::None)
                    .valign(gtk::Align::Start)
                    .hexpand(true)
                    .css_classes(["plain-list"])
                    .build()
            })
            .collect();
        let grid = gtk::Box::builder().spacing(18).homogeneous(true).build();
        for list in &lists {
            grid.append(list);
        }
        for (index, item) in items.iter().enumerate() {
            let list = &lists[index % columns];
            let facts = Facts::of(item);
            let row = adw::ActionRow::builder()
                .use_markup(false)
                .activatable(true)
                .build();
            row.set_title(&facts.title);
            row.set_subtitle(&facts.detail.clone().unwrap_or_default());
            if let Some(length) = facts.length {
                row.add_suffix(
                    &gtk::Label::builder()
                        .label(format::length(length))
                        .css_classes(["dim-label", "monospace", "caption"])
                        .build(),
                );
            }
            // An episode leads with its still, as on the board.
            if let Item::Episode(episode) = item {
                let still = views::tile(&episode.id, "", "thumb", (78, 44));
                self.cards.art(
                    &still,
                    &episode.artwork,
                    &[ArtworkKind::Thumb, ArtworkKind::Background],
                    ArtworkSize::Small,
                );
                still.set_valign(gtk::Align::Center);
                row.add_prefix(&still);
            }
            // A track leads with its album's cover, square.
            if let Item::Track(track) = item {
                let cover = views::tile(&track.album_id, "", "thumb", (44, 44));
                self.cards.art(
                    &cover,
                    &track.artwork,
                    &[ArtworkKind::Poster, ArtworkKind::Thumb],
                    ArtworkSize::Small,
                );
                cover.set_valign(gtk::Align::Center);
                row.add_prefix(&cover);
            }
            // An episode's menu goes to its show or season; a track's, as
            // on the album page, queues it too; either adds to a playlist.
            let mut groups = Vec::new();
            if let Item::Episode(episode) = item {
                groups.push(crate::views::go_to_entries(episode, &self.cards.open));
            }
            if let (Item::Track(track), Some(enqueue)) = (item, &self.cards.enqueue) {
                let (enqueue, queued) = (enqueue.clone(), item.clone());
                groups.push(crate::views::enqueue_entries(move |at| {
                    enqueue(queued.clone(), at)
                }));
                groups.push(crate::views::track_go_to_entries(
                    track,
                    &self.cards.open,
                    true,
                ));
            }
            groups.extend(
                crate::views::add_entry(item, self.cards.add_to_playlist.as_ref()).map(|e| vec![e]),
            );
            if !groups.is_empty() {
                crate::views::context_menu(row.upcast_ref(), &crate::views::menu_popover(groups));
            }
            let (open, item) = (self.cards.open.clone(), item.clone());
            row.connect_activated(move |_| open(item.clone()));
            list.append(&row);
        }
        grid.upcast()
    }

    /// Nothing in this scope: where else the query matches, or nowhere.
    fn show_none(self: &Rc<Self>, scope: SearchScope, query: &str, counts: &SearchCounts) {
        self.none.set_title(&empty_title(scope, query));
        clear(&self.elsewhere);
        let places = elsewhere(counts, scope);
        if places.is_empty() {
            self.none
                .set_description(Some(&crate::markup::plain(&format!(
                    "Nothing anywhere in {} matches. Check the spelling, or try fewer words.",
                    self.needs.server
                ))));
        } else {
            self.none
                .set_description(Some("It does match in other places:"));
            let chips = gtk::Box::builder()
                .spacing(8)
                .halign(gtk::Align::Center)
                .build();
            for (other, count) in places {
                let chip = gtk::Button::builder()
                    .child(
                        &gtk::Label::builder()
                            .use_markup(true)
                            .label(crate::markup::markup!(
                                "{} <span fgalpha=\"65%\" weight=\"500\">{}</span>",
                                scope_name(other),
                                count
                            ))
                            .build(),
                    )
                    .css_classes(["elsewhere-chip"])
                    .build();
                let weak = Rc::downgrade(self);
                chip.connect_clicked(move |_| {
                    if let Some(page) = weak.upgrade() {
                        page.pick_scope(other);
                    }
                });
                chips.append(&chip);
            }
            self.elsewhere.append(&chips);
            let everything = gtk::Button::builder()
                .label("Search everything")
                .halign(gtk::Align::Center)
                .css_classes(["small-button", "suggested-action"])
                .build();
            let weak = Rc::downgrade(self);
            everything.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.pick_scope(SearchScope::All);
                }
            });
            self.elsewhere.append(&everything);
        }
        self.body.set_visible_child_name("none");
    }

    fn failed(&self, error: &BackendError) {
        self.results.set_opacity(1.0);
        self.none.set_title("Search failed");
        self.none
            .set_description(Some(&crate::markup::plain(&error.to_string())));
        clear(&self.elsewhere);
        self.body.set_visible_child_name("none");
    }

    /// The chips as the state has them, and the box's placeholder.
    fn show_chips(&self) {
        let state = self.state.borrow();
        let scope = state.scope;
        let note = state
            .opened_from
            .as_ref()
            .map(|from| format!("Opened from {from} · All widens to everything"));
        drop(state);
        self.state.borrow_mut().setting_chips = true;
        for (chip_scope, chip, _) in &self.chips {
            chip.set_active(*chip_scope == scope);
        }
        self.state.borrow_mut().setting_chips = false;
        self.entry.set_placeholder_text(Some(&match scope {
            SearchScope::All => "Search everything".to_owned(),
            other => format!("Search {}", scope_name(other).to_lowercase()),
        }));
        self.note.set_label(note.as_deref().unwrap_or_default());
        self.note.set_visible(note.is_some());
    }

    /// See all's head instead of the chips, with "Movies matching “dune”"
    /// and how many there are once known.
    fn show_see_all_head(&self, on: bool, known: Option<(SearchKind, &str, usize, SearchScope)>) {
        self.chips_row.set_visible(!on);
        self.back.set_visible(on);
        self.see_all_head.set_visible(on);
        if let Some((kind, query, count, scope)) = known {
            self.see_all_title.set_label(&see_all_title(kind, query));
            self.see_all_note.set_label(&match scope {
                SearchScope::All => format!("{count} results"),
                other => format!("{count} results in {}", scope_name(other)),
            });
        }
    }

    fn show_recent(self: &Rc<Self>) {
        if let Some(load) = self.load.take() {
            load.abort();
        }
        self.results.set_opacity(1.0);
        self.year.set_visible(false);
        self.show_see_all_head(false, None);
        for (_, _, count) in &self.chips {
            count.set_visible(false);
        }
        clear(&self.recent_list);
        let recent = self.state.borrow().recent.clone();
        let some = !recent.is_empty();
        self.recent_list.set_visible(some);
        self.recent_note.set_visible(some);
        self.recent_empty.set_visible(!some);
        self.recent_clear.set_visible(some);
        for search in recent {
            let row = adw::ActionRow::builder()
                .use_markup(false)
                .activatable(true)
                .css_classes(["recent-row"])
                .build();
            row.set_title(&search.query);
            row.add_prefix(
                &gtk::Image::builder()
                    .icon_name(crate::icons::HISTORY)
                    .css_classes(["dim-label"])
                    .build(),
            );
            // The scope beside the query, not under it.
            if search.scope != SearchScope::All {
                row.add_suffix(
                    &gtk::Label::builder()
                        .label(format!("in {}", scope_name(search.scope)))
                        .css_classes(["dim-label", "caption"])
                        .build(),
                );
            }
            let remove = gtk::Button::builder()
                .icon_name(crate::icons::CLOSE)
                .tooltip_text("Remove from recent searches")
                .valign(gtk::Align::Center)
                .css_classes(["flat", "circular", "dim-label"])
                .build();
            {
                let (weak, query) = (Rc::downgrade(self), search.query.clone());
                remove.connect_clicked(move |_| {
                    if let Some(page) = weak.upgrade() {
                        let mut recent = page.state.borrow().recent.clone();
                        recent.retain(|r| r.query != query);
                        page.set_recent(recent);
                    }
                });
            }
            row.add_suffix(&remove);
            let weak = Rc::downgrade(self);
            row.connect_activated(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.state.borrow_mut().scope = search.scope;
                    page.show_chips();
                    page.entry.set_text(&search.query);
                    page.entry.set_position(-1);
                }
            });
            self.recent_list.append(&row);
        }
        self.body.set_visible_child_name("recent");
    }

    /// Keeps the search as it stands among the recent ones.
    fn remember(self: &Rc<Self>) {
        let (query, scope, recent) = {
            let state = self.state.borrow();
            (state.query.clone(), state.scope, state.recent.clone())
        };
        if query.is_empty() {
            return;
        }
        self.set_recent(remembered(recent, &query, scope));
    }

    fn set_recent(self: &Rc<Self>, recent: Vec<Recent>) {
        if let Some(store) = &self.needs.recent {
            store.save(&recent);
        }
        let showing = self.body.visible_child_name().as_deref() == Some("recent");
        self.state.borrow_mut().recent = recent;
        if showing {
            self.show_recent();
        }
    }
}

/// A recent search, with the scope it was made in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recent {
    pub query: String,
    pub scope: SearchScope,
}

/// A profile's recent searches: `searches/<hash>.json` in the app's data
/// directory, named by a hash of the server and the username.
pub struct RecentSearches {
    store: DataStore,
    key: String,
}

impl RecentSearches {
    pub fn for_profile(profile: &Profile) -> Option<Self> {
        let who = format!("{}/{}", profile.server.0, profile.username);
        let hash = glib::compute_checksum_for_string(glib::ChecksumType::Sha256, &who)?;
        Some(Self {
            store: DataStore::new(DataStore::default_dir()?),
            key: format!("searches/{hash}.json"),
        })
    }

    fn load(&self) -> Vec<Recent> {
        match self.store.get(&self.key) {
            Ok(Some(bytes)) => serde_json::from_slice(&bytes)
                .inspect_err(|e| tracing::warn!(error = %e, "cannot read the recent searches"))
                .unwrap_or_default(),
            Ok(None) => Vec::new(),
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the recent searches");
                Vec::new()
            }
        }
    }

    fn save(&self, recent: &[Recent]) {
        let saved = serde_json::to_vec(recent)
            .map_err(|e| e.to_string())
            .and_then(|bytes| {
                self.store
                    .set(&self.key, Some(&bytes))
                    .map_err(|e| e.to_string())
            });
        if let Err(e) = saved {
            tracing::warn!(error = %e, "cannot keep the recent searches");
        }
    }
}

/// `recent` with `query` first, once, and no more than [`RECENT`].
fn remembered(recent: Vec<Recent>, query: &str, scope: SearchScope) -> Vec<Recent> {
    std::iter::once(Recent {
        query: query.to_owned(),
        scope,
    })
    .chain(recent.into_iter().filter(|r| r.query != query))
    .take(RECENT)
    .collect()
}

/// The scopes other than `scope` (and All) that do match, with counts.
fn elsewhere(counts: &SearchCounts, scope: SearchScope) -> Vec<(SearchScope, u32)> {
    SCOPES
        .iter()
        .map(|(s, _)| *s)
        .filter(|s| *s != scope && *s != SearchScope::All)
        .filter_map(|s| counts.of(s).filter(|n| *n > 0).map(|n| (s, n)))
        .collect()
}

fn empty_title(scope: SearchScope, query: &str) -> String {
    let what = match scope {
        SearchScope::All => "results".to_owned(),
        other => scope_name(other).to_lowercase(),
    };
    format!("No {what} match “{query}”")
}

fn see_all_title(kind: SearchKind, query: &str) -> String {
    format!("{} matching “{query}”", kind_label(kind))
}

fn hidden_note(hidden: u32) -> String {
    match hidden {
        1 => "1 more under its show".to_owned(),
        n => format!("{n} more under their shows"),
    }
}

/// The query without the year at its end.
fn without_year(query: &str, year: u16) -> String {
    let year = year.to_string();
    query
        .trim_end()
        .strip_suffix(&year)
        .unwrap_or(query)
        .trim_end()
        .to_owned()
}

fn person_detail(person: &Person) -> String {
    let role = match person.role {
        PersonRole::Actor => "Actor",
        PersonRole::Director => "Director",
    };
    match person.count {
        Some(1) => format!("{role} · 1 item"),
        Some(n) => format!("{role} · {n} items"),
        None => role.to_owned(),
    }
}

fn scope_name(scope: SearchScope) -> &'static str {
    SCOPES
        .iter()
        .find(|(s, _)| *s == scope)
        .map_or("All", |(_, name)| name)
}

fn kind_label(kind: SearchKind) -> &'static str {
    match kind {
        SearchKind::Shows => "Shows",
        SearchKind::Movies => "Movies",
        SearchKind::People => "People",
        SearchKind::Artists => "Artists",
        SearchKind::Albums => "Albums",
        SearchKind::Tracks => "Tracks",
        SearchKind::Episodes => "Episodes",
    }
}

fn kind_name(item: &Item) -> &'static str {
    match item {
        Item::Movie(_) => "Movie",
        Item::Show(_) => "Show",
        Item::Season(_) => "Season",
        Item::Episode(_) => "Episode",
        Item::Artist(_) => "Artist",
        Item::Album(_) => "Album",
        Item::Track(_) => "Track",
    }
}

fn open_label(item: &Item) -> &'static str {
    match item {
        Item::Show(_) => "Go to show",
        Item::Movie(_) => "Details",
        _ => "Open",
    }
}

/// The top result's line: the year and length for a movie, the detail
/// otherwise.
/// The size of a person's circle as the top result, as the board draws it.
const TOP_PERSON: i32 = 146;

/// The most of a person's items shuffled from their top result.
const PERSON_SHUFFLE: u32 = 200;

/// "See all 23", or "See all" where the server cannot count them.
fn see_all_label(total: Option<u32>) -> String {
    match total {
        Some(total) => format!("See all {total}"),
        None => "See all".to_owned(),
    }
}

/// What plays of a person's items: movies, episodes and tracks; a show
/// or an album is a page, not something to shuffle in.
fn playable(items: Vec<Item>) -> Vec<Item> {
    items
        .into_iter()
        .filter(|i| matches!(i, Item::Movie(_) | Item::Episode(_) | Item::Track(_)))
        .collect()
}

fn top_detail(item: &Item, facts: &Facts) -> Option<String> {
    match item {
        Item::Movie(_) => {
            let parts: Vec<String> = [facts.detail.clone(), facts.length.map(format::length)]
                .into_iter()
                .flatten()
                .collect();
            (!parts.is_empty()).then(|| parts.join(" · "))
        }
        _ => facts.detail.clone(),
    }
}

/// The shape, size and artwork of an item's tile: posters for films and
/// shows, squares for albums, circles for artists.
fn art_of(item: &Item, large: bool) -> (&'static str, (i32, i32), &'static [ArtworkKind]) {
    match item {
        Item::Movie(_) | Item::Show(_) => (
            "portrait",
            if large { (98, 146) } else { (110, 165) },
            &[ArtworkKind::Poster],
        ),
        Item::Artist(_) => (
            "circle",
            if large { (146, 146) } else { (96, 96) },
            &[ArtworkKind::Thumb, ArtworkKind::Poster],
        ),
        _ => (
            "square",
            if large { (146, 146) } else { (140, 140) },
            &[ArtworkKind::Thumb, ArtworkKind::Poster],
        ),
    }
}

/// While the first answer is on its way: the shape of a page of results,
/// pulsing.
fn skeleton() -> gtk::Box {
    let block = |w: i32, h: i32| {
        gtk::Box::builder()
            .width_request(w)
            .height_request(h)
            .halign(gtk::Align::Start)
            .css_classes(["skeleton-block"])
            .build()
    };
    let bars = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_top(18)
        .build();
    bars.append(&block(90, 12));
    bars.append(&block(280, 26));
    bars.append(&block(180, 12));
    let hero = gtk::Box::builder().spacing(20).build();
    hero.append(&block(260, 146));
    hero.append(&bars);
    let posters = gtk::Box::builder().spacing(14).build();
    for _ in 0..6 {
        posters.append(&block(110, 165));
    }
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .css_classes(["search-skeleton"])
        .build();
    page.append(&hero);
    page.append(&block(120, 16));
    page.append(&posters);
    page
}

/// How many of a kind a group shows before See all, as the boards have
/// it; episodes are quieter among everything.
fn kind_limit(kind: SearchKind, everything: bool) -> usize {
    match kind {
        SearchKind::Shows | SearchKind::Movies => 6,
        SearchKind::People | SearchKind::Artists | SearchKind::Albums => 5,
        SearchKind::Tracks => 4,
        SearchKind::Episodes if everything => 3,
        SearchKind::Episodes => 6,
    }
}

fn group_is_empty(group: &SearchGroup) -> bool {
    group.items.is_empty() && group.people.is_empty() && group.hidden == 0
}

fn caption(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(classes)
        .build()
}

/// Empties a list or a box the way each expects. A list box keeps its rows
/// in a list of its own: a row unparented behind its back stayed in it,
/// and the next append went after a row with no parent (GTK criticals in
/// gtk_widget_insert_after and on a freed widget, then a crash).
fn clear(container: &impl IsA<gtk::Widget>) {
    let container = container.as_ref();
    if let Some(list) = container.downcast_ref::<gtk::ListBox>() {
        list.remove_all();
    } else if let Some(flow) = container.downcast_ref::<gtk::FlowBox>() {
        flow.remove_all();
    } else if let Some(row) = container.downcast_ref::<gtk::Box>() {
        while let Some(child) = row.first_child() {
            row.remove(&child);
        }
    } else {
        while let Some(child) = container.first_child() {
            child.unparent();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use spoolframe_linux_core::backend::Client;
    use spoolframe_linux_core::fake::FakeClient;

    use super::*;

    /// Runs the main loop until `done` or `limit`.
    fn run_until(limit: Duration, done: impl Fn() -> bool) -> bool {
        let context = glib::MainContext::default();
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
        done()
    }

    /// The page on the fake's Home Media, in a window.
    fn page() -> Rc<SearchPage> {
        let client = FakeClient::new(None);
        let session = client.session(&FakeClient::profile("andrey")).unwrap();
        let page = SearchPage::new(Needs {
            session,
            cards: Rc::new(Cards {
                open: Rc::new(|_| {}),
                art: None,
                set_watched: None,
                hide: None,
                enqueue: None,
                open_person: None,
                add_to_playlist: None,
            }),
            play: Rc::new(|_| {}),
            open_person: Rc::new(|_| {}),
            shuffle: Rc::new(|_| {}),
            signed_out: Rc::new(|| {}),
            server: "Home Media".into(),
            profile: "Andrey".into(),
            recent: None,
        });
        let window = gtk::Window::builder().child(&page.widget).build();
        window.present();
        page.open(SearchScope::All, None);
        page
    }

    fn showing(page: &SearchPage) -> String {
        page.body
            .visible_child_name()
            .map(|n| n.to_string())
            .unwrap_or_default()
    }

    fn reaches(page: &SearchPage, state: &str) -> bool {
        run_until(Duration::from_secs(5), || showing(page) == state)
    }

    fn type_in(page: &SearchPage, text: &str) {
        page.entry.set_text(text);
    }

    #[test]
    fn episodes_are_quieter_among_everything() {
        assert_eq!(
            (
                kind_limit(SearchKind::Episodes, true),
                kind_limit(SearchKind::Episodes, false),
                kind_limit(SearchKind::Movies, true)
            ),
            (3, 6, 6)
        );
    }

    #[test]
    fn see_all_says_how_many_when_the_server_counts_them() {
        assert_eq!(
            (see_all_label(Some(23)), see_all_label(None)),
            ("See all 23".to_owned(), "See all".to_owned())
        );
    }

    #[test]
    fn a_persons_shows_are_not_shuffled_in() {
        use spoolframe_types::{MediaId, MediaKind, Movie, Namespace, PlaybackState, Show};
        let id = |kind, key| MediaId::new(Namespace::Plex, kind, key).unwrap();
        let show = Item::Show(Show {
            id: id(MediaKind::Show, "1"),
            title: "Severance".into(),
            year: None,
            summary: None,
            artwork: Vec::new(),
            content_rating: None,
            season_count: None,
            genres: Vec::new(),
            cast: Vec::new(),
            last_aired: None,
        });
        let movie = Item::Movie(Movie {
            id: id(MediaKind::Movie, "2"),
            title: "Arrival".into(),
            year: None,
            summary: None,
            duration: None,
            artwork: Vec::new(),
            playback: PlaybackState::default(),
            content_rating: None,
            cast: Vec::new(),
            release_date: None,
        });
        assert_eq!(playable(vec![show, movie.clone()]), vec![movie]);
    }

    /// One test: GTK binds itself to the first thread that starts it.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn search_on_the_fake_server() {
        gtk::init().unwrap();
        adw::init().unwrap();
        typing_shows_the_top_result_and_the_counts();
        a_scope_that_finds_nothing_says_where_else_it_matches();
        a_year_becomes_a_token_that_can_be_dropped();
        see_all_shows_one_group_under_its_own_head();
        a_search_is_remembered_and_shown_when_the_box_is_empty();
        recent_searches_show_again_without_stale_rows();
        a_person_named_in_full_is_the_top_result();
    }

    fn a_person_named_in_full_is_the_top_result() {
        let page = page();
        type_in(&page, "denis villeneuve");
        assert!(reaches(&page, "results"), "no results");
        let top = page.results.first_child().unwrap();
        let circle = top.first_child().unwrap();
        assert!(circle.has_css_class("search-top-person"));
    }

    fn recent_searches_show_again_without_stale_rows() {
        let page = page();
        let recent = |query: &str| Recent {
            query: query.into(),
            scope: SearchScope::All,
        };
        page.set_recent(vec![recent("low tide"), recent("arrival")]);
        page.show_recent();
        page.set_recent(vec![recent("dune")]);
        page.show_recent();
        let mut rows = 0;
        let mut child = page.recent_list.first_child();
        while let Some(row) = child {
            assert!(row.parent().as_ref() == Some(page.recent_list.upcast_ref()));
            rows += 1;
            child = row.next_sibling();
        }
        assert_eq!(rows, 1);
    }

    fn typing_shows_the_top_result_and_the_counts() {
        let page = page();
        assert_eq!(showing(&page), "recent");
        type_in(&page, "low tide");
        assert!(reaches(&page, "results"), "no results");
        let all = &page.chips[0];
        assert!(all.2.is_visible() && all.2.label() == "2");
        assert!(page.state.borrow().last.as_ref().unwrap().top.is_some());
    }

    fn a_scope_that_finds_nothing_says_where_else_it_matches() {
        let page = page();
        type_in(&page, "low tide");
        assert!(reaches(&page, "results"));
        page.chips[1].1.set_active(true); // Movies
        assert!(reaches(&page, "none"), "no empty state");
        assert_eq!(page.none.title(), "No movies match “low tide”");
        // Music, and Search everything.
        assert!(page.elsewhere.first_child().is_some());
        page.pick_scope(SearchScope::All);
        assert!(reaches(&page, "results"));
        assert!(page.chips[0].1.is_active());
    }

    fn a_year_becomes_a_token_that_can_be_dropped() {
        let page = page();
        type_in(&page, "arrival 1990");
        assert!(run_until(Duration::from_secs(5), || page.year.is_visible()));
        assert_eq!(page.year_label.label(), "1990");
        page.year.emit_clicked();
        assert_eq!(page.entry.text(), "arrival");
        assert!(run_until(Duration::from_secs(5), || !page
            .year
            .is_visible()
            && showing(&page) == "results"));
    }

    fn see_all_shows_one_group_under_its_own_head() {
        let page = page();
        type_in(&page, "a");
        assert!(reaches(&page, "results"));
        page.state.borrow_mut().kind = Some(SearchKind::Movies);
        page.run();
        assert!(run_until(Duration::from_secs(5), || page
            .see_all_head
            .is_visible()
            && page.see_all_title.label() == "Movies matching “a”"));
        assert!(page.back.is_visible() && !page.chips_row.is_visible());
        page.back.emit_clicked();
        assert!(run_until(Duration::from_secs(5), || page
            .chips_row
            .is_visible()));
    }

    fn a_search_is_remembered_and_shown_when_the_box_is_empty() {
        let page = page();
        type_in(&page, "low tide");
        assert!(reaches(&page, "results"));
        page.entry.emit_activate();
        type_in(&page, "");
        assert!(reaches(&page, "recent"));
        assert_eq!(page.state.borrow().recent[0].query, "low tide");
        assert!(page.recent_list.first_child().is_some());
    }

    #[test]
    fn a_new_search_goes_first_and_once() {
        let recent = vec![
            Recent {
                query: "dune".into(),
                scope: SearchScope::Movies,
            },
            Recent {
                query: "severance".into(),
                scope: SearchScope::All,
            },
        ];
        let queries: Vec<String> = remembered(recent, "severance", SearchScope::Shows)
            .into_iter()
            .map(|r| format!("{} {:?}", r.query, r.scope))
            .collect();
        assert_eq!(queries, ["severance Shows", "dune Movies"]);
    }

    #[test]
    fn only_a_few_searches_are_kept() {
        let mut recent = Vec::new();
        for n in 0..10 {
            recent = remembered(recent, &format!("q{n}"), SearchScope::All);
        }
        assert_eq!(recent.len(), RECENT);
    }

    #[test]
    fn the_empty_state_offers_the_scopes_that_match() {
        let counts = SearchCounts {
            all: Some(3),
            movies: Some(0),
            shows: Some(2),
            episodes: Some(1),
            music: None,
        };
        assert_eq!(
            elsewhere(&counts, SearchScope::Movies),
            [(SearchScope::Shows, 2), (SearchScope::Episodes, 1)]
        );
    }

    #[test]
    fn the_empty_title_names_the_scope() {
        assert_eq!(
            (
                empty_title(SearchScope::Movies, "zz"),
                empty_title(SearchScope::All, "zz")
            ),
            (
                "No movies match “zz”".to_owned(),
                "No results match “zz”".to_owned()
            )
        );
    }

    #[test]
    fn dropping_the_year_keeps_the_words() {
        assert_eq!(without_year("dune 2021", 2021), "dune");
    }

    #[test]
    fn a_person_reads_as_what_they_did() {
        let person = Person {
            id: spoolframe_linux_core::backend::PersonId::new(
                spoolframe_types::Namespace::Plex,
                "x",
            )
            .unwrap(),
            name: "Denis Villeneuve".into(),
            role: PersonRole::Director,
            count: Some(3),
        };
        assert_eq!(
            (views::initials(&person.name), person_detail(&person)),
            ("DV".to_owned(), "Director · 3 items".to_owned())
        );
    }
}
