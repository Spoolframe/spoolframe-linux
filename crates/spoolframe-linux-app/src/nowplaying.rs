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
    let title_row = gtk::Box::builder()
        .spacing(12)
        .halign(gtk::Align::Center)
        .build();
    title_row.append(&title);
    let follow = cards.hearts.as_ref().map(|hearts| {
        let (heart, follow) = hearts.following();
        heart.set_valign(gtk::Align::Center);
        title_row.append(&heart);
        follow
    });
    titles.append(&title_row);
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
    // How this device reaches the server now, as it changes (P6-06).
    let path_chip = chip("");
    chips.append(&path_chip);
    show_path(&path_chip, playback);
    {
        let (chip, weak) = (path_chip.downgrade(), Rc::downgrade(playback));
        gtk::glib::timeout_add_seconds_local(2, move || {
            let (Some(chip), Some(playback)) = (chip.upgrade(), weak.upgrade()) else {
                return gtk::glib::ControlFlow::Break;
            };
            show_path(&chip, &playback);
            gtk::glib::ControlFlow::Continue
        });
    }

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
    for widget in [cover_slot.upcast_ref::<gtk::Widget>(), titles.upcast_ref()] {
        let menu = current_track_menu(
            &Rc::downgrade(playback),
            &go_to,
            cards.add_to_playlist.as_ref(),
        );
        crate::views::context_menu(widget, &menu);
    }
    let queue = Queue::new(
        playback,
        go_to,
        cards.art.clone(),
        cards.add_to_playlist.clone(),
    );
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.append(&current);
    body.append(&queue.aside);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    let content = visualizer_view(playback, &body, &header, cards.art.clone());
    toolbar.set_content(Some(&content));
    let page = adw::NavigationPage::builder()
        .title("Now Playing")
        .child(&toolbar)
        .build();

    let view = Rc::new(View {
        art: cards.art.clone(),
        cover_slot,
        cover_album: RefCell::new(None),
        follow,
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

/// A visualizer fills the same application page as Now Playing. The window
/// stays in its normal mode, and returning to the cover never stops music.
fn visualizer_view(
    playback: &Rc<Playback>,
    normal: &gtk::Box,
    header: &adw::HeaderBar,
    art: Option<Rc<Art>>,
) -> gtk::Stack {
    use crate::visualizer::{Canvas, Mode};
    let stack = gtk::Stack::builder().hexpand(true).vexpand(true).build();
    // Hidden queue contents must not impose their minimum size on the visualizer.
    stack.set_hhomogeneous(false);
    stack.set_vhomogeneous(false);
    stack.add_named(normal, Some("cover"));
    let measurements = playback.analysis(false);
    let canvas = Canvas::new(measurements.clone().unwrap_or_default());
    let picture = gtk::Picture::builder()
        .paintable(playback.paintable())
        .content_fit(gtk::ContentFit::Fill)
        .can_shrink(true)
        .hexpand(true)
        .vexpand(true)
        .build();
    let visuals = gtk::Stack::new();
    visuals.add_named(&canvas, Some("custom"));
    visuals.add_named(&picture, Some("gstreamer"));
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&visuals));
    let transport = Rc::new(Transport::new(playback, false));
    transport.row.set_halign(gtk::Align::Center);
    transport.row.set_valign(gtk::Align::End);
    transport.row.set_margin_bottom(24);
    transport.row.add_css_class("visualizer-controls");
    overlay.add_overlay(&transport.row);
    stack.add_named(&overlay, Some("visualizer"));
    let options = gtk::Box::builder()
        .spacing(12)
        .halign(gtk::Align::End)
        .valign(gtk::Align::Start)
        .margin_top(16)
        .margin_end(16)
        .css_classes(["visualizer-controls"])
        .build();
    let peaks = gtk::CheckButton::with_label("Peak markers");
    peaks.set_active(true);
    let trails = gtk::CheckButton::with_label("Phosphor trails");
    trails.set_active(true);
    options.append(&peaks);
    options.append(&trails);
    options.set_visible(false);
    overlay.add_overlay(&options);
    {
        let canvas = canvas.downgrade();
        peaks.connect_toggled(move |button| {
            if let Some(canvas) = canvas.upgrade() {
                canvas.set_peaks(button.is_active());
            }
        });
        let playback = Rc::downgrade(playback);
        trails.connect_toggled(move |button| {
            if let Some(playback) = playback.upgrade() {
                playback.visualization_trails(button.is_active());
            }
        });
    }

    let plugins = crate::visualizer::plugins();
    let mut labels = vec!["Album & queue".to_owned()];
    let mut choices: Vec<(Option<Mode>, Option<String>)> = vec![(None, None)];
    if measurements.is_some() {
        for (label, mode) in [
            ("Radial spectrum", Mode::Radial),
            ("Radial waves", Mode::Rings),
            ("VU deck", Mode::Vu),
            ("Ambient artwork", Mode::Ambient),
            ("Spectrum", Mode::Spectrum),
            ("Spectrogram", Mode::Spectrogram),
        ] {
            labels.push(label.to_owned());
            choices.push((Some(mode), None));
        }
    }
    for (name, label) in plugins {
        labels.push(label);
        choices.push((None, Some(name)));
    }
    let selector =
        gtk::DropDown::from_strings(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    selector.set_tooltip_text(Some("Visualizer"));
    selector.set_sensitive(choices.len() > 1);
    header.pack_end(&selector);
    let plugin_selected = Rc::new(Cell::new(false));
    let apply: Rc<dyn Fn(&gtk::DropDown)> = {
        let plugin_selected = plugin_selected.clone();
        let weak = Rc::downgrade(playback);
        let stack = stack.downgrade();
        let canvas = canvas.downgrade();
        let visuals = visuals.downgrade();
        Rc::new(move |selector| {
            let (Some(playback), Some(stack), Some(canvas), Some(visuals)) = (
                weak.upgrade(),
                stack.upgrade(),
                canvas.upgrade(),
                visuals.upgrade(),
            ) else {
                return;
            };
            let Some((mode, plugin)) = choices.get(selector.selected() as usize) else {
                return;
            };
            plugin_selected.set(plugin.is_some());
            let scope = matches!(plugin.as_deref(), Some("wavescope" | "spacescope"));
            peaks.set_visible(*mode == Some(Mode::Spectrum));
            trails.set_visible(scope);
            options.set_visible(*mode == Some(Mode::Spectrum) || scope);
            playback.analysis(mode.is_some());
            if !playback.set_visualization(plugin.as_deref()) {
                selector.set_selected(0);
                return;
            }
            if scope {
                playback.visualization_trails(trails.is_active());
            }
            if let Some(mode) = mode {
                canvas.set_mode(*mode);
            }
            visuals.set_visible_child_name(if mode.is_some() {
                "custom"
            } else {
                "gstreamer"
            });
            stack.set_visible_child_name(if selector.selected() == 0 {
                "cover"
            } else {
                "visualizer"
            });
        })
    };
    {
        let apply = apply.clone();
        selector.connect_selected_notify(move |selector| apply(selector));
    }
    {
        let selector = selector.downgrade();
        stack.connect_map(move |_| {
            if let Some(selector) = selector.upgrade() {
                apply(&selector);
            }
        });
        let weak = Rc::downgrade(playback);
        stack.connect_unmap(move |_| {
            if let Some(playback) = weak.upgrade() {
                playback.analysis(false);
                playback.set_visualization(None);
            }
        });
    }
    let weak = Rc::downgrade(playback);
    let canvas = canvas.downgrade();
    let controls = Rc::downgrade(&transport);
    let generation = Rc::new(Cell::new(0u64));
    let selector = selector.downgrade();
    playback.subscribe(move |change| {
        let (Some(playback), Some(canvas), Some(controls)) =
            (weak.upgrade(), canvas.upgrade(), controls.upgrade())
        else {
            return false;
        };
        match change {
            Change::Item(item) => {
                controls.show_item(&playback);
                canvas.set_texture(None);
                generation.set(generation.get().wrapping_add(1));
                if let Some(art) = art.clone() {
                    let album = playback.album();
                    let (_, artwork) = crate::views::cover_of(item, album.as_ref());
                    let artwork = artwork.to_vec();
                    let version = generation.get();
                    let generation = generation.clone();
                    let canvas = canvas.downgrade();
                    gtk::glib::spawn_future_local(async move {
                        let result = art
                            .bytes(
                                &artwork,
                                &[ArtworkKind::Poster, ArtworkKind::Thumb],
                                ArtworkSize::Large,
                            )
                            .await;
                        if generation.get() == version
                            && let Some(canvas) = canvas.upgrade()
                        {
                            canvas.set_texture(
                                result.and_then(|(_, bytes)| crate::artwork::decode(&bytes).ok()),
                            );
                        }
                    });
                }
            }
            Change::TrackFailed(_) if plugin_selected.get() && !playback.visualization_active() => {
                if let Some(selector) = selector.upgrade() {
                    selector.set_selected(0);
                }
            }
            Change::Queue => controls.show_item(&playback),
            Change::Playing(playing) => controls.show_playing(*playing),
            Change::Modes { shuffle, repeat } => controls.show_modes(*shuffle, *repeat),
            _ => {}
        }
        true
    });
    // Own the control state for exactly the life of this page.
    stack.connect_destroy(move |_| {
        let _ = &transport;
    });
    stack
}

struct View {
    art: Option<Rc<Art>>,
    cover_slot: adw::Bin,
    /// The album whose cover shows, so the next track of it keeps it.
    cover_album: RefCell<Option<spoolframe_types::MediaId>>,
    follow: Option<crate::hearts::Follow>,
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
                if let Some(follow) = &self.follow {
                    follow(match item {
                        Item::Track(track) => Some(track.id.clone()),
                        _ => None,
                    });
                }
                let facts = crate::views::Facts::of(item);
                self.title.set_label(&facts.title);
                let album = playback.album();
                self.show_cover(item, album.as_ref());
                let artist = crate::views::track_artist(item, album.as_ref());
                self.artist.set_label(artist.as_deref().unwrap_or_default());
                self.artist.set_visible(artist.is_some());
                let number = match item {
                    Item::Track(t) => t.track.map(|n| format!("track {n}")),
                    _ => None,
                };
                let detail: Vec<String> = [
                    crate::views::track_album(item, album.as_ref()),
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
            | Change::Stopped
            | Change::SignedOut
            | Change::PlaylistFinished(_)
            | Change::Skipped(_)
            | Change::TrackFailed(_)
            | Change::Offer(_)
            | Change::IntroSkipped => {}
        }
    }
}

