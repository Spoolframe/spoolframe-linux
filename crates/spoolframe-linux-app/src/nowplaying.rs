//! Now Playing, the full player for music, as in the design: the cover,
//! the track, its artist and album, the seek bar, shuffle, previous,
//! play, next and repeat, volume and what the file is, beside the queue
//! ("Up next": what plays now, the hand-added queue, then the rest of the
//! album, P1-23). Leaving it keeps playing in the bar.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gst_play::prelude::*;
use spoolframe_linux_core::backend::ArtworkSize;
use spoolframe_linux_core::format;
use spoolframe_types::{ArtworkKind, Item};

use crate::artwork::Art;
use crate::playback::{Change, Playback, Repeat};
use crate::player::{Actions, mono, round_button};
use crate::views::{Cards, Enqueue};
use crate::volume::Volume;

const COVER: (i32, i32) = (360, 360);

pub fn page(playback: &Rc<Playback>, actions: Actions, cards: &Cards) -> adw::NavigationPage {
    let collapse = gtk::Button::builder()
        .icon_name(crate::icons::DOWN)
        .tooltip_text("Back (Esc) · keeps playing in the bar")
        .build();
    {
        let minimize = actions.minimize.clone();
        collapse.connect_clicked(move |_| minimize());
    }
    let header = adw::HeaderBar::builder()
        .css_classes(["item-header"])
        .title_widget(&adw::WindowTitle::new("Now Playing", ""))
        .show_back_button(false)
        .build();
    header.pack_start(&collapse);
    // The current track to the queue again (P1-23).
    let more = {
        let weak = Rc::downgrade(playback);
        crate::views::menu_popover(vec![crate::views::enqueue_entries(move |at| {
            let Some(playback) = weak.upgrade() else {
                return;
            };
            let Some(item) = playback.current() else {
                return;
            };
            let album = playback.album();
            match at {
                Enqueue::Next => playback.play_next(vec![item], album),
                Enqueue::Last => playback.add_to_queue(vec![item], album),
            }
        })])
    };
    header.pack_end(&crate::views::menu_button(&more, "More"));

    // The cover is the album's, set on each track: a queued one may be
    // from another album. A single track found by search has none.
    let cover_slot = adw::Bin::builder().halign(gtk::Align::Center).build();

    let title = gtk::Label::builder()
        .css_classes(["title-1", "now-title"])
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    let artist = gtk::Label::builder().css_classes(["now-artist"]).build();
    let detail = gtk::Label::builder()
        .css_classes(["dim-label", "small-text"])
        .build();
    let titles = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    titles.append(&title);
    titles.append(&artist);
    titles.append(&detail);

    let scale = {
        let weak = Rc::downgrade(playback);
        crate::transport::seek_scale(move |to| {
            if let Some(playback) = weak.upgrade() {
                playback.seek(to);
            }
        })
    };
    let position = mono("0:00");
    let left = mono("");
    for label in [&position, &left] {
        label.add_css_class("dim-label");
        label.add_css_class("caption");
    }
    left.set_hexpand(true);
    left.set_xalign(1.0);
    let times = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    times.append(&position);
    times.append(&left);
    let progress = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .width_request(520)
        .halign(gtk::Align::Center)
        .build();
    progress.append(&scale);
    progress.append(&times);

    let transport = Transport::new(playback, true);
    transport.row.set_halign(gtk::Align::Center);

    let volume = Volume::of(playback, 180);
    volume.row.set_halign(gtk::Align::Center);

    let chips = gtk::Box::builder()
        .spacing(8)
        .halign(gtk::Align::Center)
        .build();
    let format_chip = chip("");
    format_chip.set_visible(false);
    chips.append(&format_chip);
    let path_chip = chip("Direct · LAN");
    path_chip.add_css_class("success");
    chips.append(&path_chip);

    let current = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(22)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .margin_top(28)
        .margin_bottom(28)
        .margin_start(48)
        .margin_end(48)
        .build();
    for widget in [
        cover_slot.upcast_ref::<gtk::Widget>(),
        titles.upcast_ref(),
        progress.upcast_ref(),
        transport.row.upcast_ref(),
        volume.row.upcast_ref(),
        chips.upcast_ref(),
    ] {
        current.append(widget);
    }

    // Going to a track's artist or album leaves Now Playing for the bar,
    // so the page shows.
    let go_to: crate::views::Open = {
        let (minimize, open) = (actions.minimize.clone(), cards.open.clone());
        Rc::new(move |item| {
            minimize();
            open(item);
        })
    };
    let queue = Queue::new(playback, go_to);
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.append(&current);
    body.append(&queue.aside);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));
    let page = adw::NavigationPage::builder()
        .title("Now Playing")
        .child(&toolbar)
        .build();

    let view = Rc::new(View {
        art: cards.art.clone(),
        cover_slot,
        cover_album: RefCell::new(None),
        title,
        artist,
        detail,
        scale,
        position,
        left,
        transport,
        volume,
        format_chip,
        queue,
    });
    {
        let view = Rc::downgrade(&view);
        let weak_playback = Rc::downgrade(playback);
        playback.subscribe(move |change| {
            let (Some(view), Some(playback)) = (view.upgrade(), weak_playback.upgrade()) else {
                return false;
            };
            view.update(&playback, change);
            true
        });
    }

    // Leaving keeps playing, in the bar. The page owns its view.
    page.connect_hidden(move |_| {
        let _ = &view;
    });
    page
}

