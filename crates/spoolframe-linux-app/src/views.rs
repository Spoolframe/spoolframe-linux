//! Widgets built from the media model, laid out as in the UI mockups. Tiles
//! start as coloured placeholders; artwork replaces them as it loads.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use gtk::prelude::*;
use spoolframe_linux_core::backend::ArtworkSize;
use spoolframe_linux_core::format;
use spoolframe_types::{
    Album, Artist, Artwork, ArtworkKind, Episode, Item, MediaId, PlaybackState, Season, Show, Track,
};

use crate::artwork::Art;

const LANDSCAPE: (i32, i32) = (300, 169);
const PORTRAIT: (i32, i32) = (140, 210);
/// Albums and artists: square, as their covers are.
const ALBUM: (i32, i32) = (160, 160);

/// What a card does when clicked: open the item's page.
pub type Open = Rc<dyn Fn(Item)>;

/// What every card needs from the app: how to open its item, and where its
/// artwork comes from (none before a session exists).
pub struct Cards {
    pub open: Open,
    pub art: Option<Rc<Art>>,
    /// Marks an item watched (`true`) or clears its progress (`false`),
    /// from a Continue watching card's menu; none before a session exists.
    pub set_watched: Option<Rc<dyn Fn(MediaId, bool)>>,
    /// Takes a card out of its row (P1-22): that card only.
    pub hide: Option<Rc<dyn Fn(Item, Shelf)>>,
    /// Puts a track, an album or an artist's albums in the play queue
    /// (P1-23), from its card's menu.
    pub enqueue: Option<Rc<dyn Fn(Item, Enqueue)>>,
}

/// Where in the play queue (P1-23) items go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enqueue {
    /// "Play next": the front of the queue.
    Next,
    /// "Add to queue": the end of it.
    Last,
}

impl Cards {
    /// Draws the item's artwork over `tile` once it loads, if the item has
    /// one of `kinds`.
    pub fn art(
        &self,
        tile: &gtk::Overlay,
        artwork: &[Artwork],
        kinds: &[ArtworkKind],
        size: ArtworkSize,
    ) {
        if let Some(art) = &self.art {
            art.show(tile, artwork, kinds, size);
        }
    }
}

/// The hues a placeholder tile can take; see `.tile-*` in style.css.
const TILE_HUES: [&str; 8] = [
    "tile-teal",
    "tile-purple",
    "tile-yellow",
    "tile-blue",
    "tile-orange",
    "tile-red",
    "tile-green",
    "tile-pink",
];

/// A heading over a horizontally scrolling row of cards, with ‹ › buttons
/// in the heading. The wheel over the row scrolls the row (see
/// [`wheel_scrolls_row`]); elsewhere it scrolls the page.
pub fn row(title: &str, cards: impl IntoIterator<Item = gtk::Widget>) -> gtk::Box {
    // Cards keep their own width: a caption's hexpand would otherwise
    // stretch a lone card across the row.
    let cards_box = gtk::Box::builder()
        .spacing(16)
        .halign(gtk::Align::Start)
        .build();
    lift_room(&cards_box);
    for card in cards {
        cards_box.append(&card);
    }
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .child(&cards_box)
        .build();
    wheel_scrolls_row(&scroller);

    let (back, forward) = scroll_buttons(&scroller);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    // In line with the cards, which sit inside the row's side room.
    header.set_margin_start(LIFT_SIDE);
    header.append(&heading(title));
    header.append(&back);
    header.append(&forward);

    // The heading's gap, less the room above the cards.
    let section = gtk::Box::new(gtk::Orientation::Vertical, 14 - LIFT_ABOVE);
    section.append(&header);
    section.append(&scroller);
    section
}

/// How far a card rises on hover, with a margin, and how far its shadow
/// falls: a scrolled row clips its cards to itself, and without the room
/// a lifted card lost its top edge (and its rounded corners) and its
/// shadow.
pub const LIFT_ABOVE: i32 = 8;
pub const LIFT_BELOW: i32 = 20;
/// A lifted card grows 2% about its centre, 3 px a side on the widest:
/// with no room at the row's ends the first card lost its left edge, and
/// with it the first letter of its caption.
pub const LIFT_SIDE: i32 = 4;

