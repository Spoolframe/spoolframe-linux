//! A library as a grid of posters that loads as it scrolls.
//!
//! A library can hold thousands of items, and the server serves them in
//! pages of at most 500. The grid is a GtkGridView over a list as long as
//! the library: it builds widgets only for the rows on screen and recycles
//! them, and a page is fetched the first time one of its items comes into
//! view. Until then its items are placeholder tiles.
//!
//! Libraries change rarely, so a session keeps each one's items in memory
//! ([`Cache`]): opening a library again shows it at once, and the pages
//! already seen are fetched again in the background to pick up changes.
//! Every library's first page is fetched in the background as soon as the
//! libraries are known, so even a first visit rarely waits.
//!
//! A library can be sorted from the header bar's Sort menu and filtered
//! from the Filters pane beside the grid ([`View`]), by what it says it
//! offers. Sorted or filtered pages are fetched afresh and not kept; the
//! choice itself is saved per profile and library ([`SavedChoices`]), and
//! comes back when the library opens again, after a restart too, with a
//! toast the first time in a session.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};
use spoolframe_linux_core::backend::{
    BackendError, Counted, FilterPart, ItemFilter, ItemFilterValues, ItemSort, Session, SortKey,
};
use spoolframe_types::{Item, Library, LibraryId};

use crate::library_browse::{
    self as browse, Body, Choice, First, Noun, Plan, Request, Section, Value,
};
use crate::saved_choices::SavedChoices;
use crate::views::{self, Cards};

/// The server's largest page.
const PAGE: u32 = 500;

/// One cell of the grid: an item once its page has loaded.
type Cell = Option<Item>;

/// Shared by the grid's factory and its page fetches.
struct Pages {
    session: Arc<dyn Session>,
    library: Library,
    store: gio::ListStore,
    /// Pages fetched or being fetched.
    asked: RefCell<HashSet<u32>>,
    /// How its items are ordered and kept; `None` for the library as
    /// `items` gives it.
    request: Option<Request>,
    /// Told when the server refuses `request` for a later page: the
    /// library changed since it listed what it offers.
    refused: Option<Rc<dyn Fn()>>,
}

impl Pages {
    /// Fetches the page holding `position`, unless it was asked for.
    fn want(self: &Rc<Self>, position: u32) {
        let page = position / PAGE;
        if self.asked.borrow_mut().insert(page) {
            self.fetch(page);
        }
    }

    /// Fetches `page` and puts its items in place.
    fn fetch(self: &Rc<Self>, page: u32) {
        let session = self.session.clone();
        let id = self.library.id.clone();
        let request = self.request.clone();
        let fetch = relm4::spawn(async move {
            browse::page(session.as_ref(), &id, request.as_ref(), page * PAGE, PAGE).await
        });
        let pages = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = fetch.await;
            let Some(pages) = pages.upgrade() else { return };
            match result {
                Ok(Ok((items, total))) => {
                    pages.resize(total);
                    pages.fill(page * PAGE, items);
                }
                Ok(Err(BackendError::InvalidInput(why))) if pages.refused.is_some() => {
                    tracing::info!(why, page, "sorted library page refused");
                    if let Some(refused) = &pages.refused {
                        refused();
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, page, "library page not loaded");
                    // Asked again when its items come into view again.
                    pages.asked.borrow_mut().remove(&page);
                }
                Err(e) => tracing::warn!(error = %e, page, "library page fetch failed"),
            }
        });
    }

    /// Makes the list as long as the library now is: placeholders added at
    /// the end, or cells taken off it.
    fn resize(&self, total: u32) {
        let now = self.store.n_items();
        if total > now {
            let more: Vec<glib::BoxedAnyObject> = (now..total)
                .map(|_| glib::BoxedAnyObject::new(None as Cell))
                .collect();
            self.store.splice(now, 0, &more);
        } else if total < now {
            self.store
                .splice(total, now - total, &[] as &[glib::BoxedAnyObject]);
        }
    }

    /// Fetches again every page fetched before, to pick up changes.
    fn refresh(self: &Rc<Self>) {
        let pages: Vec<u32> = self.asked.borrow().iter().copied().collect();
        for page in pages {
            self.fetch(page);
        }
    }

    /// Puts a page's items in place of its placeholders. Items past the
    /// list (the library grew since it was opened) are left out.
    fn fill(&self, offset: u32, items: Vec<Item>) {
        let room = self.store.n_items().saturating_sub(offset) as usize;
        let cells: Vec<glib::BoxedAnyObject> = items
            .into_iter()
            .take(room)
            .map(|item| glib::BoxedAnyObject::new(Some(item) as Cell))
            .collect();
        self.store.splice(offset, cells.len() as u32, &cells);
    }
}

/// Each library's sort and filter, by its id.
type Choices = Rc<RefCell<SavedChoices>>;

/// The libraries of one session, as far as they have been fetched.
pub struct Cache {
    session: Arc<dyn Session>,
    libraries: RefCell<HashMap<String, Rc<Pages>>>,
    /// Loaded with the cache and saved on every change: a library opens
    /// sorted and filtered as it was left, until the choice is changed or
    /// cleared.
    choices: Choices,
    /// The libraries opened this session: a saved choice is "back" only
    /// on the first, not each time the library opens again.
    opened: RefCell<HashSet<String>>,
    /// The library page showing, or shown last. Its menus hold it only
    /// weakly; the next library opened replaces it.
    shown: RefCell<Option<Rc<View>>>,
}

impl Cache {
    /// The cache of `session`'s libraries, opening them as `choices` has
    /// them.
    pub fn new(session: Arc<dyn Session>, choices: SavedChoices) -> Rc<Self> {
        Rc::new(Self {
            session,
            libraries: RefCell::new(HashMap::new()),
            choices: Rc::new(RefCell::new(choices)),
            opened: RefCell::new(HashSet::new()),
            shown: RefCell::new(None),
        })
    }

    /// Fetches `library`'s first page in the background, if it has not
    /// been fetched yet.
    pub fn prefetch(self: &Rc<Self>, library: Library) {
        if self
            .libraries
            .borrow()
            .contains_key(&library.id.to_string())
        {
            return;
        }
        let cache = self.clone();
        glib::spawn_future_local(async move {
            if let Err(e) = cache.pages(library).await {
                tracing::debug!(error = %e, "library not fetched ahead");
            }
        });
    }

