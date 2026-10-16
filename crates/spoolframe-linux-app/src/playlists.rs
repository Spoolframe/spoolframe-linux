//! Playlists (PX-40) on screen: the Playlists section's cards in their
//! groups (board LinuxPlaylists), a playlist's own page (LinuxPlaylist,
//! LinuxPlaylistMissing, LinuxPlaylistStates 3 and 7), and "New playlist".
//! What they show is worked out in the core's `playlists`; this lays it
//! out.

use std::cell::{Cell, RefCell};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use adw::prelude::*;
use spoolframe_linux_core::backend::{
    ArtworkSize, EntryId, Kinds, PlaySettings, PlaylistEdit, PlaylistEntry, PlaylistPage,
    PlaylistSummary, PlaylistUpdate, Repeat, Visibility,
};
use spoolframe_linux_core::playlists::{self, Offer};
use spoolframe_types::{ArtworkKind, Item, MediaKind};

use crate::views::{self, Cards, Facts};

/// A card's mosaic in the section.
const CARD: i32 = 196;
/// The mosaic beside a playlist's name on its page.
const HEADER: i32 = 150;
/// An entry's still.
const THUMB: (i32, i32) = (112, 63);

/// A playlist's picture until it has one: four squares in hues picked
/// from its id, so it keeps its colours from one screen to the next.
pub fn mosaic(id: &str, size: i32) -> gtk::Widget {
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    let bits = hasher.finish();
    let grid = gtk::Grid::builder()
        .width_request(size)
        .height_request(size)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Center)
        .overflow(gtk::Overflow::Hidden)
        .css_classes(["mosaic"])
        .build();
    if size < 40 {
        grid.add_css_class("small");
    }
    for cell in 0..4_i32 {
        let hue = views::TILE_HUES[((bits >> (cell * 8)) % views::TILE_HUES.len() as u64) as usize];
        grid.attach(
            &gtk::Box::builder()
                .width_request(size / 2)
                .height_request(size / 2)
                .hexpand(true)
                .vexpand(true)
                .css_classes([hue])
                .build(),
            cell % 2,
            cell / 2,
            1,
            1,
        );
    }
    grid.upcast()
}

/// What a playlist's card, row and menu do, by its id.
pub struct ListActions {
    pub open: Rc<dyn Fn(String)>,
    /// Plays it (`true`: shuffled).
    pub play: Rc<dyn Fn(String, bool)>,
    /// Pins it, last (`true`), or unpins it.
    pub pin: Rc<dyn Fn(String, bool)>,
}

/// The Playlists section: My playlists, Shared with me and Server-wide,
/// each a row of cards; a group with none is left out.
pub fn section(list: &[PlaylistSummary], actions: &Rc<ListActions>) -> Vec<gtk::Box> {
    let groups = playlists::sections(list);
    let rows: Vec<gtk::Box> = [
        ("My playlists", groups.mine),
        ("Shared with me", groups.shared_with_me),
        ("Server-wide", groups.server_wide),
    ]
    .into_iter()
    .filter(|(_, group)| !group.is_empty())
    .map(|(title, group)| views::row(title, group.into_iter().map(|p| card(p, actions))))
    .collect();
    if !rows.is_empty() {
        return rows;
    }
    let empty = gtk::Box::new(gtk::Orientation::Vertical, 0);
    empty.append(
        &adw::StatusPage::builder()
            .icon_name(crate::icons::PLAYLISTS)
            .title("No playlists yet")
            .description("Make one with New playlist.")
            .vexpand(true)
            .build(),
    );
    vec![empty]
}

/// A playlist's card: its mosaic, name, and whose it is; right-click for
/// Play, Shuffle and its pin.
fn card(playlist: &PlaylistSummary, actions: &Rc<ListActions>) -> gtk::Widget {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.append(&mosaic(&playlist.id, CARD));
    let caption = gtk::Box::new(gtk::Orientation::Vertical, 2);
    caption.append(&views::caption_label(&playlist.name, &["heading"]));
    caption.append(&views::caption_label(
        &playlists::owner_line(playlist),
        &["dim-label", "caption"],
    ));
    body.append(&caption);
    let button = gtk::Button::builder()
        .child(&body)
        .width_request(CARD)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .css_classes(["card-button", "flat", "lift"])
        .build();
    {
        let (open, id) = (actions.open.clone(), playlist.id.clone());
        button.connect_clicked(move |_| open(id.clone()));
    }
    let menu = views::menu_popover(menu_entries(playlist, actions));
    views::context_menu(button.upcast_ref(), &menu);
    button.upcast()
}