/// Room inside a scrolled row for its cards to lift and cast a shadow.
pub fn lift_room(cards: &impl IsA<gtk::Widget>) {
    cards.set_margin_top(LIFT_ABOVE);
    cards.set_margin_bottom(LIFT_BELOW);
    cards.set_margin_start(LIFT_SIDE);
    cards.set_margin_end(LIFT_SIDE);
}

/// ‹ and › for a row that scrolls sideways: shown only while it
/// overflows, each greyed out at its end.
pub fn scroll_buttons(scroller: &gtk::ScrolledWindow) -> (gtk::Button, gtk::Button) {
    let back = row_button(crate::icons::BACK, "Scroll back", scroller, -1.0);
    let forward = row_button(crate::icons::RIGHT, "Scroll forward", scroller, 1.0);
    let adjustment = scroller.hadjustment();
    let update = {
        let (back, forward) = (back.clone(), forward.clone());
        // The adjustment changes while GTK lays the row out, so the
        // buttons keep their space and only fade out: hiding them would
        // change the heading's height in the middle of that layout.
        move |a: &gtk::Adjustment| {
            let overflows = a.upper() - a.lower() > a.page_size() + 0.5;
            for button in [&back, &forward] {
                button.set_opacity(if overflows { 1.0 } else { 0.0 });
                button.set_can_target(overflows);
                button.set_can_focus(overflows);
            }
            back.set_sensitive(a.value() > a.lower() + 0.5);
            forward.set_sensitive(a.value() + a.page_size() < a.upper() - 0.5);
        }
    };
    update(&adjustment);
    adjustment.connect_changed(update.clone());
    adjustment.connect_value_changed(update);

    (back, forward)
}

/// A ‹ or › button that scrolls the row by most of what is visible, so the
/// last card of one view is still in sight as the first of the next.
fn row_button(icon: &str, label: &str, row: &gtk::ScrolledWindow, direction: f64) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(label)
        .valign(gtk::Align::Center)
        .css_classes(["circular", "flat"])
        .build();
    let adjustment = row.hadjustment();
    button.connect_clicked(move |_| {
        let target = adjustment.value() + direction * adjustment.page_size() * 0.8;
        adjustment.set_value(target.clamp(
            adjustment.lower(),
            adjustment.upper() - adjustment.page_size(),
        ));
    });
    button
}

/// The wheel over a row scrolls that row, sideways; anywhere else on the
/// page it scrolls the page, as the user asked. A wheel turns only up and
/// down, so its turns move the row along; sideways scrolling (a touchpad,
/// a tilting wheel) moves it as ever.
pub fn wheel_scrolls_row(row: &gtk::ScrolledWindow) {
    let controller = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let row_widget = row.clone();
    controller.connect_scroll(move |controller, _dx, dy| {
        let adjustment = row_widget.hadjustment();
        // A wheel reports whole notches; GTK scrolls page_size^(2/3) per
        // notch. A touchpad reports pixels.
        let step = match controller.unit() {
            gtk::gdk::ScrollUnit::Wheel => adjustment.page_size().powf(2.0 / 3.0),
            _ => 1.0,
        };
        adjustment.set_value(adjustment.value() + dy * step);
        gtk::glib::Propagation::Stop
    });
    row.add_controller(controller);
}

/// A heading over a wrapping grid of cards.
pub fn grid(title: &str, cards: impl IntoIterator<Item = gtk::Widget>) -> gtk::Box {
    let flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .column_spacing(16)
        .row_spacing(24)
        .halign(gtk::Align::Start)
        .max_children_per_line(12)
        .build();
    for card in cards {
        flow.append(&card);
    }
    titled(title, &flow)
}

fn titled(title: &str, body: &impl IsA<gtk::Widget>) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 14);
    section.append(&heading(title));
    section.append(body);
    section
}

fn heading(title: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["title-2"])
        .build()
}

/// A 16:9 card for "Continue watching": the title on the tile, a progress
/// strip along its bottom edge, and below it what the item is and how much
/// is left.
/// The home rows a watch-state card sits in, which its Remove names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shelf {
    ContinueWatching,
    NextUp,
}

