//! The bar along the bottom of the window while the player is minimized,
//! as in the design: what plays (the live picture for video, the cover for
//! music; a click opens the full player), the transport with a seek bar,
//! and volume, expand and stop. The library stays usable above it.
//!
//! The bar is built once; its buttons act on whichever playback is
//! attached.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use spoolframe_linux_core::backend::ArtworkSize;
use spoolframe_linux_core::format;
use spoolframe_types::{ArtworkKind, Item};

use crate::nowplaying::Transport;
use crate::playback::{Change, Playback};
use crate::player::{mono, round_button};
use crate::views::Cards;
use crate::volume::Volume;

/// The live picture's size in the bar.
const THUMB: (i32, i32) = (128, 72);
/// A cover's size in the bar.
const COVER: i32 = 52;
/// Stable space for artwork, track details and the heart.
const NOW_PLAYING_WIDTH: i32 = 340;

type Current = Rc<RefCell<Option<Rc<Playback>>>>;

pub struct MiniBar {
    /// Hidden while nothing plays.
    pub revealer: gtk::Revealer,
    thumb: adw::Bin,
    title: gtk::Label,
    detail: gtk::Label,
    /// Video's transport: 10 s back, play, 30 s forward.
    video_transport: gtk::Box,
    toggle: gtk::Button,
    /// Music's, made for each playback: shuffle, previous, play, next,
    /// repeat.
    music_slot: adw::Bin,
    music: RefCell<Option<Transport>>,
    scale: gtk::Scale,
    position: gtk::Label,
    length: gtk::Label,
    volume: Volume,
    tracks: gtk::MenuButton,
    bar: gtk::CenterBox,
    /// Video ends here; music has none on the board, and Queue in its place.
    stop: gtk::Button,
    queue: gtk::Button,
    /// The audio and subtitle stream counts the menu was built for.
    tracks_built: Cell<Option<(usize, usize)>>,
    current: Current,
    /// Draws a music cover; set on attach.
    art: RefCell<Option<Rc<crate::artwork::Art>>>,
    /// The album whose cover shows, so the next track of it keeps it.
    cover_album: RefCell<Option<spoolframe_types::MediaId>>,
    /// Bumped on each attach and detach, so an old playback's listener
    /// lets go.
    generation: Rc<Cell<u64>>,
    /// The playing track's heart (PX-43), beside its title; empty for
    /// video and without Liked.
    heart: adw::Bin,
    follow: RefCell<Option<crate::hearts::Follow>>,
}

