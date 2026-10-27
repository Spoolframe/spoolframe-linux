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
//! A library can be sorted and filtered from the page's header ([`View`]),
//! by what it says it offers. Sorted or filtered pages are fetched afresh
//! and not kept; the choice itself is saved per profile and library
//! ([`SavedChoices`]), and comes back when the library opens again, after a
//! restart too.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};
use spoolframe_linux_core::backend::{
    BackendError, ItemFilter, ItemFilterValues, ItemSort, Session, SortKey,
};
use spoolframe_types::{Item, Library, LibraryId};

use crate::library_browse::{self as browse, Body, Chip, Choice, First, Plan, Request, Value};
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
    /// left, and the header's Sort and Filter menus; or what went wrong
    /// getting its first page. A library seen before shows at once, and
    /// refreshes behind it.
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
        // A sorted or filtered first page is waited for as a plain one is,
        // so the page lands whole.
        let first = match browse::plan(&choice, values.as_ref()) {
            Plan::Sorted(request) => {
                match sorted_first(&self.session, &library.id, &request).await {
                    First::Failed(e) => return Err(e),
                    first => Some(first),
                }
            }
            Plan::Plain | Plan::Refused(_) => None,
        };
        let view = View::new(Shared {
            session: self.session.clone(),
            library,
            cards,
            slots,
            plain,
            values,
            choices: self.choices.clone(),
        });
        view.start(choice, first);
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
    /// In the header: the Sort and Filter menus.
    pub controls: adw::Bin,
    /// Under the library's name: the active filter's chips.
    pub bar: adw::Bin,
    pub toasts: adw::ToastOverlay,
}

/// What a [`View`] is made from.
struct Shared {
    session: Arc<dyn Session>,
    library: Library,
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

/// A library page: its grid, and the menus that sort and filter it.
pub struct View {
    shared: Shared,
    choice: RefCell<Choice>,
    /// Counts the loads; an answer to an older one is dropped.
    generation: std::cell::Cell<u64>,
    /// What it put in the scroller last. A load's answer replaces it only
    /// while it is still there: the user has not gone elsewhere.
    shown: RefCell<Option<gtk::Widget>>,
    widgets: Widgets,
}

/// The menus' and the bar's widgets, to show a choice made elsewhere (a
/// chip removed, a fall back) in them.
struct Widgets {
    sort: gtk::MenuButton,
    keys: Vec<(SortKey, gtk::CheckButton)>,
    directions: [gtk::CheckButton; 2],
    filter: gtk::MenuButton,
    checks: Vec<(Value, gtk::CheckButton)>,
    /// The years offered, oldest first, and the From and To dropdowns.
    years: Option<(Vec<u16>, gtk::DropDown, gtk::DropDown)>,
    bar: gtk::Box,
    chips: gtk::FlowBox,
    count: gtk::Label,
    empty: adw::StatusPage,
}

impl View {
    fn new(shared: Shared) -> Rc<Self> {
        Rc::new_cyclic(|view: &Weak<View>| {
            let widgets = Widgets::new(view, shared.values.as_ref());
            shared.slots.controls.set_child(Some(&widgets.header()));
            shared.slots.bar.set_child(Some(&widgets.bar));
            View {
                shared,
                choice: RefCell::new(Choice::default()),
                generation: std::cell::Cell::new(0),
                shown: RefCell::new(None),
                widgets,
            }
        })
    }

    /// Shows `choice`, with its `first` page when it was fetched; a choice
    /// the library refuses falls back.
    fn start(self: &Rc<Self>, choice: Choice, first: Option<First>) {
        self.choice.replace(choice.clone());
        self.sync();
        match (browse::plan(&choice, self.shared.values.as_ref()), first) {
            (Plan::Sorted(request), Some(first)) => {
                if let Some(e) = self.show(request, first, 0) {
                    self.toast(&e.to_string());
                }
            }
            (Plan::Refused(why), _) => self.fall_back(&why),
            _ => self.put(grid(&self.shared.plain, self.shared.cards.clone()).upcast()),
        }
    }

