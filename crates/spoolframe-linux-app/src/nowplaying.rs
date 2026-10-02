//! Now Playing, the full player for music, as in the design: the cover,
//! the track, its artist and album, the seek bar, shuffle, previous,
//! play, next and repeat, volume and what the file is, beside the queue
//! ("Up next"). Collapsing it keeps playing in the bar; back stops.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gst_play::prelude::*;
use gtk::glib;
use spoolframe_linux_core::backend::ArtworkSize;
use spoolframe_linux_core::format;
use spoolframe_types::{ArtworkKind, Item};

use crate::playback::{Change, Playback, Repeat};
use crate::player::{Actions, mono, round_button};
use crate::views::Cards;

const COVER: (i32, i32) = (360, 360);

pub fn page(playback: &Rc<Playback>, actions: Actions, cards: &Cards) -> adw::NavigationPage {
    let minimizing = Rc::new(Cell::new(false));
    let collapse = gtk::Button::builder()
        .icon_name(crate::icons::DOWN)
        .tooltip_text("Collapse to the bar and keep playing")
        .build();
    {
        let (minimize, minimizing) = (actions.minimize.clone(), minimizing.clone());
        collapse.connect_clicked(move |_| {
            minimizing.set(true);
            minimize();
        });
    }
    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new("Now Playing", ""))
        .show_back_button(false)
        .build();
    header.pack_start(&collapse);

    // The cover is the album's; a single track found by search has none.
    let cover_slot = adw::Bin::builder().halign(gtk::Align::Center).build();
    let cover_id = playback.current().map(|i| i.id().clone());
    if let Some(id) = &cover_id {
        let cover = crate::views::tile(
            id,
            playback.album().map_or("", |a| a.title.as_str()),
            "now-cover",
            COVER,
        );
        if let Some(album) = playback.album() {
            cards.art(
                &cover,
                &album.artwork,
                &[ArtworkKind::Poster, ArtworkKind::Thumb],
                ArtworkSize::Large,
            );
        }
        cover_slot.set_child(Some(&cover));
    }

    let title = gtk::Label::builder()
        .css_classes(["title-1"])
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    let artist = gtk::Label::builder()
        .css_classes(["heading", "accent"])
        .build();
    let detail = gtk::Label::builder()
        .css_classes(["dim-label", "caption"])
        .build();
    let titles = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    titles.append(&title);
    titles.append(&artist);
    titles.append(&detail);

    let scale = gtk::Scale::builder()
        .orientation(gtk::Orientation::Horizontal)
        .adjustment(&gtk::Adjustment::new(0.0, 0.0, 0.0, 1.0, 10.0, 0.0))
        .hexpand(true)
        .css_classes(["seek"])
        .build();
    {
        let weak = Rc::downgrade(playback);
        scale.connect_change_value(move |_, _, value| {
            if let Some(playback) = weak.upgrade() {
                playback.seek(Duration::from_secs_f64(value.max(0.0)));
            }
            glib::Propagation::Proceed
        });
    }
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

    let volume = volume_row(playback);
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

    let queue = Queue::new(playback);
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

    let close = actions.close.clone();
    page.connect_hidden(move |_| {
        let _ = &view;
        if !minimizing.get() {
            close();
        }
    });
    page
}

struct View {
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
                let artist = album.and_then(|a| a.artist_name.clone());
                self.artist.set_label(artist.as_deref().unwrap_or_default());
                self.artist.set_visible(artist.is_some());
                let (queue, at) = playback.queue();
                let detail: Vec<String> = [
                    album.map(|a| a.title.clone()),
                    album.and_then(|a| a.year).map(|y| y.to_string()),
                    Some(format!("track {} of {}", at + 1, queue.len())),
                ]
                .into_iter()
                .flatten()
                .collect();
                self.detail.set_label(&detail.join(" · "));
                self.format_chip.set_visible(false);
                self.transport.show_item(playback);
                self.queue.show(playback);
            }
            Change::Queue => self.queue.show(playback),
            Change::Playing(playing) => self.transport.show_playing(*playing),
            Change::Position { at, length } => {
                self.scale.adjustment().set_upper(length.as_secs_f64());
                self.scale.set_value(at.as_secs_f64());
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
            Change::Volume(volume) => self.volume.scale.set_value(*volume),
            Change::Modes { shuffle, repeat } => self.transport.show_modes(*shuffle, *repeat),
            Change::Starting | Change::Buffering(_) | Change::Failed(_) => {}
        }
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
        self.toggle.set_icon_name(if playing {
            crate::icons::PAUSE
        } else {
            crate::icons::PLAY
        });
        self.toggle
            .set_tooltip_text(Some(if playing { "Pause" } else { "Play" }));
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