/// A playlist's menu: Play and Shuffle, then its pin.
fn menu_entries(
    playlist: &PlaylistSummary,
    actions: &Rc<ListActions>,
) -> Vec<Vec<views::MenuEntry>> {
    let id = playlist.id.clone();
    let play = |label: &str, icon, shuffle| {
        let (play, id) = (actions.play.clone(), id.clone());
        views::MenuEntry::new(label, icon, move || play(id.clone(), shuffle))
    };
    let pinned = playlist.pinned.is_some();
    let pin = {
        let (pin, id) = (actions.pin.clone(), id.clone());
        let (label, icon) = pin_label(pinned);
        views::MenuEntry::new(label, icon, move || pin(id.clone(), !pinned))
    };
    vec![
        vec![
            play("Play", crate::icons::PLAY, false),
            play("Shuffle", crate::icons::SHUFFLE, true),
        ],
        vec![pin],
    ]
}

/// What the pin control says and shows, for a playlist pinned or not.
fn pin_label(pinned: bool) -> (&'static str, &'static str) {
    match pinned {
        true => ("Unpin from the sidebar", crate::icons::UNPIN),
        false => ("Pin to the sidebar", crate::icons::PIN),
    }
}

/// The main button: "Continue · S4 · E6 Halloween IV" at the user's place,
/// else "Play".
pub fn play_label(page: &PlaylistPage) -> String {
    match playlists::continue_label(page) {
        Some(next) => format!("Continue · {next}"),
        None => "Play".to_owned(),
    }
}

/// Play from start's entry: the first that can play. `None` with a place
/// not kept, where the main button already plays from the top.
pub fn from_start(page: &PlaylistPage) -> Option<EntryId> {
    playlists::up_next(page)?;
    page.entries
        .iter()
        .find(|e| !e.unavailable)
        .map(|e| e.entry)
}

/// The Repeat button's next state on a click: off, all, one, off.
pub fn next_repeat(repeat: Repeat) -> Repeat {
    match repeat {
        Repeat::Off | Repeat::Unknown => Repeat::All,
        Repeat::All => Repeat::One,
        Repeat::One => Repeat::Off,
    }
}

fn repeat_label(repeat: Repeat) -> (&'static str, &'static str) {
    match repeat {
        Repeat::All => ("Repeat all", crate::icons::REPEAT),
        Repeat::One => ("Repeat one", crate::icons::REPEAT_ONE),
        Repeat::Off | Repeat::Unknown => ("Repeat", crate::icons::REPEAT),
    }
}

/// The line under the name: its counts and whose it is.
pub fn header_line(page: &PlaylistPage) -> String {
    format!(
        "{} · {}",
        playlists::count_line(page),
        playlists::owner_line(&page.playlist)
    )
}

/// "Holds episodes", "Holds movies, episodes".
pub fn holds_line(kinds: Kinds) -> String {
    format!("Holds {}", playlists::kinds_line(kinds).to_lowercase())
}

/// What a playlist's page does.
pub struct PageActions {
    /// Plays from an entry, or the user's place, with the settings shown.
    pub play: Rc<dyn Fn(Option<EntryId>, PlaySettings)>,
    /// Pins it, last (`true`), or unpins it.
    pub pin: Rc<dyn Fn(bool)>,
    /// Asks for the entries from this offset on: Show more.
    pub more: Rc<dyn Fn(u32)>,
    /// Into the editor (`true`), or out of it as it is (Cancel).
    pub edit: Rc<dyn Fn(bool)>,
    /// Done: out of the editor, with the new name if one was typed.
    pub done: Rc<dyn Fn(Option<String>)>,
    /// Changes to the entries, as seen at a revision.
    pub edit_entries: Rc<dyn Fn(u64, Vec<PlaylistEdit>)>,
    /// Its kinds or who sees it.
    pub update: Rc<dyn Fn(PlaylistUpdate)>,
    pub delete: Rc<dyn Fn()>,
}