    /// `library`'s pages: kept ones, or a first page fetched now.
    async fn pages(&self, library: Library) -> Result<Rc<Pages>, BackendError> {
        let key = library.id.to_string();
        if let Some(pages) = self.libraries.borrow().get(&key) {
            return Ok(pages.clone());
        }
        let (items, total) = {
            let (session, id) = (self.session.clone(), library.id.clone());
            relm4::spawn(async move { session.items(&id, 0, PAGE).await })
                .await
                .map_err(|e| BackendError::Other(format!("the call did not finish: {e}")))??
        };
        // Fetched twice at once (ahead, and by opening it): keep the first.
        if let Some(pages) = self.libraries.borrow().get(&key) {
            return Ok(pages.clone());
        }
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let pages = Rc::new(Pages {
            session: self.session.clone(),
            library,
            store,
            asked: RefCell::new(HashSet::from([0])),
            request: None,
            refused: None,
        });
        pages.resize(total);
        pages.fill(0, items);
        self.libraries.borrow_mut().insert(key, pages.clone());
        Ok(pages)
    }

    /// The catalog's re-read of `library`'s page at `offset` came back
    /// different: that page is fetched again, if it was fetched before,
    /// and gets the new value the catalog keeps.
    pub fn changed(&self, library: &LibraryId, offset: u32) {
        let pages = self.libraries.borrow().get(&library.to_string()).cloned();
        let Some(pages) = pages else { return };
        let page = offset / PAGE;
        if pages.asked.borrow().contains(&page) {
            pages.fetch(page);
        }
    }

    /// Opens `library` in `slots`: its grid, sorted and filtered as it was
    /// left, the header's Sort menu and Filters button, and the Filters
    /// pane; or what went wrong getting its first page. A library seen
    /// before shows at once, and refreshes behind it.
    pub async fn open(
        &self,
        library: Library,
        cards: Rc<Cards>,
        slots: Slots,
    ) -> Result<(), BackendError> {
        // Asked alongside the first page; the page works without it.
        let values = {
            let (session, id) = (self.session.clone(), library.id.clone());
            relm4::spawn(async move { session.item_filter_values(&id).await })
        };
        let key = library.id.to_string();
        let kept = self.libraries.borrow().get(&key).cloned();
        let plain = match kept {
            Some(pages) => {
                pages.refresh();
                pages
            }
            None => self.pages(library.clone()).await?,
        };
        let values = match values.await {
            Ok(Ok(values)) => Some(values),
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "library's sorts and filters not loaded");
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "library's sorts and filters fetch failed");
                None
            }
        };
        let choice = self.choices.borrow().get(&key).cloned().unwrap_or_default();
        let plan = browse::plan(&choice, values.as_ref());
        // A sorted or filtered first page is waited for as a plain one is,
        // so the page lands whole.
        let first = match &plan {
            Plan::Sorted(request) => {
                match sorted_first(&self.session, &library.id, request).await {
                    First::Failed(e) => return Err(e),
                    first => Some(first),
                }
            }
            Plan::Plain | Plan::Refused(_) => None,
        };
        let greet = browse::greets(&choice, &plan, self.opened.borrow_mut().insert(key));
        let view = View::new(Shared {
            session: self.session.clone(),
            noun: browse::noun(library.kind),
            library,
            cards,
            slots,
            plain,
            values,
            choices: self.choices.clone(),
        });
        view.start(choice, first, greet);
        self.shown.replace(Some(view));
        Ok(())
    }
}

/// `request`'s first page of `library`, fetched on the runtime.
async fn sorted_first(session: &Arc<dyn Session>, library: &LibraryId, request: &Request) -> First {
    let (session, id, request) = (session.clone(), library.clone(), request.clone());
    relm4::spawn(async move { browse::first(session.as_ref(), &id, &request, PAGE).await })
        .await
        .unwrap_or_else(|e| {
            First::Failed(BackendError::Other(format!("the call did not finish: {e}")))
        })
}

/// Where a library page's parts go; the app's root page holds the places
/// and empties them when another root shows.
pub struct Slots {
    /// The grid, or the empty state.
    pub scroller: gtk::ScrolledWindow,
    /// At the header bar's end: the Sort menu and the Filters button.
    pub controls: adw::Bin,
    /// Over the grid: the library's name and count, then the chips.
    pub bar: adw::Bin,
    /// Around the bar and the grid: its sidebar, at the right, is the
    /// Filters pane, and the grid stays in view beside it.
    pub pane: adw::OverlaySplitView,
    pub toasts: adw::ToastOverlay,
}

/// What a [`View`] is made from.
struct Shared {
    session: Arc<dyn Session>,
    library: Library,
    /// What the library's items are called, for its count.
    noun: Noun,
    cards: Rc<Cards>,
    slots: Slots,
    /// The library unsorted and unfiltered: the cached pages, and the
    /// count a filter's is out of.
    plain: Rc<Pages>,
    /// What the library offers; `None` when that did not load, and the
    /// page has no menus.
    values: Option<ItemFilterValues>,
    choices: Choices,
}

/// A library page: its grid, the Sort menu and the Filters pane.
pub struct View {
    shared: Shared,
    choice: RefCell<Choice>,
    /// Counts the loads; an answer to an older one is dropped.
    generation: std::cell::Cell<u64>,
    /// What it put in the scroller last. A load's answer replaces it only
    /// while it is still there: the user has not gone elsewhere.
    shown: RefCell<Option<gtk::Widget>>,
    widgets: Widgets,
    /// What it told the user, for the tests: a toast overlay does not say.
    #[cfg(test)]
    toasted: RefCell<Vec<String>>,
}

/// The menu's, the pane's and the bar's widgets, to show a choice made
/// elsewhere (a chip removed, a fall back) in them.
struct Widgets {
    sort: gtk::MenuButton,
    /// The Sort button's text: "Released · Newest".
    sort_label: gtk::Label,
    keys: Vec<(SortKey, gtk::CheckButton)>,
    /// The two direction rows, top first; which is which follows the key
    /// ([`browse::menu_directions`]).
    directions: [gtk::CheckButton; 2],
    /// The line under the Sort menu.
    note: gtk::Label,
    /// The keys the menu lists, for the note.
    offered: Vec<SortKey>,
    filters: gtk::ToggleButton,
    /// On the Filters button: how many parts are in use.
    badge: gtk::Label,
    /// The Filters pane, the split view's sidebar.
    pane: gtk::ScrolledWindow,
    /// The pane's Clear all.
    clear: gtk::Button,
    /// Watched's three toggles.
    toggles: Vec<(Value, gtk::ToggleButton)>,
    checks: Vec<(Value, gtk::CheckButton)>,
    /// Rows past a long section's first ones, shown once ticked or once
    /// "Show all" is pressed.
    more: Vec<gtk::CheckButton>,
    resets: Vec<(FilterPart, gtk::Button)>,
    /// The years offered, newest first, and the From and To dropdowns.
    years: Option<(Vec<u16>, gtk::DropDown, gtk::DropDown)>,
    bar: gtk::Box,
    chips: gtk::FlowBox,
    /// The count's number, set apart, and the rest: "37", " of 412 films".
    count: (gtk::Label, gtk::Label),
    /// The two together, named as one line for a screen reader.
    counted: gtk::Box,
    empty: adw::StatusPage,
}