impl Shelf {
    pub fn name(self) -> &'static str {
        match self {
            Shelf::ContinueWatching => "Continue watching",
            Shelf::NextUp => "Next up",
        }
    }
}

pub fn continue_card(item: &Item, cards: &Cards, shelf: Shelf) -> gtk::Widget {
    let facts = Facts::of(item);
    // An episode's tile shows its show; the caption says which episode.
    let on_tile = facts.parent.clone().unwrap_or_else(|| facts.title.clone());
    let tile = tile(item.id(), &on_tile, "landscape", LANDSCAPE);
    cards.art(
        &tile,
        facts.artwork,
        &[
            ArtworkKind::Thumb,
            ArtworkKind::Background,
            ArtworkKind::Poster,
        ],
        ArtworkSize::Small,
    );
    if let (Some(length), Some(position)) = (facts.length, facts.position()) {
        let strip = gtk::ProgressBar::builder()
            .fraction(format::progress(length, position))
            .valign(gtk::Align::End)
            .css_classes(["tile-progress"])
            .build();
        tile.add_overlay(&strip);
    }

    // The show, then which episode: once the artwork is in, the tile's
    // own title is hidden and the still alone does not say which show.
    let below = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let first = caption_label(&facts.caption(), &["card-caption"]);
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let left = facts
        .length
        .zip(facts.position())
        .map(|(length, position)| {
            gtk::Label::builder()
                .label(format::remaining(length, position))
                .css_classes(["dim-label", "small-text"])
                .build()
        });
    match facts.episode_line() {
        Some(episode) => {
            below.append(&first);
            let episode = caption_label(episode, &["dim-label", "small-text"]);
            episode.set_hexpand(true);
            line.append(&episode);
        }
        None => {
            first.set_hexpand(true);
            line.append(&first);
        }
    }
    if let Some(left) = &left {
        line.append(left);
    }
    below.append(&line);
    let card = card(item, &tile, &below, LANDSCAPE.0, &cards.open);
    card.add_css_class("continue-card");
    if let (Some(set_watched), Some(hide)) = (&cards.set_watched, &cards.hide) {
        card_menu(&card, item, shelf, set_watched, hide, &cards.open);
    }
    card
}

/// A library grid's card: as a poster card, with only the title below, as
/// LinuxHomeVideo has it.
pub fn grid_card(item: &Item, cards: &Cards) -> gtk::Widget {
    poster(item, cards, false)
}

/// A poster card: the title on the tile, a badge when watched, and the
/// title and year below.
pub fn poster_card(item: &Item, cards: &Cards) -> gtk::Widget {
    poster(item, cards, true)
}

/// A poster card: the title on the tile, a badge when watched, and the
/// title below, with the year when `detail`. 2:3 for films and shows, square for albums and
/// artists.
fn poster(item: &Item, cards: &Cards, detail: bool) -> gtk::Widget {
    let facts = Facts::of(item);
    let music = matches!(item, Item::Album(_) | Item::Artist(_) | Item::Track(_));
    let (shape, size) = if music {
        ("album", ALBUM)
    } else {
        ("portrait", PORTRAIT)
    };
    let tile = tile(item.id(), &facts.title, shape, size);
    cards.art(
        &tile,
        facts.artwork,
        &[ArtworkKind::Poster],
        ArtworkSize::Small,
    );
    if facts.playback.is_some_and(|p| p.watched) {
        let badge = gtk::Image::builder()
            .icon_name(crate::icons::CHECK)
            .pixel_size(12)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .margin_top(8)
            .margin_end(8)
            .css_classes(["watched-badge"])
            .tooltip_text("Watched")
            .build();
        tile.add_overlay(&badge);
    }

    let caption = gtk::Box::new(gtk::Orientation::Vertical, 2);
    if detail {
        caption.append(&caption_label(&facts.title, &["heading"]));
        if let Some(detail) = &facts.detail {
            caption.append(&caption_label(detail, &["dim-label", "caption"]));
        }
    } else {
        caption.append(&caption_label(&facts.title, &["card-caption"]));
    }
    let card = card(item, &tile, &caption, size.0, &cards.open);
    if music && let Some(enqueue) = &cards.enqueue {
        let (item, enqueue) = (item.clone(), enqueue.clone());
        let popover = menu_popover(vec![enqueue_entries(move |at| enqueue(item.clone(), at))]);
        context_menu(&card, &popover);
    }
    card
}