/// How a playlist's page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    View,
    /// The editor; `admin` offers Server-wide.
    Edit {
        admin: bool,
    },
}

/// A playlist's page as shown: its header and the entries loaded so far.
pub struct PlaylistView {
    pub id: String,
    pub name: String,
    pub widget: gtk::Box,
    list: gtk::ListBox,
    more: gtk::Button,
    pin: gtk::ToggleButton,
    /// Set while the pin follows the list, so it sends nothing.
    syncing: Cell<bool>,
    shown: Cell<u32>,
    total: u32,
    up_next: Option<EntryId>,
    cards: Rc<Cards>,
    /// In the editor: the revision shown, and the entries' ids in order.
    editing: Option<(u64, Rc<RefCell<Vec<EntryId>>>)>,
    actions: Rc<PageActions>,
    /// The play settings the header shows, for Play from here.
    settings: Rc<Cell<PlaySettings>>,
}

impl PlaylistView {
    pub fn new(
        page: &PlaylistPage,
        actions: PageActions,
        cards: Rc<Cards>,
        mode: Mode,
    ) -> Rc<Self> {
        let actions = Rc::new(actions);
        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(26)
            .margin_bottom(28)
            .margin_start(28)
            .margin_end(28)
            .build();
        // The entries, and beside them in the editor the side panel.
        let columns = gtk::Box::builder().spacing(28).build();
        let entries = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .hexpand(true)
            .build();
        columns.append(&entries);
        let settings = Rc::new(Cell::new(page.settings));
        let (pin, editing) = match mode {
            Mode::View => {
                let (header, pin) = header(page, &actions, &settings);
                widget.append(&header);
                (pin, None)
            }
            Mode::Edit { admin } => {
                let controls = playlists::editor_controls(&page.playlist, admin);
                let side = controls
                    .panel
                    .then(|| crate::playlist_edit::panel(page, &actions, admin));
                widget.append(&crate::playlist_edit::header(
                    page,
                    &actions,
                    side.as_ref().map(|(_, name)| name),
                ));
                if let Some((side, _)) = side {
                    columns.append(&side);
                }
                // Not shown: the pin is the page's, not the editor's.
                let pin = gtk::ToggleButton::new();
                (pin, Some((page.playlist.revision, Rc::default())))
            }
        };
        widget.append(&columns);
        let widget_for_entries = entries;
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .valign(gtk::Align::Start)
            .css_classes(["boxed-list", "playlist-entries"])
            .build();
        let more = gtk::Button::builder()
            .label("Show more")
            .halign(gtk::Align::Center)
            .css_classes(["pill"])
            .visible(false)
            .build();
        if page.total == 0 {
            widget_for_entries.append(
                &adw::StatusPage::builder()
                    .icon_name(crate::icons::PLAYLISTS)
                    .title(format!("{} is empty", page.playlist.name))
                    .description("Use Add to playlist on a film, an episode or a season.")
                    .build(),
            );
        } else {
            widget_for_entries.append(&list);
            widget_for_entries.append(&more);
        }
        let view = Rc::new(PlaylistView {
            id: page.playlist.id.clone(),
            name: page.playlist.name.clone(),
            widget,
            list,
            more,
            pin,
            syncing: Cell::new(false),
            shown: Cell::new(0),
            total: page.total,
            up_next: playlists::up_next(page),
            cards,
            editing,
            actions: actions.clone(),
            settings,
        });
        {
            let (view_weak, more) = (Rc::downgrade(&view), actions.more.clone());
            view.more.connect_clicked(move |button| {
                if let Some(view) = view_weak.upgrade() {
                    button.set_sensitive(false);
                    more(view.shown.get());
                }
            });
        }
        {
            let view_weak = Rc::downgrade(&view);
            let pin = actions.pin.clone();
            view.pin.connect_toggled(move |button| {
                let on = button.is_active();
                show_pin(button, on);
                if view_weak.upgrade().is_some_and(|v| !v.syncing.get()) {
                    pin(on);
                }
            });
        }
        view.append(page);
        view
    }