impl MiniBar {
    /// `expand` opens the full player; `stop` ends playback.
    pub fn new(expand: Rc<dyn Fn()>, stop: Rc<dyn Fn()>) -> Rc<Self> {
        let current: Current = Rc::default();
        let on = |current: &Current, run: fn(&Rc<Playback>)| {
            let current = current.clone();
            move || {
                let playback = current.borrow().clone();
                if let Some(playback) = playback {
                    run(&playback);
                }
            }
        };

        let thumb = adw::Bin::builder()
            .valign(gtk::Align::Center)
            .css_classes(["mini-thumb"])
            .build();
        let title = gtk::Label::builder()
            .xalign(0.0)
            .single_line_mode(true)
            .css_classes(["heading"])
            .build();
        let detail = gtk::Label::builder()
            .xalign(0.0)
            .single_line_mode(true)
            .css_classes(["dim-label", "caption"])
            .build();
        let titles = gtk::Box::builder()
            .hexpand(true)
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .build();
        titles.append(&scrolling_label(&title));
        titles.append(&scrolling_label(&detail));
        let what = gtk::Box::builder().spacing(12).build();
        what.append(&thumb);
        what.append(&titles);
        // The picture and the titles open the full player.
        let open = gtk::Button::builder()
            .hexpand(true)
            .child(&what)
            .tooltip_text("Back to the full player")
            // Never focused, so Space (play/pause) cannot press it.
            .focusable(false)
            .css_classes(["flat", "mini-open"])
            .build();
        {
            let expand = expand.clone();
            open.connect_clicked(move |_| expand());
        }
        let expand_for_queue = expand.clone();

        let back = round_button("Back 10 seconds", crate::icons::BACK_10, &["flat"]);
        let toggle = round_button("Pause", crate::icons::PAUSE, &["player-play"]);
        let forward = round_button("Forward 30 seconds", crate::icons::FORWARD_30, &["flat"]);
        let wire = |button: &gtk::Button, run: fn(&Rc<Playback>)| {
            let run = on(&current, run);
            button.connect_clicked(move |_| run());
        };
        wire(&back, |p| p.jump(-10.0));
        wire(&toggle, |p| p.toggle());
        wire(&forward, |p| p.jump(30.0));
        let video_transport = gtk::Box::builder()
            .spacing(10)
            .halign(gtk::Align::Center)
            .build();
        for button in [&back, &toggle, &forward] {
            video_transport.append(button);
        }
        let music_slot = adw::Bin::builder().halign(gtk::Align::Center).build();

        let scale = {
            let current = current.clone();
            crate::transport::seek_scale(move |to| {
                if let Some(playback) = current.borrow().as_ref() {
                    playback.seek(to);
                }
            })
        };
        let position = mono("0:00");
        let length = mono("0:00");
        for label in [&position, &length] {
            label.add_css_class("dim-label");
            label.add_css_class("mini-time");
        }
        // Let the seek shrink in narrow windows. The fixed track slot,
        // timestamps and volume controls already consume most of the
        // width; a 200 px seek forced the whole window above 849 px.
        scale.set_hexpand(true);
        scale.set_width_request(120);
        let seek_row = gtk::Box::builder().spacing(10).hexpand(true).build();
        seek_row.append(&position);
        seek_row.append(&scale);
        seek_row.append(&length);
        let centre = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .valign(gtk::Align::Center)
            .build();
        centre.append(&video_transport);
        centre.append(&music_slot);
        centre.append(&seek_row);

        let volume = {
            let current = current.clone();
            Volume::new(Rc::new(move || current.borrow().clone()), 96)
        };
        let expand_button = gtk::Button::builder()
            .icon_name(crate::icons::UP)
            .tooltip_text("Back to the full player")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        expand_button.connect_clicked(move |_| expand());
        let stop_button = gtk::Button::builder()
            .icon_name(crate::icons::CLOSE)
            .tooltip_text("Stop")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "dim-label", "mini-button"])
            .build();
        stop_button.connect_clicked(move |_| stop());
        // Music's queue is on Now Playing.
        let queue_button = gtk::Button::builder()
            .icon_name(crate::icons::PLAY_NEXT)
            .tooltip_text("Queue")
            .valign(gtk::Align::Center)
            .visible(false)
            .css_classes(["flat", "dim-label", "mini-button"])
            .build();
        {
            let expand = expand_for_queue.clone();
            queue_button.connect_clicked(move |_| expand());
        }
        // Video's audio and subtitles, built when its streams are known.
        let tracks = gtk::MenuButton::builder()
            .direction(gtk::ArrowType::Up)
            .icon_name(crate::icons::TRACKS)
            .tooltip_text("Audio and subtitles")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "dim-label", "mini-button"])
            .visible(false)
            .build();
        let end = gtk::Box::builder()
            .spacing(6)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Center)
            .build();
        volume.row.add_css_class("dim-label");
        end.append(&volume.row);
        end.append(&tracks);
        end.append(&queue_button);
        end.append(&expand_button);
        end.append(&stop_button);

        // The heart is beside the titles, not in their button: a button
        // in a button takes no clicks of its own.
        let heart = adw::Bin::builder().valign(gtk::Align::Center).build();
        // The scrolling labels do not request their full text width;
        // this slot stays the same size as the current text changes.
        let start = gtk::Box::builder()
            .spacing(4)
            .width_request(NOW_PLAYING_WIDTH)
            .hexpand(false)
            .halign(gtk::Align::Start)
            .build();
        start.append(&open);
        start.append(&heart);
        let bar = gtk::CenterBox::builder()
            .start_widget(&start)
            // Grows with the window up to the board's 520 px.
            .center_widget(
                &adw::Clamp::builder()
                    .maximum_size(520)
                    .tightening_threshold(520)
                    .hexpand(true)
                    .child(&centre)
                    .build(),
            )
            .end_widget(&end)
            .css_classes(["mini-bar"])
            .build();
        let revealer = gtk::Revealer::builder()
            .child(&bar)
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .reveal_child(false)
            .build();
        Rc::new(Self {
            revealer,
            thumb,
            title,
            detail,
            video_transport,
            toggle,
            music_slot,
            music: RefCell::new(None),
            scale,
            position,
            length,
            volume,
            tracks,
            bar,
            stop: stop_button,
            queue: queue_button,
            tracks_built: Cell::new(None),
            current,
            art: RefCell::new(None),
            cover_album: RefCell::new(None),
            generation: Rc::new(Cell::new(0)),
            heart,
            follow: RefCell::new(None),
        })
    }

    /// Shows `playback` in the bar. `cards` draws a music cover.
    pub fn attach(self: &Rc<Self>, playback: &Rc<Playback>, cards: &Cards) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.current.replace(Some(playback.clone()));

        let video = playback.is_video();
        if video {
            // Wrapped: the video's own size would size the bar.
            let picture = gtk::Picture::builder()
                .paintable(&crate::artwork::Unsized::new(playback.paintable()))
                .content_fit(gtk::ContentFit::Fill)
                .width_request(THUMB.0)
                .height_request(THUMB.1)
                .css_classes(["mini-video"])
                .build();
            // The picture opens the player: a corner mark says so.
            let badge = gtk::Image::builder()
                .icon_name(crate::icons::FULLSCREEN)
                .pixel_size(12)
                .width_request(22)
                .height_request(22)
                .halign(gtk::Align::End)
                .valign(gtk::Align::End)
                .margin_end(6)
                .margin_bottom(6)
                .css_classes(["mini-expand-badge"])
                .build();
            let overlay = gtk::Overlay::builder().child(&picture).build();
            overlay.add_overlay(&badge);
            self.thumb.set_child(Some(&overlay));
        } else {
            // Music's cover is set on each track (see `show_cover`).
            self.thumb.set_child(None::<&gtk::Widget>);
        }
        self.art.replace(cards.art.clone());
        self.cover_album.replace(None);
        let heart = cards
            .hearts
            .as_ref()
            .filter(|_| !video)
            .map(|h| h.following());
        self.heart
            .set_child(heart.as_ref().map(|(button, _)| button));
        self.follow.replace(heart.map(|(_, follow)| follow));
        // Video jumps within the film; music skips between tracks, with
        // shuffle and repeat.
        self.video_transport.set_visible(video);
        self.tracks.set_visible(video);
        self.stop.set_visible(video);
        self.queue.set_visible(!video);
        if video {
            self.bar.remove_css_class("music");
        } else {
            self.bar.add_css_class("music");
        }
        self.tracks.set_popover(None::<&gtk::Popover>);
        self.tracks_built.set(None);
        let music = (!video).then(|| Transport::new(playback, false));
        self.music_slot.set_child(music.as_ref().map(|t| &t.row));
        self.music.replace(music);

        let this = Rc::downgrade(self);
        let weak_playback = Rc::downgrade(playback);
        playback.subscribe(move |change| {
            let (Some(bar), Some(playback)) = (this.upgrade(), weak_playback.upgrade()) else {
                return false;
            };
            if bar.generation.get() != generation {
                return false;
            }
            bar.update(&playback, change);
            true
        });
        self.revealer.set_reveal_child(true);
    }

    /// Plays or pauses what the bar shows; false when it shows nothing.
    pub fn toggle(&self) -> bool {
        let playback = self.current.borrow().clone();
        playback.map(|p| p.toggle()).is_some()
    }

    /// Hides the bar and lets go of the playback.
    pub fn detach(&self) {
        self.generation.set(self.generation.get() + 1);
        self.current.replace(None);
        self.thumb.set_child(None::<&gtk::Widget>);
        self.music_slot.set_child(None::<&gtk::Widget>);
        self.heart.set_child(None::<&gtk::Widget>);
        self.follow.replace(None);
        self.music.replace(None);
        self.revealer.set_reveal_child(false);
    }

    /// The album's cover, for music: a queued track may be from another
    /// album than the one before it (P1-23).
    fn show_cover(&self, item: &Item, playback: &Playback) {
        let album = playback.album();
        let (id, artwork) = crate::views::cover_of(item, album.as_ref());
        if self.cover_album.borrow().as_ref() == Some(&id) && self.thumb.child().is_some() {
            return;
        }
        let cover = crate::views::tile(&id, "", "mini-cover", (COVER, COVER));
        if let Some(art) = self.art.borrow().as_ref() {
            art.show(
                &cover,
                artwork,
                &[ArtworkKind::Poster, ArtworkKind::Thumb],
                ArtworkSize::Small,
            );
        }
        self.thumb.set_child(Some(&cover));
        self.cover_album.replace(Some(id));
    }

    fn update(&self, playback: &Playback, change: &Change) {
        match change {
            Change::Item(item) => {
                if !playback.is_video() {
                    self.show_cover(item, playback);
                }
                if let Some(follow) = self.follow.borrow().as_ref() {
                    follow(match item {
                        Item::Track(t) => Some(t.id.clone()),
                        _ => None,
                    });
                }
                let facts = crate::views::Facts::of(item);
                self.title.set_label(&facts.title);
                self.title.set_tooltip_text(Some(&facts.title));
                let detail = match (item, playback.album()) {
                    (Item::Track(_), album) => Some(
                        [
                            crate::views::track_artist(item, album.as_ref()),
                            crate::views::track_album(item, album.as_ref()),
                        ]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" · "),
                    )
                    .filter(|d| !d.is_empty()),
                    // "1979 · Movie"; "Severance · S2 · E4".
                    (Item::Movie(_), _) => Some(
                        [facts.detail.clone(), Some("Movie".to_owned())]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" · "),
                    ),
                    (Item::Episode(_), _) => Some(
                        [facts.parent.clone(), facts.detail.clone()]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" · "),
                    ),
                    _ => facts.detail.clone(),
                };
                let detail = crate::player::with_playlist(detail, playback.source_label());
                self.detail.set_label(detail.as_deref().unwrap_or_default());
                self.detail.set_tooltip_text(detail.as_deref());
                if let Some(music) = self.music.borrow().as_ref() {
                    music.show_item(playback);
                }
            }
            Change::Playing(playing) => {
                crate::transport::show_playing(&self.toggle, *playing);
                if let Some(music) = self.music.borrow().as_ref() {
                    music.show_playing(*playing);
                }
            }
            Change::Modes { shuffle, repeat } => {
                if let Some(music) = self.music.borrow().as_ref() {
                    music.show_modes(*shuffle, *repeat);
                    // Repeat all gives the last track a next.
                    music.show_item(playback);
                }
            }
            // What comes next changed: Next may have something now.
            Change::Queue => {
                if let Some(music) = self.music.borrow().as_ref() {
                    music.show_item(playback);
                }
            }
            Change::Position { at, length } => {
                crate::transport::show_position(&self.scale, *at, *length);
                self.position.set_label(&format::timestamp(*at));
                self.length.set_label(&format::timestamp(*length));
            }
            Change::Volume(_) | Change::Muted(_) => self.volume.show(change),
            Change::MediaInfo(info) if playback.is_video() => {
                let counts = (info.audio_streams().len(), info.subtitle_streams().len());
                if counts != (0, 0) && self.tracks_built.replace(Some(counts)) != Some(counts) {
                    self.tracks.set_popover(Some(&playback.track_menu(info)));
                }
            }
            Change::Starting
            | Change::MediaInfo(_)
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