/// A line under a tile, cut short to the tile's width: a label asks for
/// its whole text's width otherwise, and a long title widened the card
/// and stretched its picture.
pub fn caption_label(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(1)
        .css_classes(classes)
        .build()
}

/// A poster-sized blank for an item whose page has not loaded yet.
pub fn placeholder_card() -> gtk::Widget {
    let tile = gtk::Box::builder()
        .width_request(PORTRAIT.0)
        .height_request(PORTRAIT.1)
        .css_classes(["tile", "tile-placeholder"])
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.append(&tile);
    // The caption's height, so rows do not jump when the page arrives.
    body.append(&gtk::Box::builder().height_request(38).build());
    body.upcast()
}

/// A clickable card: a tile over a caption.
pub(crate) fn card(
    item: &Item,
    tile: &gtk::Overlay,
    caption: &gtk::Box,
    width: i32,
    open: &Open,
) -> gtk::Widget {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.append(tile);
    body.append(caption);
    let button = gtk::Button::builder()
        .child(&body)
        .width_request(width)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        // Every card lifts on hover: Continue watching, posters in rows
        // and in library grids alike.
        .css_classes(["card-button", "flat", "lift"])
        .build();
    let (item, open) = (item.clone(), open.clone());
    button.connect_clicked(move |_| open(item.clone()));
    button.upcast()
}

/// A Continue watching card's menu: mark watched, or reset the progress;
/// below a rule, in red, out of Continue watching (progress kept).
fn card_menu(
    card: &gtk::Widget,
    item: &Item,
    shelf: Shelf,
    set_watched: &Rc<dyn Fn(MediaId, bool)>,
    hide: &Rc<dyn Fn(Item, Shelf)>,
    open: &Open,
) {
    let watch = |label, icon, watched| {
        let (id, set_watched) = (item.id().clone(), set_watched.clone());
        MenuEntry::new(label, icon, move || set_watched(id.clone(), watched))
    };
    let remove = {
        let (item, hide) = (item.clone(), hide.clone());
        MenuEntry {
            destructive: true,
            ..MenuEntry::new(
                format!("Remove from {}", shelf.name()),
                crate::icons::REMOVE,
                move || hide(item.clone(), shelf),
            )
        }
    };
    let mut groups = vec![vec![
        watch("Mark as watched", crate::icons::WATCHED, true),
        watch("Reset watch progress", crate::icons::BACK_10, false),
    ]];
    if let Item::Episode(episode) = item {
        groups.push(go_to_entries(episode, open));
    }
    groups.push(vec![remove]);
    let popover = menu_popover(groups);
    context_menu(card, &popover);
}

/// One row of a menu built by [`menu_popover`].
pub struct MenuEntry {
    pub label: String,
    pub icon: &'static str,
    /// Drawn in red.
    pub destructive: bool,
    pub run: Rc<dyn Fn()>,
}

impl MenuEntry {
    pub fn new(label: impl Into<String>, icon: &'static str, run: impl Fn() + 'static) -> Self {
        Self {
            label: label.into(),
            icon,
            destructive: false,
            run: Rc::new(run),
        }
    }
}

/// An episode's show, as an item to open: its page loads the rest.
pub fn show_of(episode: &Episode) -> Item {
    Item::Show(Show {
        id: episode.show_id.clone(),
        title: episode.show_title.clone().unwrap_or_default(),
        year: None,
        summary: None,
        artwork: Vec::new(),
        content_rating: None,
        genres: Vec::new(),
        season_count: None,
        cast: Vec::new(),
        last_aired: None,
    })
}

/// An episode's season, as an item to open: the show's page, on its tab.
pub fn season_of(episode: &Episode) -> Item {
    Item::Season(Season {
        id: episode.season_id.clone(),
        show_id: episode.show_id.clone(),
        show_title: episode.show_title.clone(),
        season: episode.season,
        title: season_name(episode.season),
        artwork: Vec::new(),
    })
}