    /// The entries of a further page, under those shown.
    pub fn append(&self, page: &PlaylistPage) {
        if page.offset != self.shown.get() {
            return;
        }
        for (n, entry) in page.entries.iter().enumerate() {
            let number = page.offset as usize + n + 1;
            match &self.editing {
                Some((revision, ids)) => {
                    ids.borrow_mut().push(entry.entry);
                    self.list.append(&crate::playlist_edit::entry_row(
                        number,
                        entry,
                        *revision,
                        ids,
                        &self.actions,
                    ));
                }
                None => self.list.append(&entry_row(
                    number,
                    entry,
                    self.up_next,
                    &self.cards,
                    self.play_from(entry.entry),
                )),
            }
        }
        self.shown.set(page.offset + page.entries.len() as u32);
        self.more.set_sensitive(true);
        self.more
            .set_visible(self.shown.get() < self.total && !page.entries.is_empty());
    }

    /// Play from here: the playlist from `entry`, with the settings shown.
    fn play_from(&self, entry: EntryId) -> Rc<dyn Fn()> {
        let (play, settings) = (self.actions.play.clone(), self.settings.clone());
        Rc::new(move || play(Some(entry), settings.get()))
    }

    /// The pin as the list now has it: unpinned from the sidebar, say.
    pub fn set_pinned(&self, pinned: bool) {
        if self.pin.is_active() != pinned {
            self.syncing.set(true);
            self.pin.set_active(pinned);
            self.syncing.set(false);
        }
    }
}

fn show_pin(button: &gtk::ToggleButton, pinned: bool) {
    let (tooltip, icon) = pin_label(pinned);
    button.set_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
}