impl View {
    fn new(shared: Shared) -> Rc<Self> {
        Rc::new_cyclic(|view: &Weak<View>| {
            let widgets = Widgets::new(view, &shared);
            shared.slots.controls.set_child(Some(&widgets.header()));
            shared.slots.bar.set_child(Some(&widgets.bar));
            shared.slots.pane.set_sidebar(Some(&widgets.pane));
            // The pane opens closed; the button and the pane (closed by a
            // click beside it when it overlays) follow each other.
            widgets
                .filters
                .bind_property("active", &shared.slots.pane, "show-sidebar")
                .bidirectional()
                .sync_create()
                .build();
            View {
                shared,
                choice: RefCell::new(Choice::default()),
                generation: std::cell::Cell::new(0),
                shown: RefCell::new(None),
                widgets,
                #[cfg(test)]
                toasted: RefCell::new(Vec::new()),
            }
        })
    }

    /// Shows `choice`, with its `first` page when it was fetched; a choice
    /// the library refuses falls back. With `greet`, the user is told the
    /// saved choice is back, once it shows.
    fn start(self: &Rc<Self>, choice: Choice, first: Option<First>, greet: bool) {
        self.choice.replace(choice.clone());
        self.sync();
        match (browse::plan(&choice, self.shared.values.as_ref()), first) {
            (Plan::Sorted(request), Some(first)) => {
                if let Some(e) = self.show(request, first, 0) {
                    self.toast(&e.to_string());
                }
            }
            (Plan::Refused(why), _) => self.fall_back(&why),
            _ => self.plain(),
        }
        // Not when the server refused it after all: that fell back.
        if greet && *self.choice.borrow() == choice {
            self.greet();
        }
    }

    /// "Your sort and filters for Movies are back.", with Clear all, which
    /// puts the library back to its default order and everything.
    fn greet(self: &Rc<Self>) {
        let text = browse::restored_text(&self.shared.library.title);
        let toast = crate::markup::toast(&text);
        toast.set_button_label(Some("Clear all"));
        toast.connect_button_clicked(on(&Rc::downgrade(self), |view| {
            view.change(|c| *c = Choice::default())
        }));
        self.shared.slots.toasts.add_toast(toast);
        #[cfg(test)]
        self.toasted.borrow_mut().push(text);
    }

    /// Changes the choice by `edit`, and shows it if it changed.
    fn change(self: &Rc<Self>, edit: impl FnOnce(&mut Choice)) {
        let mut choice = self.choice.borrow().clone();
        edit(&mut choice);
        if choice != *self.choice.borrow() {
            self.choose(choice);
        }
    }

    /// Saves `choice` for the library, shows it in the menu, the pane and
    /// the bar, and loads the grid for it from its first page.
    fn choose(self: &Rc<Self>, choice: Choice) {
        self.shared
            .choices
            .borrow_mut()
            .set(&self.shared.library.id.to_string(), choice.clone());
        self.choice.replace(choice);
        self.sync();
        self.load();
    }

    fn load(self: &Rc<Self>) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let choice = self.choice.borrow().clone();
        match browse::plan(&choice, self.shared.values.as_ref()) {
            Plan::Plain => self.plain(),
            Plan::Refused(why) => self.fall_back(&why),
            Plan::Sorted(request) => {
                let view = Rc::downgrade(self);
                let (session, id) = (self.shared.session.clone(), self.shared.library.id.clone());
                glib::spawn_future_local(async move {
                    let first = sorted_first(&session, &id, &request).await;
                    let Some(view) = view.upgrade() else { return };
                    if !view.current(generation) {
                        return;
                    }
                    if let Some(e) = view.show(request, first, generation) {
                        tracing::warn!(error = %e, "sorted library not loaded");
                        view.toast(&e.to_string());
                    }
                });
            }
        }
    }

    /// The library as `items` gives it, and its own count.
    fn plain(&self) {
        self.count(None);
        self.put(grid(&self.shared.plain, self.shared.cards.clone()).upcast());
    }

    /// Whether load `generation` is the latest, and the page still shows.
    fn current(&self, generation: u64) -> bool {
        self.generation.get() == generation
            && self
                .shown
                .borrow()
                .as_ref()
                .is_some_and(|shown| holds(&self.shared.slots.scroller, shown))
    }

    /// Shows `request`'s first page; a failure is handed back, and what
    /// showed stays.
    fn show(
        self: &Rc<Self>,
        request: Request,
        first: First,
        generation: u64,
    ) -> Option<BackendError> {
        // A sort alone keeps every item: the library's own count.
        let filters = request.filter.is_some();
        match first {
            First::Items(items, total) => {
                let refused: Rc<dyn Fn()> = {
                    let view = Rc::downgrade(self);
                    Rc::new(move || {
                        if let Some(view) = view.upgrade()
                            && view.current(generation)
                        {
                            view.fall_back("a later page was refused");
                        }
                    })
                };
                let pages = Rc::new(Pages {
                    session: self.shared.session.clone(),
                    library: self.shared.library.clone(),
                    store: gio::ListStore::new::<glib::BoxedAnyObject>(),
                    asked: RefCell::new(HashSet::from([0])),
                    request: Some(request),
                    refused: Some(refused),
                });
                pages.resize(total);
                pages.fill(0, items);
                self.count(filters.then_some(total));
                self.put(grid(&pages, self.shared.cards.clone()).upcast());
                None
            }
            First::Nothing => {
                self.count(Some(0));
                self.put(self.widgets.empty.clone().upcast());
                None
            }
            First::Refused => {
                self.fall_back("the server refused it");
                None
            }
            First::Failed(e) => Some(e),
        }
    }

    /// The server or the library's list refuses the choice: the library
    /// changed. Everything shows again, the saved choice is forgotten so
    /// the next start does not ask again, and the user is told why.
    fn fall_back(self: &Rc<Self>, why: &str) {
        tracing::info!(why, "library sort and filter fell back");
        self.choose(Choice::default());
        self.toast(browse::FELL_BACK);
    }

    fn put(&self, widget: gtk::Widget) {
        self.shared.slots.scroller.set_child(Some(&widget));
        self.shown.replace(Some(widget));
    }

    /// The count beside the library's name: `kept` of all, or all.
    fn count(&self, kept: Option<u32>) {
        let all = self.shared.plain.store.n_items();
        let (number, rest) = browse::count_parts(kept, all, self.shared.noun);
        self.widgets.count.0.set_label(&number);
        self.widgets.count.1.set_label(&rest);
        self.widgets
            .counted
            .update_property(&[gtk::accessible::Property::Label(&browse::count_text(
                kept,
                all,
                self.shared.noun,
            ))]);
    }

    fn toast(&self, text: &str) {
        self.shared
            .slots
            .toasts
            .add_toast(crate::markup::toast(text));
        #[cfg(test)]
        self.toasted.borrow_mut().push(text.to_owned());
    }

    /// Shows the choice in the Sort menu and its button, the pane, the
    /// badge and the chips. Ticks set here to what they already are change
    /// nothing.
    fn sync(self: &Rc<Self>) {
        let choice = self.choice.borrow().clone();
        let filter = &choice.filter;
        let widgets = &self.widgets;
        let key = choice.sort.key;
        widgets
            .sort_label
            .set_label(&browse::sort_label(choice.sort));
        if let Some((_, radio)) = widgets.keys.iter().find(|(k, _)| *k == key) {
            radio.set_active(true);
        }
        let order = browse::menu_directions(key);
        for (radio, descending) in widgets.directions.iter().zip(order) {
            radio.set_label(Some(browse::directions(key)[usize::from(descending)]));
            if descending == choice.sort.descending {
                radio.set_active(true);
            }
        }
        let note = browse::sort_note(key, self.shared.noun, &widgets.offered);
        widgets.note.set_label(note.as_deref().unwrap_or_default());
        widgets.note.set_visible(note.is_some());

        for (value, toggle) in &widgets.toggles {
            toggle.set_active(choice.has(value));
        }
        for (value, check) in &widgets.checks {
            check.set_active(choice.has(value));
        }
        for check in &widgets.more {
            if check.is_active() {
                check.set_visible(true);
            }
        }
        for (part, reset) in &widgets.resets {
            reset.set_visible(browse::in_use(filter, *part));
        }
        if let Some((years, from, to)) = &widgets.years {
            from.set_selected(year_index(years, filter.year_from));
            to.set_selected(year_index(years, filter.year_to));
        }
        widgets.clear.set_sensitive(choice.filters());

        let parts = browse::parts_in_use(filter);
        widgets.badge.set_label(&parts.to_string());
        widgets
            .badge
            .update_property(&[gtk::accessible::Property::Label(&browse::badge_label(
                parts,
            ))]);
        widgets.badge.set_visible(parts > 0);

        let view = Rc::downgrade(self);
        widgets.chips.remove_all();
        widgets.chips.append(&kept_for(&self.shared.library.title));
        for (part, text) in browse::chips(filter) {
            widgets.chips.append(&chip_widget(&view, part, &text));
        }
        widgets
            .chips
            .append(&clear_all(&view, &["flat", "filter-clear"]));
        widgets
            .chips
            .set_visible(choice.filters() && self.shared.values.is_some());
    }
}