impl View {
    /// The album's cover, kept while the album stays the same.
    fn show_cover(&self, item: &Item, album: Option<&spoolframe_types::Album>) {
        let (id, artwork) = crate::views::cover_of(item, album);
        if self.cover_album.borrow().as_ref() == Some(&id) && self.cover_slot.child().is_some() {
            return;
        }
        let title = crate::views::track_album(item, album).unwrap_or_default();
        let cover = crate::views::tile(&id, &title, "now-cover", COVER);
        if let Some(art) = &self.art {
            art.show(
                &cover,
                artwork,
                &[ArtworkKind::Poster, ArtworkKind::Thumb],
                ArtworkSize::Large,
            );
        }
        self.cover_slot.set_child(Some(&cover));
        self.cover_album.replace(Some(id));
    }
}

/// "FLAC" from "Free Lossless Audio Codec (FLAC)", as GStreamer names it.
fn short_codec(codec: &str) -> String {
    match (codec.rfind('('), codec.rfind(')')) {
        (Some(open), Some(close)) if open < close => codec[open + 1..close].to_owned(),
        _ => codec.to_owned(),
    }
}

/// The path chip: "Direct · LAN" and the like, green for a direct path,
/// hidden while not connected.
fn show_path(chip: &gtk::Label, playback: &Playback) {
    use spoolframe_linux_core::backend::ConnectionPath;
    let path = playback.session().connection_path();
    let Some(label) = spoolframe_linux_core::format::path_chip(path) else {
        chip.set_visible(false);
        return;
    };
    chip.set_label(label);
    chip.set_visible(true);
    if matches!(path, ConnectionPath::Relay) {
        chip.remove_css_class("success");
    } else {
        chip.add_css_class("success");
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
            .css_classes(if large {
                vec!["transport", "large-transport"]
            } else {
                vec!["transport"]
            })
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
    Playlist(spoolframe_linux_core::backend::EntryId),
}

/// "Up next", in three sections: "Now playing", "Next in queue" (the
/// hand-added items; drag to reorder, × to remove, "Clear queue") and
/// "Next from: <album>". A click plays that item: a queued one leaves the
/// queue, a context one jumps the album there and keeps the queue.
struct Queue {
    aside: gtk::Box,
    /// "Playing from Low Tide", under Up next.
    from: gtk::Label,
    /// Draws the album covers on the rows.
    art: Option<Rc<crate::artwork::Art>>,
    list: gtk::ListBox,
    rows: Rc<RefCell<Vec<Row>>>,
    playback: std::rc::Weak<Playback>,
    /// Opens a track's artist or album, leaving Now Playing.
    go_to: crate::views::Open,
    add_to_playlist: Option<crate::views::AddToPlaylist>,
}

impl Queue {
    fn new(
        playback: &Rc<Playback>,
        go_to: crate::views::Open,
        art: Option<Rc<crate::artwork::Art>>,
        add_to_playlist: Option<crate::views::AddToPlaylist>,
    ) -> Self {
        let heading = gtk::Label::builder()
            .label("Up next")
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["title-3"])
            .build();
        let from = gtk::Label::builder()
            .xalign(0.0)
            .visible(false)
            .css_classes(["dim-label", "caption"])
            .build();
        let titles = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .margin_top(18)
            .margin_bottom(6)
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
                    Some(Row::Playlist(entry)) => playback.play_playlist_entry(entry),
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
            from,
            art,
            list,
            rows,
            playback: Rc::downgrade(playback),
            go_to,
            add_to_playlist,
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
        let album = playback.album();
        // The server's playlist (PX-40) over the album its track is from.
        let from = playback
            .playing_from()
            .map(|playing| playing.line())
            .or_else(|| album.as_ref().map(|a| format!("Playing from {}", a.title)));
        if let Some(from) = &from {
            self.from.set_label(from);
        }
        self.from.set_visible(from.is_some());

        if let Some(item) = playback.current() {
            self.heading("Now playing", false, &mut rows);
            let artist = artist_of(&item, album.as_ref());
            let row = self.append(
                &queue_row(&self.thumb(&item, album.as_ref()), &item, &artist, None),
                Row::Inert,
                &mut rows,
            );
            row.add_css_class("playing");
            let menu =
                current_track_menu(&self.playback, &self.go_to, self.add_to_playlist.as_ref());
            crate::views::context_menu(row.upcast_ref(), &menu);
        }

        if !up_next.queued.is_empty() {
            self.heading("Next in queue", true, &mut rows);
            for (index, queued) in up_next.queued.iter().enumerate() {
                let artist = artist_of(&queued.item, queued.album.as_ref());
                let handle = gtk::Image::builder()
                    .icon_name(crate::icons::DRAG)
                    .tooltip_text("Drag to reorder")
                    .css_classes(["dim-label"])
                    .build();
                let content = queue_row(
                    &self.thumb(&queued.item, queued.album.as_ref()),
                    &queued.item,
                    &artist,
                    Some(handle.upcast_ref()),
                );
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
            self.heading(&heading, false, &mut rows);
            for (pos, item) in up_next.context.iter() {
                let artist = artist_of(item, up_next.from.as_ref());
                let content = queue_row(
                    &self.thumb(item, up_next.from.as_ref()),
                    item,
                    &artist,
                    None,
                );
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
        if let Some((heading, entries)) = playback.playlist_up_next()
            && !entries.is_empty()
        {
            self.heading(&heading, false, &mut rows);
            for entry in entries {
                let artist = artist_of(&entry.item, None);
                let content = queue_row(&self.thumb(&entry.item, None), &entry.item, &artist, None);
                let row = self.append(&content, Row::Playlist(entry.entry), &mut rows);
                let id = entry.entry;
                let next_item = entry.item.clone();
                let queued_item = entry.item.clone();
                let mut groups = vec![vec![
                    self.entry("Play now", crate::icons::PLAY, move |p| {
                        p.play_playlist_entry(id)
                    }),
                    self.entry("Play next", crate::icons::PLAY_NEXT, move |p| {
                        p.play_next(vec![next_item.clone()], None)
                    }),
                    self.entry("Add to queue", crate::icons::ADD_TO_QUEUE, move |p| {
                        p.add_to_queue(vec![queued_item.clone()], None)
                    }),
                ]];
                if let Item::Track(track) = &entry.item {
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

    /// A section's heading; the queue's carries Clear, as designed.
    fn heading(&self, text: &str, clear: bool, rows: &mut Vec<Row>) {
        let label = gtk::Label::builder()
            .label(text.to_uppercase())
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["queue-heading", "dim-label"])
            .build();
        let line = gtk::Box::builder().spacing(8).margin_top(10).build();
        line.append(&label);
        if clear {
            let button = gtk::Button::builder()
                .child(
                    &adw::ButtonContent::builder()
                        .icon_name(crate::icons::CLEAR_RECENT)
                        .label("Clear")
                        .build(),
                )
                .valign(gtk::Align::Center)
                .css_classes(["queue-clear"])
                .build();
            let weak = self.playback.clone();
            button.connect_clicked(move |_| {
                if let Some(playback) = weak.upgrade() {
                    playback.clear_queue();
                }
            });
            line.append(&button);
        }
        let row = self.append(&line, Row::Inert, rows);
        row.set_activatable(false);
    }

    /// A row's album cover, 40 px, in place of a number.
    fn thumb(&self, item: &Item, album: Option<&spoolframe_types::Album>) -> gtk::Widget {
        if album.is_none() && !matches!(item, Item::Track(_)) {
            return number("").upcast();
        }
        let (id, artwork) = crate::views::cover_of(item, album);
        let tile = crate::views::tile(&id, "", "queue-thumb", (40, 40));
        if let Some(art) = &self.art {
            art.show(
                &tile,
                artwork,
                &[ArtworkKind::Poster, ArtworkKind::Thumb],
                ArtworkSize::Small,
            );
        }
        tile.set_valign(gtk::Align::Center);
        tile.upcast()
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

fn artist_of(item: &Item, album: Option<&spoolframe_types::Album>) -> String {
    crate::views::track_artist(item, album).unwrap_or_default()
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
    end: Option<&gtk::Widget>,
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

/// Resolve the current item when an action is invoked: the large cover
/// and title stay alive while the player advances to another song.
fn current_track_menu(
    playback: &std::rc::Weak<Playback>,
    open: &crate::views::Open,
    add: Option<&crate::views::AddToPlaylist>,
) -> gtk::Popover {
    let weak = playback.clone();
    let mut groups = vec![crate::views::enqueue_entries(move |at| {
        if let Some(p) = weak.upgrade()
            && let Some(item) = p.current()
        {
            match at {
                Enqueue::Next => p.play_next(vec![item], p.album()),
                Enqueue::Last => p.add_to_queue(vec![item], p.album()),
            }
        }
    })];
    let mut navigation = Vec::new();
    for (label, icon, album) in [
        ("Go to artist", crate::icons::ARTIST, false),
        ("Go to album", crate::icons::ALBUM, true),
    ] {
        let (weak, open) = (playback.clone(), open.clone());
        navigation.push(crate::views::MenuEntry::new(label, icon, move || {
            if let Some(p) = weak.upgrade()
                && let Some(Item::Track(track)) = p.current()
            {
                open(if album {
                    crate::views::album_of(&track)
                } else {
                    crate::views::artist_of(&track)
                });
            }
        }));
    }
    groups.push(navigation);
    if let Some(add) = add {
        let (weak, add) = (playback.clone(), add.clone());
        groups.push(vec![crate::views::MenuEntry::new(
            "Add to playlist…",
            crate::icons::PLAYLISTS,
            move || {
                if let Some(p) = weak.upgrade()
                    && let Some(item) = p.current()
                {
                    add(item);
                }
            },
        )]);
    }
    crate::views::menu_popover(groups)
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