/// The mosaic, "PLAYLIST", the name, its lines, and the controls: play,
/// shuffle, repeat, skip watched, and the pin.
fn header(
    page: &PlaylistPage,
    actions: &PageActions,
    settings: &Rc<Cell<PlaySettings>>,
) -> (gtk::Box, gtk::ToggleButton) {
    let header = gtk::Box::builder().spacing(28).build();
    header.append(&mosaic(&page.playlist.id, HEADER));
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .hexpand(true)
        .valign(gtk::Align::End)
        .build();
    column.append(&label("PLAYLIST", &["picker-label"]));
    let name = label(&page.playlist.name, &["album-title"]);
    name.set_wrap(true);
    column.append(&name);
    let line = label(&header_line(page), &["item-meta"]);
    line.set_wrap(true);
    column.append(&line);
    column.append(&label(
        &holds_line(page.playlist.kinds),
        &["dim-label", "caption"],
    ));

    let controls = gtk::Box::builder().spacing(10).margin_top(6).build();
    let playable = page.playable > 0;
    let play = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name(crate::icons::PLAY)
                .label(play_label(page))
                .build(),
        )
        .sensitive(playable)
        .css_classes(["action", "compact", "suggested-action"])
        .build();
    {
        let (run, settings) = (actions.play.clone(), settings.clone());
        play.connect_clicked(move |_| run(None, settings.get()));
    }
    controls.append(&play);
    if let Some(first) = from_start(page) {
        let again = gtk::Button::builder()
            .label("Play from start")
            .css_classes(["action", "compact", "outlined"])
            .build();
        let (run, settings) = (actions.play.clone(), settings.clone());
        again.connect_clicked(move |_| run(Some(first), settings.get()));
        controls.append(&again);
    }
    let shuffle = gtk::ToggleButton::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name(crate::icons::SHUFFLE)
                .label("Shuffle")
                .build(),
        )
        .active(page.settings.shuffle)
        .css_classes(["action", "compact", "outlined"])
        .build();
    {
        let settings = settings.clone();
        shuffle.connect_toggled(move |button| {
            settings.set(PlaySettings {
                shuffle: button.is_active(),
                ..settings.get()
            })
        });
    }
    controls.append(&shuffle);
    let repeat_content = adw::ButtonContent::new();
    let repeat = gtk::Button::builder()
        .child(&repeat_content)
        .css_classes(["action", "compact", "outlined"])
        .build();
    let show_repeat = {
        let (button, content) = (repeat.clone(), repeat_content.clone());
        move |repeat: Repeat| {
            let (text, icon) = repeat_label(repeat);
            content.set_label(text);
            content.set_icon_name(icon);
            if matches!(repeat, Repeat::Off | Repeat::Unknown) {
                button.remove_css_class("on");
            } else {
                button.add_css_class("on");
            }
        }
    };
    show_repeat(page.settings.repeat);
    {
        let settings = settings.clone();
        repeat.connect_clicked(move |_| {
            let now = PlaySettings {
                repeat: next_repeat(settings.get().repeat),
                ..settings.get()
            };
            settings.set(now);
            show_repeat(now.repeat);
        });
    }
    controls.append(&repeat);
    let skip = gtk::Switch::builder()
        .active(page.settings.skip_watched)
        .valign(gtk::Align::Center)
        .build();
    {
        let settings = settings.clone();
        skip.connect_active_notify(move |switch| {
            settings.set(PlaySettings {
                skip_watched: switch.is_active(),
                ..settings.get()
            })
        });
    }
    let skip_box = gtk::Box::builder().spacing(10).margin_start(8).build();
    skip_box.append(&skip);
    skip_box.append(&gtk::Label::new(Some("Skip watched")));
    controls.append(&skip_box);
    controls.append(&gtk::Box::builder().hexpand(true).build());
    let pinned = page.playlist.pinned.is_some();
    let pin = gtk::ToggleButton::builder()
        .active(pinned)
        .valign(gtk::Align::Center)
        .css_classes(["action", "compact", "outlined", "square-action"])
        .build();
    show_pin(&pin, pinned);
    controls.append(&pin);
    // Edit for whoever may change it; a read-only playlist has none.
    if playlists::editor_controls(&page.playlist, false).edit {
        let edit = gtk::Button::builder()
            .icon_name(crate::icons::EDIT)
            .tooltip_text("Edit")
            .valign(gtk::Align::Center)
            .css_classes(["action", "compact", "outlined", "square-action"])
            .build();
        let open = actions.edit.clone();
        edit.connect_clicked(move |_| open(true));
        controls.append(&edit);
    }
    let wrap = gtk::ScrolledWindow::builder()
        .vscrollbar_policy(gtk::PolicyType::Never)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .child(&controls)
        .build();
    column.append(&wrap);
    header.append(&column);
    (header, pin)
}

/// An entry: its number, still, title, where it is from, and its note;
/// dimmed when its file cannot be read. A click opens its item.
fn entry_row(
    number: usize,
    entry: &PlaylistEntry,
    up_next: Option<EntryId>,
    cards: &Rc<Cards>,
    play_from: Rc<dyn Fn()>,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .activatable(true)
        .build();
    row.set_title(&playlists::title(&entry.item));
    row.set_subtitle(&playlists::entry_context(&entry.item));
    let count = label(&number.to_string(), &["dim-label", "numeric", "small-text"]);
    count.set_width_chars(3);
    count.set_xalign(1.0);
    row.add_prefix(&count);
    let thumb = views::tile(entry.item.id(), "", "thumb", THUMB);
    cards.art(
        &thumb,
        Facts::of(&entry.item).artwork,
        &[
            ArtworkKind::Thumb,
            ArtworkKind::Background,
            ArtworkKind::Poster,
        ],
        ArtworkSize::Small,
    );
    thumb.set_valign(gtk::Align::Center);
    thumb.set_margin_top(6);
    thumb.set_margin_bottom(6);
    row.add_prefix(&thumb);
    if let Some(note) = playlists::entry_note(entry, up_next) {
        let note = label(&note, &["small-text"]);
        if Some(entry.entry) == up_next {
            note.add_css_class("accent");
        } else {
            note.add_css_class("dim-label");
        }
        row.add_suffix(&note);
    }
    if Some(entry.entry) == up_next {
        row.add_css_class("up-next");
    }
    if entry.unavailable {
        row.add_css_class("unavailable");
    }
    let (open, item) = (cards.open.clone(), entry.item.clone());
    // A click opens the item, as a card does; its menu plays the
    // playlist from it.
    {
        let (open, item) = (open.clone(), item.clone());
        let menu = views::menu_popover(vec![vec![
            views::MenuEntry::new("Play from here", crate::icons::PLAY, move || play_from()),
            views::MenuEntry::new("Open", crate::icons::RIGHT, move || open(item.clone())),
        ]]);
        views::context_menu(row.upcast_ref(), &menu);
    }
    row.connect_activated(move |_| open(item.clone()));
    row
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(classes)
        .build()
}