/// A handler that runs `act` on the view while it lives.
fn on<W>(view: &Weak<View>, act: impl Fn(&Rc<View>) + 'static) -> impl Fn(&W) + 'static {
    let view = view.clone();
    move |_| {
        if let Some(view) = view.upgrade() {
            act(&view);
        }
    }
}

/// A Clear all: every filter part off; the sort stays.
fn clear_all(view: &Weak<View>, classes: &[&str]) -> gtk::Button {
    let clear = gtk::Button::builder()
        .label("Clear all")
        .valign(gtk::Align::Center)
        .css_classes(classes)
        .build();
    clear.connect_clicked(on(view, |view| {
        view.change(|c| c.filter = ItemFilter::default())
    }));
    clear
}

/// The dropdown position of `year`: "Any" is the first.
fn year_index(years: &[u16], year: Option<u16>) -> u32 {
    year.and_then(|y| years.iter().position(|&o| o == y))
        .map_or(0, |i| i as u32 + 1)
}

/// The year at dropdown position `index`; `None` for "Any".
fn year_at(years: &[u16], index: u32) -> Option<u16> {
    (index as usize)
        .checked_sub(1)
        .and_then(|i| years.get(i).copied())
}

/// The chips row's start: "Kept for Movies", by a clock turning back.
fn kept_for(library: &str) -> gtk::Widget {
    let kept = gtk::Box::builder()
        .spacing(6)
        .valign(gtk::Align::Center)
        .tooltip_text("Kept for this library until you change it")
        .css_classes(["dim-label", "caption-heading", "filter-kept"])
        .build();
    kept.append(&gtk::Image::from_icon_name(crate::icons::HISTORY));
    kept.append(&gtk::Label::new(Some(&browse::kept_for(library))));
    kept.upcast()
}

/// A part's chip, with a button that resets the part.
fn chip_widget(view: &Weak<View>, part: FilterPart, text: &str) -> gtk::Widget {
    let widget = gtk::Box::builder()
        .spacing(2)
        .valign(gtk::Align::Center)
        .css_classes(["chip", "accent", "filter-chip"])
        .build();
    widget.append(&gtk::Label::new(Some(text)));
    let remove = gtk::Button::builder()
        .icon_name(crate::icons::CLOSE)
        .tooltip_text(format!("Remove {text}"))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular", "filter-chip-remove"])
        .build();
    remove.connect_clicked(on(view, move |view| view.change(|c| c.reset(part))));
    widget.append(&remove);
    widget.upcast()
}

impl Widgets {
    /// The Sort menu and the Filters pane for what the library offers,
    /// each button hidden when it offers nothing; with no values, neither
    /// shows.
    fn new(view: &Weak<View>, shared: &Shared) -> Self {
        let default = ItemFilterValues::default();
        let values = shared.values.as_ref().unwrap_or(&default);
        let (sort, sort_label, keys, directions, note, offered) = sort_menu(view, values);

        // The pane: its header, then a section per part, applied as it
        // is ticked.
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .css_classes(["library-filter-pane"])
            .build();
        let head = gtk::Box::builder().spacing(8).build();
        head.append(
            &gtk::Label::builder()
                .label("Filters")
                .xalign(0.0)
                .hexpand(true)
                .css_classes(["title-4"])
                .build(),
        );
        let clear = clear_all(view, &[]);
        head.append(&clear);
        column.append(&head);
        let mut parts = Parts::default();
        let sections = browse::sections(values);
        for section in &sections {
            column.append(&parts.section(view, section));
        }
        let pane = gtk::ScrolledWindow::builder()
            .child(&column)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();

        let badge = gtk::Label::builder()
            .visible(false)
            .valign(gtk::Align::Center)
            .css_classes(["filter-badge", "numeric"])
            .build();
        let content = gtk::Box::builder().spacing(8).build();
        content.append(&gtk::Image::from_icon_name(crate::icons::FILTER));
        content.append(&gtk::Label::new(Some("Filters")));
        content.append(&badge);
        let filters = gtk::ToggleButton::builder()
            .child(&content)
            .tooltip_text("Show the filters")
            .visible(!sections.is_empty())
            .build();

        // Over the grid: the name and its count, then the chips (the
        // view fills them), which wrap.
        let count = (
            gtk::Label::builder()
                .css_classes(["heading", "numeric"])
                .build(),
            gtk::Label::builder()
                .css_classes(["dim-label", "numeric"])
                .build(),
        );
        let title = gtk::Box::builder()
            .spacing(12)
            .css_classes(["library-heading"])
            .build();
        title.append(
            &gtk::Label::builder()
                .label(&shared.library.title)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["title-1"])
                .build(),
        );
        let counted = gtk::Box::builder().valign(gtk::Align::Baseline).build();
        counted.append(&count.0);
        counted.append(&count.1);
        title.append(&counted);
        let chips = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .column_spacing(8)
            .row_spacing(8)
            .max_children_per_line(64)
            .visible(false)
            .css_classes(["library-filters"])
            .build();
        let bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        bar.append(&title);
        bar.append(&chips);

        let empty = adw::StatusPage::builder()
            .icon_name(crate::icons::NO_MATCH)
            .title("No items match these filters")
            // A status page's description is markup; the library's name
            // is the server's.
            .description(crate::markup::plain(&browse::nothing_matches_text(
                shared.noun,
                &shared.library.title,
            )))
            .child(&{
                let clear = clear_all(view, &["pill", "suggested-action"]);
                clear.set_halign(gtk::Align::Center);
                clear
            })
            .vexpand(true)
            .build();

        Widgets {
            sort,
            sort_label,
            keys,
            directions,
            note,
            offered,
            filters,
            badge,
            pane,
            clear,
            toggles: parts.toggles,
            checks: parts.checks,
            more: parts.more,
            resets: parts.resets,
            years: parts.years,
            bar,
            chips,
            count,
            counted,
            empty,
        }
    }

    /// The header bar's part: Sort, then Filters.
    fn header(&self) -> gtk::Box {
        let header = gtk::Box::builder().spacing(8).build();
        header.append(&self.sort);
        header.append(&self.filters);
        header
    }
}