/// "Season 2", or "Specials" for season 0.
pub fn season_name(season: u32) -> String {
    match season {
        0 => "Specials".to_owned(),
        n => format!("Season {n}"),
    }
}

/// "Go to show" and "Go to Season N", for an episode's menu anywhere but
/// its own show's page.
pub fn go_to_entries(episode: &Episode, open: &Open) -> Vec<MenuEntry> {
    let (show, season) = (show_of(episode), season_of(episode));
    let (open_show, open_season) = (open.clone(), open.clone());
    vec![
        MenuEntry::new("Go to show", crate::icons::SHOWS, move || {
            open_show(show.clone())
        }),
        MenuEntry::new(
            format!("Go to {}", season_name(episode.season)),
            crate::icons::SEASON,
            move || open_season(season.clone()),
        ),
    ]
}

/// A track's artist, as an item to open: their page loads the rest.
pub fn artist_of(track: &Track) -> Item {
    Item::Artist(Artist {
        id: track.artist_id.clone(),
        name: String::new(),
        summary: None,
        artwork: Vec::new(),
    })
}

/// A track's album, as an item to open: its page loads the rest.
pub fn album_of(track: &Track) -> Item {
    Item::Album(Album {
        id: track.album_id.clone(),
        artist_id: track.artist_id.clone(),
        artist_name: None,
        title: String::new(),
        year: None,
        artwork: Vec::new(),
        track_count: None,
    })
}

/// "Go to artist", and "Go to album" unless on the album's own page.
pub fn track_go_to_entries(track: &Track, open: &Open, with_album: bool) -> Vec<MenuEntry> {
    let artist = artist_of(track);
    let open_artist = open.clone();
    let mut entries = vec![MenuEntry::new(
        "Go to artist",
        crate::icons::ARTIST,
        move || open_artist(artist.clone()),
    )];
    if with_album {
        let (album, open_album) = (album_of(track), open.clone());
        entries.push(MenuEntry::new(
            "Go to album",
            crate::icons::ALBUM,
            move || open_album(album.clone()),
        ));
    }
    entries
}

/// "Play next" and "Add to queue", calling `run` with where.
pub fn enqueue_entries(run: impl Fn(Enqueue) + 'static) -> Vec<MenuEntry> {
    let run = Rc::new(run);
    let entry = |label, icon, at| {
        let run = run.clone();
        MenuEntry::new(label, icon, move || run(at))
    };
    vec![
        entry("Play next", crate::icons::PLAY_NEXT, Enqueue::Next),
        entry("Add to queue", crate::icons::ADD_TO_QUEUE, Enqueue::Last),
    ]
}

/// A menu of icon-and-label rows, in groups with a rule between them, as
/// designed: GTK's own menu drops the icon of any item that has a label.
/// A row closes the menu, then runs.
pub fn menu_popover(groups: Vec<Vec<MenuEntry>>) -> gtk::Popover {
    let rows = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .build();
    let popover = gtk::Popover::builder()
        .child(&rows)
        .has_arrow(false)
        .halign(gtk::Align::Start)
        .css_classes(["card-menu"])
        .build();
    for (n, group) in groups.into_iter().enumerate() {
        if n > 0 {
            rows.append(
                &gtk::Separator::builder()
                    .margin_top(4)
                    .margin_bottom(4)
                    .build(),
            );
        }
        for entry in group {
            let row = gtk::Button::builder()
                .child(
                    &adw::ButtonContent::builder()
                        .icon_name(entry.icon)
                        .label(&entry.label)
                        .halign(gtk::Align::Start)
                        .build(),
                )
                .css_classes(["flat", "card-menu-item"])
                .build();
            if entry.destructive {
                row.add_css_class("destructive");
            }
            let (run, popover) = (entry.run, popover.downgrade());
            row.connect_clicked(move |_| {
                if let Some(popover) = popover.upgrade() {
                    popover.popdown();
                }
                run();
            });
            rows.append(&row);
        }
    }
    popover
}

