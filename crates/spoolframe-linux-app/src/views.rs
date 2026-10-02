//! Widgets built from the media model, laid out as in the UI mockups. Tiles
//! start as coloured placeholders; artwork replaces them as it loads.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use gtk::prelude::*;
use spoolframe_linux_core::backend::ArtworkSize;
use spoolframe_linux_core::format;
use spoolframe_types::{Artwork, ArtworkKind, Item, MediaId, PlaybackState};

use crate::artwork::Art;

const LANDSCAPE: (i32, i32) = (300, 169);
const PORTRAIT: (i32, i32) = (140, 210);

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
    for card in cards {
        cards_box.append(&card);
    }
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .child(&cards_box)
        .build();
    wheel_scrolls_row(&scroller);

    let back = row_button(crate::icons::BACK, "Scroll back", &scroller, -1.0);
    let forward = row_button(crate::icons::RIGHT, "Scroll forward", &scroller, 1.0);
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

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    header.append(&heading(title));
    header.append(&back);
    header.append(&forward);

    let section = gtk::Box::new(gtk::Orientation::Vertical, 14);
    section.append(&header);
    section.append(&scroller);
    section
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
fn wheel_scrolls_row(row: &gtk::ScrolledWindow) {
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
pub fn continue_card(item: &Item, cards: &Cards) -> gtk::Widget {
    let facts = Facts::of(item);
    let tile = tile(item.id(), &facts.title, "landscape", LANDSCAPE);
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

    let caption = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    caption.append(
        &gtk::Label::builder()
            .label(facts.caption())
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build(),
    );
    if let (Some(length), Some(position)) = (facts.length, facts.position()) {
        caption.append(
            &gtk::Label::builder()
                .label(format::remaining(length, position))
                .css_classes(["dim-label", "caption"])
                .build(),
        );
    }
    // An episode under its show's name: which one, and its title.
    let below = gtk::Box::new(gtk::Orientation::Vertical, 2);
    below.append(&caption);
    if let (Some(_), Some(episode)) = (&facts.parent, &facts.episode) {
        below.append(
            &gtk::Label::builder()
                .label(episode)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
    }
    let card = card(item, &tile, &below, LANDSCAPE.0, &cards.open);
    card.add_css_class("continue-card");
    if let Some(set_watched) = &cards.set_watched {
        card_menu(&card, item.id(), set_watched);
    }
    card
}

/// A 2:3 poster card: the title on the tile, a badge when watched, and the
/// title and year below.
pub fn poster_card(item: &Item, cards: &Cards) -> gtk::Widget {
    let facts = Facts::of(item);
    let tile = tile(item.id(), &facts.title, "portrait", PORTRAIT);
    cards.art(
        &tile,
        facts.artwork,
        &[ArtworkKind::Poster],
        ArtworkSize::Small,
    );
    if facts.playback.is_some_and(|p| p.watched) {
        let badge = gtk::Image::builder()
            .icon_name(crate::icons::CHECK)
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
    caption.append(
        &gtk::Label::builder()
            .label(&facts.title)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build(),
    );
    if let Some(detail) = &facts.detail {
        caption.append(
            &gtk::Label::builder()
                .label(detail)
                .xalign(0.0)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
    }
    card(item, &tile, &caption, PORTRAIT.0, &cards.open)
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
fn card(
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
        .valign(gtk::Align::Start)
        // Every card lifts on hover: Continue watching, posters in rows
        // and in library grids alike.
        .css_classes(["card-button", "flat", "lift"])
        .build();
    let (item, open) = (item.clone(), open.clone());
    button.connect_clicked(move |_| open(item.clone()));
    button.upcast()
}

/// A Continue watching card's menu, on a right-click or a long press, at
/// the pointer: mark watched, or reset the progress. While it shows, the
/// card stays lifted.
fn card_menu(card: &gtk::Widget, id: &MediaId, set_watched: &Rc<dyn Fn(MediaId, bool)>) {
    // Rows of icon and label, as designed: GTK's own menu drops the icon
    // of any item that has a label.
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
    for (label, icon, watched) in [
        ("Mark as watched", crate::icons::WATCHED, true),
        ("Reset watch progress", crate::icons::BACK_10, false),
    ] {
        let row = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name(icon)
                    .label(label)
                    .halign(gtk::Align::Start)
                    .build(),
            )
            .css_classes(["flat", "card-menu-item"])
            .build();
        let (id, set_watched, popover) = (id.clone(), set_watched.clone(), popover.downgrade());
        row.connect_clicked(move |_| {
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
            set_watched(id.clone(), watched);
        });
        rows.append(&row);
    }
    popover.set_parent(card);
    {
        let card = card.downgrade();
        popover.connect_visible_notify(move |popover| {
            if let Some(card) = card.upgrade() {
                if popover.is_visible() {
                    card.add_css_class("menu-open");
                } else {
                    card.remove_css_class("menu-open");
                }
            }
        });
    }
    // The popover is the card's child, and goes when the card does.
    {
        let popover = popover.clone();
        card.connect_destroy(move |_| popover.unparent());
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
    card.add_controller(right_click);
    let long_press = gtk::GestureLongPress::builder().touch_only(true).build();
    long_press.connect_pressed(move |_, x, y| open_at(x, y));
    card.add_controller(long_press);
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
        .margin_start(14)
        .margin_end(14)
        .margin_bottom(16)
        .css_classes(["tile-title"])
        .build();
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
            Item::Track(t) => plain(&t.title, t.track.map(|n| format!("Track {n}")), &[]),
        }
    }

    /// Where playback stopped, for an item started and not finished.
    pub fn position(&self) -> Option<std::time::Duration> {
        self.playback
            .filter(|p| !p.watched)
            .and_then(|p| p.position)
    }

    /// What a landscape card says first: the show for an episode (its
    /// episode label goes under it), otherwise the title.
    fn caption(&self) -> String {
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
        })
    }

    #[test]
    fn an_episode_card_leads_with_its_show() {
        let item = episode(Some("12 Monkeys"));
        assert_eq!(Facts::of(&item).caption(), "12 Monkeys");
    }

    #[test]
    fn an_episode_without_a_show_name_leads_with_its_label() {
        let item = episode(None);
        assert_eq!(Facts::of(&item).caption(), "S1 · E4 · Seven");
    }

    #[test]
    fn an_episodes_detail_names_its_show() {
        let item = episode(Some("12 Monkeys"));
        assert_eq!(
            Facts::of(&item).detail.as_deref(),
            Some("12 Monkeys · S1 · E4")
        );
    }
}