/// Clip long text to its slot and gently pan back and forth, pausing at
/// either end. The viewport never contributes the text's natural width.
/// The pan runs on the frame clock only while the text overflows and is
/// shown: text that fits costs no frames.
fn scrolling_label(label: &gtk::Label) -> gtk::ScrolledWindow {
    let viewport = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::External)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(false)
        .min_content_width(1)
        .hexpand(true)
        .can_focus(false)
        .child(label)
        .build();
    let pan = Rc::new(Pan::default());
    let update = {
        let (pan, viewport) = (pan.clone(), viewport.downgrade());
        move || {
            if let Some(viewport) = viewport.upgrade() {
                pan.update(&pan, &viewport);
            }
        }
    };
    {
        let (pan, update) = (pan.clone(), update.clone());
        let adjustment = viewport.hadjustment();
        label.connect_label_notify(move |_| {
            pan.start.set(None);
            adjustment.set_value(0.0);
            update();
        });
    }
    {
        // The text's width or the slot's changed.
        let update = update.clone();
        viewport.hadjustment().connect_changed(move |_| update());
    }
    viewport.connect_map(move |_| update());
    viewport.connect_unmap(move |viewport| pan.stop(viewport));
    viewport
}

/// A scrolling label's pan: when it began, and its tick while it runs.
#[derive(Default)]
struct Pan {
    start: Cell<Option<i64>>,
    previous_range: Cell<f64>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
}