/// A "⋯" button that opens `popover`.
pub fn menu_button(popover: &gtk::Popover, tooltip: &str) -> gtk::MenuButton {
    popover.set_halign(gtk::Align::Fill);
    gtk::MenuButton::builder()
        .icon_name(crate::icons::MORE)
        .tooltip_text(tooltip)
        .popover(popover)
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build()
}

/// Opens `popover` at the pointer on a right-click or a long press on
/// `widget`. While it shows, the widget keeps its hover look (a card stays
/// lifted).
pub fn context_menu(widget: &gtk::Widget, popover: &gtk::Popover) {
    popover.set_parent(widget);
    {
        let widget = widget.downgrade();
        popover.connect_visible_notify(move |popover| {
            if let Some(widget) = widget.upgrade() {
                if popover.is_visible() {
                    widget.add_css_class("menu-open");
                } else {
                    widget.remove_css_class("menu-open");
                }
            }
        });
    }
    // The popover is the widget's child, and goes when the widget does.
    {
        let popover = popover.clone();
        widget.connect_destroy(move |_| popover.unparent());
    }
    let open_at = {
        let popover = popover.clone();
        move |x: f64, y: f64| {
            popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            popover.popup();
        }
    };
    let right_click = gtk::GestureClick::builder().button(3).build();
    {
        let open_at = open_at.clone();
        right_click.connect_pressed(move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            open_at(x, y);
        });
    }
    widget.add_controller(right_click);
    let long_press = gtk::GestureLongPress::builder().touch_only(true).build();
    long_press.connect_pressed(move |_, x, y| open_at(x, y));
    widget.add_controller(long_press);
}

/// A placeholder tile in a hue picked from the item's ID, so an item keeps
/// its colour from one screen to the next.
pub fn tile(id: &MediaId, title: &str, shape: &str, (width, height): (i32, i32)) -> gtk::Overlay {
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    let hue = TILE_HUES[(hasher.finish() % TILE_HUES.len() as u64) as usize];

    let background = gtk::Box::builder()
        .width_request(width)
        .height_request(height)
        .css_classes(["tile", shape, hue])
        .build();
    let label = gtk::Label::builder()
        .label(title)
        .wrap(true)
        .xalign(0.0)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::End)
        .css_classes(["tile-title"])
        .build();
    // The title's inset: 14 px on wide tiles, 12 on posters and squares.
    let inset = if shape == "landscape" { 14 } else { 12 };
    label.set_margin_start(inset);
    label.set_margin_end(inset);
    label.set_margin_bottom(inset);
    let overlay = gtk::Overlay::builder()
        .child(&background)
        .css_classes([shape])
        .overflow(gtk::Overflow::Hidden)
        .build();
    overlay.add_overlay(&label);
    overlay
}

/// What a card shows of an item.
pub struct Facts<'a> {
    pub title: String,
    /// The year, or "S2 · E4" for an episode.
    pub detail: Option<String>,
    pub playback: Option<&'a PlaybackState>,
    pub length: Option<std::time::Duration>,
    pub artwork: &'a [Artwork],
    /// For an episode, its show-relative label for captions.
    episode: Option<String>,
    /// The show an episode or season belongs to, the artist of an album.
    pub parent: Option<String>,
}

