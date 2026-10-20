//! Playlists (PX-40) on screen: the Playlists section's cards in their
//! groups (board LinuxPlaylists), a playlist's own page (LinuxPlaylist,
//! LinuxPlaylistMissing, LinuxPlaylistStates 3 and 7), and "New playlist".
//! What they show is worked out in the core's `playlists`; this lays it
//! out.

use std::cell::{Cell, RefCell};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use spoolframe_linux_core::backend::{
    ArtworkSize, BackendError, Builtin, EntryId, Kinds, MAX_ART_BYTES, MAX_ART_PIXELS,
    PlaySettings, PlaylistEdit, PlaylistEntry, PlaylistOwnerKind, PlaylistPage, PlaylistRef,
    PlaylistSummary, PlaylistUpdate, Repeat, Visibility,
};
use spoolframe_linux_core::playlists::{self, Offer};
use spoolframe_types::{ArtworkId, ArtworkKind, Item, MediaKind};

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

/// The size to ask the server for a playlist's picture `size` px wide:
/// twice that, for a HiDPI screen.
pub fn picture_size(size: i32) -> ArtworkSize {
    match size {
        ..=80 => ArtworkSize::Thumb,
        81..=160 => ArtworkSize::Small,
        _ => ArtworkSize::Medium,
    }
}

/// A playlist's picture: the server's (PX-53: its cover, its mosaic, or
/// Liked's and All's tiles) where it sends one, drawn over the app's own
/// once it loads, so a picture that fails leaves the app's in place.
/// The app's own is Liked's heart and All's tile (PX-43, board
/// LinuxPlaylists), or for any other its [`mosaic`].
pub fn picture(
    id: &str,
    builtin: Option<Builtin>,
    artwork: Option<&ArtworkId>,
    art: Option<&Rc<crate::artwork::Art>>,
    size: i32,
) -> gtk::Widget {
    let own = own_picture(id, builtin, size);
    let (Some(artwork), Some(art)) = (artwork, art) else {
        return own;
    };
    let tile = gtk::Overlay::builder()
        .child(&own)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Center)
        .overflow(gtk::Overflow::Hidden)
        .css_classes(["mosaic"])
        .build();
    if size < 40 {
        tile.add_css_class("small");
    }
    art.show_id(&tile, artwork, picture_size(size));
    tile.upcast()
}

fn own_picture(id: &str, builtin: Option<Builtin>, size: i32) -> gtk::Widget {
    let (icon, class) = match builtin {
        None => return mosaic(id, size),
        Some(Builtin::Liked) => (crate::icons::HEART_FULL, "builtin-liked"),
        Some(_) => (crate::icons::ALL_TRACKS, "builtin-all"),
    };
    let tile = gtk::Box::builder()
        .width_request(size)
        .height_request(size)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Center)
        .css_classes(["mosaic", "builtin-tile", class])
        .build();
    if size < 40 {
        tile.add_css_class("small");
    }
    tile.append(
        &gtk::Image::builder()
            .icon_name(icon)
            .pixel_size(size * 7 / 16)
            .hexpand(true)
            .halign(gtk::Align::Center)
            .build(),
    );
    tile.upcast()
}

/// What an item page's row says of a playlist holding it: Liked is built
/// in; one shared with the user, whose it is by name where the server
/// says (sharing by name), as a card does; any other, whose it is.
pub fn ref_subtitle(playlist: &PlaylistRef) -> String {
    match (playlist.builtin, playlist.owner_kind, &playlist.owner_name) {
        (Some(_), _, _) => "Built in".to_owned(),
        (None, PlaylistOwnerKind::SharedWithMe, Some(owner)) => format!("{owner}’s"),
        (None, kind, _) => ref_line(kind).to_owned(),
    }
}

/// Whose a playlist an item's page lists is: all a reference says (it
/// carries no counts).
pub fn ref_line(kind: PlaylistOwnerKind) -> &'static str {
    match kind {
        PlaylistOwnerKind::Mine => "Yours",
        PlaylistOwnerKind::ServerWide => "Server-wide",
        PlaylistOwnerKind::SharedWithMe | PlaylistOwnerKind::Unknown => "Shared with you",
    }
}

/// The playlists an item's page lists, if it lists any: none without the
/// feature, and no section when none hold the item.
pub fn shown_refs(has_playlists: bool, refs: &[PlaylistRef]) -> Option<&[PlaylistRef]> {
    (has_playlists && !refs.is_empty()).then_some(refs)
}

/// What a row of an item page's Playlists does, by the playlist's id: a
/// click opens it, and its menu plays it.
pub struct RefActions {
    pub open: Rc<dyn Fn(String)>,
    pub play: Rc<dyn Fn(String)>,
}

/// An item page's Playlists (board LinuxPlaylistRows): a row per
/// playlist, its mosaic, name and whose it is. A show's are tabs after its
/// seasons instead (the pages' `show`).
pub fn refs_section(
    refs: &[PlaylistRef],
    actions: &RefActions,
    art: Option<&Rc<crate::artwork::Art>>,
) -> gtk::Widget {
    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .valign(gtk::Align::Start)
        .css_classes(["playlist-refs"])
        .build();
    section.append(&label("Playlists", &["title-3"]));
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    for playlist in refs {
        let row = adw::ActionRow::builder()
            .use_markup(false)
            .activatable(true)
            .build();
        row.set_title(&playlist.name);
        row.set_subtitle(&ref_subtitle(playlist));
        row.add_prefix(&picture(
            &playlist.id,
            playlist.builtin,
            playlist.artwork.as_ref(),
            art,
            36,
        ));
        let (open, id) = (actions.open.clone(), playlist.id.clone());
        row.connect_activated(move |_| open(id.clone()));
        let (play, id) = (actions.play.clone(), playlist.id.clone());
        let menu = views::menu_popover(vec![vec![views::MenuEntry::new(
            "Play",
            crate::icons::PLAY,
            move || play(id.clone()),
        )]]);
        views::context_menu(row.upcast_ref(), &menu);
        list.append(&row);
    }
    section.append(&list);
    section.upcast()
}