/// The Sort menu's widgets: its button and the button's label, the key
/// rows, the two direction rows, the note and the keys listed.
type SortMenu = (
    gtk::MenuButton,
    gtk::Label,
    Vec<(SortKey, gtk::CheckButton)>,
    [gtk::CheckButton; 2],
    gtk::Label,
    Vec<SortKey>,
);

/// "Sort by", the library's keys, the two directions worded for the key
/// and a note on what sorts last.
fn sort_menu(view: &Weak<View>, values: &ItemFilterValues) -> SortMenu {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .build();
    column.append(&heading("Sort by"));
    let offered = browse::sort_keys(values);
    let mut keys = Vec::new();
    let mut group: Option<gtk::CheckButton> = None;
    for &key in &offered {
        let radio = radio(&mut group, browse::key_label(key).unwrap_or_default());
        radio.connect_toggled(when_on(view, move |view| view.change(|c| c.sort.key = key)));
        column.append(&radio);
        keys.push((key, radio));
    }
    column.append(
        &gtk::Separator::builder()
            .margin_top(4)
            .margin_bottom(4)
            .build(),
    );
    let mut group = None;
    let directions = [0, 1].map(|row: usize| {
        let radio = radio(&mut group, "");
        radio.connect_toggled(when_on(view, move |view| {
            view.change(|c| c.sort.descending = browse::menu_directions(c.sort.key)[row])
        }));
        column.append(&radio);
        radio
    });
    let note = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .max_width_chars(30)
        .margin_top(6)
        .css_classes(["caption", "dim-label"])
        .build();
    column.append(&note);

    let sort_label = gtk::Label::new(Some(&browse::sort_label(ItemSort::DEFAULT)));
    let content = gtk::Box::builder().spacing(8).build();
    content.append(&gtk::Image::from_icon_name(crate::icons::SORT));
    content.append(&sort_label);
    let sort = gtk::MenuButton::builder()
        .child(&content)
        .always_show_arrow(true)
        .tooltip_text("Sort the library")
        .popover(
            &gtk::Popover::builder()
                .child(&column)
                .width_request(240)
                .css_classes(["library-menu"])
                .build(),
        )
        .visible(!keys.is_empty())
        .build();
    (sort, sort_label, keys, directions, note, offered)
}

/// The pane's sections as they are built, and the widgets that show a
/// choice in them.
#[derive(Default)]
struct Parts {
    toggles: Vec<(Value, gtk::ToggleButton)>,
    checks: Vec<(Value, gtk::CheckButton)>,
    more: Vec<gtk::CheckButton>,
    resets: Vec<(FilterPart, gtk::Button)>,
    years: Option<(Vec<u16>, gtk::DropDown, gtk::DropDown)>,
}

impl Parts {
    /// A section: its name and Reset (shown while the part is in use),
    /// then its values.
    fn section(&mut self, view: &Weak<View>, section: &Section) -> gtk::Box {
        let part = section.part;
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .build();
        let head = gtk::Box::builder().spacing(8).build();
        let title = heading(section.title);
        title.set_hexpand(true);
        title.set_margin_top(0);
        head.append(&title);
        let reset = gtk::Button::builder()
            .label("Reset")
            .visible(false)
            .tooltip_text(format!("Reset {}", section.title.to_lowercase()))
            .css_classes(["flat", "accent", "filter-reset"])
            .build();
        reset.connect_clicked(on(view, move |view| view.change(|c| c.reset(part))));
        head.append(&reset);
        self.resets.push((part, reset));
        column.append(&head);
        match (&section.body, part) {
            (Body::Checks(list), FilterPart::Watched) => column.append(&self.watched(view, list)),
            (Body::Checks(list), _) => self.checks(view, part, list, &column),
            (Body::Years(offered), _) => column.append(&self.years(view, offered)),
        }
        column
    }