impl<'a> Facts<'a> {
    pub fn of(item: &'a Item) -> Self {
        let year = |y: Option<u16>| y.map(|y| y.to_string());
        let plain = |title: &str, detail, artwork| Facts {
            title: title.to_owned(),
            detail,
            playback: None,
            length: None,
            artwork,
            episode: None,
            parent: None,
        };
        match item {
            Item::Movie(m) => Facts {
                playback: Some(&m.playback),
                length: m.duration,
                ..plain(&m.title, year(m.year), &m.artwork)
            },
            Item::Episode(e) => Facts {
                playback: Some(&e.playback),
                length: e.duration,
                episode: Some(format!("S{} · E{} · {}", e.season, e.episode, e.title)),
                parent: e.show_title.clone(),
                ..plain(
                    &e.title,
                    Some(joined(&[
                        e.show_title.clone(),
                        Some(format!("S{} · E{}", e.season, e.episode)),
                    ])),
                    &e.artwork,
                )
            },
            Item::Show(s) => plain(&s.title, year(s.year), &s.artwork),
            Item::Season(s) => Facts {
                parent: s.show_title.clone(),
                ..plain(&s.title, s.show_title.clone(), &s.artwork)
            },
            Item::Artist(a) => plain(&a.name, None, &a.artwork),
            Item::Album(a) => Facts {
                parent: a.artist_name.clone(),
                ..plain(
                    &a.title,
                    Some(joined(&[a.artist_name.clone(), year(a.year)])).filter(|d| !d.is_empty()),
                    &a.artwork,
                )
            },
            // Whose it is, "Adele · 25", as the search board has it; the
            // number only where the server says neither.
            Item::Track(t) => Facts {
                parent: t.artist_name.clone(),
                ..plain(
                    &t.title,
                    Some(joined(&[t.artist_name.clone(), t.album_title.clone()]))
                        .filter(|d| !d.is_empty())
                        .or_else(|| t.track.map(|n| format!("Track {n}"))),
                    &t.artwork,
                )
            },
        }
    }

    /// Where playback stopped, for an item started and not finished.
    pub fn position(&self) -> Option<std::time::Duration> {
        self.playback
            .filter(|p| !p.watched)
            .and_then(|p| p.position)
    }

    /// Which episode, "S2 · E4 · Woe's Hollow", for the line under an
    /// episode's show; `None` when the caption already says it (no show
    /// name) or the item is no episode.
    pub fn episode_line(&self) -> Option<&str> {
        self.parent.as_ref().and(self.episode.as_deref())
    }

    /// What a landscape card says first: the show for an episode (its
    /// episode label goes under it), otherwise the title.
    pub fn caption(&self) -> String {
        if let Some(parent) = self.parent.as_ref().filter(|_| self.episode.is_some()) {
            return parent.clone();
        }
        self.episode.clone().unwrap_or_else(|| self.title.clone())
    }
}

/// The parts that are there, joined with " · ".
fn joined(parts: &[Option<String>]) -> String {
    parts
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// A track's artist: its own, else its album's (P1-23's queue can mix
/// albums, and a track played from search has no album with it).
pub fn track_artist(item: &Item, album: Option<&Album>) -> Option<String> {
    match item {
        Item::Track(t) => t
            .artist_name
            .clone()
            .or_else(|| album.and_then(|a| a.artist_name.clone())),
        _ => None,
    }
}

/// A track's album title: its own, else the album's played from.
pub fn track_album(item: &Item, album: Option<&Album>) -> Option<String> {
    match item {
        Item::Track(t) => t
            .album_title
            .clone()
            .or_else(|| album.map(|a| a.title.clone())),
        _ => None,
    }
}

/// The cover for what plays, and the id it is kept under: its album's,
/// else a track's own (the album cover the server gives with it).
pub fn cover_of<'a>(item: &'a Item, album: Option<&'a Album>) -> (MediaId, &'a [Artwork]) {
    let own: &[Artwork] = match item {
        Item::Track(t) => &t.artwork,
        _ => &[],
    };
    let id = match (album, item) {
        (Some(album), _) => album.id.clone(),
        (None, Item::Track(t)) => t.album_id.clone(),
        (None, other) => other.id().clone(),
    };
    let artwork = album
        .map(|a| a.artwork.as_slice())
        .filter(|a| !a.is_empty())
        .unwrap_or(own);
    (id, artwork)
}

/// A page under `header`, with `above` (a banner, a heading, tabs) between
/// the two. Only the header is a top bar: libadwaita shrinks a header that
/// shares the top bar and pads it by 3 px, which this sheet's own header
/// colour shows as a dark band above it.
pub fn toolbar(
    header: &adw::HeaderBar,
    above: &[&gtk::Widget],
    content: &impl IsA<gtk::Widget>,
) -> adw::ToolbarView {
    let view = adw::ToolbarView::new();
    view.add_top_bar(header);
    if above.is_empty() {
        view.set_content(Some(content));
    } else {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        for widget in above {
            column.append(*widget);
        }
        column.append(content);
        view.set_content(Some(&column));
    }
    view
}