struct View {
    art: Option<Rc<Art>>,
    cover_slot: adw::Bin,
    /// The album whose cover shows, so the next track of it keeps it.
    cover_album: RefCell<Option<spoolframe_types::MediaId>>,
    title: gtk::Label,
    artist: gtk::Label,
    detail: gtk::Label,
    scale: gtk::Scale,
    position: gtk::Label,
    left: gtk::Label,
    transport: Transport,
    volume: Volume,
    format_chip: gtk::Label,
    queue: Queue,
}

impl View {
    fn update(&self, playback: &Playback, change: &Change) {
        match change {
            Change::Item(item) => {
                let facts = crate::views::Facts::of(item);
                self.title.set_label(&facts.title);
                let album = playback.album();
                self.show_cover(item, album.as_ref());
                let artist = album.as_ref().and_then(|a| a.artist_name.clone());
                self.artist.set_label(artist.as_deref().unwrap_or_default());
                self.artist.set_visible(artist.is_some());
                let number = match item {
                    Item::Track(t) => t.track.map(|n| format!("track {n}")),
                    _ => None,
                };
                let detail: Vec<String> = [
                    album.as_ref().map(|a| a.title.clone()),
                    album.as_ref().and_then(|a| a.year).map(|y| y.to_string()),
                    number,
                ]
                .into_iter()
                .flatten()
                .collect();
                self.detail.set_label(&detail.join(" · "));
                self.format_chip.set_visible(false);
                self.transport.show_item(playback);
                self.queue.show(playback);
            }
            // What comes next changed: Next may have something now.
            Change::Queue => {
                self.queue.show(playback);
                self.transport.show_item(playback);
            }
            Change::Playing(playing) => self.transport.show_playing(*playing),
            Change::Position { at, length } => {
                crate::transport::show_position(&self.scale, *at, *length);
                self.position.set_label(&format::timestamp(*at));
                self.left.set_label(&format!(
                    "−{}",
                    format::timestamp(length.saturating_sub(*at))
                ));
            }
            Change::MediaInfo(info) => {
                if let Some(audio) = info.audio_streams().first() {
                    let codec = audio.codec().map(|c| short_codec(&c)).unwrap_or_default();
                    let rate = audio.sample_rate();
                    let label = match rate {
                        r if r > 0 => format!("{codec} · {} kHz", r as f64 / 1000.0),
                        _ => codec,
                    };
                    self.format_chip.set_label(&label);
                    self.format_chip.set_visible(!label.is_empty());
                }
            }
            Change::Volume(_) | Change::Muted(_) => self.volume.show(change),
            Change::Modes { shuffle, repeat } => {
                self.transport.show_modes(*shuffle, *repeat);
                // Repeat all gives the last track a next.
                self.transport.show_item(playback);
            }
            Change::Starting
            | Change::Buffering(_)
            | Change::Failed(_)
            | Change::Skipped(_)
            | Change::Offer(_)
            | Change::IntroSkipped => {}
        }
    }
}

impl View {
    /// The album's cover, kept while the album stays the same.
    fn show_cover(&self, item: &Item, album: Option<&spoolframe_types::Album>) {
        let id = album.map(|a| a.id.clone());
        if *self.cover_album.borrow() == id && self.cover_slot.child().is_some() {
            return;
        }
        let cover = crate::views::tile(
            id.as_ref().unwrap_or(item.id()),
            album.map_or("", |a| a.title.as_str()),
            "now-cover",
            COVER,
        );
        if let (Some(art), Some(album)) = (&self.art, album) {
            art.show(
                &cover,
                &album.artwork,
                &[ArtworkKind::Poster, ArtworkKind::Thumb],
                ArtworkSize::Large,
            );
        }
        self.cover_slot.set_child(Some(&cover));
        self.cover_album.replace(id);
    }
}