/// One playlist "Add to playlist" lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddRow {
    pub id: String,
    pub name: String,
    /// What it holds and whose it is; for a greyed one, why it is.
    pub line: String,
    /// One click adds; otherwise greyed.
    pub adds: bool,
}

/// The playlists "Add to playlist" lists for an item of `kind`: only those
/// that take it, the ones it can go in first.
pub fn add_rows(list: &[PlaylistSummary], kind: MediaKind) -> Vec<AddRow> {
    playlists::add_targets(list, kind)
        .into_iter()
        .map(|(playlist, offer)| AddRow {
            id: playlist.id.clone(),
            name: playlist.name.clone(),
            line: offer.reason().map(str::to_owned).unwrap_or_else(|| {
                format!(
                    "{} · {}",
                    playlists::kinds_line(playlist.kinds),
                    playlists::owner_line(playlist)
                )
            }),
            adds: offer == Offer::Adds,
        })
        .collect()
}

/// The line under "Add … to a playlist": what the item is and, for one
/// that stands for many, what goes in.
pub fn add_context(item: &Item) -> String {
    match item {
        Item::Season(season) => format!(
            "{} · {} · each of its episodes is added, in order",
            season.show_title.clone().unwrap_or_default(),
            season.title
        ),
        Item::Show(_) => "Every episode is added, in airing order".to_owned(),
        Item::Album(album) => match &album.artist_name {
            Some(artist) => format!("{artist} · each of its tracks is added, in order"),
            None => "Each of its tracks is added, in order".to_owned(),
        },
        Item::Artist(_) => "Each album’s tracks are added, an album at a time".to_owned(),
        other => playlists::entry_context(other),
    }
}

/// "Add <item> to a playlist": the playlists that take it, one click
/// adding to one (`add` gets its id and name); the others greyed with
/// why; and New playlist… (`new`).
pub fn add_dialog(
    parent: &impl IsA<gtk::Widget>,
    item: &Item,
    rows: Vec<AddRow>,
    add: Rc<dyn Fn(String, String)>,
    new: Rc<dyn Fn()>,
) {
    let title = format!("Add {} to a playlist", playlists::title(item));
    let dialog = adw::Dialog::builder()
        .title(&title)
        .content_width(440)
        .build();
    let column = crate::dialog::column();
    column.append(&crate::dialog::dim(&add_context(item)));
    if rows.is_empty() {
        column.append(&crate::dialog::dim(
            "No playlist takes this yet: make one that holds it.",
        ));
    } else {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        for row in rows {
            let line = adw::ActionRow::builder()
                .use_markup(false)
                .activatable(row.adds)
                .sensitive(row.adds)
                .build();
            line.set_title(&row.name);
            line.set_subtitle(&row.line);
            line.add_prefix(&mosaic(&row.id, 32));
            let (add, closing) = (add.clone(), dialog.downgrade());
            line.connect_activated(move |_| {
                if let Some(dialog) = closing.upgrade() {
                    dialog.close();
                }
                add(row.id.clone(), row.name.clone());
            });
            list.append(&line);
        }
        column.append(&list);
    }
    let buttons = gtk::Box::builder().spacing(8).build();
    let make = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name(crate::icons::ADD)
                .label("New playlist…")
                .build(),
        )
        .hexpand(true)
        .halign(gtk::Align::Start)
        .css_classes(["flat"])
        .build();
    {
        let closing = dialog.downgrade();
        make.connect_clicked(move |_| {
            if let Some(dialog) = closing.upgrade() {
                dialog.close();
            }
            new();
        });
    }
    let cancel = gtk::Button::builder().label("Cancel").build();
    crate::dialog::closes(&cancel, &dialog);
    buttons.append(&make);
    buttons.append(&cancel);
    column.append(&buttons);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&column));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(parent));
}