struct Volume {
    row: gtk::Box,
    scale: gtk::Scale,
}

fn volume_row(playback: &Rc<Playback>) -> Volume {
    let scale = gtk::Scale::builder()
        .orientation(gtk::Orientation::Horizontal)
        .adjustment(&gtk::Adjustment::new(1.0, 0.0, 1.0, 0.05, 0.1, 0.0))
        .width_request(180)
        .tooltip_text("Volume")
        .build();
    let weak = Rc::downgrade(playback);
    scale.connect_change_value(move |_, _, value| {
        if let Some(playback) = weak.upgrade() {
            playback.set_volume(value);
        }
        glib::Propagation::Proceed
    });
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.append(
        &gtk::Image::builder()
            .icon_name(crate::icons::VOLUME)
            .css_classes(["dim-label"])
            .build(),
    );
    row.append(&scale);
    Volume { row, scale }
}

/// "Up next": the queue in play order, the current track marked; a click
/// plays that track.
struct Queue {
    aside: gtk::Box,
    from: gtk::Label,
    list: gtk::ListBox,
    /// The queue the list shows, to skip rebuilding when only the item
    /// moved.
    shown: RefCell<Vec<spoolframe_types::MediaId>>,
}

impl Queue {
    fn new(playback: &Rc<Playback>) -> Self {
        let heading = gtk::Label::builder()
            .label("Up next")
            .xalign(0.0)
            .css_classes(["title-3"])
            .build();
        let from = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["dim-label", "caption"])
            .build();
        let titles = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .margin_top(18)
            .margin_bottom(10)
            .margin_start(20)
            .margin_end(20)
            .build();
        titles.append(&heading);
        titles.append(&from);
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .activate_on_single_click(true)
            .css_classes(["navigation-sidebar", "queue"])
            .build();
        let weak = Rc::downgrade(playback);
        list.connect_row_activated(move |_, row| {
            if let Some(playback) = weak.upgrade() {
                playback.play_at(row.index() as usize);
            }
        });
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
            from,
            list,
            shown: RefCell::new(Vec::new()),
        }
    }

    fn show(&self, playback: &Playback) {
        let (items, at) = playback.queue();
        self.from.set_label(&match playback.album() {
            Some(album) => format!("Playing from {}", album.title),
            None => String::new(),
        });
        let ids: Vec<_> = items.iter().map(|i| i.id().clone()).collect();
        if *self.shown.borrow() != ids {
            while let Some(row) = self.list.row_at_index(0) {
                self.list.remove(&row);
            }
            let artist = playback
                .album()
                .and_then(|a| a.artist_name.clone())
                .unwrap_or_default();
            for (i, item) in items.iter().enumerate() {
                self.list.append(&queue_row(i, item, &artist));
            }
            self.shown.replace(ids);
        }
        let mut index = 0;
        while let Some(row) = self.list.row_at_index(index) {
            let i = index as usize;
            for (class, on) in [("played", i < at), ("playing", i == at)] {
                if on {
                    row.add_css_class(class);
                } else {
                    row.remove_css_class(class);
                }
            }
            if let Some(mark) = row
                .child()
                .and_then(|c| c.first_child())
                .and_downcast::<gtk::Label>()
            {
                mark.set_label(&if i == at {
                    "▶".to_owned()
                } else {
                    (i + 1).to_string()
                });
            }
            index += 1;
        }
    }
}

fn queue_row(index: usize, item: &Item, artist: &str) -> gtk::Box {
    let facts = crate::views::Facts::of(item);
    let mark = gtk::Label::builder()
        .label((index + 1).to_string())
        .width_chars(3)
        .css_classes(["numeric", "dim-label"])
        .build();
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
    row.append(&mark);
    row.append(&titles);
    row.append(&length);
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
