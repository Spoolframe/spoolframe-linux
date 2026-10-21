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
use crate::playback::{Change, Place, Playback, Repeat};
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
    use crate::visualizer::{Canvas, Choice, Mode};
    let stack = gtk::Stack::builder().hexpand(true).vexpand(true).build();
    // Hidden queue contents must not impose their minimum size on the visualizer.
    stack.set_hhomogeneous(false);
    stack.set_vhomogeneous(false);
    stack.add_named(normal, Some("cover"));
    let measurements = playback.measurements();
    let canvas = Canvas::new(measurements.clone().unwrap_or_default());
    let canvas_art = CanvasArt::new(art, &canvas);
    {
        let canvas_art = Rc::downgrade(&canvas_art);
        canvas.connect_map(move |_| {
            if let Some(canvas_art) = canvas_art.upgrade() {
                canvas_art.refresh();
            }
        });
    }
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
                playback.set_visualizer_trails(button.is_active());
            }
        });
    }

    let plugins = crate::visualizer::plugins();
    let mut labels = vec!["Album & queue".to_owned()];
    let mut choices = vec![Choice::Cover];
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
            choices.push(Choice::Canvas(mode));
        }
    }
    for (name, label) in plugins {
        labels.push(label);
        choices.push(Choice::Effect(name));
    }
    let selector =
        gtk::DropDown::from_strings(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    selector.set_tooltip_text(Some("Visualizer"));
    selector.set_sensitive(choices.len() > 1);
    header.pack_end(&selector);
    let apply: Rc<dyn Fn(&gtk::DropDown)> = {
        let weak = Rc::downgrade(playback);
        let stack = stack.downgrade();
        let canvas = canvas.downgrade();
        let visuals = visuals.downgrade();
        let canvas_art = Rc::downgrade(&canvas_art);
        Rc::new(move |selector| {
            let (Some(playback), Some(stack), Some(canvas), Some(visuals)) = (
                weak.upgrade(),
                stack.upgrade(),
                canvas.upgrade(),
                visuals.upgrade(),
            ) else {
                return;
            };
            let Some(choice) = choices.get(selector.selected() as usize) else {
                return;
            };
            // Every effect offered is a scope (see `plugins`).
            let (spectrum, scope) = (
                *choice == Choice::Canvas(Mode::Spectrum),
                matches!(choice, Choice::Effect(_)),
            );
            peaks.set_visible(spectrum);
            trails.set_visible(scope);
            options.set_visible(spectrum || scope);
            if !playback.set_visualizer(Some(choice)) {
                selector.set_selected(0);
                return;
            }
            if let Choice::Canvas(mode) = choice {
                canvas.set_mode(*mode);
                if let Some(canvas_art) = canvas_art.upgrade() {
                    canvas_art.refresh();
                }
            }
            visuals.set_visible_child_name(match choice {
                Choice::Canvas(_) => "custom",
                _ => "gstreamer",
            });
            stack.set_visible_child_name(match choice {
                Choice::Cover => "cover",
                _ => "visualizer",
            });
        })
    };
    {
        let apply = apply.clone();
        selector.connect_selected_notify(move |selector| apply(selector));
    }
    let controls = Rc::downgrade(&transport);
    {
        // The page owns the controls over the visualizer, for its life.
        let (selector, weak) = (selector.downgrade(), Rc::downgrade(playback));
        stack.connect_map(move |_| {
            if let Some(selector) = selector.upgrade() {
                apply(&selector);
            }
            if let Some(playback) = weak.upgrade() {
                transport.show_item(&playback);
            }
        });
        let weak = Rc::downgrade(playback);
        stack.connect_unmap(move |_| {
            if let Some(playback) = weak.upgrade() {
                playback.set_visualizer(None);
            }
        });
    }
    let weak = Rc::downgrade(playback);
    let canvas = canvas.downgrade();
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
                canvas.new_song();
                let album = playback.album();
                canvas_art.track(crate::views::cover_of(item, album.as_ref()).1.to_vec());
            }
            Change::VisualizerStopped => {
                if let Some(selector) = selector.upgrade() {
                    selector.set_selected(0);
                }
            }
            Change::Queue => controls.show_item(&playback),
            Change::Playing(playing) => {
                controls.show_playing(*playing);
                canvas.set_playing(*playing);
            }
            Change::Modes { shuffle, repeat } => controls.show_modes(*shuffle, *repeat),
            _ => {}
        }
        true
    });
    stack
}