/// "New playlist": a name, the kinds it holds (at least one; `preset`
/// ticked to begin with), and for an admin whether every user sees it.
/// `create` gets what to send.
pub fn new_playlist_dialog(
    parent: &impl IsA<gtk::Widget>,
    admin: bool,
    preset: Kinds,
    create: Rc<dyn Fn(String, Kinds, Visibility)>,
) {
    let dialog = adw::Dialog::builder()
        .title("New playlist")
        .content_width(420)
        .build();
    let column = crate::dialog::column();
    let fields = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let name = adw::EntryRow::builder()
        .use_markup(false)
        .title("Name")
        .build();
    fields.append(&name);
    column.append(&fields);
    column.append(&label("WHAT IT HOLDS", &["picker-label"]));
    let kinds = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let check = |title: &str, on: bool| {
        let check = gtk::CheckButton::builder().active(on).build();
        let row = adw::ActionRow::builder()
            .use_markup(false)
            .activatable_widget(&check)
            .build();
        row.set_title(title);
        row.add_prefix(&check);
        kinds.append(&row);
        check
    };
    let (movies, episodes, tracks) = (
        check("Movies", preset.movies),
        check("Episodes", preset.episodes),
        check("Music tracks", preset.tracks),
    );
    column.append(&kinds);
    column.append(&crate::dialog::dim(
        "Kinds can be added later, never taken away.",
    ));
    let server_wide = gtk::Switch::builder().valign(gtk::Align::Center).build();
    if admin {
        let who = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        let row = adw::ActionRow::builder()
            .use_markup(false)
            .title("Server-wide")
            .subtitle("Every user of the server sees it; admins edit it")
            .activatable_widget(&server_wide)
            .build();
        row.add_suffix(&server_wide);
        who.append(&row);
        column.append(&who);
    } else {
        column.append(&crate::dialog::dim(
            "Personal: only you see it until you share it.",
        ));
    }
    let refusal = crate::dialog::dim("");
    refusal.add_css_class("error");
    refusal.set_visible(false);
    column.append(&refusal);
    let buttons = crate::dialog::buttons();
    let cancel = gtk::Button::builder().label("Cancel").build();
    let make = gtk::Button::builder()
        .label("Create")
        .css_classes(["suggested-action"])
        .build();
    buttons.append(&cancel);
    buttons.append(&make);
    column.append(&buttons);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&column));
    dialog.set_child(Some(&toolbar));
    dialog.set_default_widget(Some(&make));
    crate::dialog::closes(&cancel, &dialog);
    let closing = dialog.downgrade();
    make.connect_clicked(move |_| {
        let kinds = Kinds {
            movies: movies.is_active(),
            episodes: episodes.is_active(),
            tracks: tracks.is_active(),
        };
        match playlists::new_playlist(&name.text(), kinds, server_wide.is_active()) {
            Ok((name, kinds, visibility)) => {
                if let Some(dialog) = closing.upgrade() {
                    dialog.close();
                }
                create(name, kinds, visibility);
            }
            Err(why) => {
                refusal.set_label(why);
                refusal.set_visible(true);
            }
        }
    });
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use spoolframe_linux_core::backend::{Client, Session};
    use spoolframe_linux_core::fake::{FakeClient, PLAYLIST_PAGE_LIMIT};
    use spoolframe_linux_core::testing::ready;

    use super::*;

    fn andrey(client: &FakeClient) -> Arc<dyn Session> {
        client.session(&FakeClient::profile("andrey")).unwrap()
    }

    fn page_of(session: &dyn Session, name: &str) -> PlaylistPage {
        let list = ready(session.playlists()).unwrap().playlists;
        let id = list.iter().find(|p| p.name == name).unwrap().id.clone();
        ready(session.playlist(&id, 0, PLAYLIST_PAGE_LIMIT)).unwrap()
    }

    #[test]
    fn a_playlist_never_played_plays_from_the_top() {
        let client = FakeClient::new(None);
        let page = page_of(andrey(&client).as_ref(), "Andor: the heists");
        assert_eq!(
            (play_label(&page), from_start(&page)),
            ("Play".to_owned(), None)
        );
    }

    #[test]
    fn a_playlist_played_before_continues_and_can_start_again() {
        let client = FakeClient::new(None);
        let session = andrey(&client);
        let page = page_of(session.as_ref(), "Andor: the heists");
        let first = page.entries[0].entry;
        ready(session.play_playlist(
            &page.playlist.id,
            Some(page.entries[1].entry),
            page.settings,
        ))
        .unwrap();
        let page = page_of(session.as_ref(), "Andor: the heists");
        let label = play_label(&page);
        assert_eq!(
            (label.starts_with("Continue · S"), from_start(&page)),
            (true, Some(first))
        );
    }

    #[test]
    fn a_season_is_offered_only_the_playlists_that_take_episodes() {
        let client = FakeClient::new(None);
        let list = ready(andrey(&client).playlists()).unwrap().playlists;
        let names: Vec<_> = add_rows(&list, MediaKind::Season)
            .into_iter()
            .map(|row| (row.name, row.adds))
            .collect();
        assert_eq!(
            names,
            [
                ("Andor: the heists".to_owned(), true),
                ("Movie night".to_owned(), true)
            ]
        );
    }

    #[test]
    fn a_greyed_playlist_says_why() {
        let client = FakeClient::new(None);
        let alice = client.session(&FakeClient::profile("alice")).unwrap();
        ready(alice.unlock(spoolframe_linux_core::fake::FAKE_PIN)).unwrap();
        let list = ready(alice.playlists()).unwrap().playlists;
        assert_eq!(
            add_rows(&list, MediaKind::Movie),
            [AddRow {
                id: list
                    .iter()
                    .find(|p| p.name == "New this month")
                    .unwrap()
                    .id
                    .clone(),
                name: "New this month".into(),
                line: "Server-wide · admins edit".into(),
                adds: false,
            }]
        );
    }

    #[test]
    fn a_playable_row_says_what_it_holds_and_whose_it_is() {
        let client = FakeClient::new(None);
        let list = ready(andrey(&client).playlists()).unwrap().playlists;
        let lines: Vec<_> = add_rows(&list, MediaKind::Track)
            .into_iter()
            .map(|row| row.line)
            .collect();
        assert_eq!(lines, ["Music · Shared with you · you can edit"]);
    }

    #[test]
    fn repeat_goes_round_off_all_one() {
        let mut seen = vec![Repeat::Off];
        for _ in 0..3 {
            seen.push(next_repeat(*seen.last().unwrap()));
        }
        assert_eq!(seen, [Repeat::Off, Repeat::All, Repeat::One, Repeat::Off]);
    }

    #[test]
    fn the_header_says_how_many_whose_and_what_it_holds() {
        let client = FakeClient::new(None);
        let page = page_of(andrey(&client).as_ref(), "Movie night");
        assert_eq!(
            (header_line(&page), holds_line(page.playlist.kinds)),
            (
                "2 films, 1 episode · Yours".to_owned(),
                "Holds movies, episodes".to_owned()
            )
        );
    }

    #[test]
    fn the_menu_offers_to_pin_what_is_not_pinned_and_to_unpin_what_is() {
        let client = FakeClient::new(None);
        let list = ready(andrey(&client).playlists()).unwrap().playlists;
        let labels: Vec<_> = ["Andor: the heists", "Movie night"]
            .into_iter()
            .map(|name| {
                let playlist = list.iter().find(|p| p.name == name).unwrap();
                pin_label(playlist.pinned.is_some()).0
            })
            .collect();
        assert_eq!(labels, ["Unpin from the sidebar", "Pin to the sidebar"]);
    }
}