    /// Changes the choice by `edit`, and shows it if it changed.
    fn change(self: &Rc<Self>, edit: impl FnOnce(&mut Choice)) {
        let mut choice = self.choice.borrow().clone();
        edit(&mut choice);
        if choice != *self.choice.borrow() {
            self.choose(choice);
        }
    }

    /// Saves `choice` for the library, shows it in the menus and the bar,
    /// and loads the grid for it from its first page.
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
            Plan::Plain => self.put(grid(&self.shared.plain, self.shared.cards.clone()).upcast()),
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
                self.count(total);
                self.put(grid(&pages, self.shared.cards.clone()).upcast());
                None
            }
            First::Nothing => {
                self.count(0);
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

    fn count(&self, kept: u32) {
        let all = self.shared.plain.store.n_items();
        self.widgets.count.set_label(&browse::count_text(kept, all));
    }

    fn toast(&self, text: &str) {
        self.shared
            .slots
            .toasts
            .add_toast(crate::markup::toast(text));
    }

    /// Shows the choice in the menus, the Sort button's label and the
    /// chips. Ticks set here to what they already are change nothing.
    fn sync(self: &Rc<Self>) {
        let choice = self.choice.borrow().clone();
        let widgets = &self.widgets;
        widgets.sort.set_label(&browse::sort_label(choice.sort));
        if let Some((_, radio)) = widgets.keys.iter().find(|(k, _)| *k == choice.sort.key) {
            radio.set_active(true);
        }
        for (radio, label) in widgets
            .directions
            .iter()
            .zip(browse::directions(choice.sort.key))
        {
            radio.set_label(Some(label));
        }
        widgets.directions[usize::from(choice.sort.descending)].set_active(true);
        for (value, check) in &widgets.checks {
            check.set_active(choice.has(value));
        }
        if let Some((years, from, to)) = &widgets.years {
            from.set_selected(year_index(years, choice.filter.year_from));
            to.set_selected(year_index(years, choice.filter.year_to));
        }
        widgets.chips.remove_all();
        for (chip, text) in browse::chips(&choice.filter) {
            widgets
                .chips
                .append(&chip_widget(&Rc::downgrade(self), chip, &text));
        }
        let clear = gtk::Button::builder()
            .label("Clear all")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        clear.connect_clicked(on(&Rc::downgrade(self), |view| {
            view.change(|c| c.filter = ItemFilter::default())
        }));
        widgets.chips.append(&clear);
        widgets
            .bar
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

/// A chip of the active filter, with a button that takes it off.
fn chip_widget(view: &Weak<View>, chip: Chip, text: &str) -> gtk::Widget {
    let widget = gtk::Box::builder()
        .spacing(2)
        .css_classes(["chip", "filter-chip"])
        .build();
    widget.append(&gtk::Label::new(Some(text)));
    let remove = gtk::Button::builder()
        .icon_name(crate::icons::CLOSE)
        .tooltip_text(format!("Remove {text}"))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular", "filter-chip-remove"])
        .build();
    remove.connect_clicked(on(view, move |view| view.change(|c| c.remove(&chip))));
    widget.append(&remove);
    widget.upcast()
}

impl Widgets {
    /// The menus for what `values` offers, each hidden when it offers
    /// nothing; with no values, neither shows.
    fn new(view: &Weak<View>, values: Option<&ItemFilterValues>) -> Self {
        let default = ItemFilterValues::default();
        let values = values.unwrap_or(&default);

        // Sort: the keys, then the two directions worded for the key.
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .build();
        column.append(&heading("Sort by"));
        let mut keys = Vec::new();
        let mut group: Option<gtk::CheckButton> = None;
        for key in browse::sort_keys(values) {
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
        let directions = [false, true].map(|descending| {
            let radio = radio(&mut group, "");
            radio.connect_toggled(when_on(view, move |view| {
                view.change(|c| c.sort.descending = descending)
            }));
            column.append(&radio);
            radio
        });
        let sort = gtk::MenuButton::builder()
            .label(browse::sort_label(ItemSort::DEFAULT))
            .tooltip_text("Sort the library")
            .popover(&popover(&column))
            .visible(!keys.is_empty())
            .build();

        // Filter: a section per part, applied as it is ticked.
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .build();
        let mut checks = Vec::new();
        let mut years = None;
        let sections = browse::sections(values);
        for (n, section) in sections.iter().enumerate() {
            let title = heading(section.title);
            if n > 0 {
                title.set_margin_top(10);
            }
            column.append(&title);
            match &section.body {
                Body::Checks(list) => {
                    let rows = gtk::Box::builder()
                        .orientation(gtk::Orientation::Vertical)
                        .build();
                    for check in list {
                        let button = gtk::CheckButton::with_label(&check.label);
                        let value = check.value.clone();
                        let weak = view.clone();
                        button.connect_toggled(move |button| {
                            if let Some(view) = weak.upgrade() {
                                let on = button.is_active();
                                view.change(|c| c.set(&value, on));
                            }
                        });
                        rows.append(&button);
                        checks.push((check.value.clone(), button));
                    }
                    // A long list (genres) scrolls in its section.
                    column.append(
                        &gtk::ScrolledWindow::builder()
                            .child(&rows)
                            .hscrollbar_policy(gtk::PolicyType::Never)
                            .max_content_height(240)
                            .propagate_natural_height(true)
                            .build(),
                    );
                }
                Body::Years(offered) => {
                    let names: Vec<String> = std::iter::once("Any".to_owned())
                        .chain(offered.iter().map(u16::to_string))
                        .collect();
                    let names: Vec<&str> = names.iter().map(String::as_str).collect();
                    let row = gtk::Box::builder().spacing(8).build();
                    let pick = |label: &str, set: fn(&mut Choice, Option<u16>)| {
                        row.append(&gtk::Label::new(Some(label)));
                        let dropdown = gtk::DropDown::from_strings(&names);
                        dropdown.update_property(&[gtk::accessible::Property::Label(&format!(
                            "{label} year"
                        ))]);
                        let (weak, offered) = (view.clone(), offered.clone());
                        dropdown.connect_selected_notify(move |dropdown| {
                            if let Some(view) = weak.upgrade() {
                                let year = year_at(&offered, dropdown.selected());
                                view.change(|c| set(c, year));
                            }
                        });
                        row.append(&dropdown);
                        dropdown
                    };
                    let from = pick("From", Choice::set_year_from);
                    let to = pick("To", Choice::set_year_to);
                    column.append(&row);
                    years = Some((offered.clone(), from, to));
                }
            }
        }
        let filter = gtk::MenuButton::builder()
            .label("Filter")
            .tooltip_text("Filter the library")
            .popover(&popover(
                // Every section together may be taller than the window.
                &gtk::ScrolledWindow::builder()
                    .child(&column)
                    .hscrollbar_policy(gtk::PolicyType::Never)
                    .max_content_height(520)
                    .propagate_natural_height(true)
                    .build(),
            ))
            .visible(!sections.is_empty())
            .build();

        // The bar: the chips and Clear all, then the count.
        let chips = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .column_spacing(6)
            .row_spacing(6)
            .max_children_per_line(64)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        let count = gtk::Label::builder()
            .valign(gtk::Align::Center)
            .css_classes(["dim-label", "numeric"])
            .build();
        let bar = gtk::Box::builder()
            .spacing(12)
            .visible(false)
            .css_classes(["library-filters"])
            .build();
        bar.append(&chips);
        bar.append(&count);

        let clear = gtk::Button::builder()
            .label("Clear filters")
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        clear.connect_clicked(on(view, |view| {
            view.change(|c| c.filter = ItemFilter::default())
        }));
        let empty = adw::StatusPage::builder()
            .icon_name(crate::icons::SEARCH)
            .title("No items match these filters")
            .child(&clear)
            .vexpand(true)
            .build();

        Widgets {
            sort,
            keys,
            directions,
            filter,
            checks,
            years,
            bar,
            chips,
            count,
            empty,
        }
    }

    /// The header's part: Sort, then Filter.
    fn header(&self) -> gtk::Box {
        let header = gtk::Box::builder().spacing(6).build();
        header.append(&self.sort);
        header.append(&self.filter);
        header
    }
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

fn popover(child: &impl IsA<gtk::Widget>) -> gtk::Popover {
    gtk::Popover::builder()
        .child(child)
        .width_request(260)
        .css_classes(["library-menu"])
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
    use spoolframe_linux_core::backend::Client;
    use spoolframe_linux_core::fake::FakeClient;

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
    fn opened(kind: spoolframe_types::LibraryKind) -> (Rc<Cache>, Rc<View>, gtk::ScrolledWindow) {
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

    /// The library of `kind` opened again from `cache`.
    fn open_in(
        cache: &Rc<Cache>,
        kind: spoolframe_types::LibraryKind,
    ) -> (Rc<View>, gtk::ScrolledWindow) {
        let library = spoolframe_linux_core::testing::ready(cache.session.libraries())
            .unwrap()
            .into_iter()
            .find(|l| l.kind == kind)
            .unwrap();
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

    #[test]
    #[ignore = "needs a display; run on its own"]
    fn a_library_opened_again_keeps_its_sort() {
        let (cache, view, _) = opened(spoolframe_types::LibraryKind::Movies);
        let (_, added) = view
            .widgets
            .keys
            .iter()
            .find(|(key, _)| *key == SortKey::Added)
            .unwrap();
        added.set_active(true);
        view.widgets.directions[1].set_active(true);
        let (again, _) = open_in(&cache, spoolframe_types::LibraryKind::Movies);
        assert_eq!(
            again.widgets.sort.label().as_deref(),
            Some("Date added, newest first")
        );
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
        let (view, _) = open_in(&cache(saved()), spoolframe_types::LibraryKind::Movies);
        view.change(|c| {
            c.sort = ItemSort {
                key: SortKey::Added,
                descending: true,
            }
        });
        let (again, _) = open_in(&cache(saved()), spoolframe_types::LibraryKind::Movies);
        assert_eq!(
            again.widgets.sort.label().as_deref(),
            Some("Date added, newest first")
        );
    }

    fn grid_length(scroller: &gtk::ScrolledWindow) -> u32 {
        scroller
            .child()
            .and_downcast::<gtk::GridView>()
            .and_then(|grid| grid.model())
            .map_or(0, |model| model.n_items())
    }

    #[test]
    #[ignore = "needs a display; run on its own"]
    fn ticking_a_genre_narrows_the_grid_and_counts_it() {
        let (_cache, view, scroller) = opened(spoolframe_types::LibraryKind::Movies);
        let (_, drama) = view
            .widgets
            .checks
            .iter()
            .find(|(value, _)| *value == Value::Genre("Drama".into()))
            .unwrap();
        drama.set_active(true);
        assert!(crate::ui_test::spin_until(
            std::time::Duration::from_secs(5),
            || view.widgets.count.label() == "4 of 8" && grid_length(&scroller) == 4
        ));
    }

    #[test]
    #[ignore = "needs a display; run on its own"]
    fn a_music_library_offers_no_watched_filter() {
        let (_cache, view, _) = opened(spoolframe_types::LibraryKind::Music);
        assert!(
            !view
                .widgets
                .checks
                .iter()
                .any(|(value, _)| matches!(value, Value::Watched(_)))
        );
    }

    #[test]
    #[ignore = "needs a display; run on its own"]
    fn clearing_a_filter_that_keeps_nothing_shows_the_library_again() {
        let (_cache, view, scroller) = opened(spoolframe_types::LibraryKind::Movies);
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
        let (cache, view, _) = opened(spoolframe_types::LibraryKind::Shows);
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
                view.widgets.sort.label()
            ),
            (None, Some("Title, A to Z".into()))
        );
    }

    #[test]
    fn any_year_is_the_dropdowns_first_place() {
        assert_eq!(year_index(&[1979, 1995], None), 0);
    }

    #[test]
    fn a_year_sits_after_any() {
        assert_eq!(year_index(&[1979, 1995], Some(1995)), 2);
    }

    #[test]
    fn the_first_place_picks_any_year() {
        assert_eq!(year_at(&[1979, 1995], 0), None);
    }

    #[test]
    fn a_later_place_picks_its_year() {
        assert_eq!(year_at(&[1979, 1995], 1), Some(1979));
    }
}