/// "FLAC" from "Free Lossless Audio Codec (FLAC)", as GStreamer names it.
fn short_codec(codec: &str) -> String {
    match (codec.rfind('('), codec.rfind(')')) {
        (Some(open), Some(close)) if open < close => codec[open + 1..close].to_owned(),
        _ => codec.to_owned(),
    }
}

fn chip(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes(["chip", "caption-heading"])
        .build()
}

/// Shuffle, previous, play, next and repeat. `large` is Now Playing's
/// size; the bar uses the small one.
pub struct Transport {
    pub row: gtk::Box,
    shuffle: gtk::ToggleButton,
    repeat: gtk::ToggleButton,
    toggle: gtk::Button,
    next: gtk::Button,
    /// True while the toggles are set from the playback, not clicked.
    syncing: Rc<Cell<bool>>,
}

impl Transport {
    pub fn new(playback: &Rc<Playback>, large: bool) -> Self {
        let syncing = Rc::new(Cell::new(false));
        let shuffle = gtk::ToggleButton::builder()
            .icon_name(crate::icons::SHUFFLE)
            .tooltip_text("Shuffle")
            .valign(gtk::Align::Center)
            .css_classes(["circular", "flat", "mode"])
            .build();
        let repeat = gtk::ToggleButton::builder()
            .icon_name(crate::icons::REPEAT)
            .tooltip_text("Repeat")
            .valign(gtk::Align::Center)
            .css_classes(["circular", "flat", "mode"])
            .build();
        let previous = round_button("Previous", crate::icons::PREVIOUS, &["flat"]);
        let toggle = round_button("Pause", crate::icons::PAUSE, &["player-play"]);
        let next = round_button("Next", crate::icons::NEXT, &["flat"]);
        if large {
            toggle.add_css_class("large");
        }
        let weak = Rc::downgrade(playback);
        let with = move |run: fn(&Rc<Playback>)| {
            let weak = weak.clone();
            move || {
                if let Some(playback) = weak.upgrade() {
                    run(&playback);
                }
            }
        };
        {
            let (run, syncing) = (with(|p| p.toggle_shuffle()), syncing.clone());
            shuffle.connect_toggled(move |_| {
                if !syncing.get() {
                    run();
                }
            });
        }
        {
            let (run, syncing) = (with(|p| p.cycle_repeat()), syncing.clone());
            repeat.connect_toggled(move |_| {
                if !syncing.get() {
                    run();
                }
            });
        }
        let run = with(|p| p.previous());
        previous.connect_clicked(move |_| run());
        let run = with(|p| p.toggle());
        toggle.connect_clicked(move |_| run());
        let run = with(|p| p.next());
        next.connect_clicked(move |_| run());
        let row = gtk::Box::builder()
            .spacing(if large { 18 } else { 10 })
            .build();
        for widget in [
            shuffle.upcast_ref::<gtk::Widget>(),
            previous.upcast_ref(),
            toggle.upcast_ref(),
            next.upcast_ref(),
            repeat.upcast_ref(),
        ] {
            row.append(widget);
        }
        Self {
            row,
            shuffle,
            repeat,
            toggle,
            next,
            syncing,
        }
    }

    pub fn show_item(&self, playback: &Playback) {
        self.next.set_sensitive(playback.has_next());
    }

    pub fn show_playing(&self, playing: bool) {
        crate::transport::show_playing(&self.toggle, playing);
    }

    pub fn show_modes(&self, shuffle: bool, repeat: Repeat) {
        self.syncing.set(true);
        self.shuffle.set_active(shuffle);
        self.repeat.set_active(repeat != Repeat::Off);
        self.syncing.set(false);
        let (icon, tip) = match repeat {
            Repeat::Off => (crate::icons::REPEAT, "Repeat"),
            Repeat::All => (crate::icons::REPEAT, "Repeating the queue"),
            Repeat::One => (crate::icons::REPEAT_ONE, "Repeating this track"),
        };
        self.repeat.set_icon_name(icon);
        self.repeat.set_tooltip_text(Some(tip));
    }
}

/// What a row of "Up next" stands for.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Row {
    /// A section heading, or the track playing: not clickable.
    Inert,
    /// A hand-added item, by its queue id.
    Queued(u64),
    /// The context's item at this position in its play order.
    Context(usize),
}