    /// Watched: a toggle per state, any of them on (an item in any of
    /// them is kept), none on for every item.
    fn watched(&mut self, view: &Weak<View>, list: &[browse::Check]) -> gtk::Box {
        let row = gtk::Box::builder()
            .homogeneous(true)
            .css_classes(["linked"])
            .build();
        for check in list {
            let toggle = gtk::ToggleButton::with_label(&check.label);
            let (value, weak) = (check.value.clone(), view.clone());
            toggle.connect_toggled(move |toggle| {
                if let Some(view) = weak.upgrade() {
                    let on = toggle.is_active();
                    view.change(|c| c.set(&value, on));
                }
            });
            row.append(&toggle);
            self.toggles.push((check.value.clone(), toggle));
        }
        row
    }

    /// A check row per value with its count when the server says, laid
    /// out in [`browse::columns`]; a long single column shows its first
    /// rows and "Show all".
    fn checks(
        &mut self,
        view: &Weak<View>,
        part: FilterPart,
        list: &[browse::Check],
        column: &gtk::Box,
    ) {
        let across = browse::columns(part, list.len());
        let grid = gtk::Grid::builder()
            .column_spacing(16)
            .column_homogeneous(true)
            .build();
        let folds = across == 1 && list.len() > browse::SHOWN_CHECKS;
        let mut more = Vec::new();
        for (n, check) in list.iter().enumerate() {
            let button = check_row(check);
            let value = check.value.clone();
            let weak = view.clone();
            button.connect_toggled(move |button| {
                if let Some(view) = weak.upgrade() {
                    let on = button.is_active();
                    view.change(|c| c.set(&value, on));
                }
            });
            let n = n as u32;
            grid.attach(&button, (n % across) as i32, (n / across) as i32, 1, 1);
            if folds && n as usize >= browse::SHOWN_CHECKS {
                button.set_visible(false);
                more.push(button.clone());
            }
            self.checks.push((check.value.clone(), button));
        }
        column.append(&grid);
        if folds {
            let show_all = gtk::Button::builder()
                .label(browse::show_all_label(part, list.len()))
                .halign(gtk::Align::Start)
                .css_classes(["flat", "accent"])
                .build();
            let rows = more.clone();
            show_all.connect_clicked(move |button| {
                rows.iter().for_each(|row| row.set_visible(true));
                button.set_visible(false);
            });
            column.append(&show_all);
        }
        self.more.extend(more);
    }

    /// Years: From and to, each a dropdown of "Any" and the years, newest
    /// first, with their counts.
    fn years(&mut self, view: &Weak<View>, offered: &[Counted<u16>]) -> gtk::Box {
        let years: Vec<u16> = offered.iter().map(|c| c.value).collect();
        let row = gtk::Box::builder().spacing(8).build();
        let pick = |label: &str, name: &str, set: fn(&mut Choice, Option<u16>)| {
            row.append(
                &gtk::Label::builder()
                    .label(label)
                    .css_classes(["dim-label"])
                    .build(),
            );
            let dropdown = year_dropdown(offered);
            dropdown.update_property(&[gtk::accessible::Property::Label(name)]);
            let (weak, years) = (view.clone(), years.clone());
            dropdown.connect_selected_notify(move |dropdown| {
                if let Some(view) = weak.upgrade() {
                    let year = year_at(&years, dropdown.selected());
                    view.change(|c| set(c, year));
                }
            });
            row.append(&dropdown);
            dropdown
        };
        let from = pick("From", "From year", Choice::set_year_from);
        let to = pick("to", "To year", Choice::set_year_to);
        self.years = Some((years, from, to));
        row
    }
}

/// A value's check row: its name, and its count at the end when the
/// server sent one.
fn check_row(check: &browse::Check) -> gtk::CheckButton {
    let row = gtk::Box::builder().spacing(8).build();
    row.append(
        &gtk::Label::builder()
            .label(&check.label)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
    );
    if let Some(count) = check.count {
        row.append(
            &gtk::Label::builder()
                .label(count.to_string())
                .css_classes(["caption", "dim-label", "numeric"])
                .build(),
        );
    }
    let button = gtk::CheckButton::builder().child(&row).build();
    button.update_property(&[gtk::accessible::Property::Label(&check.label)]);
    button
}

/// A year dropdown: "Any", then `offered`; its list shows each year's
/// count, its button the year alone.
fn year_dropdown(offered: &[Counted<u16>]) -> gtk::DropDown {
    let names: Vec<String> = std::iter::once("Any".to_owned())
        .chain(offered.iter().map(|c| c.value.to_string()))
        .collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let dropdown = gtk::DropDown::from_strings(&names);
    dropdown.set_hexpand(true);
    let counts: Vec<Option<u32>> = std::iter::once(None)
        .chain(offered.iter().map(|c| c.count))
        .collect();
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        let row = gtk::Box::builder().spacing(12).build();
        row.append(&gtk::Label::builder().xalign(0.0).hexpand(true).build());
        row.append(
            &gtk::Label::builder()
                .css_classes(["caption", "dim-label", "numeric"])
                .build(),
        );
        item.set_child(Some(&row));
    });
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        let name = item.item().and_downcast::<gtk::StringObject>();
        let year = item.child().and_then(|row| row.first_child());
        let count = year.as_ref().and_then(|y| y.next_sibling());
        let (Some(name), Some(year), Some(count)) = (
            name,
            year.and_downcast::<gtk::Label>(),
            count.and_downcast::<gtk::Label>(),
        ) else {
            return;
        };
        year.set_label(&name.string());
        let n = counts.get(item.position() as usize).copied().flatten();
        count.set_label(&n.map(|n| n.to_string()).unwrap_or_default());
        count.set_visible(n.is_some());
    });
    dropdown.set_list_factory(Some(&factory));
    dropdown
}

/// A radio in `group`, the first made its leader.
fn radio(group: &mut Option<gtk::CheckButton>, label: &str) -> gtk::CheckButton {
    let radio = gtk::CheckButton::with_label(label);
    match group {
        Some(first) => radio.set_group(Some(first)),
        None => *group = Some(radio.clone()),
    }
    radio
}

/// A radio's handler: turning one on turns its group's last one off, and
/// only the one turned on acts.
fn when_on(
    view: &Weak<View>,
    act: impl Fn(&Rc<View>) + 'static,
) -> impl Fn(&gtk::CheckButton) + 'static {
    let view = view.clone();
    move |radio| {
        if radio.is_active()
            && let Some(view) = view.upgrade()
        {
            act(&view);
        }
    }
}

/// Whether `scroller` holds `widget`: as it is, or in the viewport it
/// wraps a widget that does not scroll itself in.
fn holds(scroller: &gtk::ScrolledWindow, widget: &gtk::Widget) -> bool {
    let Some(child) = scroller.child() else {
        return false;
    };
    &child == widget
        || child
            .downcast_ref::<gtk::Viewport>()
            .and_then(|v| v.child())
            .as_ref()
            == Some(widget)
}