/// What the visualizer's canvas draws of the song's cover: the cover
/// (large) and the tint of its colours (from the small one, about for the
/// queue's rows already). Each is read only while the canvas shows a mode
/// that draws it, and kept while the cover stays the same.
struct CanvasArt {
    art: Option<Rc<Art>>,
    canvas: gtk::glib::WeakRef<crate::visualizer::Canvas>,
    /// The song's artwork.
    artwork: RefCell<Vec<spoolframe_types::Artwork>>,
    /// The artwork the canvas's cover and tint are from, or are being
    /// read from.
    cover: RefCell<Option<spoolframe_types::ArtworkId>>,
    tint: RefCell<Option<spoolframe_types::ArtworkId>>,
}

/// Which of a song's artwork the canvas draws.
const CANVAS_ART: [ArtworkKind; 2] = [ArtworkKind::Poster, ArtworkKind::Thumb];

impl CanvasArt {
    fn new(art: Option<Rc<Art>>, canvas: &crate::visualizer::Canvas) -> Rc<Self> {
        Rc::new(Self {
            art,
            canvas: canvas.downgrade(),
            artwork: RefCell::default(),
            cover: RefCell::default(),
            tint: RefCell::default(),
        })
    }

    /// A new song, with its `artwork`.
    fn track(self: &Rc<Self>, artwork: Vec<spoolframe_types::Artwork>) {
        self.artwork.replace(artwork);
        self.refresh();
    }

    /// Reads what the canvas's mode draws of the song's cover, if it
    /// shows and does not have it.
    fn refresh(self: &Rc<Self>) {
        let Some(canvas) = self.canvas.upgrade().filter(|c| c.is_mapped()) else {
            return;
        };
        let id = crate::artwork::first_of(&self.artwork.borrow(), &CANVAS_ART);
        if canvas.mode().draws_cover() {
            self.load(&canvas, Part::Cover, id.clone());
        }
        if canvas.mode().tinted() {
            self.load(&canvas, Part::Tint, id);
        }
    }

    fn slot(&self, part: Part) -> &RefCell<Option<spoolframe_types::ArtworkId>> {
        match part {
            Part::Cover => &self.cover,
            Part::Tint => &self.tint,
        }
    }

    /// `part` from artwork `id`, unless the canvas has it or is getting
    /// it; until then, none.
    fn load(
        self: &Rc<Self>,
        canvas: &crate::visualizer::Canvas,
        part: Part,
        id: Option<spoolframe_types::ArtworkId>,
    ) {
        if *self.slot(part).borrow() == id {
            return;
        }
        self.slot(part).replace(id.clone());
        part.show(canvas, None);
        let (Some(art), Some(id)) = (self.art.clone(), id) else {
            return;
        };
        let size = match part {
            Part::Cover => ArtworkSize::Large,
            Part::Tint => ArtworkSize::Small,
        };
        let (this, artwork) = (Rc::downgrade(self), self.artwork.borrow().clone());
        gtk::glib::spawn_future_local(async move {
            let read = art.bytes(&artwork, &CANVAS_ART, size).await;
            let Some(this) = this.upgrade() else { return };
            if this.slot(part).borrow().as_ref() != Some(&id) {
                return;
            }
            let texture = read.and_then(|(_, bytes)| crate::artwork::decode(&bytes).ok());
            if texture.is_none() {
                // Asked for again when next wanted.
                this.slot(part).replace(None);
            }
            if let Some(canvas) = this.canvas.upgrade() {
                part.show(&canvas, texture);
            }
        });
    }
}

/// What [`CanvasArt`] reads of a cover.
#[derive(Clone, Copy)]
enum Part {
    Cover,
    Tint,
}