/// "Up next", in three sections: "Now playing", "Next in queue" (the
/// hand-added items; drag to reorder, × to remove, "Clear queue") and
/// "Next from: <album>". A click plays that item: a queued one leaves the
/// queue, a context one jumps the album there and keeps the queue.
struct Queue {
    aside: gtk::Box,
    clear: gtk::Button,
    list: gtk::ListBox,
    rows: Rc<RefCell<Vec<Row>>>,
    playback: std::rc::Weak<Playback>,
    /// Opens a track's artist or album, leaving Now Playing.
    go_to: crate::views::Open,
}

impl Queue {
    fn new(playback: &Rc<Playback>, go_to: crate::views::Open) -> Self {
        let heading = gtk::Label::builder()
            .label("Up next")
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["title-3"])
            .build();
        let clear = gtk::Button::builder()
            .label("Clear queue")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "caption"])
            .build();
        {
            let weak = Rc::downgrade(playback);
            clear.connect_clicked(move |_| {
                if let Some(playback) = weak.upgrade() {
                    playback.clear_queue();
                }
            });
        }
        let titles = gtk::Box::builder()
            .spacing(8)
            .margin_top(18)
            .margin_bottom(10)
            .margin_start(20)
            .margin_end(20)
            .build();
        titles.append(&heading);
        titles.append(&clear);
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .activate_on_single_click(true)
            .css_classes(["navigation-sidebar", "queue"])
            .build();
        let rows: Rc<RefCell<Vec<Row>>> = Rc::default();
        {
            let (weak, rows) = (Rc::downgrade(playback), rows.clone());
            list.connect_row_activated(move |_, row| {
                let Some(playback) = weak.upgrade() else {
                    return;
                };
                let picked = rows.borrow().get(row.index() as usize).copied();
                match picked {
                    Some(Row::Queued(id)) => playback.play_queued(id),
                    Some(Row::Context(pos)) => playback.play_context_at(pos),
                    Some(Row::Inert) | None => {}
                }
            });
        }
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let aside = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .width_request(400)
            .css_classes(["queue-aside"])
            .build();
        aside.append(&titles);
        aside.append(&scroller);
        Self {
            aside,
            clear,
            list,
            rows,
            playback: Rc::downgrade(playback),
            go_to,
        }
    }

    /// Rebuilt on each change: a queue is short, and rows that move
    /// between sections would otherwise need tracking one by one.
    fn show(&self, playback: &Playback) {
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }
        let mut rows = Vec::new();
        let up_next = playback.up_next();
        self.clear.set_visible(!up_next.queued.is_empty());

        if let Some(item) = playback.current() {
            self.heading("Now playing", &mut rows);
            let artist = artist_of(playback.album().as_ref());
            let row = self.append(
                &queue_row(&number("▶"), &item, &artist, None),
                Row::Inert,
                &mut rows,
            );
            row.add_css_class("playing");
        }

        if !up_next.queued.is_empty() {
            self.heading("Next in queue", &mut rows);
            for (index, queued) in up_next.queued.iter().enumerate() {
                let artist = artist_of(queued.album.as_ref());
                let handle = gtk::Image::builder()
                    .icon_name(crate::icons::DRAG)
                    .tooltip_text("Drag to reorder")
                    .css_classes(["dim-label"])
                    .build();
                let content = queue_row(&handle, &queued.item, &artist, None);
                let row = self.append(&content, Row::Queued(queued.id), &mut rows);
                self.reorderable(&row, queued.id, index);
                // Right-click: play it now, or take it out of the queue.
                let id = queued.id;
                let mut groups =
                    vec![vec![self.entry("Play now", crate::icons::PLAY, move |p| {
                        p.play_queued(id)
                    })]];
                if let Item::Track(track) = &queued.item {
                    groups.push(crate::views::track_go_to_entries(track, &self.go_to, true));
                }
                groups.push(vec![crate::views::MenuEntry {
                    destructive: true,
                    ..self.entry("Remove from queue", crate::icons::REMOVE, move |p| {
                        p.remove_queued(id)
                    })
                }]);
                let menu = crate::views::menu_popover(groups);
                crate::views::context_menu(row.upcast_ref(), &menu);
            }
        }

        if !up_next.context.is_empty() {
            let heading = match &up_next.from {
                Some(album) => format!("Next from {}", album.title),
                None => "Next".to_owned(),
            };
            self.heading(&heading, &mut rows);
            let artist = artist_of(up_next.from.as_ref());
            for (n, (pos, item)) in up_next.context.iter().enumerate() {
                let content = queue_row(&number(&(n + 1).to_string()), item, &artist, None);
                let row = self.append(&content, Row::Context(*pos), &mut rows);
                // Right-click: play it now, or queue it next.
                let mut groups = Vec::new();
                {
                    let (pos, item, from) = (*pos, item.clone(), up_next.from.clone());
                    groups.push(vec![
                        self.entry("Play now", crate::icons::PLAY, move |p| {
                            p.play_context_at(pos)
                        }),
                        self.entry("Play next", crate::icons::PLAY_NEXT, move |p| {
                            p.play_next(vec![item.clone()], from.clone())
                        }),
                    ]);
                }
                if let Item::Track(track) = item {
                    groups.push(crate::views::track_go_to_entries(track, &self.go_to, true));
                }
                let menu = crate::views::menu_popover(groups);
                crate::views::context_menu(row.upcast_ref(), &menu);
            }
        }
        self.rows.replace(rows);
    }

    /// A menu row that runs `run` on the playback, while there is one.
    fn entry(
        &self,
        label: &'static str,
        icon: &'static str,
        run: impl Fn(&Rc<Playback>) + 'static,
    ) -> crate::views::MenuEntry {
        let weak = self.playback.clone();
        crate::views::MenuEntry::new(label, icon, move || {
            if let Some(playback) = weak.upgrade() {
                run(&playback);
            }
        })
    }

    fn heading(&self, text: &str, rows: &mut Vec<Row>) {
        let label = gtk::Label::builder()
            .label(text.to_uppercase())
            .xalign(0.0)
            .margin_top(10)
            .css_classes(["queue-heading", "dim-label"])
            .build();
        let row = self.append(&label, Row::Inert, rows);
        row.set_activatable(false);
    }

    fn append(
        &self,
        child: &impl IsA<gtk::Widget>,
        what: Row,
        rows: &mut Vec<Row>,
    ) -> gtk::ListBoxRow {
        let row = gtk::ListBoxRow::builder().child(child).build();
        row.set_activatable(what != Row::Inert);
        self.list.append(&row);
        rows.push(what);
        row
    }

    /// Drag a queued row onto another to move it there.
    fn reorderable(&self, row: &gtk::ListBoxRow, id: u64, index: usize) {
        let source = gtk::DragSource::builder()
            .actions(gtk::gdk::DragAction::MOVE)
            .build();
        source.connect_prepare(move |_, _, _| {
            Some(gtk::gdk::ContentProvider::for_value(&id.to_value()))
        });
        {
            let row = row.clone();
            source.connect_drag_begin(move |source, _| {
                source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&row))), 0, 0);
            });
        }
        row.add_controller(source);
        let target = gtk::DropTarget::new(u64::static_type(), gtk::gdk::DragAction::MOVE);
        let weak = self.playback.clone();
        target.connect_drop(move |_, value, _, _| {
            let (Some(playback), Ok(dragged)) = (weak.upgrade(), value.get::<u64>()) else {
                return false;
            };
            if dragged != id {
                playback.move_queued(dragged, index);
            }
            true
        });
        row.add_controller(target);
    }
}