/// A section's name over its rows, as the tracks menu has them.
fn heading(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text.to_uppercase())
        .xalign(0.0)
        .margin_top(4)
        .css_classes(["caption-heading", "dim-label"])
        .build()
}

/// A grid over `pages`; their items fill in as they arrive.
fn grid(pages: &Rc<Pages>, cards: Rc<Cards>) -> gtk::GridView {
    let store = pages.store.clone();
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        item.set_child(Some(&adw::Bin::new()));
        // The card inside is a button; the list item itself is not.
        item.set_activatable(false);
    });
    {
        let pages = pages.clone();
        factory.connect_bind(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
            let Some(bin) = item.child().and_downcast::<adw::Bin>() else {
                return;
            };
            let Some(cell) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
                return;
            };
            let card = match &*cell.borrow::<Cell>() {
                Some(media) => views::grid_card(media, &cards),
                None => {
                    pages.want(item.position());
                    views::placeholder_card()
                }
            };
            bin.set_child(Some(&card));
        });
    }
    factory.connect_unbind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        if let Some(bin) = item.child().and_downcast::<adw::Bin>() {
            bin.set_child(gtk::Widget::NONE);
        }
    });

    gtk::GridView::builder()
        .model(&gtk::NoSelection::new(Some(store)))
        .factory(&factory)
        .max_columns(16)
        .min_columns(1)
        .css_classes(["library-grid"])
        .build()
}

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::backend::{Client, WatchedState};
    use spoolframe_linux_core::fake::FakeClient;
    use spoolframe_types::LibraryKind;

    use super::*;

    fn pages() -> Pages {
        let client = FakeClient::new(None);
        let profile = FakeClient::profile("andrey");
        let session = client.session(&profile).unwrap();
        // A context of its own: tests run on parallel threads.
        let library = glib::MainContext::new()
            .block_on(session.libraries())
            .unwrap()
            .remove(0);
        Pages {
            session,
            library,
            store: gio::ListStore::new::<glib::BoxedAnyObject>(),
            asked: RefCell::new(HashSet::new()),
            request: None,
            refused: None,
        }
    }

    /// A library that grew while kept gets placeholders for its new items.
    #[test]
    fn a_grown_library_gets_more_cells() {
        let pages = pages();
        pages.resize(3);
        pages.resize(5);
        assert_eq!(pages.store.n_items(), 5);
    }

    /// One that shrank loses the cells past its end.
    #[test]
    fn a_shrunk_library_loses_cells() {
        let pages = pages();
        pages.resize(5);
        pages.resize(2);
        assert_eq!(pages.store.n_items(), 2);
    }

    /// The fake's library of `kind` opened in fresh slots, once its page
    /// shows; the cache, the view and the scroller.
    fn opened(kind: LibraryKind) -> (Rc<Cache>, Rc<View>, gtk::ScrolledWindow) {
        let cache = cache(SavedChoices::default());
        let (view, scroller) = open_in(&cache, kind);
        (cache, view, scroller)
    }

    /// A cache over the fake's session, with `choices`.
    fn cache(choices: SavedChoices) -> Rc<Cache> {
        gtk::init().unwrap();
        adw::init().unwrap();
        let session = FakeClient::new(None)
            .session(&FakeClient::profile("andrey"))
            .unwrap();
        Cache::new(session, choices)
    }

    fn library_of(cache: &Cache, kind: LibraryKind) -> Library {
        spoolframe_linux_core::testing::ready(cache.session.libraries())
            .unwrap()
            .into_iter()
            .find(|l| l.kind == kind)
            .unwrap()
    }

    /// The library of `kind` opened again from `cache`.
    fn open_in(cache: &Rc<Cache>, kind: LibraryKind) -> (Rc<View>, gtk::ScrolledWindow) {
        let library = library_of(cache, kind);
        let cards = Rc::new(Cards {
            open: Rc::new(|_| {}),
            art: None,
            set_watched: None,
            hide: None,
            enqueue: None,
            open_person: None,
            add_to_playlist: None,
            hearts: None,
        });
        let scroller = gtk::ScrolledWindow::new();
        let slots = Slots {
            scroller: scroller.clone(),
            controls: adw::Bin::new(),
            bar: adw::Bin::new(),
            pane: adw::OverlaySplitView::new(),
            toasts: adw::ToastOverlay::new(),
        };
        let done = Rc::new(std::cell::Cell::new(false));
        {
            let (cache, done) = (cache.clone(), done.clone());
            glib::spawn_future_local(async move {
                cache.open(library, cards, slots).await.unwrap();
                done.set(true);
            });
        }
        assert!(crate::ui_test::spin_until(
            std::time::Duration::from_secs(5),
            || done.get()
        ));
        let view = cache.shown.borrow().clone().unwrap();
        (view, scroller)
    }

    /// The direction row now worded `label`.
    fn direction<'a>(view: &'a View, label: &str) -> &'a gtk::CheckButton {
        view.widgets
            .directions
            .iter()
            .find(|radio| radio.label().as_deref() == Some(label))
            .unwrap()
    }

    #[test]
    #[ignore = "needs a display; run on its own"]
    fn a_library_opened_again_keeps_its_sort() {
        let (cache, view, _) = opened(LibraryKind::Movies);
        let (_, added) = view
            .widgets
            .keys
            .iter()
            .find(|(key, _)| *key == SortKey::Added)
            .unwrap();
        added.set_active(true);
        direction(&view, "Newest").set_active(true);
        let (again, _) = open_in(&cache, LibraryKind::Movies);
        assert_eq!(again.widgets.sort_label.label(), "Added · Newest");
    }

    /// As after a restart: a new cache, its choices read from the file.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn a_library_keeps_its_sort_in_the_next_session() {
        let dir = spoolframe_linux_core::testing::TestDir::new_blocking(
            "spoolframe-library-choices-next-session",
        );
        let saved = || {
            SavedChoices::in_store(
                spoolframe_linux_core::storage::DataStore::new(dir.path()),
                &FakeClient::profile("andrey"),
            )
        };
        let (view, _) = open_in(&cache(saved()), LibraryKind::Movies);
        view.change(|c| {
            c.sort = ItemSort {
                key: SortKey::Added,
                descending: true,
            }
        });
        let (again, _) = open_in(&cache(saved()), LibraryKind::Movies);
        assert_eq!(again.widgets.sort_label.label(), "Added · Newest");
    }

    fn grid_length(scroller: &gtk::ScrolledWindow) -> u32 {
        scroller
            .child()
            .and_downcast::<gtk::GridView>()
            .and_then(|grid| grid.model())
            .map_or(0, |model| model.n_items())
    }

    fn count_text(view: &View) -> String {
        let (number, rest) = &view.widgets.count;
        format!("{}{}", number.label(), rest.label())
    }

    fn check<'a>(view: &'a View, value: &Value) -> &'a gtk::CheckButton {
        let (_, check) = view
            .widgets
            .checks
            .iter()
            .find(|(v, _)| v == value)
            .unwrap();
        check
    }

    #[test]
    #[ignore = "needs a display; run on its own"]
    fn ticking_a_genre_narrows_the_grid_and_counts_it() {
        let (_cache, view, scroller) = opened(LibraryKind::Movies);
        check(&view, &Value::Genre("Drama".into())).set_active(true);
        assert!(crate::ui_test::spin_until(
            std::time::Duration::from_secs(5),
            || count_text(&view) == "4 of 8 films" && grid_length(&scroller) == 4
        ));
    }

    #[test]
    #[ignore = "needs a display; run on its own"]
    fn an_unfiltered_library_counts_all_its_items() {
        let (_cache, view, _) = opened(LibraryKind::Movies);
        assert_eq!(count_text(&view), "8 films");
    }

    #[test]
    #[ignore = "needs a display; run on its own"]
    fn a_music_library_offers_no_watched_filter() {
        let (_cache, view, _) = opened(LibraryKind::Music);
        assert!(view.widgets.toggles.is_empty());
    }

    #[test]
    #[ignore = "needs a display; run on its own"]
    fn a_watched_toggle_keeps_items_in_that_state() {
        let (_cache, view, _) = opened(LibraryKind::Movies);
        let (_, unwatched) = view
            .widgets
            .toggles
            .iter()
            .find(|(v, _)| *v == Value::Watched(WatchedState::Unwatched))
            .unwrap();
        unwatched.set_active(true);
        assert_eq!(
            view.choice.borrow().filter.watched,
            vec![WatchedState::Unwatched]
        );
    }

    #[test]
    #[ignore = "needs a display; run on its own"]
    fn the_badge_counts_the_parts_in_use() {
        let (_cache, view, _) = opened(LibraryKind::Movies);
        view.change(|c| {
            c.set(&Value::Genre("Drama".into()), true);
            c.set(&Value::Genre("Science Fiction".into()), true);
            c.set(&Value::Watched(WatchedState::Unwatched), true);
        });
        let badge = &view.widgets.badge;
        assert_eq!((badge.label().as_str(), badge.is_visible()), ("2", true));
    }

    #[test]
    #[ignore = "needs a display; run on its own"]
    fn a_parts_reset_shows_while_it_is_in_use() {
        let (_cache, view, _) = opened(LibraryKind::Movies);
        view.change(|c| c.set(&Value::Genre("Drama".into()), true));
        let shown: Vec<FilterPart> = view
            .widgets
            .resets
            .iter()
            .filter(|(_, reset)| reset.is_visible())
            .map(|(part, _)| *part)
            .collect();
        assert_eq!(shown, vec![FilterPart::Genres]);
    }

    #[test]
    #[ignore = "needs a display; run on its own"]
    fn the_filters_button_opens_the_pane() {
        let (_cache, view, _) = opened(LibraryKind::Movies);
        view.widgets.filters.set_active(true);
        assert!(view.shared.slots.pane.shows_sidebar());
    }

    /// A choice saved before this session, as one read from the file.
    fn saved_drama(cache: &Cache) {
        let id = library_of(cache, LibraryKind::Movies).id.to_string();
        let mut drama = Choice::default();
        drama.set(&Value::Genre("Drama".into()), true);
        cache.choices.borrow_mut().set(&id, drama);
    }

    #[test]
    #[ignore = "needs a display; run on its own"]
    fn a_saved_choice_is_said_to_be_back_on_opening() {
        let cache = cache(SavedChoices::default());
        saved_drama(&cache);
        let (view, _) = open_in(&cache, LibraryKind::Movies);
        assert_eq!(
            *view.toasted.borrow(),
            vec![browse::restored_text(&view.shared.library.title)]
        );
    }

    #[test]
    #[ignore = "needs a display; run on its own"]
    fn a_saved_choice_is_said_to_be_back_once_a_session() {
        let cache = cache(SavedChoices::default());
        saved_drama(&cache);
        open_in(&cache, LibraryKind::Movies);
        let (again, _) = open_in(&cache, LibraryKind::Movies);
        assert!(again.toasted.borrow().is_empty());
    }

    #[test]
    #[ignore = "needs a display; run on its own"]
    fn the_default_opens_without_a_toast() {
        let (_cache, view, _) = opened(LibraryKind::Movies);
        assert!(view.toasted.borrow().is_empty());
    }

    #[test]
    #[ignore = "needs a display; run on its own"]
    fn clearing_a_filter_that_keeps_nothing_shows_the_library_again() {
        let (_cache, view, scroller) = opened(LibraryKind::Movies);
        view.change(|c| {
            c.set(&Value::Genre("Horror".into()), true);
            c.set(
                &Value::Resolution(spoolframe_linux_core::backend::Resolution::Uhd),
                true,
            );
        });
        assert!(crate::ui_test::spin_until(
            std::time::Duration::from_secs(5),
            || holds(&scroller, view.widgets.empty.upcast_ref())
        ));
        view.change(|c| c.filter = ItemFilter::default());
        assert_eq!(grid_length(&scroller), 8);
    }

    /// A choice the library no longer offers (the TV library has no
    /// duration), as one saved from before it changed, falls back and is
    /// forgotten.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn a_choice_the_library_refuses_falls_back_to_everything() {
        let (cache, view, _) = opened(LibraryKind::Shows);
        let id = view.shared.library.id.to_string();
        let refused = Choice {
            sort: ItemSort {
                key: SortKey::Duration,
                descending: true,
            },
            ..Choice::default()
        };
        cache.choices.borrow_mut().set(&id, refused.clone());
        view.choice.replace(refused);
        view.load();
        assert_eq!(
            (
                cache.choices.borrow().get(&id).cloned(),
                view.widgets.sort_label.label()
            ),
            (None, "Title · A to Z".into())
        );
    }

    #[test]
    fn any_year_is_the_dropdowns_first_place() {
        assert_eq!(year_index(&[1995, 1979], None), 0);
    }

    #[test]
    fn a_year_sits_after_any() {
        assert_eq!(year_index(&[1995, 1979], Some(1979)), 2);
    }

    #[test]
    fn the_first_place_picks_any_year() {
        assert_eq!(year_at(&[1995, 1979], 0), None);
    }

    #[test]
    fn a_later_place_picks_its_year() {
        assert_eq!(year_at(&[1995, 1979], 1), Some(1995));
    }
}