impl Part {
    fn show(self, canvas: &crate::visualizer::Canvas, texture: Option<gtk::gdk::Texture>) {
        match self {
            Part::Cover => canvas.set_cover(texture),
            Part::Tint => canvas.set_tint(texture.as_ref()),
        }
    }
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
            | Change::VisualizerStopped
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
    let Some(path) = playback.session().map(|s| s.connection_path()) else {
        chip.set_visible(false);
        return;
    };
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
        {
            // Hidden, Next is not kept up to date; it is as it shows.
            let (weak, next) = (Rc::downgrade(playback), next.downgrade());
            row.connect_map(move |_| {
                if let (Some(playback), Some(next)) = (weak.upgrade(), next.upgrade()) {
                    next.set_sensitive(playback.has_next());
                }
            });
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

    /// Whether Next plays something, while the controls show.
    pub fn show_item(&self, playback: &Playback) {
        if self.row.is_mapped() {
            self.next.set_sensitive(playback.has_next());
        }
    }

    pub fn show_playing(&self, playing: bool) {
        crate::transport::show_playing(&self.toggle, playing);
    }

    pub fn show_modes(&self, shuffle: bool, repeat: Repeat) {
        self.syncing.set(true);
        self.shuffle.set_active(shuffle);
        self.repeat.set_active(repeat != Repeat::Off);
        self.syncing.set(false);
        let tip = match repeat {
            Repeat::Off => "Repeat",
            Repeat::All => "Repeating the queue",
            Repeat::One => "Repeating this track",
        };
        self.repeat.set_icon_name(repeat.icon());
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
    /// What follows the queue: the context's or the playlist's.
    Ahead(Place),
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
                    Some(Row::Ahead(place)) => playback.play_ahead(place),
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
            .source_label()
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
                let (weak, go_to) = (self.playback.clone(), self.go_to.clone());
                let (id, item) = (queued.id, queued.item.clone());
                crate::views::lazy_context_menu(row.upcast_ref(), move || {
                    let mut groups = vec![vec![entry(
                        &weak,
                        "Play now",
                        crate::icons::PLAY,
                        move |p| p.play_queued(id),
                    )]];
                    if let Item::Track(track) = &item {
                        groups.push(crate::views::track_go_to_entries(track, &go_to, true));
                    }
                    groups.push(vec![crate::views::MenuEntry {
                        destructive: true,
                        ..entry(&weak, "Remove from queue", crate::icons::REMOVE, move |p| {
                            p.remove_queued(id)
                        })
                    }]);
                    crate::views::menu_popover(groups)
                });
            }
        }

        for ahead in &up_next.ahead {
            self.heading(&ahead.heading, false, &mut rows);
            for (place, item) in &ahead.entries {
                let artist = artist_of(item, ahead.album.as_ref());
                let content =
                    queue_row(&self.thumb(item, ahead.album.as_ref()), item, &artist, None);
                let row = self.append(&content, Row::Ahead(*place), &mut rows);
                // Right-click: play it now, or queue it next; a
                // playlist's entry can go to the end of the queue too.
                let (weak, go_to) = (self.playback.clone(), self.go_to.clone());
                let (place, item, from) = (*place, item.clone(), ahead.album.clone());
                crate::views::lazy_context_menu(row.upcast_ref(), move || {
                    let (next, from) = (item.clone(), from.clone());
                    let mut play = vec![
                        entry(&weak, "Play now", crate::icons::PLAY, move |p| {
                            p.play_ahead(place)
                        }),
                        entry(&weak, "Play next", crate::icons::PLAY_NEXT, move |p| {
                            p.play_next(vec![next.clone()], from.clone())
                        }),
                    ];
                    if matches!(place, Place::Playlist(_)) {
                        let last = item.clone();
                        play.push(entry(
                            &weak,
                            "Add to queue",
                            crate::icons::ADD_TO_QUEUE,
                            move |p| p.add_to_queue(vec![last.clone()], None),
                        ));
                    }
                    let mut groups = vec![play];
                    if let Item::Track(track) = &item {
                        groups.push(crate::views::track_go_to_entries(track, &go_to, true));
                    }
                    crate::views::menu_popover(groups)
                });
            }
            if let Some(note) = ahead.note {
                let label = gtk::Label::builder()
                    .label(note)
                    .xalign(0.0)
                    .wrap(true)
                    .margin_top(4)
                    .margin_bottom(4)
                    .css_classes(["dim-label"])
                    .build();
                let row = self.append(&label, Row::Inert, &mut rows);
                row.set_activatable(false);
            }
        }
        self.rows.replace(rows);
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

/// A menu row that runs `run` on the playback, while there is one.
fn entry(
    playback: &std::rc::Weak<Playback>,
    label: &'static str,
    icon: &'static str,
    run: impl Fn(&Rc<Playback>) + 'static,
) -> crate::views::MenuEntry {
    let weak = playback.clone();
    crate::views::MenuEntry::new(label, icon, move || {
        if let Some(playback) = weak.upgrade() {
            run(&playback);
        }
    })
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