fn artist_of(album: Option<&spoolframe_types::Album>) -> String {
    album
        .and_then(|a| a.artist_name.clone())
        .unwrap_or_default()
}

/// A row's mark: its number, or ▶ for the track playing.
fn number(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .width_chars(3)
        .css_classes(["numeric", "dim-label"])
        .build()
}

/// A track's row: a mark (number, ▶ or drag handle), its title and
/// artist, its length, and an optional button at the end.
fn queue_row(
    mark: &impl IsA<gtk::Widget>,
    item: &Item,
    artist: &str,
    end: Option<&gtk::Button>,
) -> gtk::Box {
    let facts = crate::views::Facts::of(item);
    let title = gtk::Label::builder()
        .label(&facts.title)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let by = gtk::Label::builder()
        .label(artist)
        .xalign(0.0)
        .visible(!artist.is_empty())
        .css_classes(["dim-label", "caption"])
        .build();
    let titles = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .build();
    titles.append(&title);
    titles.append(&by);
    let length = mono(&facts.length.map(format::timestamp).unwrap_or_default());
    length.add_css_class("dim-label");
    length.add_css_class("caption");
    let row = gtk::Box::builder()
        .spacing(10)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    row.append(mark);
    row.append(&titles);
    row.append(&length);
    if let Some(end) = end {
        row.append(end);
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_codec_name_is_shortened_to_its_parenthesis() {
        assert_eq!(short_codec("Free Lossless Audio Codec (FLAC)"), "FLAC");
    }

    #[test]
    fn a_codec_name_without_a_parenthesis_stays() {
        assert_eq!(short_codec("Opus"), "Opus");
    }
}