impl Pan {
    /// How far the text overflows, if it is to pan at all.
    fn overflow(viewport: &gtk::ScrolledWindow) -> Option<f64> {
        let adjustment = viewport.hadjustment();
        let range = (adjustment.upper() - adjustment.page_size()).max(0.0);
        let animations = viewport.settings().is_gtk_enable_animations();
        (animations && range > 0.5 && viewport.is_mapped()).then_some(range)
    }

    /// Starts the tick if the text overflows now, or stops it.
    fn update(&self, me: &Rc<Self>, viewport: &gtk::ScrolledWindow) {
        if Self::overflow(viewport).is_none() {
            return self.stop(viewport);
        }
        if self.tick.borrow().is_some() {
            return;
        }
        let pan = Rc::downgrade(me);
        let tick = viewport.add_tick_callback(move |viewport, clock| {
            let Some(pan) = pan.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            let Some(range) = Self::overflow(viewport) else {
                // Returning Break removes it: the id is only let go.
                pan.tick.take();
                pan.start.set(None);
                viewport.hadjustment().set_value(0.0);
                return gtk::glib::ControlFlow::Break;
            };
            if (pan.previous_range.replace(range) - range).abs() > 0.5 {
                pan.start.set(None);
            }
            let now = clock.frame_time();
            let began = pan.start.get().unwrap_or_else(|| {
                pan.start.set(Some(now));
                now
            });
            let elapsed = (now - began) as f64 / 1_000_000.0;
            viewport.hadjustment().set_value(pan_offset(elapsed, range));
            gtk::glib::ControlFlow::Continue
        });
        self.tick.replace(Some(tick));
    }