/// What a playlist's card, row and menu do, by its id.
pub struct ListActions {
    pub open: Rc<dyn Fn(String)>,
    /// Plays it (`true`: shuffled).
    pub play: Rc<dyn Fn(String, bool)>,
    /// Pins it, last (`true`), or unpins it.
    pub pin: Rc<dyn Fn(String, bool)>,
}

/// The Playlists section: Browse all over Music (Liked and All, PX-43),
/// My playlists, Shared with me and Server-wide, each a row of cards; a
/// group with none is left out.
pub fn section(
    list: &[PlaylistSummary],
    actions: &Rc<ListActions>,
    art: Option<&Rc<crate::artwork::Art>>,
    browse: Rc<dyn Fn()>,
) -> Vec<gtk::Box> {
    let groups = playlists::sections(list);
    let rows: Vec<gtk::Box> = [
        ("Music", groups.builtin),
        ("My playlists", groups.mine),
        ("Shared with me", groups.shared_with_me),
        ("Server-wide", groups.server_wide),
    ]
    .into_iter()
    .filter(|(_, group)| !group.is_empty())
    .map(|(title, group)| views::row(title, group.into_iter().map(|p| card(p, actions, art))))
    .collect();
    if !rows.is_empty() {
        // The section's own, over whichever groups there are: someone
        // with no playlists of their own browses the others too.
        let button = gtk::Button::builder()
            .label("Browse all")
            .css_classes(["flat"])
            .build();
        button.connect_clicked(move |_| browse());
        let bar = gtk::Box::builder().halign(gtk::Align::End).build();
        bar.append(&button);
        return std::iter::once(bar).chain(rows).collect();
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
fn card(
    playlist: &PlaylistSummary,
    actions: &Rc<ListActions>,
    art: Option<&Rc<crate::artwork::Art>>,
) -> gtk::Widget {
    sized_card(playlist, actions, art, CARD)
}

pub struct PlaylistBrowser {
    pub widget: gtk::Box,
    list: Rc<RefCell<Vec<PlaylistSummary>>>,
    populate: Rc<dyn Fn()>,
}

impl PlaylistBrowser {
    /// Refresh the cards without replacing the page or its filter state.
    pub fn update(&self, list: &[PlaylistSummary]) {
        *self.list.borrow_mut() = list.to_vec();
        (self.populate)();
    }
}

/// Which of the browser's kinds are ticked: movies, episodes, music.
type Ticked = [bool; 3];

/// Whether a playlist of `kinds` shows with `ticked`: it holds one of
/// them.
fn ticked_holds(ticked: Ticked, kinds: Kinds) -> bool {
    (ticked[0] && kinds.movies) || (ticked[1] && kinds.episodes) || (ticked[2] && kinds.tracks)
}

/// All playlists, as cards: a search by name and the kinds they hold
/// filter them. The cards are made once for each list and width; the
/// search and the kinds only hide and show them.
pub fn browse(
    list: &[PlaylistSummary],
    actions: &Rc<ListActions>,
    art: Option<&Rc<crate::artwork::Art>>,
) -> PlaylistBrowser {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 20);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search playlists by name")
        .hexpand(true)
        .build();
    body.append(&search);
    let query = Rc::new(RefCell::new(String::new()));
    let filters = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let buttons =
        [("Movies", false), ("Episodes", false), ("Music", true)].map(|(name, active)| {
            let button = gtk::ToggleButton::builder()
                .label(name)
                .active(active)
                .build();
            filters.append(&button);
            button
        });
    body.append(&filters);
    let flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .column_spacing(16)
        .row_spacing(24)
        .homogeneous(true)
        .css_classes(["playlist-browser-grid"])
        .hexpand(true)
        .build();
    body.append(&flow);
    let empty = label("No playlists match your search and filters", &["dim-label"]);
    body.append(&empty);
    let width = Rc::new(Cell::new(0));
    let selected = Rc::new(Cell::new(
        buttons.each_ref().map(|button| button.is_active()),
    ));
    let list = Rc::new(RefCell::new(list.to_vec()));
    // Each card's name, as a search compares it, and kinds, in the
    // flow's order.
    let shown: Rc<RefCell<Vec<(String, Kinds)>>> = Rc::default();
    {
        let (shown, query, selected) = (shown.clone(), query.clone(), selected.clone());
        flow.set_filter_func(move |child| {
            let shown = shown.borrow();
            let Some((name, kinds)) = shown.get(child.index() as usize) else {
                return false;
            };
            playlists::matches_query(name, &query.borrow()) && ticked_holds(selected.get(), *kinds)
        });
    }
    let filter: Rc<dyn Fn()> = {
        let (flow, empty, shown) = (flow.clone(), empty.clone(), shown.clone());
        let (query, selected) = (query.clone(), selected.clone());
        Rc::new(move || {
            flow.invalidate_filter();
            let (query, ticked) = (query.borrow(), selected.get());
            let any = shown.borrow().iter().any(|(name, kinds)| {
                playlists::matches_query(name, &query) && ticked_holds(ticked, *kinds)
            });
            empty.set_visible(!any);
        })
    };
    let rebuild: Rc<dyn Fn()> = {
        let (list, actions, art) = (list.clone(), actions.clone(), art.cloned());
        let (flow, width, filter) = (flow.clone(), width.clone(), filter.clone());
        Rc::new(move || {
            if width.get() <= 0 {
                return;
            }
            while let Some(child) = flow.first_child() {
                flow.remove(&child);
            }
            let (columns, size) = browse_dimensions(width.get());
            flow.set_min_children_per_line(1);
            flow.set_max_children_per_line(columns);
            let list = list.borrow();
            shown.replace(
                list.iter()
                    .map(|p| (playlists::search_text(&p.name), p.kinds))
                    .collect(),
            );
            for playlist in list.iter() {
                flow.append(&sized_card(playlist, &actions, art.as_ref(), size));
            }
            filter();
        })
    };
    // A resize is emitted during GTK allocation. Replacing the flow's
    // children there leaves GTK allocating new cards using the old layout
    // (including zero-width slots). Rebuild once allocation has finished.
    let pending = Rc::new(Cell::new(false));
    let populate: Rc<dyn Fn()> = Rc::new(move || {
        if pending.replace(true) {
            return;
        }
        let (pending, rebuild) = (pending.clone(), rebuild.clone());
        glib::idle_add_local_once(move || {
            pending.set(false);
            rebuild();
        });
    });
    {
        let filter = filter.clone();
        search.connect_changed(move |entry| {
            *query.borrow_mut() = playlists::search_text(&entry.text());
            filter();
        });
    }
    for (index, button) in buttons.iter().enumerate() {
        let (filter, selected) = (filter.clone(), selected.clone());
        button.connect_toggled(move |button| {
            let mut kinds = selected.get();
            kinds[index] = button.is_active();
            selected.set(kinds);
            filter();
        });
    }
    // Watch the available width rather than the cards' natural widths,
    // so a resize can both add and remove columns.
    let measure = gtk::DrawingArea::builder()
        .height_request(1)
        .hexpand(true)
        .build();
    let resized = populate.clone();
    measure.connect_resize(move |_, available, _| {
        if available > 0 && width.replace(available) != available {
            resized();
        }
    });
    body.prepend(&measure);
    PlaylistBrowser {
        widget: body,
        list,
        populate,
    }
}

fn browse_dimensions(width: i32) -> (u32, i32) {
    let columns = ((width + 16) / 256).clamp(1, 12);
    (
        columns as u32,
        ((width - (columns - 1) * 16) / columns - 4).max(1),
    )
}

fn sized_card(
    playlist: &PlaylistSummary,
    actions: &Rc<ListActions>,
    art: Option<&Rc<crate::artwork::Art>>,
    size: i32,
) -> gtk::Widget {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.append(&picture(
        &playlist.id,
        playlist.builtin,
        playlist.artwork.as_ref(),
        art,
        size,
    ));
    let caption = gtk::Box::new(gtk::Orientation::Vertical, 2);
    caption.append(&views::caption_label(&playlist.name, &["heading"]));
    caption.append(&views::caption_label(
        &playlists::owner_line(playlist),
        &["dim-label", "caption"],
    ));
    body.append(&caption);
    let button = gtk::Button::builder()
        .child(&body)
        .width_request(size)
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

/// What the Repeat button says and shows for a playlist's `repeat`.
fn repeat_label(repeat: Repeat) -> (&'static str, &'static str) {
    let repeat = crate::queue::Repeat::from(repeat);
    let text = match repeat {
        crate::queue::Repeat::All => "Repeat all",
        crate::queue::Repeat::One => "Repeat one",
        crate::queue::Repeat::Off => "Repeat",
    };
    (text, repeat.icon())
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
    pub start_music: Rc<dyn Fn(Option<EntryId>, PlaySettings)>,
    pub random: Rc<dyn Fn(PlaySettings)>,
    pub settings_changed: Rc<dyn Fn(PlaySettings)>,
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
    /// Whom it can be shared with, for the editor's Add someone; none
    /// where the server does not share by name.
    pub people: Option<crate::share_picker::People>,
    /// Its cover (PX-53); none where the server has no covers.
    pub cover: Option<CoverActions>,
}

/// What the editor's Cover does.
pub struct CoverActions {
    /// Opens the file chooser for a picture, and uploads it.
    pub choose: Rc<dyn Fn()>,
    /// Takes the uploaded cover away.
    pub remove: Rc<dyn Fn()>,
}

/// Whether the editor offers to change `playlist`'s cover: to whoever may
/// edit its entries, as the server allows, and never on Liked or All.
pub fn cover_editable(playlist: &PlaylistSummary) -> bool {
    playlist.can_edit && playlist.builtin.is_none()
}

/// `bytes` refused before they are sent, where the app can tell they are
/// past the server's limits (PX-53): too many bytes, or too many pixels
/// by the header. A header the app cannot read (HEIC) is the server's to
/// judge.
pub fn cover_check(bytes: &[u8]) -> Result<(), BackendError> {
    let too_large = BackendError::ImageTooLarge {
        max_bytes: MAX_ART_BYTES,
        max_pixels: MAX_ART_PIXELS,
    };
    if bytes.len() as u64 > MAX_ART_BYTES {
        return Err(too_large);
    }
    match crate::artwork::image_size(bytes) {
        Some((width, height)) if width.saturating_mul(height) > MAX_ART_PIXELS => Err(too_large),
        _ => Ok(()),
    }
}

/// What the app says when a cover change is refused.
pub fn cover_refusal(error: &BackendError) -> String {
    match error {
        BackendError::NotAllowed => {
            "Only someone who can edit this playlist can change its cover.".to_owned()
        }
        other => other.to_string(),
    }
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
    sync_settings: Option<Rc<dyn Fn(PlaySettings)>>,
}

impl PlaylistView {
    pub fn set_play_settings(&self, settings: PlaySettings) {
        if let Some(sync) = &self.sync_settings {
            sync(settings);
        }
    }

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
        let settings = Rc::new(Cell::new(playlists::play_settings(
            page.playlist.kinds,
            page.settings,
        )));
        let (pin, editing, sync_settings) = match mode {
            Mode::View => {
                let (header, pin, sync) = header(page, &actions, &settings, cards.art.as_ref());
                widget.append(&header);
                (pin, None, Some(sync))
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
                // The cover first, for whoever may edit the entries too.
                let cover = actions
                    .cover
                    .as_ref()
                    .filter(|_| cover_editable(&page.playlist))
                    .map(|cover| {
                        crate::playlist_edit::cover(&page.playlist, cover, cards.art.as_ref())
                    });
                match (cover, side) {
                    (Some(cover), Some((side, _))) => {
                        side.prepend(&cover);
                        columns.append(&side);
                    }
                    (Some(cover), None) => columns.append(&cover),
                    (None, Some((side, _))) => columns.append(&side),
                    (None, None) => {}
                }
                // Not shown: the pin is the page's, not the editor's.
                let pin = gtk::ToggleButton::new();
                (pin, Some((page.playlist.revision, Rc::default())), None)
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
            let empty = adw::StatusPage::builder()
                .icon_name(crate::icons::PLAYLISTS)
                .title(format!("{} is empty", page.playlist.name))
                .description("Use Add to playlist on a film, an episode or a season.")
                .build();
            // Liked fills by hearts, never by Add to playlist.
            if page.playlist.builtin.is_some() {
                empty.set_description(Some("Like a track or an album with its heart."));
            }
            widget_for_entries.append(&empty);
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
            up_next: if playlists::music_only(page.playlist.kinds) {
                None
            } else {
                playlists::up_next(page)
            },
            cards,
            editing,
            actions: actions.clone(),
            settings,
            sync_settings,
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
                    (page.playlist.builtin.is_none()
                        && playlists::editor_controls(&page.playlist, false).edit)
                        .then(|| {
                            let (edit, revision, entry) = (
                                self.actions.edit_entries.clone(),
                                page.playlist.revision,
                                entry.entry,
                            );
                            Rc::new(move || edit(revision, vec![PlaylistEdit::Remove { entry }]))
                                as Rc<dyn Fn()>
                        }),
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
    art: Option<&Rc<crate::artwork::Art>>,
) -> (gtk::Box, gtk::ToggleButton, Rc<dyn Fn(PlaySettings)>) {
    let header = gtk::Box::builder().spacing(28).build();
    header.append(&picture(
        &page.playlist.id,
        page.playlist.builtin,
        page.playlist.artwork.as_ref(),
        art,
        HEADER,
    ));
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

    let controls = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .column_spacing(10)
        .row_spacing(10)
        .min_children_per_line(1)
        .max_children_per_line(8)
        .margin_top(6)
        .build();
    let playable = page.playable > 0;
    let music_only = playlists::music_only(page.playlist.kinds);
    let play = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name(crate::icons::PLAY)
                .label(if !music_only && page.place.is_some() {
                    "Continue"
                } else {
                    "Play"
                })
                .build(),
        )
        .sensitive(playable)
        .tooltip_text(if music_only {
            "Play from the first song".to_owned()
        } else {
            play_label(page)
        })
        .css_classes(["action", "compact", "suggested-action"])
        .build();
    {
        let run = if music_only {
            actions.start_music.clone()
        } else {
            actions.play.clone()
        };
        let settings = settings.clone();
        let from = music_only.then(|| from_start(page)).flatten();
        play.connect_clicked(move |_| run(from, settings.get()));
    }
    controls.append(&play);
    if music_only {
        let random = gtk::Button::builder()
            .label("Play random")
            .tooltip_text("Play a random song and turn shuffle on")
            .sensitive(playable)
            .css_classes(["action", "compact", "outlined"])
            .build();
        let (run, settings) = (actions.random.clone(), settings.clone());
        random.connect_clicked(move |_| {
            run(PlaySettings {
                shuffle: true,
                ..settings.get()
            })
        });
        controls.append(&random);
    } else if let Some(first) = from_start(page) {
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
        let (settings, changed) = (settings.clone(), actions.settings_changed.clone());
        shuffle.connect_toggled(move |button| {
            change_settings(&settings, &*changed, |now| PlaySettings {
                shuffle: button.is_active(),
                ..now
            });
        });
    }
    if !music_only {
        controls.append(&shuffle);
    }
    let repeat_content = adw::ButtonContent::new();
    let repeat = gtk::Button::builder()
        .child(&repeat_content)
        .css_classes(["action", "compact", "outlined"])
        .build();
    let show_repeat = {
        let (button, content) = (repeat.clone(), repeat_content.clone());
        Rc::new(move |repeat: Repeat| {
            let (text, icon) = repeat_label(repeat);
            content.set_label(text);
            content.set_icon_name(icon);
            if crate::queue::Repeat::from(repeat) == crate::queue::Repeat::Off {
                button.remove_css_class("on");
            } else {
                button.add_css_class("on");
            }
        })
    };
    show_repeat(page.settings.repeat);
    {
        let (settings, changed) = (settings.clone(), actions.settings_changed.clone());
        let show_repeat = show_repeat.clone();
        repeat.connect_clicked(move |_| {
            let now = change_settings(&settings, &*changed, |now| PlaySettings {
                repeat: crate::queue::Repeat::from(now.repeat).next().into(),
                ..now
            });
            show_repeat(now.repeat);
        });
    }
    if !music_only {
        controls.append(&repeat);
    }
    let skip = gtk::Switch::builder()
        .active(page.settings.skip_watched)
        .valign(gtk::Align::Center)
        .build();
    {
        // As Shuffle and Repeat: the playback playing this playlist takes
        // it, or a later sync of its modes would put the switch back.
        let (settings, changed) = (settings.clone(), actions.settings_changed.clone());
        skip.connect_active_notify(move |switch| {
            change_settings(&settings, &*changed, |now| PlaySettings {
                skip_watched: switch.is_active(),
                ..now
            });
        });
    }
    let skip_box = gtk::Box::builder().spacing(10).margin_start(8).build();
    skip_box.append(&skip);
    skip_box.append(&gtk::Label::new(Some("Skip watched")));
    if page.playlist.kinds.movies || page.playlist.kinds.episodes {
        controls.append(&skip_box);
    }
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
    column.append(&controls);
    header.append(&column);
    let settings = settings.clone();
    let sync = Rc::new(move |now: PlaySettings| {
        settings.set(now);
        shuffle.set_active(now.shuffle);
        show_repeat(now.repeat);
        skip.set_active(now.skip_watched);
    });
    (header, pin, sync)
}

/// The settings shown, changed by `change`: kept, and told on when that
/// changed them (not when a sync from the playback set a control to what
/// they are already). What they are now.
fn change_settings(
    settings: &Cell<PlaySettings>,
    changed: &dyn Fn(PlaySettings),
    change: impl FnOnce(PlaySettings) -> PlaySettings,
) -> PlaySettings {
    let now = change(settings.get());
    if settings.replace(now) != now {
        changed(now);
    }
    now
}

/// An entry: its number, still, title, where it is from, and its note;
/// dimmed when its file cannot be read. A click plays the playlist from it.
fn entry_row(
    number: usize,
    entry: &PlaylistEntry,
    up_next: Option<EntryId>,
    cards: &Rc<Cards>,
    play_from: Rc<dyn Fn()>,
    remove: Option<Rc<dyn Fn()>>,
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
    let music = matches!(entry.item, Item::Track(_));
    let thumb = views::tile(
        entry.item.id(),
        "",
        "thumb",
        if music { (THUMB.1, THUMB.1) } else { THUMB },
    );
    cards.art(
        &thumb,
        Facts::of(&entry.item).artwork,
        if music {
            &[ArtworkKind::Poster, ArtworkKind::Thumb]
        } else {
            &[
                ArtworkKind::Thumb,
                ArtworkKind::Background,
                ArtworkKind::Poster,
            ]
        },
        ArtworkSize::Small,
    );
    thumb.set_valign(gtk::Align::Center);
    thumb.set_margin_top(6);
    thumb.set_margin_bottom(6);
    row.add_prefix(&thumb);
    // A track's heart (PX-43), as the server says it is.
    if let (Some(hearts), Item::Track(track)) = (&cards.hearts, &entry.item) {
        hearts.learn([(track.id.clone(), entry.liked)]);
        row.add_suffix(&hearts.track(&track.id));
    }
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
    let mut groups = if let Item::Track(track) = &entry.item {
        let enqueue = cards.enqueue.as_ref().map(|enqueue| {
            let (enqueue, item) = (enqueue.clone(), entry.item.clone());
            Rc::new(move |at| enqueue(item.clone(), at)) as Rc<dyn Fn(views::Enqueue)>
        });
        views::track_menu_entries(
            track,
            &cards.open,
            true,
            enqueue,
            cards.add_to_playlist.as_ref(),
        )
    } else {
        let (open, item) = (cards.open.clone(), entry.item.clone());
        let play_from = play_from.clone();
        vec![vec![
            views::MenuEntry::new("Play from here", crate::icons::PLAY, move || play_from()),
            views::MenuEntry::new("Open", crate::icons::RIGHT, move || open(item.clone())),
        ]]
    };
    if let Some(remove) = remove {
        groups.push(vec![views::MenuEntry {
            destructive: true,
            ..views::MenuEntry::new("Remove from playlist", crate::icons::REMOVE, move || {
                remove()
            })
        }]);
    }
    let menu = views::menu_popover(groups);
    views::context_menu(row.upcast_ref(), &menu);
    row.connect_activated(move |_| play_from());
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
    pub builtin: Option<Builtin>,
    /// The server's picture of it (PX-53), if it sends one.
    pub artwork: Option<ArtworkId>,
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
            builtin: playlist.builtin,
            artwork: playlist.artwork.clone(),
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
    art: Option<&Rc<crate::artwork::Art>>,
) {
    let title = format!("Add {} to a playlist", playlists::title(item));
    let dialog = adw::Dialog::builder()
        .title(&title)
        .content_width(440)
        .content_height(560)
        .build();
    let column = crate::dialog::column();
    column.append(&crate::dialog::dim(&add_context(item)));
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search playlists by name")
        .hexpand(true)
        .build();
    column.append(&search);
    if rows.is_empty() {
        column.append(&crate::dialog::dim(
            "No playlist takes this yet: make one that holds it.",
        ));
    } else {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        let mut searchable = Vec::new();
        for row in rows {
            let line = adw::ActionRow::builder()
                .use_markup(false)
                .activatable(row.adds)
                .sensitive(row.adds)
                .build();
            line.set_title(&row.name);
            line.set_subtitle(&row.line);
            line.add_prefix(&picture(
                &row.id,
                row.builtin,
                row.artwork.as_ref(),
                art,
                32,
            ));
            searchable.push((playlists::search_text(&row.name), line.clone()));
            let (add, closing) = (add.clone(), dialog.downgrade());
            line.connect_activated(move |_| {
                if let Some(dialog) = closing.upgrade() {
                    dialog.close();
                }
                add(row.id.clone(), row.name.clone());
            });
            list.append(&line);
        }
        let empty = crate::dialog::dim("No playlists match your search.");
        empty.set_visible(false);
        let results = gtk::Box::new(gtk::Orientation::Vertical, 12);
        results.append(&list);
        results.append(&empty);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(false)
            .vexpand(true)
            .child(&results)
            .build();
        let adjustment = scroller.vadjustment();
        search.connect_changed(move |search| {
            let query = playlists::search_text(&search.text());
            let mut any = false;
            for (name, row) in &searchable {
                let matches = playlists::matches_query(name, &query);
                row.set_visible(matches);
                any |= matches;
            }
            empty.set_visible(!any);
            list.set_visible(any);
            adjustment.set_value(0.0);
        });
        column.append(&scroller);
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

    #[test]
    #[ignore = "needs a GTK display"]
    fn playlist_rows_play_in_context_and_controls_fit() {
        gtk::init().unwrap();
        let drain = || {
            let context = glib::MainContext::default();
            while context.pending() {
                context.iteration(false);
            }
        };
        let client = FakeClient::new(None);
        let session = client.session(&FakeClient::profile("andrey")).unwrap();
        let list = ready(session.playlists()).unwrap().playlists;
        let all = list.iter().find(|p| p.name == "All").unwrap();
        let mut page = ready(session.playlist(&all.id, 0, 200)).unwrap();
        let opened = Rc::new(Cell::new(0));
        let played = Rc::new(Cell::new(0));
        let cards = Rc::new(Cards {
            open: {
                let opened = opened.clone();
                Rc::new(move |_| opened.set(opened.get() + 1))
            },
            art: None,
            set_watched: None,
            hide: None,
            enqueue: None,
            open_person: None,
            add_to_playlist: None,
            hearts: None,
        });
        let removed = Rc::new(Cell::new(false));
        let row = entry_row(
            1,
            &page.entries[0],
            None,
            &cards,
            {
                let played = played.clone();
                Rc::new(move || played.set(played.get() + 1))
            },
            Some({
                let removed = removed.clone();
                Rc::new(move || removed.set(true))
            }),
        );
        button_named(row.upcast_ref(), "Remove from playlist")
            .unwrap()
            .emit_clicked();
        assert!(removed.get());
        row.emit_by_name::<()>("activated", &[]);
        assert_eq!((played.get(), opened.get()), (1, 0));
        page.place = Some(page.entries[0].entry);
        if let Item::Track(track) = &mut page.entries[0].item {
            track.title = "A very long song title ".repeat(20);
        }
        let requested = Rc::new(Cell::new(None));
        let random_requested = Rc::new(Cell::new(None));
        let changed = Rc::new(Cell::new(None));
        let actions = PageActions {
            start_music: {
                let requested = requested.clone();
                Rc::new(move |from, settings| requested.set(Some((from, settings))))
            },
            random: {
                let requested = random_requested.clone();
                Rc::new(move |settings| requested.set(Some(settings)))
            },
            settings_changed: {
                let changed = changed.clone();
                Rc::new(move |settings| changed.set(Some(settings)))
            },
            play: {
                let requested = requested.clone();
                Rc::new(move |from, settings| requested.set(Some((from, settings))))
            },
            pin: Rc::new(|_| {}),
            more: Rc::new(|_| {}),
            edit: Rc::new(|_| {}),
            done: Rc::new(|_| {}),
            edit_entries: Rc::new(|_, _| {}),
            update: Rc::new(|_| {}),
            delete: Rc::new(|| {}),
            people: None,
            cover: None,
        };
        let (header_widget, _, sync_modes) =
            header(&page, &actions, &Rc::new(Cell::new(page.settings)), None);
        assert!(header_widget.measure(gtk::Orientation::Horizontal, -1).0 < 900);
        fn button_named(widget: &gtk::Widget, name: &str) -> Option<gtk::Button> {
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                let label = button.label().map(|s| s.to_string()).or_else(|| {
                    button
                        .child()?
                        .downcast::<adw::ButtonContent>()
                        .ok()
                        .map(|c| c.label().to_string())
                });
                if label.as_deref() == Some(name) {
                    return Some(button.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(button) = button_named(&widget, name) {
                    return Some(button);
                }
                child = widget.next_sibling();
            }
            None
        }
        let modes = PlaySettings {
            shuffle: true,
            repeat: Repeat::All,
            skip_watched: false,
        };
        sync_modes(modes);
        button_named(header_widget.upcast_ref(), "Play")
            .unwrap()
            .emit_clicked();
        assert_eq!(requested.get(), Some((from_start(&page), modes)));
        assert!(button_named(header_widget.upcast_ref(), "Repeat all").is_none());
        assert!(button_named(header_widget.upcast_ref(), "Shuffle").is_none());
        sync_modes(PlaySettings::default());
        button_named(header_widget.upcast_ref(), "Play random")
            .unwrap()
            .emit_clicked();
        assert!(random_requested.get().unwrap().shuffle);

        fn has_skip(widget: &gtk::Widget) -> bool {
            if widget
                .downcast_ref::<gtk::Label>()
                .is_some_and(|label| label.text() == "Skip watched")
            {
                return true;
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                if has_skip(&current) {
                    return true;
                }
                child = current.next_sibling();
            }
            false
        }
        assert!(!has_skip(header_widget.upcast_ref()));
        page.playlist.kinds.episodes = true;
        let (mixed, _, _) = header(&page, &actions, &Rc::new(Cell::new(page.settings)), None);
        assert!(has_skip(mixed.upcast_ref()));
        fn switch_in(widget: &gtk::Widget) -> Option<gtk::Switch> {
            if let Some(switch) = widget.downcast_ref::<gtk::Switch>() {
                return Some(switch.clone());
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                if let Some(switch) = switch_in(&current) {
                    return Some(switch);
                }
                child = current.next_sibling();
            }
            None
        }
        // Skip watched reaches the playback, as Shuffle and Repeat do.
        let skip = switch_in(mixed.upcast_ref()).unwrap();
        skip.set_active(!skip.is_active());
        assert_eq!(
            changed.get().map(|settings| settings.skip_watched),
            Some(skip.is_active())
        );
        let actions = Rc::new(ListActions {
            open: Rc::new(|_| {}),
            play: Rc::new(|_, _| {}),
            pin: Rc::new(|_, _| {}),
        });
        let mut browser_list = list.clone();
        for playlist in &mut browser_list {
            playlist.artwork = None;
        }
        let art = crate::artwork::Art::new(
            session.clone(),
            spoolframe_linux_core::backend::ServerKey("test".into()),
            None,
        );
        let grid = browse(&browser_list, &actions, Some(&art));
        let measure = grid.widget.first_child().unwrap();
        measure.emit_by_name::<()>("resize", &[&1100i32, &1i32]);
        drain();
        let search = measure.next_sibling().unwrap();
        let filters = search.next_sibling().unwrap();
        let flow = filters
            .next_sibling()
            .unwrap()
            .downcast::<gtk::FlowBox>()
            .unwrap();
        let card = flow.child_at_index(0).unwrap().child().unwrap();
        assert_eq!(flow.max_children_per_line(), 4);
        assert_eq!(card.width_request(), browse_dimensions(1100).1);
        // Movies + episodes excludes the built-in music playlists.
        let movies = filters.first_child().unwrap();
        movies
            .clone()
            .downcast::<gtk::ToggleButton>()
            .unwrap()
            .set_active(true);
        movies
            .next_sibling()
            .unwrap()
            .downcast::<gtk::ToggleButton>()
            .unwrap()
            .set_active(true);
        filters
            .last_child()
            .unwrap()
            .downcast::<gtk::ToggleButton>()
            .unwrap()
            .set_active(false);
        drain();
        let count = list
            .iter()
            .filter(|p| p.kinds.movies || p.kinds.episodes)
            .count();
        assert!(flow.child_at_index(count as i32).is_none());
        assert!(flow.child_at_index(count as i32 - 1).is_some());
        let picture = || {
            flow.child_at_index(0)
                .unwrap()
                .child()
                .unwrap()
                .downcast::<gtk::Button>()
                .unwrap()
                .child()
                .unwrap()
                .first_child()
                .unwrap()
        };
        assert!(!picture().is::<gtk::Overlay>());
        let target = browser_list
            .iter_mut()
            .find(|p| p.kinds.movies || p.kinds.episodes)
            .unwrap();
        target.artwork =
            Some(ArtworkId::new(spoolframe_types::Namespace::Preview, "updated-cover").unwrap());
        grid.update(&browser_list);
        drain();
        assert!(
            picture().is::<gtk::Overlay>(),
            "the existing browser should render the new cover"
        );
        assert!(
            flow.child_at_index(count as i32).is_none(),
            "refresh should preserve the media filters"
        );
        assert_eq!(flow.max_children_per_line(), 4);
        for playlist in &mut browser_list {
            playlist.artwork = None;
        }
        grid.update(&browser_list);
        drain();
        assert!(
            !picture().is::<gtk::Overlay>(),
            "removing a cover should refresh too"
        );
        // Transient zero-width notifications must leave the current cards
        // alone; a subsequent real resize should still update the grid.
        let before = flow.child_at_index(0).unwrap();
        measure.emit_by_name::<()>("resize", &[&0i32, &1i32]);
        drain();
        assert_eq!(flow.child_at_index(0).unwrap(), before);
        measure.emit_by_name::<()>("resize", &[&800i32, &1i32]);
        measure.emit_by_name::<()>("resize", &[&1100i32, &1i32]);
        assert_eq!(
            flow.child_at_index(0).unwrap(),
            before,
            "resizing must not rebuild inside allocation"
        );
        drain();
        assert_ne!(flow.child_at_index(0).unwrap(), before);
        assert_eq!(flow.max_children_per_line(), 4);
        let navigation = adw::NavigationView::new();
        let browser_page = crate::pages::page("All playlists", &grid.widget);
        navigation.add(&browser_page);
        let window = gtk::Window::builder()
            .default_width(1100)
            .default_height(600)
            .child(&navigation)
            .build();
        let settle = || {
            let until = std::time::Instant::now() + std::time::Duration::from_millis(250);
            while std::time::Instant::now() < until {
                drain();
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        };
        window.present();
        settle();
        window.set_default_size(800, 600);
        settle();
        navigation.push(&adw::NavigationPage::new(
            &gtk::Label::new(Some("Playlist")),
            "Playlist",
        ));
        grid.update(&browser_list);
        settle();
        navigation.pop();
        settle();
        window.close();
        drain();
    }

    #[test]
    #[ignore = "needs a GTK display"]
    fn browse_all_is_offered_without_playlists_of_ones_own() {
        gtk::init().unwrap();
        let client = FakeClient::new(None);
        let list: Vec<_> = ready(andrey(&client).playlists())
            .unwrap()
            .playlists
            .into_iter()
            .filter(|p| p.builtin.is_some())
            .collect();
        assert!(!list.is_empty());
        let browsed = Rc::new(Cell::new(false));
        let actions = Rc::new(ListActions {
            open: Rc::new(|_| {}),
            play: Rc::new(|_, _| {}),
            pin: Rc::new(|_, _| {}),
        });
        let rows = section(&list, &actions, None, {
            let browsed = browsed.clone();
            Rc::new(move || browsed.set(true))
        });
        fn browse_button(widget: &gtk::Widget) -> Option<gtk::Button> {
            if let Some(button) = widget.downcast_ref::<gtk::Button>()
                && button.label().as_deref() == Some("Browse all")
            {
                return Some(button.clone());
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                if let Some(button) = browse_button(&current) {
                    return Some(button);
                }
                child = current.next_sibling();
            }
            None
        }
        rows.iter()
            .find_map(|row| browse_button(row.upcast_ref()))
            .expect("Browse all")
            .emit_clicked();
        assert!(browsed.get());
    }

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

    /// A PNG's signature and IHDR saying it is `width` × `height`.
    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        bytes.extend(width.to_be_bytes());
        bytes.extend(height.to_be_bytes());
        bytes.extend([8, 6, 0, 0, 0]);
        bytes
    }

    #[test]
    fn a_picture_within_the_limits_is_sent() {
        assert_eq!(cover_check(&png_header(3000, 3000)), Ok(()));
    }

    #[test]
    fn a_picture_with_too_many_pixels_is_refused_unsent() {
        // 8000 × 6000 is 48 megapixels, past the server's 40.
        assert!(matches!(
            cover_check(&png_header(8000, 6000)),
            Err(BackendError::ImageTooLarge { .. })
        ));
    }

    #[test]
    fn a_picture_with_too_many_bytes_is_refused_unsent() {
        let mut big = png_header(100, 100);
        big.resize(MAX_ART_BYTES as usize + 1, 0);
        assert!(matches!(
            cover_check(&big),
            Err(BackendError::ImageTooLarge { .. })
        ));
    }

    #[test]
    fn a_header_the_app_cannot_read_is_left_to_the_server() {
        assert_eq!(cover_check(b"\0\0\0\x18ftypheic"), Ok(()));
    }

    #[test]
    fn a_cover_refused_to_a_reader_says_who_may() {
        assert_eq!(
            cover_refusal(&BackendError::NotAllowed),
            "Only someone who can edit this playlist can change its cover."
        );
    }

    #[test]
    fn whoever_edits_the_entries_may_change_the_cover() {
        let client = FakeClient::new(None);
        let alice = client.session(&FakeClient::profile("alice")).unwrap();
        ready(alice.unlock(spoolframe_linux_core::fake::FAKE_PIN)).unwrap();
        let editable: Vec<(String, bool)> = ready(alice.playlists())
            .unwrap()
            .playlists
            .iter()
            .map(|p| (p.name.clone(), cover_editable(p)))
            .collect();
        assert_eq!(
            editable,
            [
                ("Liked".to_owned(), false),
                ("All".to_owned(), false),
                ("Andor: the heists".to_owned(), false),
                ("Road trip".to_owned(), true),
                ("New this month".to_owned(), false),
            ]
        );
    }

    #[test]
    fn a_small_picture_asks_for_a_small_size() {
        assert_eq!(
            [22, 72, 150, 196].map(picture_size),
            [
                ArtworkSize::Thumb,
                ArtworkSize::Thumb,
                ArtworkSize::Small,
                ArtworkSize::Medium
            ]
        );
    }

    #[test]
    fn a_greyed_playlist_says_why() {
        let client = FakeClient::new(None);
        let alice = client.session(&FakeClient::profile("alice")).unwrap();
        ready(alice.unlock(spoolframe_linux_core::fake::FAKE_PIN)).unwrap();
        let list = ready(alice.playlists()).unwrap().playlists;
        let month = list.iter().find(|p| p.name == "New this month").unwrap();
        assert_eq!(
            add_rows(&list, MediaKind::Movie),
            [AddRow {
                id: month.id.clone(),
                name: "New this month".into(),
                builtin: None,
                artwork: month.artwork.clone(),
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
        assert_eq!(
            lines,
            [
                "Music · Alice’s · you can edit",
                "Use the heart instead",
                "Built in · every track already"
            ]
        );
    }

    #[test]
    fn an_items_playlists_say_only_whose_they_are() {
        assert_eq!(
            [
                PlaylistOwnerKind::Mine,
                PlaylistOwnerKind::SharedWithMe,
                PlaylistOwnerKind::ServerWide,
                PlaylistOwnerKind::Unknown,
            ]
            .map(ref_line),
            ["Yours", "Shared with you", "Server-wide", "Shared with you"]
        );
    }

    #[test]
    fn an_albums_page_says_liked_is_built_in() {
        let client = FakeClient::new(None);
        let session = andrey(&client);
        let id = |kind, key| {
            spoolframe_types::MediaId::new(spoolframe_types::Namespace::Plex, kind, key).unwrap()
        };
        ready(session.like(&[id(MediaKind::Track, "41")])).unwrap();
        let lines: Vec<_> = refs_of(session.as_ref(), &id(MediaKind::Album, "31"))
            .iter()
            .map(|r| (r.name.clone(), ref_subtitle(r)))
            .collect();
        assert_eq!(
            lines,
            [
                ("Liked".to_owned(), "Built in".to_owned()),
                ("Road trip".to_owned(), "Alice’s".to_owned())
            ]
        );
    }

    /// A server without sharing by name sends no owner's name: the row
    /// says only that it is shared.
    #[test]
    fn without_names_a_shared_playlists_row_says_it_is_shared() {
        let client =
            FakeClient::new(None).with_features(spoolframe_linux_core::backend::Features {
                playlists: true,
                ..Default::default()
            });
        let session = andrey(&client);
        let album = spoolframe_types::MediaId::new(
            spoolframe_types::Namespace::Plex,
            MediaKind::Album,
            "31",
        )
        .unwrap();
        let lines: Vec<_> = refs_of(session.as_ref(), &album)
            .iter()
            .map(ref_subtitle)
            .collect();
        assert_eq!(lines, ["Shared with you"]);
    }

    fn refs_of(session: &dyn Session, id: &spoolframe_types::MediaId) -> Vec<PlaylistRef> {
        ready(session.item_detail(id)).unwrap().playlists
    }

    fn andor() -> spoolframe_types::MediaId {
        spoolframe_types::MediaId::new(spoolframe_types::Namespace::Plex, MediaKind::Show, "60")
            .unwrap()
    }

    #[test]
    fn a_show_in_a_playlist_lists_it() {
        let client = FakeClient::new(None);
        let refs = refs_of(andrey(&client).as_ref(), &andor());
        let shown: Vec<_> = shown_refs(true, &refs)
            .unwrap()
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(shown, ["Andor: the heists"]);
    }

    #[test]
    fn an_item_in_no_playlist_has_no_section() {
        let client = FakeClient::new(None);
        let mum = client.session(&FakeClient::profile("mum")).unwrap();
        let refs = refs_of(mum.as_ref(), &andor());
        assert_eq!(shown_refs(true, &refs), None);
    }

    #[test]
    fn without_the_feature_there_is_no_section() {
        let client = FakeClient::new(None);
        let refs = refs_of(andrey(&client).as_ref(), &andor());
        assert_eq!(shown_refs(false, &refs), None);
    }

    #[test]
    fn browse_all_shows_a_playlist_holding_any_kind_ticked() {
        let music = Kinds {
            tracks: true,
            ..Kinds::default()
        };
        let mixed = Kinds {
            movies: true,
            episodes: true,
            tracks: false,
        };
        assert_eq!(
            [
                ticked_holds([false, false, true], music),
                ticked_holds([false, false, true], mixed),
                ticked_holds([false, true, false], mixed),
                ticked_holds([false, false, false], music),
            ],
            [true, false, true, false]
        );
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