#[cfg(test)]
mod tests {
    use spoolframe_types::{Episode, MediaId, MediaKind, Namespace, PlaybackState};

    use super::*;

    fn id(kind: MediaKind, key: &str) -> MediaId {
        MediaId::new(Namespace::Plex, kind, key).unwrap()
    }

    fn episode(show_title: Option<&str>) -> Item {
        Item::Episode(Episode {
            id: id(MediaKind::Episode, "3"),
            show_id: id(MediaKind::Show, "1"),
            show_title: show_title.map(Into::into),
            season_id: id(MediaKind::Season, "2"),
            season: 1,
            episode: 4,
            title: "Seven".into(),
            summary: None,
            duration: None,
            artwork: Vec::new(),
            playback: PlaybackState::default(),
            content_rating: None,
            year: None,
            air_date: None,
            cast: Vec::new(),
        })
    }

    #[test]
    fn an_episode_card_leads_with_its_show() {
        let item = episode(Some("12 Monkeys"));
        assert_eq!(Facts::of(&item).caption(), "12 Monkeys");
    }

    #[test]
    fn under_its_show_an_episode_says_which_one_it_is() {
        let item = episode(Some("12 Monkeys"));
        assert_eq!(Facts::of(&item).episode_line(), Some("S1 · E4 · Seven"));
    }

    #[test]
    fn an_episode_without_a_show_name_says_it_once() {
        let item = episode(None);
        assert_eq!(Facts::of(&item).episode_line(), None);
    }

    #[test]
    fn an_episode_without_a_show_name_leads_with_its_label() {
        let item = episode(None);
        assert_eq!(Facts::of(&item).caption(), "S1 · E4 · Seven");
    }

    fn track(artist: Option<&str>, album: Option<&str>) -> Item {
        Item::Track(Track {
            id: id(MediaKind::Track, "7"),
            album_id: id(MediaKind::Album, "5"),
            artist_id: id(MediaKind::Artist, "4"),
            title: "Can I Get It".into(),
            track: Some(6),
            disc: None,
            duration: None,
            playback: PlaybackState::default(),
            codec: None,
            bit_depth: None,
            sample_rate: None,
            album_track_count: None,
            artist_name: artist.map(Into::into),
            album_title: album.map(Into::into),
            artwork: Vec::new(),
        })
    }

    #[test]
    fn a_track_says_whose_it_is() {
        assert_eq!(
            Facts::of(&track(Some("Adele"), Some("30")))
                .detail
                .as_deref(),
            Some("Adele · 30")
        );
    }

    #[test]
    fn a_track_the_server_says_nothing_about_shows_its_number() {
        assert_eq!(
            Facts::of(&track(None, None)).detail.as_deref(),
            Some("Track 6")
        );
    }

    #[test]
    fn a_tracks_own_artist_wins_over_the_album_played_from() {
        assert_eq!(
            track_artist(&track(Some("Adele"), None), None).as_deref(),
            Some("Adele")
        );
    }

    #[test]
    fn an_episodes_detail_names_its_show() {
        let item = episode(Some("12 Monkeys"));
        assert_eq!(
            Facts::of(&item).detail.as_deref(),
            Some("12 Monkeys · S1 · E4")
        );
    }

    /// A banner and a heading over the page leave the header its full
    /// height at the top edge, with no band above it.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn a_header_with_bars_under_it_keeps_the_top_edge() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let header = adw::HeaderBar::new();
        let banner = adw::Banner::new("Reconnecting");
        banner.set_use_markup(false);
        banner.set_revealed(true);
        let heading = gtk::Label::new(Some("Music"));
        let view = toolbar(
            &header,
            &[banner.upcast_ref(), heading.upcast_ref()],
            &gtk::ScrolledWindow::new(),
        );
        let window = gtk::Window::builder()
            .child(&view)
            .default_width(800)
            .default_height(600)
            .build();
        window.present();
        let context = gtk::glib::MainContext::default();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while std::time::Instant::now() < until {
            context.iteration(false);
        }
        let bounds = header.compute_bounds(&view).unwrap();
        window.close();
        assert_eq!(bounds.y(), 0.0);
    }
}