    fn stop(&self, viewport: &gtk::ScrolledWindow) {
        if let Some(tick) = self.tick.take() {
            tick.remove();
        }
        self.start.set(None);
        viewport.hadjustment().set_value(0.0);
    }
}

/// Where the pan is `elapsed` seconds in, over text `range` px too wide:
/// a pause, 28 px a second along, a pause, and back.
fn pan_offset(elapsed: f64, range: f64) -> f64 {
    let pause = 1.8;
    let travel = range / 28.0;
    let phase = elapsed % (2.0 * (pause + travel));
    let offset = if phase < pause {
        0.0
    } else if phase < pause + travel {
        (phase - pause) * 28.0
    } else if phase < 2.0 * pause + travel {
        range
    } else {
        range - (phase - 2.0 * pause - travel) * 28.0
    };
    offset.clamp(0.0, range)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs a GTK display"]
    fn player_bar_fits_an_849_pixel_window() {
        adw::init().unwrap();
        let provider = gtk::CssProvider::new();
        provider.load_from_string(include_str!("style.css"));
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().unwrap(),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let bar = MiniBar::new(Rc::new(|| {}), Rc::new(|| {}));
        bar.title
            .set_label("A very long title that must never resize the player controls");
        bar.detail
            .set_label("Artist · Album · Playing from a very long playlist name");
        bar.thumb.set_child(Some(
            &gtk::Box::builder()
                .width_request(128)
                .height_request(72)
                .build(),
        ));
        bar.position.set_label("1:23:45");
        bar.length.set_label("2:34:56");
        let minimum = bar.bar.measure(gtk::Orientation::Horizontal, -1).0;
        assert!(minimum <= 849, "player bar requests {minimum}px");
    }
}
